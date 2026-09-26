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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
        if H2_FAILURE_MARKERS.iter().any(|marker| rendered.contains(marker)) {
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

/// Mirror rig's non-success-status handling: drain the body for the message.
async fn non_success_status_error(response: reqwest::Response) -> HttpError {
    let status = response.status();
    let message = response
        .text()
        .await
        .unwrap_or_else(|error| format!("failed to read error response body: {error}"));
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
        return Err(non_success_status_error(response).await);
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
                    return Err(non_success_status_error(response).await);
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
                return Err(non_success_status_error(response).await);
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
}
