//! reqwest HTTP backend with a one-shot HTTP/2 → HTTP/1.1 fallback (P0-3).
//!
//! Some providers / CDN edges negotiate HTTP/2 via ALPN but then break at the
//! protocol level (preface or GOAWAY family failures), which reqwest surfaces
//! as opaque connection errors with no recovery. Upstream's fix (0.9.10) was
//! to retry once over HTTP/1.1 when the HTTP/2 attempt fails at the preface
//! stage. This module wraps that policy as an [`HttpClientExt`] backend that
//! the provider factories inject into rig's `ClientBuilder`, so every rig
//! request gets the fallback without touching the engine.
//!
//! Policy: delegate to a normal pooled client (HTTP/2 negotiable, exactly
//! reqwest's default behaviour). The first time a request fails with an error
//! that looks like an HTTP/2 protocol failure, flip a sticky `h2_degraded`
//! flag and replay that request once over an HTTP/1.1-only client; every
//! later request goes straight to HTTP/1.1. Only connection-establishment /
//! execute errors are considered — mid-stream degradation is a different
//! failure with its own recovery (the P0-3 termination proof + transparent
//! retry in the engine).
//!
//! Detection is string matching over the rendered error chain: the `h2`
//! crate's error type is not a direct dependency, so it cannot be matched
//! structurally. A false positive costs one harmless HTTP/1.1 replay (HTTP/1.1
//! works everywhere); a false negative simply keeps today's behaviour.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bytes::Bytes;
use rig_core::http_client::sse::BoxedStream;
use rig_core::http_client::{
    Error as HttpError, HttpClientExt, LazyBody, MultipartForm, Request, Response,
    Result as HttpResult, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;

use futures_util::StreamExt;

/// Lowercased markers that identify an HTTP/2 protocol failure in a rendered
/// reqwest error chain (preface handshake, GOAWAY, generic h2 protocol
/// errors).
const H2_FAILURE_MARKERS: &[&str] = &["http2", "http/2", "h2 protocol", "goaway", "preface"];

/// Whether an error chain looks like an HTTP/2 protocol failure. Walks the
/// whole `source()` chain — the h2 error is typically nested two levels below
/// the reqwest wrapper. Takes `&dyn Error` so tests can exercise it with
/// synthetic chains (reqwest errors have no public constructor).
fn looks_like_h2_failure(err: &dyn std::error::Error) -> bool {
    let mut cursor: Option<&dyn std::error::Error> = Some(err);
    while let Some(e) = cursor {
        let rendered = e.to_string().to_lowercase();
        if H2_FAILURE_MARKERS
            .iter()
            .any(|marker| rendered.contains(marker))
        {
            return true;
        }
        cursor = e.source();
    }
    false
}

/// Pooled reqwest backend that degrades to HTTP/1.1 after the first
/// HTTP/2-looking failure. See the module doc for the policy.
///
/// `Debug` + `Clone` are required because rig's `CompletionModel` bound (and
/// through it our `LlmClient` impl) demands them of the HTTP backend type.
/// Cloning is cheap and, importantly, SHARES the `h2_degraded` flag
/// (`Arc<AtomicBool>`), so every clone of a client reroutes after the first
/// HTTP/2 failure observed by any of them.
#[derive(Clone, Debug)]
pub(crate) struct H2FallbackClient {
    /// Default pooled client — HTTP/2 negotiable via ALPN (reqwest default).
    primary: reqwest::Client,
    /// HTTP/1.1-only client used once `h2_degraded` is set.
    http1: reqwest::Client,
    /// Sticky, shared with in-flight futures so a failure observed by one
    /// request reroutes all subsequent ones.
    h2_degraded: Arc<AtomicBool>,
}

impl H2FallbackClient {
    pub(crate) fn new() -> Self {
        Self {
            primary: reqwest::Client::default(),
            // `http1_only` disables the ALPN h2 offer; build failure (TLS
            // backend init) matches reqwest's own default-client policy of
            // failing loudly.
            http1: reqwest::Client::builder()
                .http1_only()
                .build()
                .expect("reqwest http1 client construction cannot fail with a default TLS stack"),
            h2_degraded: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Default for H2FallbackClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared request pieces, saved so a request can be replayed over the
/// fallback client after the primary attempt consumed them.
struct ReplaySpec {
    method: reqwest::Method,
    uri: String,
    headers: reqwest::header::HeaderMap,
    body: Bytes,
}

impl ReplaySpec {
    fn build(&self, client: &reqwest::Client) -> reqwest::RequestBuilder {
        client
            .request(self.method.clone(), self.uri.clone())
            .headers(self.headers.clone())
            .body(self.body.clone())
    }
}

/// Map a reqwest transport error into rig's `http_client::Error` the same way
/// rig's own reqwest backend does.
fn instance_error(error: reqwest::Error) -> HttpError {
    HttpError::Instance(Box::new(error))
}

/// Monotonic suffix so concurrent processes (or repeated failures in one
/// process) never clobber each other's dumps.
static DUMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Truthy values accepted for `CODESMITH_DUMP_400_PAYLOAD` (same family as the
/// TUI's `CODESMITH_TUI_DEBUG`).
fn env_flag_enabled(raw: Option<&str>) -> bool {
    raw.is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// One shallow per-message entry for the dump's `wire_summary` — role, block
/// kinds, sizes and tool-call/result ids. Deliberately provider-shape agnostic
/// (works over any OpenAI-style chat body rig serializes); the deep violation
/// analysis (orphan pairing, duplicate ids, …) happens offline on the dump.
fn wire_message_summary(msg: &serde_json::Value) -> serde_json::Value {
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?");
    let mut kinds: Vec<&str> = Vec::new();
    let mut chars = 0usize;
    let mut tool_call_ids: Vec<String> = Vec::new();
    let mut tool_result_id: Option<&str> = None;
    let mut empty_content = false;

    match msg.get("content") {
        Some(serde_json::Value::String(text)) => {
            chars = text.len();
            if text.trim().is_empty() {
                empty_content = true;
            }
            kinds.push("text");
        }
        Some(serde_json::Value::Array(blocks)) => {
            for block in blocks {
                match block.get("type").and_then(|v| v.as_str()) {
                    Some(kind) => kinds.push(kind),
                    None => kinds.push("?"),
                }
                chars += block
                    .get("text")
                    .or_else(|| block.get("thinking"))
                    .and_then(|v| v.as_str())
                    .map(str::len)
                    .unwrap_or(0);
            }
            if blocks.is_empty() {
                empty_content = true;
            }
        }
        Some(serde_json::Value::Null) | None => empty_content = true,
        _ => {}
    }
    if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
        for call in calls {
            if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                tool_call_ids.push(id.to_string());
            }
            chars += call
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
                .map(str::len)
                .unwrap_or(0);
        }
    }
    if let Some(id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
        tool_result_id = Some(id);
        chars += msg
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::len)
            .unwrap_or(0);
    }
    let has_reasoning_content = msg.get("reasoning_content").is_some();

    serde_json::json!({
        "role": role,
        "kinds": kinds,
        "chars": chars,
        "empty_content": empty_content,
        "tool_call_ids": tool_call_ids,
        "tool_result_id": tool_result_id,
        "has_reasoning_content": has_reasoning_content,
    })
}

/// Shallow structural summary of an OpenAI-style request body: per-message
/// entries plus aggregate counts the offline analyzer keys off.
fn wire_summary(request_body: &serde_json::Value) -> serde_json::Value {
    let messages = request_body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let summaries: Vec<serde_json::Value> = messages.iter().map(wire_message_summary).collect();
    let tool_call_count: usize = summaries
        .iter()
        .map(|m| m["tool_call_ids"].as_array().map(Vec::len).unwrap_or(0))
        .sum();
    let tool_result_count = summaries
        .iter()
        .filter(|m| m["tool_result_id"].is_string())
        .count();
    serde_json::json!({
        "message_count": messages.len(),
        "tool_call_count": tool_call_count,
        "tool_result_count": tool_result_count,
        "messages": summaries,
    })
}

/// Env-gated forensic dump of a failed provider request. Off by default and
/// zero-cost when off (one env read on an already-failing request); enable
/// with `CODESMITH_DUMP_400_PAYLOAD=1`, optionally redirecting the dump
/// directory with `CODESMITH_DUMP_400_PAYLOAD_DIR` (default `/tmp`).
///
/// The dump holds the full serialized request rig was about to send plus the
/// provider's error body — the evidence needed to diagnose strict-provider
/// 400s (e.g. GLM 1214 "messages 参数非法") that only surface mid-session.
fn dump_failed_request(spec: &ReplaySpec, status: reqwest::StatusCode, response_body: &str) {
    if !env_flag_enabled(std::env::var("CODESMITH_DUMP_400_PAYLOAD").ok().as_deref()) {
        return;
    }
    let seq = DUMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::var("CODESMITH_DUMP_400_PAYLOAD_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&dir).join(format!(
        "codesmith-400-dump-{}-{}.json",
        std::process::id(),
        seq
    ));

    let request_body: serde_json::Value = serde_json::from_slice(&spec.body)
        .unwrap_or_else(|error| serde_json::json!({ "parse_error": error.to_string() }));
    let dump = serde_json::json!({
        "uri": spec.uri,
        "status": status.as_u16(),
        "response_body": response_body,
        "request_body": request_body,
    });

    let mut note = String::new();
    if let Ok(payload) = serde_json::to_string_pretty(&dump) {
        match std::fs::write(&path, payload) {
            Ok(()) => note = format!("dump written to {}", path.display()),
            Err(error) => note = format!("dump write failed: {error}"),
        }
    }
    let summary = wire_summary(&request_body);
    tracing::error!(
        uri = %spec.uri,
        status = status.as_u16(),
        message_count = summary["message_count"].as_u64().unwrap_or(0),
        tool_call_count = summary["tool_call_count"].as_u64().unwrap_or(0),
        tool_result_count = summary["tool_result_count"].as_u64().unwrap_or(0),
        "{note}; provider rejected request: {}",
        truncate_for_log(response_body)
    );
}

fn truncate_for_log(text: &str) -> String {
    const MAX: usize = 400;
    if text.len() <= MAX {
        text.to_string()
    } else {
        let mut cut = MAX;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…(+{} bytes)", &text[..cut], text.len() - cut)
    }
}

/// Mirror rig's non-success-status handling: drain the body for the message.
/// Chat endpoints pass `Some(spec)` so a gated forensic dump can capture the
/// exact request the provider rejected; the multipart path has no replayable
/// body and passes `None`.
async fn non_success_status_error(
    response: reqwest::Response,
    spec: Option<&ReplaySpec>,
) -> HttpError {
    let status = response.status();
    let message = response
        .text()
        .await
        .unwrap_or_else(|error| format!("failed to read error response body: {error}"));
    if let Some(spec) = spec {
        dump_failed_request(spec, status, &message);
    }
    HttpError::InvalidStatusCodeWithMessage(status, message)
}

/// Execute `spec` on `client`, mapping failures exactly like rig's reqwest
/// backend (non-success statuses are errors, not `Ok` responses).
async fn execute(
    client: &reqwest::Client,
    spec: &ReplaySpec,
) -> std::result::Result<reqwest::Response, HttpError> {
    let response = spec.build(client).send().await.map_err(instance_error)?;
    if !response.status().is_success() {
        return Err(non_success_status_error(response, Some(spec)).await);
    }
    Ok(response)
}

/// One attempt on the primary client; on an HTTP/2-looking transport failure
/// (and while not already degraded), flip the sticky flag and replay the
/// request once over the HTTP/1.1 client.
async fn execute_with_fallback(
    primary: reqwest::Client,
    http1: reqwest::Client,
    degraded: Arc<AtomicBool>,
    spec: &ReplaySpec,
) -> std::result::Result<reqwest::Response, HttpError> {
    if !degraded.load(Ordering::Relaxed) {
        match spec.build(&primary).send().await {
            Ok(response) => {
                if !response.status().is_success() {
                    return Err(non_success_status_error(response, Some(spec)).await);
                }
                return Ok(response);
            }
            Err(err) if looks_like_h2_failure(&err) => {
                degraded.store(true, Ordering::Relaxed);
                return execute(&http1, spec).await;
            }
            Err(err) => return Err(instance_error(err)),
        }
    }
    execute(&http1, spec).await
}

impl HttpClientExt for H2FallbackClient {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = HttpResult<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let (parts, body) = req.into_parts();
        let spec = ReplaySpec {
            method: parts.method,
            uri: parts.uri.to_string(),
            headers: parts.headers,
            body: body.into(),
        };
        let primary = self.primary.clone();
        let http1 = self.http1.clone();
        let degraded = Arc::clone(&self.h2_degraded);

        async move {
            let response = execute_with_fallback(primary, http1, degraded, &spec).await?;

            let mut res = Response::builder().status(response.status());
            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            let body: LazyBody<U> = Box::pin(async move {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| HttpError::Instance(Box::new(e)))?;
                Ok(U::from(bytes))
            });

            res.body(body).map_err(HttpError::Protocol)
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = HttpResult<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        // No fallback path: the LLM chat endpoints never use multipart, and
        // the form body cannot be cheaply replayed. Mirrors rig's reqwest
        // backend otherwise.
        let (parts, body) = req.into_parts();
        let body = reqwest::multipart::Form::from(body);

        let client = self.primary.clone();

        async move {
            let response = client
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .multipart(body)
                .send()
                .await
                .map_err(instance_error)?;
            if !response.status().is_success() {
                return Err(non_success_status_error(response, None).await);
            }

            let mut res = Response::builder().status(response.status());
            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            let body: LazyBody<U> = Box::pin(async move {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| HttpError::Instance(Box::new(e)))?;
                Ok(U::from(bytes))
            });

            res.body(body).map_err(HttpError::Protocol)
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = HttpResult<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let (parts, body) = req.into_parts();
        let spec = ReplaySpec {
            method: parts.method,
            uri: parts.uri.to_string(),
            headers: parts.headers,
            body: body.into(),
        };
        let primary = self.primary.clone();
        let http1 = self.http1.clone();
        let degraded = Arc::clone(&self.h2_degraded);

        async move {
            let response = execute_with_fallback(primary, http1, degraded, &spec).await?;

            #[cfg(not(target_family = "wasm"))]
            let mut res = Response::builder()
                .status(response.status())
                .version(response.version());
            #[cfg(target_family = "wasm")]
            let mut res = Response::builder().status(response.status());

            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            let mapped_stream: BoxedStream = Box::pin(
                response
                    .bytes_stream()
                    .map(|chunk| chunk.map_err(|e| HttpError::Instance(Box::new(e)))),
            );

            res.body(mapped_stream).map_err(HttpError::Protocol)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h2_protocol_failures_are_detected() {
        let cases = [
            "http2 error: protocol error",
            "Connection error: HTTP/2 preface mismatch",
            "connection error received: GOAWAY",
            "h2 protocol error",
        ];
        for message in cases {
            let wrapper = Shim(message);
            assert!(looks_like_h2_failure(&wrapper), "must detect: {message}");
        }
    }

    #[test]
    fn non_h2_failures_are_not_detected() {
        let cases = [
            "operation timed out",
            "error sending request for url (https://api.example.com/v1/chat)",
            "HTTP status client error (404 Not Found)",
            "invalid dns name",
        ];
        for message in cases {
            let wrapper = Shim(message);
            assert!(
                !looks_like_h2_failure(&wrapper),
                "must NOT detect: {message}"
            );
        }
    }

    /// Single-error chain stand-in for `reqwest::Error` (which has no public
    /// constructor).
    #[derive(Debug)]
    struct Shim(&'static str);

    impl std::fmt::Display for Shim {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Shim {}

    #[test]
    fn dump_env_flag_parsing() {
        assert!(env_flag_enabled(Some("1")));
        assert!(env_flag_enabled(Some("TRUE")));
        assert!(env_flag_enabled(Some("yes")));
        assert!(env_flag_enabled(Some("on")));
        assert!(!env_flag_enabled(Some("0")));
        assert!(!env_flag_enabled(Some("false")));
        assert!(!env_flag_enabled(Some("")));
        assert!(!env_flag_enabled(None));
    }

    #[test]
    fn log_truncation_respects_char_boundaries() {
        assert_eq!(truncate_for_log("short"), "short");
        let long = "x".repeat(500);
        let truncated = truncate_for_log(&long);
        assert!(truncated.starts_with(&"x".repeat(400)));
        assert!(truncated.ends_with("…(+100 bytes)"));
        let multibyte = "编译".repeat(300);
        let truncated = truncate_for_log(&multibyte);
        // The cut must land on a char boundary (a mis-cut would panic while
        // slicing) and keep whole characters.
        assert!(
            truncated.starts_with("编"),
            "cut must land on a char boundary"
        );
        assert!(truncated.contains("…(+") && truncated.ends_with(")"));
        assert!(truncated.len() < multibyte.len());
    }
}
