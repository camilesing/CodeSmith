//! Provider-specific request shaping.
//!
//! rig's `CompletionRequest` is intentionally provider-agnostic, so it has no
//! first-class slots for fields CodeSmith already carries on `MessageRequest`
//! — Anthropic `cache_control`, DeepSeek `reasoning_effort`, `thinking`,
//! `metadata`, per-tool `strict`, …. Those ride through rig's
//! `additional_params` escape hatch, but the *shape* of that passthrough is
//! provider-specific. A [`RequestShaper`] is the strategy object that decides
//! how to fold those fields into the rig request for one provider.
//!
//! The adapter holds an owned `S: RequestShaper` by value, keeping
//! [`RigLlmClient`](super::RigLlmClient) provider-agnostic while letting each
//! factory plug in its own shaping strategy with zero vtable cost.

use codesmith_agent::models::{MessageRequest, SystemPrompt, Tool};
use rig_core::completion::ToolDefinition;
use rig_core::message::ToolChoice;

use super::convert;

/// Strategy for folding CodeSmith's provider-specific request fields into rig's
/// provider-agnostic `CompletionRequest`.
///
/// Each method takes `&self` so a concrete shaper can be held by value as the
/// `S` generic on [`RigLlmClient`](super::RigLlmClient) and borrowed for the
/// request's `&self` lifetime.
pub(crate) trait RequestShaper: Send + Sync {
    /// Stable provider tag surfaced via `LlmClient::provider_name`.
    fn provider_name(&self) -> &'static str;

    /// Reduce CodeSmith's `system` prompt to a plain preamble string for rig's
    /// `CompletionRequestBuilder::preamble`. Return `None` when the provider
    /// needs structured system (e.g. Anthropic `cache_control` blocks), in
    /// which case the system is forwarded through `additional_params` instead.
    fn system_message(&self, system: &SystemPrompt) -> Option<String>;

    /// Build provider-specific `additional_params` merged into the rig request
    /// (e.g. `reasoning_effort`, `thinking`, `metadata`, Anthropic `system`).
    fn additional_params(&self, req: &MessageRequest) -> Option<serde_json::Value>;

    /// Split CodeSmith tools into rig `ToolDefinition`s plus an optional
    /// `additional_params` fragment carrying per-provider tool metadata
    /// (e.g. Anthropic `cache_control` on the tools array).
    fn shape_tools(&self, tools: &[Tool]) -> (Vec<ToolDefinition>, Option<serde_json::Value>);

    /// Map CodeSmith's free-form `tool_choice` JSON to rig's typed
    /// `ToolChoice`. `None` means "let the provider default".
    fn shape_tool_choice(&self, tc: &serde_json::Value) -> Option<ToolChoice>;
}

/// Default shaper for the OpenAI / OpenAI-compatible / DeepSeek family: system
/// is plain text, `thinking` / `reasoning_effort` / `metadata` pass straight
/// through, tools map 1:1 onto `ToolDefinition`. Used as-is by `openai`,
/// `openai-compat`, and `deepseek`; `anthropic` has its own shaper. Compiled
/// only when at least one of those features is enabled — the `anthropic`-only
/// Lego build pulls in neither this type nor its `RequestShaper` impl.
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
pub(crate) struct GenericShaper {
    name: &'static str,
}

#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
impl GenericShaper {
    pub(crate) const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
impl RequestShaper for GenericShaper {
    fn provider_name(&self) -> &'static str {
        self.name
    }

    fn system_message(&self, system: &SystemPrompt) -> Option<String> {
        match system {
            SystemPrompt::Text(s) => Some(s.clone()),
            SystemPrompt::Blocks(blocks) => {
                let text = blocks
                    .iter()
                    .map(|b| b.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                Some(text)
            }
        }
    }

    fn additional_params(&self, req: &MessageRequest) -> Option<serde_json::Value> {
        let mut map = serde_json::Map::new();
        if let Some(thinking) = req.thinking.clone() {
            map.insert("thinking".to_string(), thinking);
        }
        if let Some(effort) = req.reasoning_effort.clone() {
            map.insert("reasoning_effort".to_string(), serde_json::Value::String(effort));
        }
        if let Some(metadata) = req.metadata.clone() {
            map.insert("metadata".to_string(), metadata);
        }
        if map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(map))
        }
    }

    fn shape_tools(&self, tools: &[Tool]) -> (Vec<ToolDefinition>, Option<serde_json::Value>) {
        (convert::tools_to_rig(tools), None)
    }

    fn shape_tool_choice(&self, tc: &serde_json::Value) -> Option<ToolChoice> {
        convert::tool_choice_to_rig(tc)
    }
}

/// Anthropic shaper: forwards the structured `system` prompt (with per-block
/// `cache_control`) plus `thinking` / `metadata` through rig's
/// `additional_params`, deliberately bypassing rig's plain-text `preamble`.
///
/// rig's `AnthropicCompletionRequest` carries a named `system:
/// Vec<SystemContent>` field built from `CompletionRequest.preamble`, alongside
/// a `#[serde(flatten)] additional_params`. By returning `None` from
/// [`RequestShaper::system_message`], rig's preamble stays `None` → the named
/// `system` is `vec![]` → it is dropped by `skip_serializing_if = "Vec::is_empty"`
/// → the `system` key we inject via `additional_params` becomes the *sole*
/// top-level `system` field Anthropic sees, preserving the per-block
/// `cache_control` breakpoints CodeSmith's `SystemPrompt::Blocks` carries.
/// (Verified at `rig-core-0.39.0/src/providers/anthropic/completion.rs:2301` —
/// rig never strips a caller-supplied `system` from `additional_params`.)
#[cfg(feature = "anthropic")]
pub(crate) struct AnthropicShaper;

#[cfg(feature = "anthropic")]
impl RequestShaper for AnthropicShaper {
    fn provider_name(&self) -> &'static str {
        "anthropic"
    }

    fn system_message(&self, _system: &SystemPrompt) -> Option<String> {
        // Routed through `additional_params` instead — see the struct docs.
        None
    }

    fn additional_params(&self, req: &MessageRequest) -> Option<serde_json::Value> {
        let mut map = serde_json::Map::new();
        if let Some(system) = &req.system {
            // `SystemPrompt` is `#[serde(untagged)]`: `Text(String)` serializes
            // to a plain string (Anthropic accepts that), `Blocks(_)` to the
            // `[{type, text, cache_control?}]` array with breakpoints intact.
            if let Ok(value) = serde_json::to_value(system) {
                map.insert("system".to_string(), value);
            }
        }
        if let Some(thinking) = req.thinking.clone() {
            map.insert("thinking".to_string(), thinking);
        }
        if let Some(metadata) = req.metadata.clone() {
            map.insert("metadata".to_string(), metadata);
        }
        if map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(map))
        }
    }

    fn shape_tools(&self, tools: &[Tool]) -> (Vec<ToolDefinition>, Option<serde_json::Value>) {
        (convert::tools_to_rig(tools), None)
    }

    fn shape_tool_choice(&self, tc: &serde_json::Value) -> Option<ToolChoice> {
        convert::tool_choice_to_rig(tc)
    }
}
