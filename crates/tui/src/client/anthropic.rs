//! Anthropic Messages API client (`/v1/messages`).
//!
//! This is a sibling of the OpenAI-shaped `DeepSeekClient` and implements
//! the same `LlmClient` trait, but talks to Anthropic's native protocol:
//!
//! * `POST {base_url}/messages` with the `MessageRequest` JSON shape
//!   (the internal `MessageRequest` already mirrors Anthropic's wire format,
//!   so no schema translation is needed).
//! * SSE streaming events deserialize directly into `StreamEvent` because
//!   the `#[serde(tag = "type")]` discriminators already match
//!   (`message_start`, `content_block_delta`, `message_stop`, etc.).
//! * Auth via `x-api-key` for Anthropic's official API; compatible
//!   gateways may use `Authorization: Bearer` instead.
//! * Reasoning effort is translated into the Anthropic-native
//!   `thinking: {type, budget_tokens}` field on the request body.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout as tokio_timeout;

use crate::config::{ApiProvider, Config, RetryPolicy};
use crate::llm_client::{
    LlmClient, LlmError, RetryConfig as LlmRetryConfig, StreamEventBox, extract_retry_after,
    with_retry,
};
use crate::logging;
use crate::models::{MessageRequest, MessageResponse, StreamEvent, Usage};

use super::{
    ERROR_BODY_MAX_BYTES, SSE_BACKPRESSURE_HIGH_WATERMARK, SSE_BACKPRESSURE_SLEEP_MS,
    SSE_MAX_LINES_PER_CHUNK, acquire_stream_buffer, add_extra_root_certs, bounded_error_text,
    force_http1_from_env, release_stream_buffer, validate_base_url_security,
};

/// Default value for the `anthropic-version` header.
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default idle timeout for SSE stream reads (300 seconds = 5 minutes).
const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Initial timeout for the streaming response headers.
const DEFAULT_STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(45);

fn stream_open_timeout() -> Duration {
    let secs = std::env::var("ANTHROPIC_STREAM_OPEN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_STREAM_OPEN_TIMEOUT.as_secs())
        .clamp(5, 300);
    Duration::from_secs(secs)
}

fn stream_idle_timeout() -> Duration {
    let secs = std::env::var("ANTHROPIC_STREAM_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT.as_secs())
        .clamp(1, 3600);
    Duration::from_secs(secs)
}

/// Anthropic Messages API client.
#[must_use]
pub struct AnthropicClient {
    http_client: reqwest::Client,
    #[allow(dead_code)] // Retained for diagnostics / future surfacing.
    api_key: String,
    base_url: String,
    api_provider: ApiProvider,
    retry: RetryPolicy,
    default_model: String,
    rate_limiter: Arc<AsyncMutex<TokenBucket>>,
}

const DEFAULT_CLIENT_RATE_LIMIT_RPS: f64 = 8.0;
const DEFAULT_CLIENT_RATE_LIMIT_BURST: f64 = 16.0;

#[derive(Debug)]
struct TokenBucket {
    enabled: bool,
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_refill: std::time::Instant,
}

impl TokenBucket {
    fn from_env() -> Self {
        let rps = std::env::var("ANTHROPIC_RATE_LIMIT_RPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(DEFAULT_CLIENT_RATE_LIMIT_RPS)
            .max(0.0);
        let burst = std::env::var("ANTHROPIC_RATE_LIMIT_BURST")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(DEFAULT_CLIENT_RATE_LIMIT_BURST)
            .max(1.0);
        let enabled = rps > 0.0;
        Self {
            enabled,
            capacity: burst,
            tokens: burst,
            refill_per_sec: rps,
            last_refill: std::time::Instant::now(),
        }
    }

    fn delay_until_available(&mut self, cost: f64) -> Option<Duration> {
        if !self.enabled {
            return None;
        }
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            self.last_refill = now;
        }
        if self.tokens >= cost {
            self.tokens -= cost;
            return None;
        }
        let deficit = cost - self.tokens;
        let secs = deficit / self.refill_per_sec.max(f64::EPSILON);
        Some(Duration::from_secs_f64(secs))
    }
}

impl AnthropicClient {
    /// Construct an `AnthropicClient` from the resolved CLI configuration.
    pub fn new(config: &Config) -> Result<Self> {
        let api_key = config.deepseek_api_key()?;
        let base_url = config.deepseek_base_url();
        let api_provider = config.api_provider();
        validate_base_url_security(&base_url)?;
        let retry = config.retry_policy();
        let default_model = config.default_model();
        let http_headers = config.http_headers();

        logging::info(format!("API provider: {}", api_provider.as_str()));
        logging::info(format!("API base URL: {base_url}"));
        if !http_headers.is_empty() {
            logging::info(format!(
                "{} custom HTTP header(s) configured",
                http_headers.len()
            ));
        }
        logging::info(format!(
            "Retry policy: enabled={}, max_retries={}, initial_delay={}s, max_delay={}s",
            retry.enabled, retry.max_retries, retry.initial_delay, retry.max_delay
        ));

        let http_client = build_http_client(&api_key, &base_url, &http_headers)?;

        Ok(Self {
            http_client,
            api_key,
            base_url,
            api_provider,
            retry,
            default_model,
            rate_limiter: Arc::new(AsyncMutex::new(TokenBucket::from_env())),
        })
    }

    /// Returns the API base URL used by this client.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn messages_url(&self) -> String {
        anthropic_messages_url(&self.base_url)
    }

    async fn wait_for_rate_limit(&self) {
        let maybe_delay = {
            let mut limiter = self.rate_limiter.lock().await;
            limiter.delay_until_available(1.0)
        };
        if let Some(delay) = maybe_delay {
            tokio::time::sleep(delay).await;
        }
    }

    async fn send_with_retry<F>(&self, mut build: F) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        let retry_cfg: LlmRetryConfig = self.retry.clone().into();
        let request_result = with_retry(
            &retry_cfg,
            || {
                let request = build();
                async move {
                    self.wait_for_rate_limit().await;
                    let response = request
                        .send()
                        .await
                        .map_err(|err| LlmError::from_reqwest(&err))?;
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    let retry_after = extract_retry_after(response.headers());
                    let body = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
                    Err(LlmError::from_http_response_with_retry_after(
                        status.as_u16(),
                        &body,
                        retry_after,
                    ))
                }
            },
            Some(Box::new(|err, attempt, delay| {
                let (label, human) = retry_reason_label_and_human(err);
                logging::warn(format!(
                    "HTTP retry reason={} attempt={} delay={:.2}s",
                    label,
                    attempt + 1,
                    delay.as_secs_f64(),
                ));
                crate::retry_status::start(attempt + 1, delay, human);
            })),
        )
        .await;

        match request_result {
            Ok(response) => {
                crate::retry_status::succeeded();
                Ok(response)
            }
            Err(err) => {
                let last = err.last_error.to_string();
                if err.attempts > 1 {
                    crate::retry_status::failed(last.clone());
                } else {
                    crate::retry_status::clear();
                }
                Err(anyhow::anyhow!(last))
            }
        }
    }

    async fn create_message_messages(&self, request: &MessageRequest) -> Result<MessageResponse> {
        let body = build_request_body(request, false)?;
        let url = self.messages_url();
        let open_timeout = stream_open_timeout();
        let response = match tokio_timeout(
            open_timeout,
            self.send_with_retry(|| self.http_client.post(&url).json(&body)),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => {
                anyhow::bail!(
                    "Anthropic /messages request did not return headers after {}s.",
                    open_timeout.as_secs()
                );
            }
        };

        let status = response.status();
        if !status.is_success() {
            let error_text = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            anyhow::bail!("Failed to call Anthropic Messages API: HTTP {status}: {error_text}");
        }

        let response_text = response.text().await.unwrap_or_default();
        let parsed: MessageResponse = serde_json::from_str(&response_text)
            .context("Failed to parse Anthropic /messages response")?;
        Ok(parsed)
    }

    async fn handle_messages_stream(&self, request: MessageRequest) -> Result<StreamEventBox> {
        let body = build_request_body(&request, true)?;
        let url = self.messages_url();
        let response = self
            .send_with_retry(|| self.http_client.post(&url).json(&body))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            anyhow::bail!("Anthropic SSE stream request failed: HTTP {status}: {error_text}");
        }

        let byte_stream = response.bytes_stream();

        let stream = async_stream::stream! {
            use futures_util::StreamExt;

            let mut line_buf = String::new();
            let mut byte_buf = acquire_stream_buffer();

            let mut byte_stream = std::pin::pin!(byte_stream);
            let idle = stream_idle_timeout();

            let stream_start = std::time::Instant::now();
            let mut last_event_at = std::time::Instant::now();
            let mut bytes_received: usize = 0;

            loop {
                let chunk_result = match tokio_timeout(idle, byte_stream.next()).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break,
                    Err(_elapsed) => {
                        yield Err(anyhow::anyhow!(
                            "SSE stream idle timeout after {}s — no data received",
                            idle.as_secs(),
                        ));
                        break;
                    }
                };
                let chunk = match chunk_result {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        let mut error_chain = format!("{e}");
                        let mut current: Option<&(dyn std::error::Error + 'static)> =
                            std::error::Error::source(&e);
                        while let Some(source) = current {
                            error_chain.push_str(&format!(" -> {source}"));
                            current = std::error::Error::source(source);
                        }
                        crate::logging::warn(format!(
                            "Anthropic stream read error: {error_chain} \
                             (elapsed: {}ms, bytes_received: {}, ms_since_last_event: {})",
                            stream_start.elapsed().as_millis(),
                            bytes_received,
                            last_event_at.elapsed().as_millis(),
                        ));
                        yield Err(anyhow::anyhow!("Stream read error: {e}"));
                        break;
                    }
                };

                bytes_received = bytes_received.saturating_add(chunk.len());
                last_event_at = std::time::Instant::now();
                byte_buf.extend_from_slice(&chunk);

                const MAX_SSE_BUF: usize = 10 * 1024 * 1024;
                if byte_buf.len() > MAX_SSE_BUF {
                    yield Err(anyhow::anyhow!(
                        "SSE buffer exceeded {MAX_SSE_BUF} bytes — aborting stream"
                    ));
                    break;
                }

                if byte_buf.len() > SSE_BACKPRESSURE_HIGH_WATERMARK {
                    tokio::time::sleep(Duration::from_millis(SSE_BACKPRESSURE_SLEEP_MS)).await;
                }

                let mut lines_processed = 0usize;
                while let Some(newline_pos) = byte_buf.iter().position(|&b| b == b'\n') {
                    let mut end = newline_pos;
                    if end > 0 && byte_buf[end - 1] == b'\r' {
                        end -= 1;
                    }
                    let line = String::from_utf8_lossy(&byte_buf[..end]).into_owned();
                    byte_buf.drain(..newline_pos + 1);

                    if line.is_empty() {
                        if !line_buf.is_empty() {
                            let data = std::mem::take(&mut line_buf);
                            if data.trim() == "[DONE]" {
                                continue;
                            }
                            match parse_anthropic_sse_event(&data) {
                                ParseOutcome::Event(event) => yield Ok(event),
                                ParseOutcome::Error(err) => {
                                    yield Err(err);
                                    return;
                                }
                                ParseOutcome::Skip => {}
                            }
                        }
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.strip_prefix(' ').unwrap_or(data);
                        line_buf.push_str(data);
                    }
                    // Ignore other SSE fields (event:, id:, retry:, comment lines).

                    lines_processed = lines_processed.saturating_add(1);
                    if lines_processed >= SSE_MAX_LINES_PER_CHUNK {
                        break;
                    }
                }
            }

            release_stream_buffer(byte_buf);
        };

        Ok(Pin::from(Box::new(stream)
            as Box<
                dyn futures_util::Stream<Item = Result<StreamEvent>> + Send,
            >))
    }
}

#[derive(Debug)]
enum ParseOutcome {
    Event(StreamEvent),
    Error(anyhow::Error),
    Skip,
}

fn parse_anthropic_sse_event(data: &str) -> ParseOutcome {
    // Try to parse as a structured StreamEvent first. If the server sent an
    // `error` event (Anthropic spec §"Error events"), surface it as a stream
    // error so downstream consumers can break out.
    let value: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(err) => {
            return ParseOutcome::Error(anyhow::anyhow!(
                "Failed to parse Anthropic SSE chunk: {err}"
            ));
        }
    };

    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if event_type == "error" {
        let message = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        return ParseOutcome::Error(anyhow::anyhow!("Anthropic stream error: {message}"));
    }

    match serde_json::from_value::<StreamEvent>(value) {
        Ok(event) => ParseOutcome::Event(event),
        Err(_) => ParseOutcome::Skip,
    }
}

fn anthropic_messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with("/messages") {
        trimmed.to_string()
    } else if lower.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else if anthropic_base_url_uses_bearer_gateway(trimmed) {
        format!("{trimmed}/v1/messages")
    } else {
        format!("{trimmed}/messages")
    }
}

fn build_http_client(
    api_key: &str,
    base_url: &str,
    extra_headers: &HashMap<String, String>,
) -> Result<reqwest::Client> {
    let headers = build_default_headers(api_key, base_url, extra_headers)?;
    let mut builder = reqwest::Client::builder()
        .default_headers(headers)
        .user_agent(concat!(
            "Mozilla/5.0 (compatible; codesmith/",
            env!("CARGO_PKG_VERSION"),
            "; +https://github.com/Hmbown/CodeSmith)"
        ))
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(Duration::from_secs(15)))
        .http2_keep_alive_timeout(Duration::from_secs(20))
        .min_tls_version(reqwest::tls::Version::TLS_1_2);
    if force_http1_from_env() {
        logging::info("DEEPSEEK_FORCE_HTTP1=1 — pinning HTTP client to HTTP/1.1");
        builder = builder.http1_only();
    }
    if let Ok(cert_path) = std::env::var("SSL_CERT_FILE")
        && !cert_path.is_empty()
    {
        builder = add_extra_root_certs(builder, &cert_path);
    }
    builder.build().map_err(Into::into)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnthropicAuthMode {
    ApiKey,
    Bearer,
}

impl AnthropicAuthMode {
    fn from_base_url(base_url: &str) -> Self {
        if anthropic_base_url_uses_bearer_gateway(base_url) {
            Self::Bearer
        } else {
            Self::ApiKey
        }
    }
}

fn anthropic_base_url_uses_bearer_gateway(base_url: &str) -> bool {
    let lower = base_url.trim().to_ascii_lowercase();
    lower.contains("token-plan.cn-beijing.maas.aliyuncs.com") || lower.contains("/apps/anthropic")
}

/// Build the default Anthropic headers: `x-api-key`, `anthropic-version`,
/// optional gateway `Authorization: Bearer`, optional `anthropic-beta`, and
/// any user-supplied custom headers (which can override the defaults).
fn build_default_headers(
    api_key: &str,
    base_url: &str,
    extra_headers: &HashMap<String, String>,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION),
    );

    if !api_key.trim().is_empty() {
        insert_anthropic_auth_headers(
            &mut headers,
            api_key.trim(),
            AnthropicAuthMode::from_base_url(base_url),
        )?;
    }

    // Allow ANTHROPIC_BETA env var to set the anthropic-beta header for
    // per-shell experimentation without editing config files.
    if let Ok(beta) = std::env::var("ANTHROPIC_BETA")
        && !beta.trim().is_empty()
    {
        headers.insert(
            HeaderName::from_static("anthropic-beta"),
            HeaderValue::from_str(beta.trim())?,
        );
    }

    for (name, value) in extra_headers {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())?;
        // Disallow CONTENT_TYPE override; everything else (including
        // anthropic-version, anthropic-beta, x-api-key, authorization) can be
        // overridden by user config — the user is responsible for the value.
        if header_name == CONTENT_TYPE {
            continue;
        }
        headers.insert(header_name, HeaderValue::from_str(value)?);
    }
    Ok(headers)
}

fn insert_anthropic_auth_headers(
    headers: &mut HeaderMap,
    api_key: &str,
    mode: AnthropicAuthMode,
) -> Result<()> {
    if matches!(mode, AnthropicAuthMode::ApiKey) {
        let mut value = HeaderValue::from_str(api_key)?;
        value.set_sensitive(true);
        headers.insert(HeaderName::from_static("x-api-key"), value);
    }

    if matches!(mode, AnthropicAuthMode::Bearer) {
        let mut value = HeaderValue::from_str(&format!("Bearer {api_key}"))?;
        value.set_sensitive(true);
        headers.insert(HeaderName::from_static("authorization"), value);
    }

    Ok(())
}

/// Build the JSON body posted to `/v1/messages`.
///
/// This translates the internal `MessageRequest` into Anthropic-shape:
/// * Strips DeepSeek-specific `reasoning_effort` from the wire payload.
/// * Strips agent-internal `Tool` fields (`allowed_callers`, `defer_loading`,
///   `input_examples`, `output_schema`, `strict`, `caller`) that Anthropic does not understand.
/// * If `request.thinking` is unset, derives a `thinking` field from
///   `request.reasoning_effort` (Anthropic-native shape).
/// * Forces `stream` to match the caller's intent.
fn build_request_body(request: &MessageRequest, stream: bool) -> Result<Value> {
    let mut body = serde_json::to_value(request).context("serialize MessageRequest")?;

    let derived_thinking = request
        .thinking
        .clone()
        .or_else(|| derive_thinking(request.reasoning_effort.as_deref()));

    if let Some(obj) = body.as_object_mut() {
        obj.remove("reasoning_effort");
        // The internal `caller` field on tool_use blocks is agent-internal;
        // strip it from outgoing assistant-message content too.
        if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
                    for block in content {
                        if let Some(b) = block.as_object_mut() {
                            b.remove("caller");
                        }
                    }
                }
            }
        }

        if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                if let Some(tool_obj) = tool.as_object_mut() {
                    // These are agent-internal hints not recognized by
                    // Anthropic. Removing them avoids 400 "extra_inputs"
                    // rejections and keeps cache_control byte-identical
                    // across runs.
                    tool_obj.remove("allowed_callers");
                    tool_obj.remove("defer_loading");
                    tool_obj.remove("input_examples");
                    tool_obj.remove("output_schema");
                    tool_obj.remove("strict");
                    // Anthropic requires `name`, `description`, `input_schema`.
                    // `type` is allowed (e.g. "custom", "computer_20250124").
                }
            }
        }

        match derived_thinking {
            Some(value) => {
                obj.insert("thinking".to_string(), value);
            }
            None => {
                obj.remove("thinking");
            }
        }

        obj.insert("stream".to_string(), Value::Bool(stream));
    }

    Ok(body)
}

/// Translate the user-facing reasoning-effort tier into Anthropic's
/// native `thinking` field shape. Returns `None` when no effort is
/// specified — in which case the field is omitted entirely.
fn derive_thinking(effort: Option<&str>) -> Option<Value> {
    let effort = effort?.trim().to_ascii_lowercase();
    match effort.as_str() {
        "" => None,
        "off" | "disabled" | "none" | "false" => Some(json!({ "type": "disabled" })),
        "low" | "minimal" => Some(json!({ "type": "enabled", "budget_tokens": 4_096 })),
        "medium" | "mid" => Some(json!({ "type": "enabled", "budget_tokens": 8_192 })),
        "high" => Some(json!({ "type": "enabled", "budget_tokens": 16_384 })),
        "xhigh" | "max" | "highest" => Some(json!({ "type": "enabled", "budget_tokens": 32_768 })),
        _ => None,
    }
}

fn retry_reason_label_and_human(err: &LlmError) -> (&'static str, String) {
    match err {
        LlmError::RateLimited { retry_after, .. } => {
            let human = if let Some(after) = retry_after {
                format!("rate limited (Retry-After {}s)", after.as_secs())
            } else {
                "rate limited".to_string()
            };
            ("rate_limited", human)
        }
        LlmError::ServerError { status, .. } => ("server_error", format!("upstream {status}")),
        LlmError::NetworkError(_) => ("network_error", "network error".to_string()),
        LlmError::Timeout(_) => ("timeout", "timeout".to_string()),
        _ => ("other", "other".to_string()),
    }
}

impl LlmClient for AnthropicClient {
    fn provider_name(&self) -> &'static str {
        self.api_provider.as_str()
    }

    fn model(&self) -> &str {
        &self.default_model
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn health_check(&self) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>> {
        Box::pin(async move {
            // Anthropic does not expose a public list-models endpoint analogous
            // to /v1/models, and the messages endpoint requires a body.  Treat
            // an authenticated, well-formed client as healthy until a real
            // request fails.
            Ok(true)
        })
    }

    fn create_message(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
        Box::pin(async move { self.create_message_messages(&request).await })
    }

    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
        Box::pin(async move { self.handle_messages_stream(request).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentBlock, Message, MessageRequest, Tool};

    fn sample_request() -> MessageRequest {
        MessageRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "hi".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 1024,
            system: None,
            tools: Some(vec![Tool {
                tool_type: Some("custom".to_string()),
                name: "echo".to_string(),
                description: "echo".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: Some(serde_json::json!({"type": "object"})),
                allowed_callers: Some(vec!["agent".to_string()]),
                defer_loading: Some(true),
                input_examples: Some(vec![serde_json::json!({"x": 1})]),
                strict: Some(true),
                cache_control: None,
            }]),
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("high".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    #[test]
    fn build_request_body_strips_internal_fields_and_adds_thinking() {
        let body = build_request_body(&sample_request(), true).unwrap();
        let obj = body.as_object().unwrap();
        assert_eq!(
            obj.get("model").and_then(Value::as_str),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(obj.get("stream").and_then(Value::as_bool), Some(true));
        assert!(
            obj.get("reasoning_effort").is_none(),
            "reasoning_effort must be stripped"
        );

        let thinking = obj.get("thinking").expect("thinking must be derived");
        assert_eq!(
            thinking.get("type").and_then(Value::as_str),
            Some("enabled")
        );
        assert_eq!(
            thinking.get("budget_tokens").and_then(Value::as_u64),
            Some(16_384)
        );

        let tool = obj["tools"][0].as_object().unwrap();
        assert!(tool.get("allowed_callers").is_none());
        assert!(tool.get("defer_loading").is_none());
        assert!(tool.get("input_examples").is_none());
        assert!(tool.get("output_schema").is_none());
        assert!(tool.get("strict").is_none());
        assert_eq!(tool.get("name").and_then(Value::as_str), Some("echo"));
    }

    #[test]
    fn anthropic_messages_url_preserves_official_v1_base() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn anthropic_messages_url_adds_v1_for_token_plan_gateway() {
        assert_eq!(
            anthropic_messages_url(
                "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic"
            ),
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages"
        );
    }

    #[test]
    fn anthropic_messages_url_does_not_duplicate_explicit_messages_path() {
        assert_eq!(
            anthropic_messages_url(
                "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages"
            ),
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages"
        );
    }

    #[test]
    fn build_request_body_off_emits_disabled_thinking() {
        let mut req = sample_request();
        req.reasoning_effort = Some("off".to_string());
        let body = build_request_body(&req, false).unwrap();
        let thinking = body.get("thinking").unwrap();
        assert_eq!(
            thinking.get("type").and_then(Value::as_str),
            Some("disabled")
        );
        assert_eq!(body.get("stream").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn build_request_body_omits_thinking_when_unset() {
        let mut req = sample_request();
        req.reasoning_effort = None;
        req.thinking = None;
        let body = build_request_body(&req, false).unwrap();
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn build_request_body_preserves_explicit_thinking() {
        let mut req = sample_request();
        req.reasoning_effort = Some("low".to_string());
        req.thinking = Some(serde_json::json!({ "type": "enabled", "budget_tokens": 1234 }));
        let body = build_request_body(&req, false).unwrap();
        let thinking = body.get("thinking").unwrap();
        assert_eq!(
            thinking.get("budget_tokens").and_then(Value::as_u64),
            Some(1234)
        );
    }

    #[test]
    fn build_default_headers_sets_anthropic_auth() {
        let headers = build_default_headers(
            "sk-ant-test",
            "https://api.anthropic.com/v1",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("sk-ant-test")
        );
        assert_eq!(
            headers
                .get("anthropic-version")
                .and_then(|v| v.to_str().ok()),
            Some(DEFAULT_ANTHROPIC_VERSION)
        );
        assert!(
            headers.get("authorization").is_none(),
            "Official Anthropic must keep x-api-key-only auth"
        );
    }

    #[test]
    fn build_default_headers_sets_bearer_for_token_plan_gateway() {
        let headers = build_default_headers(
            "gateway-key",
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
            &HashMap::new(),
        )
        .unwrap();
        assert!(
            headers.get("x-api-key").is_none(),
            "token-plan gateway expects bearer-only auth"
        );
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer gateway-key")
        );
    }

    #[test]
    fn build_default_headers_extra_headers_can_override_auth() {
        let mut extra = HashMap::new();
        extra.insert("authorization".to_string(), "Bearer override".to_string());
        extra.insert("x-api-key".to_string(), "override-key".to_string());
        let headers = build_default_headers(
            "gateway-key",
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
            &extra,
        )
        .unwrap();
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer override")
        );
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("override-key")
        );
    }

    #[test]
    fn build_default_headers_extra_headers_can_override_version() {
        let mut extra = HashMap::new();
        extra.insert("anthropic-version".to_string(), "2024-10-22".to_string());
        extra.insert(
            "anthropic-beta".to_string(),
            "interleaved-thinking-2025-05-14".to_string(),
        );
        let headers = build_default_headers("k", "https://api.anthropic.com/v1", &extra).unwrap();
        assert_eq!(
            headers
                .get("anthropic-version")
                .and_then(|v| v.to_str().ok()),
            Some("2024-10-22")
        );
        assert_eq!(
            headers.get("anthropic-beta").and_then(|v| v.to_str().ok()),
            Some("interleaved-thinking-2025-05-14")
        );
    }

    #[test]
    fn parse_anthropic_sse_event_decodes_message_start() {
        let data = r#"{"type":"message_start","message":{"id":"m_1","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-5","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#;
        match parse_anthropic_sse_event(data) {
            ParseOutcome::Event(StreamEvent::MessageStart { message }) => {
                assert_eq!(message.id, "m_1");
                assert_eq!(message.usage.input_tokens, 10);
            }
            other => panic!("expected MessageStart, got {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_sse_event_decodes_text_delta() {
        let data =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#;
        match parse_anthropic_sse_event(data) {
            ParseOutcome::Event(StreamEvent::ContentBlockDelta { index, .. }) => {
                assert_eq!(index, 0);
            }
            other => panic!("expected ContentBlockDelta, got {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_sse_event_decodes_message_stop() {
        let data = r#"{"type":"message_stop"}"#;
        match parse_anthropic_sse_event(data) {
            ParseOutcome::Event(StreamEvent::MessageStop) => {}
            other => panic!("expected MessageStop, got {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_sse_event_surfaces_error_event() {
        let data =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"server busy"}}"#;
        match parse_anthropic_sse_event(data) {
            ParseOutcome::Error(err) => {
                assert!(err.to_string().contains("server busy"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_sse_event_skips_unknown_type() {
        let data = r#"{"type":"future_event","payload":42}"#;
        match parse_anthropic_sse_event(data) {
            ParseOutcome::Skip => {}
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn derive_thinking_maps_efforts() {
        assert!(derive_thinking(None).is_none());
        assert!(derive_thinking(Some("")).is_none());
        assert_eq!(
            derive_thinking(Some("off")).unwrap(),
            json!({ "type": "disabled" })
        );
        assert_eq!(
            derive_thinking(Some("medium")).unwrap()["budget_tokens"],
            json!(8_192)
        );
        assert_eq!(
            derive_thinking(Some("MAX")).unwrap()["budget_tokens"],
            json!(32_768)
        );
    }

    #[test]
    fn _unused_warning_silenced() {
        // Touch DEFAULT_STREAM_OPEN_TIMEOUT etc. to keep the linter quiet
        // when SSE tests are filtered.
        let _ = stream_open_timeout();
        let _ = stream_idle_timeout();
    }
}
