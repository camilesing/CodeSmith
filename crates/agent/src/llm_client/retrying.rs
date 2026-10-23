//! Non-streaming retry decorator over an [`LlmClientHandle`].
//!
//! `create_message` calls (compaction summaries, utility-model assists,
//! in-process sub-agent steps, doctor probes) have no retry of their own:
//! the engine's transparent stream retry only covers `create_message_stream`,
//! where the committed-content semantics (`Empty` vs `Partial`) live. This
//! decorator closes that gap by wrapping the non-streaming path in
//! [`with_retry`](super::with_retry) with the host-resolved
//! [`RetryConfig`](super::RetryConfig) — the same policy the `[retry]` config
//! table expresses. Streaming is forwarded untouched: retrying a stream is
//! the engine's decision, not a client property.
//!
//! # Error classification
//!
//! By the time an adapter failure reaches the trait boundary it is an
//! `anyhow::Error` whose HTTP cause is type-erased (rig boxes the underlying
//! `reqwest::Error`), so retryability is recovered from the rendered chain
//! text — matching the transient shapes reqwest/hyper/rig actually emit
//! (`timed out`, `error sending request`, `connection refused/closed/reset`,
//! `failed to connect`), never the bare words "timeout"/"connection":
//! permanent prose that merely names one (a 400 whose body says "'timeout'
//! must be between 1 and 120") must not burn retries. Known limitation: a
//! provider whose transient error text uses none of these shapes classifies
//! as non-retryable `Other` and fails after one attempt.
//!
//! # Recording order
//!
//! Hosts that also wrap a [`RecordingClient`](super::record_replay::RecordingClient)
//! should apply it *outside* this decorator (the CLI's `wrap_with_recording`
//! already does): one recorded line per logical call carrying the final
//! post-retry outcome keeps strict-FIFO replay aligned without re-playing
//! the intermediate failures.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use super::{
    LlmClient, LlmClientHandle, LlmError, RetryCallback, RetryConfig, StreamEventBox, with_retry,
};
use crate::models::{MessageRequest, MessageResponse};

/// Recover an [`LlmError`] from a rendered anyhow chain so `with_retry`'s
/// retryability rules apply. Status codes are hunted as 3-digit runs
/// following the `status code` spelling adapters actually emit.
fn classify(err: &anyhow::Error) -> LlmError {
    let text = format!("{err:#}");
    let lower = text.to_lowercase();
    if let Some(status) = find_status_code(&lower) {
        return LlmError::from_http_response(status, &text);
    }
    // Transient shapes reqwest/hyper/rig actually emit. Bare
    // "timeout"/"connection"/"connect " matched permanent prose too (a 400
    // whose body says "'timeout' must be between 1 and 120", "invalid
    // connection string", a model named "connection-pro") and retried it.
    if lower.contains("timed out") || lower.contains("request timeout") {
        return LlmError::Timeout(Duration::ZERO);
    }
    if lower.contains("error sending request")
        || lower.contains("connection refused")
        || lower.contains("connection closed")
        || lower.contains("connection reset")
        || lower.contains("connection aborted")
        || lower.contains("connection error")
        || lower.contains("failed to connect")
        || lower.contains("unable to connect")
        || lower.contains("could not connect")
        || lower.contains("timed out connecting")
    {
        return LlmError::NetworkError(text);
    }
    LlmError::Other(text)
}

/// Scan for an HTTP status code in error prose: the digit run after
/// `status code` (rig: "Invalid status code 429 with message: …"). Only
/// 3-digit runs classify, so version strings, ports, and token counts do
/// not.
fn find_status_code(lower: &str) -> Option<u16> {
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("status code") {
        let after = search_from + rel + "status code".len();
        let digits: String = lower[after..]
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.len() == 3
            && let Ok(code) = digits.parse::<u16>()
        {
            return Some(code);
        }
        search_from = after;
        if search_from >= lower.len() {
            break;
        }
    }
    None
}

/// Observation hook invoked before each retry sleep (classified error,
/// 0-based attempt, delay). Arc'd so the boxed `RetryCallback` passed to
/// `with_retry` can be re-created per call.
pub type RetryObserver = Arc<dyn Fn(&LlmError, u32, Duration) + Send + Sync>;

/// `LlmClient` decorator retrying only the non-streaming path.
pub struct RetryingClient {
    inner: LlmClientHandle,
    retry: RetryConfig,
    on_retry: Option<RetryObserver>,
}

impl RetryingClient {
    /// Wrap `inner` with `retry` applied to `create_message` only.
    pub fn new(inner: LlmClientHandle, retry: RetryConfig) -> Self {
        Self {
            inner,
            retry,
            on_retry: None,
        }
    }

    /// Observe each retry (error, attempt index, delay). Chainable.
    #[must_use]
    pub fn with_on_retry(mut self, hook: RetryObserver) -> Self {
        self.on_retry = Some(hook);
        self
    }
}

impl LlmClient for RetryingClient {
    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    fn request_headers(&self) -> Vec<(String, String)> {
        self.inner.request_headers()
    }

    fn health_check(&self) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>> {
        self.inner.health_check()
    }

    fn create_message(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
        let inner = self.inner.clone();
        let retry = self.retry.clone();
        let on_retry = self.on_retry.clone();
        Box::pin(async move {
            let callback = on_retry.map(|hook| {
                Box::new(move |err: &LlmError, attempt: u32, delay: Duration| {
                    hook(err, attempt, delay)
                }) as RetryCallback
            });
            with_retry(
                &retry,
                || async {
                    inner
                        .create_message(request.clone())
                        .await
                        .map_err(|e| classify(&e))
                },
                callback,
            )
            .await
            .map_err(anyhow::Error::new)
        })
    }

    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
        self.inner.create_message_stream(request)
    }

    fn fim_completion(
        &self,
        model: String,
        prompt: String,
        suffix: String,
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        self.inner.fim_completion(model, prompt, suffix, max_tokens)
    }

    fn translate(
        &self,
        text: String,
        model: String,
        target_language: String,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        self.inner.translate(text, model, target_language)
    }

    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        self.inner.list_models()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentBlock, Message, Usage};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock inner client: fails the first `fails.count` calls with
    /// `fails.message`, then returns a one-word text response.
    struct FlakyClient {
        fail_count: u32,
        fail_message: &'static str,
        calls: AtomicU32,
    }

    impl LlmClient for FlakyClient {
        fn provider_name(&self) -> &'static str {
            "flaky"
        }
        fn model(&self) -> &str {
            "m"
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let fail_count = self.fail_count;
            let fail_message = self.fail_message;
            Box::pin(async move {
                if n < fail_count {
                    Err(anyhow::anyhow!("{fail_message}"))
                } else {
                    Ok(MessageResponse {
                        id: String::new(),
                        r#type: "message".to_string(),
                        role: "assistant".to_string(),
                        content: vec![ContentBlock::Text {
                            text: "ok".to_string(),
                            cache_control: None,
                        }],
                        model: String::new(),
                        stop_reason: None,
                        stop_sequence: None,
                        container: None,
                        usage: Usage::default(),
                    })
                }
            })
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            Box::pin(async { Err(anyhow::anyhow!("unused in tests")) })
        }
    }

    fn quick_retry() -> RetryConfig {
        RetryConfig {
            initial_delay: 0.0,
            max_delay: 0.0,
            jitter: false,
            ..RetryConfig::default()
        }
    }

    fn request() -> MessageRequest {
        MessageRequest {
            model: String::new(),
            messages: vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "hi".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 8,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    #[tokio::test]
    async fn retryable_rate_limit_error_recovers() {
        let inner = Arc::new(FlakyClient {
            fail_count: 1,
            fail_message: "Invalid status code 429 with message: quota exceeded",
            calls: AtomicU32::new(0),
        });
        let client = RetryingClient::new(inner.clone(), quick_retry());
        let response = client
            .create_message(request())
            .await
            .expect("second attempt succeeds");
        assert!(matches!(
            response.content.first(),
            Some(ContentBlock::Text { text, .. }) if text == "ok"
        ));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn auth_error_fails_without_retry() {
        let inner = Arc::new(FlakyClient {
            fail_count: 5,
            fail_message: "Invalid status code 401 with message: bad key",
            calls: AtomicU32::new(0),
        });
        let client = RetryingClient::new(inner.clone(), quick_retry());
        let err = client
            .create_message(request())
            .await
            .expect_err("auth is non-retryable");
        assert!(err.to_string().contains("Authentication failed"));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn timeout_and_connect_errors_are_retryable() {
        for message in ["operation timed out", "error sending request for url"] {
            let inner = Arc::new(FlakyClient {
                fail_count: 1,
                fail_message: message,
                calls: AtomicU32::new(0),
            });
            let client = RetryingClient::new(inner.clone(), quick_retry());
            client
                .create_message(request())
                .await
                .expect("transient shape recovers on retry");
            assert_eq!(inner.calls.load(Ordering::SeqCst), 2, "{message}");
        }
    }

    #[tokio::test]
    async fn permanent_prose_naming_transient_words_is_not_retried() {
        // A permanent failure whose body merely mentions "timeout"/
        // "connection" must not inherit transient retry semantics.
        for message in [
            "'timeout' must be between 1 and 120",
            "invalid connection string for provider",
        ] {
            let inner = Arc::new(FlakyClient {
                fail_count: 5,
                fail_message: message,
                calls: AtomicU32::new(0),
            });
            let client = RetryingClient::new(inner.clone(), quick_retry());
            let _ = client.create_message(request()).await;
            assert_eq!(inner.calls.load(Ordering::SeqCst), 1, "{message}");
        }
    }

    #[tokio::test]
    async fn on_retry_hook_observes_attempts() {
        let inner = Arc::new(FlakyClient {
            fail_count: 1,
            fail_message: "Invalid status code 503 with message: unavailable",
            calls: AtomicU32::new(0),
        });
        let seen = Arc::new(AtomicU32::new(0));
        let hook_seen = seen.clone();
        let client = RetryingClient::new(inner, quick_retry()).with_on_retry(Arc::new(
            move |_err, attempt, _delay| {
                assert_eq!(attempt, 0);
                hook_seen.fetch_add(1, Ordering::SeqCst);
            },
        ));
        client.create_message(request()).await.expect("recovers");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn find_status_code_rejects_non_status_digits() {
        assert_eq!(
            find_status_code("invalid status code 429 with message: x"),
            Some(429)
        );
        assert_eq!(
            find_status_code("invalid status code 503 with message: x"),
            Some(503)
        );
        assert_eq!(
            find_status_code("error sending request for url (http://x)"),
            None
        );
        assert_eq!(find_status_code("model gpt-4o not found"), None);
        // 4-digit and 2-digit runs do not classify as status codes.
        assert_eq!(find_status_code("status code 12345 broke"), None);
        assert_eq!(find_status_code("status code 42 broke"), None);
    }
}
