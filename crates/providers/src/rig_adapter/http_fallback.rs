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

// --- DeepSeek-compatible usage normalization ----------------------------
//
// rig-core's deepseek response types require `prompt_cache_hit_tokens` and
// `prompt_cache_miss_tokens` as non-Option numbers on every `usage` object.
// DeepSeek's own API always sends them, but third-party DeepSeek-compatible
// gateways (e.g. Zhipu GLM in DeepSeek-compat mode) omit the two cache
// fields — the strict parse then fails the whole response ("data did not
// match any variant of untagged enum ApiResponse") or the stream's final
// usage chunk. Defaulting them to 0 — "no cache tokens reported" — only when
// the deepseek factory enabled `deepseek_usage_compat` on this backend makes
// those gateways parse, while OpenAI/Anthropic traffic stays byte-identical
// (re-serializing every provider's bodies for a DeepSeek-only quirk was the
// previous, accidental behavior — all four factories wire this backend).

const USAGE_CACHE_FIELDS: [&str; 2] = ["prompt_cache_hit_tokens", "prompt_cache_miss_tokens"];

/// Default the DeepSeek cache-token fields on an OpenAI-style `usage` object.
/// Returns whether the value was mutated. Absent, null, or non-unsigned
/// values become `0` (nulls and floats also break the `u32` decode).
fn default_usage_cache_fields(usage: &mut serde_json::Value) -> bool {
    let Some(obj) = usage.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    for field in USAGE_CACHE_FIELDS {
        let needs_default =
            !matches!(obj.get(field), Some(serde_json::Value::Number(n)) if n.is_u64());
        if needs_default {
            obj.insert(field.to_string(), serde_json::json!(0u32));
            changed = true;
        }
    }
    changed
}

/// Rewrite a `data: {json}` SSE line's top-level `usage`, defaulting the
/// cache fields. Other lines (and payloads without a top-level `usage`)
/// pass through byte-identical.
fn normalize_sse_line(line: &[u8]) -> Vec<u8> {
    let payload_start = match line.strip_prefix(b"data:") {
        Some(rest) => rest,
        None => return line.to_vec(),
    };
    let trimmed = {
        let t = payload_start.strip_prefix(b" ").unwrap_or(payload_start);
        t.strip_suffix(b"\r").unwrap_or(t)
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(trimmed) else {
        return line.to_vec();
    };
    let Some(usage) = value.get_mut("usage") else {
        return line.to_vec();
    };
    if !default_usage_cache_fields(usage) {
        return line.to_vec();
    }
    let mut rewritten = Vec::with_capacity(line.len() + 48);
    rewritten.extend_from_slice(b"data: ");
    rewritten.extend_from_slice(&serde_json::to_vec(&value).unwrap_or_else(|_| trimmed.to_vec()));
    if line.ends_with(b"\n") {
        rewritten.push(b'\n');
    }
    rewritten
}

/// Stateful rewriter for a byte-chunk SSE stream: buffers partial lines
/// across chunk boundaries and normalizes complete `data:` lines that carry a
/// top-level `usage`. An empty push flushes the pending remainder (used as
/// the end-of-stream sentinel).
#[derive(Default)]
struct SseUsageNormalizer {
    pending: Vec<u8>,
}

impl SseUsageNormalizer {
    fn push(&mut self, chunk: Bytes) -> Bytes {
        if chunk.is_empty() {
            return self.flush();
        }
        self.pending.extend_from_slice(&chunk);
        let Some(cut) = self.pending.iter().rposition(|&b| b == b'\n') else {
            return Bytes::new();
        };
        let complete: Vec<u8> = self.pending.drain(..=cut).collect();
        self.rewrite(complete)
    }

    fn flush(&mut self) -> Bytes {
        if self.pending.is_empty() {
            return Bytes::new();
        }
        let rest = std::mem::take(&mut self.pending);
        self.rewrite(rest)
    }

    fn rewrite(&self, buf: Vec<u8>) -> Bytes {
        if !buf.windows(7).any(|w| w == b"\"usage\"") {
            return Bytes::from(buf);
        }
        let mut out = Vec::with_capacity(buf.len() + 64);
        let mut rest = buf.as_slice();
        while !rest.is_empty() {
            let (line, tail) = match rest.iter().position(|&b| b == b'\n') {
                Some(idx) => (&rest[..=idx], &rest[idx + 1..]),
                None => (rest, &[][..]),
            };
            out.extend_from_slice(&normalize_sse_line(line));
            rest = tail;
        }
        Bytes::from(out)
    }
}

/// Default the cache-token fields on a non-streaming completion response's
/// top-level `usage`. Bodies without a top-level `usage` (errors, other
/// endpoints) or unparseable bodies pass through unchanged so rig produces
/// its own native error.
fn normalize_completion_usage(body: Bytes) -> Bytes {
    if !body.windows(7).any(|w| w == b"\"usage\"") {
        return body;
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body[..]) else {
        return body;
    };
    let Some(usage) = value.get_mut("usage") else {
        return body;
    };
    if !default_usage_cache_fields(usage) {
        return body;
    }
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

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
    /// Whether to run the DeepSeek usage-field normalization on response
    /// bodies (see the module comment). Off by default: only the deepseek
    /// factory's rig types need the defaulted cache fields, and every
    /// provider shares this backend.
    deepseek_usage_compat: bool,
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
            deepseek_usage_compat: false,
        }
    }

    /// Enable the DeepSeek usage-field normalization (deepseek factory only).
    pub(crate) fn deepseek_usage_compat(mut self, enabled: bool) -> Self {
        self.deepseek_usage_compat = enabled;
        self
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

/// Truthy values accepted for env toggles in this adapter
/// (`CODESMITH_DUMP_400_PAYLOAD`, `CODESMITH_REASONING_PASSTHROUGH` — same
/// family as the TUI's `CODESMITH_TUI_DEBUG`). Shared so the flags cannot
/// drift apart; `reasoning.rs` reads one on every request build.
pub(crate) fn env_flag_enabled(raw: Option<&str>) -> bool {
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
        // `create_new` + `0o600`: the dump carries the full request
        // transcript (user code, tool outputs), so it must be owner-only —
        // `fs::write`'s default mode is world-readable — and must never
        // follow a pre-planted entry at the predictable path (symlink
        // clobber). Known limitation: dumps accumulate without a cap; the
        // flag is opt-in forensics, so cleanup stays manual.
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(unix)]
        let open = || {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
        };
        #[cfg(not(unix))]
        let open = || {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
        };
        let write = open().and_then(|mut file| {
            use std::io::Write;
            file.write_all(payload.as_bytes())
        });
        match write {
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
        let usage_compat = self.deepseek_usage_compat;

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
                Ok(U::from(if usage_compat {
                    normalize_completion_usage(bytes)
                } else {
                    bytes
                }))
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
        let usage_compat = self.deepseek_usage_compat;

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

            let byte_stream = response
                .bytes_stream()
                .map(|chunk| chunk.map_err(|e| HttpError::Instance(Box::new(e))));
            // Without the usage compat flag the stream is forwarded
            // untouched — only the deepseek factory's rig types need the
            // defaulted cache fields.
            let mapped_stream: BoxedStream = if usage_compat {
                // The trailing empty chunk is a flush sentinel: `scan` drops
                // its state at end-of-stream, so the normalizer emits any
                // pending partial line when it sees the sentinel.
                Box::pin(
                    byte_stream
                        .chain(futures_util::stream::once(async {
                            Ok::<Bytes, HttpError>(Bytes::new())
                        }))
                        .scan(SseUsageNormalizer::default(), |state, chunk| {
                            let out = match chunk {
                                Ok(bytes) => state.push(bytes),
                                Err(e) => return std::future::ready(Some(Err(e))),
                            };
                            std::future::ready(Some(Ok(out)))
                        }),
                )
            } else {
                Box::pin(byte_stream)
            };

            res.body(mapped_stream).map_err(HttpError::Protocol)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- usage cache-field normalization ------------------------------------

    /// A GLM-style response whose `usage` lacks the two DeepSeek cache
    /// fields gets them defaulted to 0; everything else stays intact.
    #[test]
    fn completion_usage_cache_fields_defaulted_when_missing() {
        let body = Bytes::from_static(
            br#"{"id":"1","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
        );
        let out = normalize_completion_usage(body);
        let v: serde_json::Value = serde_json::from_slice(&out[..]).unwrap();
        assert_eq!(v["usage"]["prompt_cache_hit_tokens"], 0);
        assert_eq!(v["usage"]["prompt_cache_miss_tokens"], 0);
        assert_eq!(v["usage"]["total_tokens"], 4);
        assert_eq!(v["choices"][0]["message"]["content"], "OK");
    }

    /// A real DeepSeek response already carrying the cache fields passes
    /// through unchanged (no re-serialization churn).
    #[test]
    fn completion_usage_untouched_when_cache_fields_present() {
        let body = Bytes::from_static(
            br#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4,"prompt_cache_hit_tokens":2,"prompt_cache_miss_tokens":1}}"#,
        );
        let out = normalize_completion_usage(body.clone());
        assert_eq!(out, body);
    }

    /// Error payloads without a top-level `usage` are passed through
    /// byte-identical so rig renders the provider's own error text.
    #[test]
    fn completion_error_bodies_pass_through() {
        let body = Bytes::from_static(br#"{"error":{"code":"1214","message":"messages invalid"}}"#);
        let out = normalize_completion_usage(body.clone());
        assert_eq!(out, body);
    }

    /// Null or non-integer cache values (which also break `u32` decoding)
    /// are defaulted to 0.
    #[test]
    fn completion_null_cache_values_defaulted() {
        let body = Bytes::from_static(
            br#"{"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2,"prompt_cache_hit_tokens":null}}"#,
        );
        let out = normalize_completion_usage(body);
        let v: serde_json::Value = serde_json::from_slice(&out[..]).unwrap();
        assert_eq!(v["usage"]["prompt_cache_hit_tokens"], 0);
        assert_eq!(v["usage"]["prompt_cache_miss_tokens"], 0);
    }

    /// The final SSE usage chunk (GLM shape) is rewritten in place while
    /// sibling content chunks and the `[DONE]` sentinel stay byte-identical.
    #[test]
    fn sse_usage_chunk_normalized_and_others_untouched() {
        let mut state = SseUsageNormalizer::default();
        let stream = b"data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\ndata: [DONE]\n\n";
        let out = state.push(Bytes::from_static(stream));
        let text = String::from_utf8(out.to_vec()).unwrap();
        let usage_line = text
            .lines()
            .find(|l| l.contains("\"usage\""))
            .expect("usage line present");
        let v: serde_json::Value =
            serde_json::from_str(usage_line.trim_start_matches("data: ")).unwrap();
        assert_eq!(v["usage"]["prompt_cache_hit_tokens"], 0);
        assert_eq!(v["usage"]["prompt_cache_miss_tokens"], 0);
        assert!(text.contains("data: [DONE]"));
        assert!(text.contains("OK"));
    }

    /// A usage JSON line split across two network chunks is still rewritten
    /// exactly once, with no bytes lost or duplicated.
    #[test]
    fn sse_usage_line_split_across_chunks() {
        let mut state = SseUsageNormalizer::default();
        let first = state.push(Bytes::from_static(
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3",
        ));
        assert!(first.is_empty(), "partial line must be buffered");
        let second = state.push(Bytes::from_static(
            br#","completion_tokens":1,"total_tokens":4}}"#,
        ));
        assert!(second.is_empty(), "no newline yet; still buffered");
        let flushed = state.push(Bytes::new()); // flush sentinel
        let text = String::from_utf8(flushed.to_vec()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(text.trim().trim_start_matches("data: ")).unwrap();
        assert_eq!(v["usage"]["prompt_cache_hit_tokens"], 0);
        assert_eq!(v["usage"]["prompt_cache_miss_tokens"], 0);
        assert_eq!(v["usage"]["prompt_tokens"], 3);
    }

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
