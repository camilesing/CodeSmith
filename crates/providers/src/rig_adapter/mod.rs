//! rig-backed adapter that implements CodeSmith's `LlmClient` trait by
//! delegating to a rig [`CompletionClient`].
//!
//! This is the heart of Strategy A from the integration plan: CodeSmith keeps
//! its own `LlmClient` abstraction as a stable seam (pi-mono style), and rig is
//! *one swappable implementation* of it — not the framework vocabulary. The
//! engine is unchanged; it still talks `MessageRequest` / `StreamEvent`. Each
//! provider factory in the parent crate instantiates a concrete
//! `RigLlmClient<rig::providers::<x>::Client>` and hands it back as an
//! `Arc<dyn LlmClient>`.
//!
//! `CompletionModel` is not object-safe (its `completion`/`stream` methods
//! return `impl Future`), so the adapter is generic over `C: CompletionClient`
//! rather than holding a `dyn CompletionModel`. Each factory monomorphises a
//! concrete `C`, then type-erases to `Arc<dyn LlmClient>`.
//!
//! A second generic `S: RequestShaper` lets each factory plug in its own
//! provider-specific shaping strategy (system-prompt handling,
//! `additional_params` layout, tool metadata) with zero vtable cost and no
//! `'static` leaking — the shaper is owned by value and borrowed for the
//! request's `'&self` lifetime.

mod convert;
// `reasoning` is consumed only by `GenericShaper` (the OpenAI / openai-compat /
// DeepSeek family); Anthropic has its own thinking config and never calls it.
// Gate it with the same cfg so the `anthropic`-only Lego build stays
// warning-free.
mod fim_translate;
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
mod reasoning;
mod shaper;
mod stream;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use codesmith_agent::llm_client::{LlmClient, StreamEventBox};
use codesmith_agent::models::MessageRequest;
use rig_core::client::CompletionClient;
use rig_core::completion::{CompletionModel, CompletionRequestBuilder};

#[cfg(feature = "deepseek")]
pub(crate) use fim_translate::resolve_base_url;
#[cfg(feature = "anthropic")]
pub(crate) use shaper::AnthropicShaper;
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
pub(crate) use shaper::GenericShaper;
pub(crate) use shaper::RequestShaper;

/// Monotonic counter for synthetic message IDs. rig doesn't always surface a
/// provider message ID (and the streaming `MessageStart` fires before the
/// `Final` payload that carries one), so we mint a local, distinguishable id
/// for the engine's bookkeeping. The `msg_synth_` prefix keeps it greppable
/// apart from real provider IDs.
static SYNTH_MSG_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn synth_message_id() -> String {
    let n = SYNTH_MSG_ID.fetch_add(1, Ordering::Relaxed);
    format!("msg_synth_{n}")
}

/// Convert CodeSmith's stringly-typed extra HTTP headers into a typed
/// `reqwest::header::HeaderMap`. `reqwest` re-exports `http::HeaderMap`, so the
/// result type is identical to what rig's `ClientBuilder::http_headers` and
/// reqwest's `ClientBuilder::default_headers` expect — one helper serves both.
///
/// Malformed header names/values are silently skipped: a provider config with a
/// bad header should not crash client construction, and the request still goes
/// out with the valid headers + bearer auth.
pub(crate) fn build_header_map(
    headers: &std::collections::HashMap<String, String>,
) -> reqwest::header::HeaderMap {
    use std::convert::TryFrom;
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let Ok(name) = reqwest::header::HeaderName::try_from(name.as_str()) else {
            continue;
        };
        let Ok(value) = reqwest::header::HeaderValue::try_from(value.as_str()) else {
            continue;
        };
        map.insert(name, value);
    }
    map
}

/// An LLM client that satisfies CodeSmith's `LlmClient` contract by delegating
/// chat completions / streaming to a rig `CompletionClient`.
///
/// The `shaper` folds provider-specific request fields (cache_control,
/// reasoning_effort, …) into rig's `additional_params`. The optional `http`
/// client powers the DeepSeek FIM/translate shim; when `None`, `fim_completion`
/// / `translate` return "not supported" (matching the trait default for
/// providers without those endpoints).
pub(crate) struct RigLlmClient<C, S> {
    client: C,
    default_model: String,
    shaper: S,
    base_url: String,
    api_key: Option<String>,
    http: Option<reqwest::Client>,
}

impl<C, S> RigLlmClient<C, S> {
    /// Assemble an adapter. Factories pass `Some(http)` only when the provider
    /// surfaces FIM/translate (DeepSeek); others pass `None`.
    pub(crate) fn new(
        client: C,
        default_model: String,
        shaper: S,
        base_url: String,
        api_key: Option<String>,
        http: Option<reqwest::Client>,
    ) -> Self {
        Self {
            client,
            default_model,
            shaper,
            base_url,
            api_key,
            http,
        }
    }
}

/// Build a rig `CompletionRequestBuilder` from a CodeSmith `MessageRequest`.
///
/// The last CodeSmith message becomes the rig `prompt`; everything before it is
/// `chat_history`. `system`, `tools`, `tool_choice`, `temperature`,
/// `max_tokens`, and provider-specific extras are routed through the shaper.
fn build_request<C, S>(
    client: &C,
    model_id: &str,
    req: &MessageRequest,
    shaper: &S,
) -> Result<CompletionRequestBuilder<C::CompletionModel>>
where
    C: CompletionClient,
    S: RequestShaper,
{
    let model = client.completion_model(model_id);

    let mut rig_messages: Vec<rig_core::completion::Message> = req
        .messages
        .iter()
        .map(convert::message_to_rig)
        .collect::<Result<_>>()?;

    // Provider-specific message rewriting (strip/inject `reasoning_content`)
    // runs on the full history before the last message is popped as the prompt,
    // so tool-call assistant turns in the history get the DeepSeek placeholder
    // (#1739/#1694) or the #1542 strip.
    shaper.shape_messages(&mut rig_messages, req);

    // The shaper may decline to produce a preamble (e.g. Anthropic forwards
    // structured system through additional_params instead).
    let preamble = req.system.as_ref().and_then(|s| shaper.system_message(s));

    let prompt = rig_messages
        .pop()
        .ok_or_else(|| anyhow!("MessageRequest must contain at least one message"))?;

    let mut builder = CompletionRequestBuilder::new(model, prompt);
    if !rig_messages.is_empty() {
        builder = builder.messages(rig_messages);
    }
    if let Some(preamble) = preamble {
        builder = builder.preamble(preamble);
    }

    let tools_in = req.tools.as_deref().unwrap_or(&[]);
    let (tools, tools_extra) = shaper.shape_tools(tools_in);
    if !tools.is_empty() {
        builder = builder.tools(tools);
    }

    match shaper.shape_max_tokens(req) {
        shaper::MaxTokensSpec::MaxTokens(n) => builder = builder.max_tokens(n),
        shaper::MaxTokensSpec::MaxCompletionTokens(n) => {
            // xiaomi-mimo rejects `max_tokens`; emit the "responses"-style
            // rename via additional_params (rig merges repeated calls).
            builder = builder.additional_params(serde_json::json!({
                "max_completion_tokens": n,
            }));
        }
    }
    if let Some(temp) = req.temperature {
        builder = builder.temperature(f64::from(temp));
    }
    if let Some(tc) = req
        .tool_choice
        .as_ref()
        .and_then(|v| shaper.shape_tool_choice(v))
    {
        builder = builder.tool_choice(tc);
    }

    // The builder merges repeated additional_params calls, so shaper output and
    // tool-level extras compose without us hand-merging JSON.
    if let Some(ap) = shaper.additional_params(req) {
        builder = builder.additional_params(ap);
    }
    if let Some(extra) = tools_extra {
        builder = builder.additional_params(extra);
    }

    Ok(builder)
}

impl<C, S> LlmClient for RigLlmClient<C, S>
where
    C: CompletionClient + Clone + Send + Sync + 'static,
    C::CompletionModel: CompletionModel + Send + Sync + 'static,
    <C::CompletionModel as CompletionModel>::Response: Send + 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse:
        Clone + Unpin + rig_core::completion::GetTokenUsage + Send + 'static,
    S: RequestShaper + Send + Sync + 'static,
{
    fn provider_name(&self) -> &'static str {
        self.shaper.provider_name()
    }

    fn model(&self) -> &str {
        &self.default_model
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn create_message(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<codesmith_agent::models::MessageResponse>> + Send + '_>>
    {
        let client = self.client.clone();
        let shaper = &self.shaper;
        let default_model = self.default_model.clone();
        Box::pin(async move {
            let model_id = if request.model.is_empty() {
                default_model
            } else {
                request.model.clone()
            };
            let builder = build_request(&client, &model_id, &request, shaper)?;
            let response = builder.send().await.map_err(anyhow::Error::new)?;
            convert::build_message_response(
                response.message_id,
                request.model.clone(),
                response.choice,
                &response.usage,
            )
        })
    }

    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
        let client = self.client.clone();
        let shaper = &self.shaper;
        let default_model = self.default_model.clone();
        Box::pin(async move {
            let model_id = if request.model.is_empty() {
                default_model
            } else {
                request.model.clone()
            };
            let builder = build_request(&client, &model_id, &request, shaper)?;
            let stream = builder.stream().await.map_err(anyhow::Error::new)?;
            let mapped = stream::map_rig_stream(stream, request.model.clone());
            Ok(Box::pin(mapped) as StreamEventBox)
        })
    }

    fn fim_completion(
        &self,
        model: String,
        prompt: String,
        suffix: String,
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        let Some(http) = self.http.clone() else {
            return Box::pin(async move {
                Err(anyhow!(
                    "FIM completion not supported by provider '{}'",
                    self.provider_name()
                ))
            });
        };
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone().unwrap_or_default();
        Box::pin(async move {
            fim_translate::fim_completion(
                &http, &base_url, &api_key, model, prompt, suffix, max_tokens,
            )
            .await
        })
    }

    fn translate(
        &self,
        text: String,
        model: String,
        target_language: String,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        let Some(http) = self.http.clone() else {
            return Box::pin(async move {
                Err(anyhow!(
                    "Translation not supported by provider '{}'",
                    self.provider_name()
                ))
            });
        };
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone().unwrap_or_default();
        Box::pin(async move {
            fim_translate::translate(&http, &base_url, &api_key, text, model, target_language).await
        })
    }

    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        // Only the `deepseek` factory wires an HTTP shim into `RigLlmClient`
        // (for FIM/translate); other providers pass `None` and inherit the
        // "empty list" behaviour. Reusing the FIM http client for `/models` is
        // sound because the two surfaces coincide on DeepSeek (same base_url +
        // bearer auth). OpenAI-compat `/models` for the other providers is a
        // separate future improvement — today those return empty, matching the
        // trait default.
        let Some(http) = self.http.clone() else {
            return Box::pin(async move { Ok(Vec::new()) });
        };
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone().unwrap_or_default();
        Box::pin(async move { fim_translate::list_models(&http, &base_url, &api_key).await })
    }
}
