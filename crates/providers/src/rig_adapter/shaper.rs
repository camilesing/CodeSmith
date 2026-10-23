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
use rig_core::completion::{Message as RigMessage, ToolDefinition};
use rig_core::message::ToolChoice;

use super::convert;
// `reasoning`, `AssistantContent`, `OneOrMany` are consumed only by
// `GenericShaper` (the OpenAI / openai-compat / DeepSeek family); gate them
// with the same cfg so the `anthropic`-only Lego build stays warning-free.
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
use super::reasoning;
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
use rig_core::OneOrMany;
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
use rig_core::completion::message::AssistantContent;

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

    /// Rewrite the converted rig messages before they become the request's
    /// `messages` / `prompt`. Default no-op. `GenericShaper` uses it to strip
    /// `reasoning_content` for providers/models that reject it (#1542) and to
    /// inject a placeholder on DeepSeek thinking-mode tool-call turns lacking
    /// reasoning (#1739 / #1694). Runs after [`convert::message_to_rig`], so
    /// the pure conversion seam stays provider-agnostic.
    fn shape_messages(&self, _messages: &mut Vec<RigMessage>, _req: &MessageRequest) {}

    /// How `max_tokens` is conveyed on the wire: the standard OpenAI
    /// `max_tokens` field, or the "responses"-style `max_completion_tokens`
    /// rename Xiaomi's API requires (it rejects `max_tokens`). See
    /// [`MaxTokensSpec`].
    fn shape_max_tokens(&self, req: &MessageRequest) -> MaxTokensSpec {
        MaxTokensSpec::MaxTokens(u64::from(req.max_tokens))
    }
}

/// Per-provider `max_tokens` wire shape. `build_request` matches on this to
/// decide whether to call rig's `builder.max_tokens(n)` (the typed
/// `max_tokens` field) or inject `max_completion_tokens` via
/// `additional_params` (xiaomi-mimo's required rename — same value, different
/// key).
pub(crate) enum MaxTokensSpec {
    /// Standard OpenAI `max_tokens` field (rig's typed slot).
    MaxTokens(u64),
    /// OpenAI "responses"-style `max_completion_tokens` key (xiaomi-mimo).
    /// Only constructed by `GenericShaper` (under `openai-compat`); the
    /// `anthropic`-only Lego build never reaches it.
    #[allow(dead_code)]
    MaxCompletionTokens(u64),
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
    /// `CODESMITH_REASONING_PASSTHROUGH` opt-in, resolved once here so the
    /// per-request shaping path stays deterministic (no env lock, testable).
    passthrough: bool,
    /// Which wire key carries `max_tokens`, decided once at construction so
    /// the per-request path matches an enum rather than the provider tag
    /// string (a renamed tag would otherwise silently fall back to
    /// `max_tokens` — the exact field Xiaomi's API rejects).
    max_tokens_key: MaxTokensKey,
}

/// Which wire key a provider expects for the output-token cap.
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaxTokensKey {
    /// Standard OpenAI `max_tokens`.
    MaxTokens,
    /// OpenAI "responses"-style `max_completion_tokens` (Xiaomi rejects
    /// `max_tokens`; same value, different field name — not a clamp).
    MaxCompletionTokens,
}

#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
impl MaxTokensKey {
    /// The wire key for a provider tag — the one place the tag is matched,
    /// at construction, so a renamed tag fails a unit test here instead of
    /// a provider 400 in production.
    fn for_provider(name: &str) -> Self {
        if name == "xiaomi-mimo" {
            Self::MaxCompletionTokens
        } else {
            Self::MaxTokens
        }
    }
}

#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
impl GenericShaper {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            passthrough: super::http_fallback::env_flag_enabled(
                std::env::var("CODESMITH_REASONING_PASSTHROUGH")
                    .ok()
                    .as_deref(),
            ),
            max_tokens_key: MaxTokensKey::for_provider(name),
        }
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
        if let Some(metadata) = req.metadata.clone() {
            map.insert("metadata".to_string(), metadata);
        }
        // Provider-specific effort shaping: sets/overwrites `thinking` and
        // `reasoning_effort`, and emits `chat_template_kwargs` for nvidia-nim /
        // vllm. No-op when `reasoning_effort` is absent. The no-op provider arms
        // (openai, moonshot, …) intentionally do NOT pass `reasoning_effort`
        // through verbatim — those providers reject it, and the hand-written
        // client's `apply_reasoning_effort` is the sole authority on the field
        // (parity fix vs. the previous unconditional passthrough).
        reasoning::apply_reasoning_effort(
            &mut map,
            req.reasoning_effort.as_deref(),
            self.name,
            self.passthrough,
        );
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

    fn shape_messages(&self, messages: &mut Vec<RigMessage>, req: &MessageRequest) {
        let replay = reasoning::should_replay_reasoning_content_for_provider(
            self.name,
            &req.model,
            req.reasoning_effort.as_deref(),
        );
        // Rebuild the vec so a reasoning-only assistant turn that we strip to
        // empty can be dropped outright (rig would drop it on serialization
        // anyway — leaving an empty `Assistant { text("") }` is worse).
        let mut rebuilt: Vec<RigMessage> = Vec::with_capacity(messages.len());
        for msg in messages.drain(..) {
            let shaped = match msg {
                RigMessage::Assistant { id, content } => {
                    let mut items: Vec<AssistantContent> = content.into_iter().collect();
                    if replay {
                        // DeepSeek thinking-mode: every tool-call assistant turn
                        // must carry `reasoning_content`, or the API 400s
                        // (#1739 / #1694). rig's openai conversion already
                        // attaches prior `Thinking` blocks as `reasoning_content`
                        // — this only injects the placeholder when a tool-call
                        // turn has none (session restored from disk, sub-agent
                        // injection, …).
                        let has_toolcall = items
                            .iter()
                            .any(|i| matches!(i, AssistantContent::ToolCall(_)));
                        let has_reasoning = items
                            .iter()
                            .any(|i| matches!(i, AssistantContent::Reasoning(_)));
                        if has_toolcall && !has_reasoning {
                            items.push(AssistantContent::reasoning("(reasoning omitted)"));
                        }
                        Some(RigMessage::Assistant {
                            id,
                            content: one_or_many_assistant(items),
                        })
                    } else {
                        // Strip `reasoning_content` for providers/models that
                        // reject it (#1542). A reasoning-only turn becomes empty
                        // and is dropped.
                        items.retain(|i| !matches!(i, AssistantContent::Reasoning(_)));
                        if items.is_empty() {
                            None
                        } else {
                            Some(RigMessage::Assistant {
                                id,
                                content: one_or_many_assistant(items),
                            })
                        }
                    }
                }
                other => Some(other),
            };
            if let Some(msg) = shaped {
                rebuilt.push(msg);
            }
        }
        *messages = rebuilt;
    }

    fn shape_max_tokens(&self, req: &MessageRequest) -> MaxTokensSpec {
        match self.max_tokens_key {
            MaxTokensKey::MaxCompletionTokens => {
                MaxTokensSpec::MaxCompletionTokens(u64::from(req.max_tokens))
            }
            MaxTokensKey::MaxTokens => MaxTokensSpec::MaxTokens(u64::from(req.max_tokens)),
        }
    }
}

/// Rebuild a non-empty `OneOrMany<AssistantContent>`, substituting a single
/// empty text block only as an unreachable fallback (rig requires non-empty).
#[cfg(any(feature = "openai", feature = "deepseek", feature = "openai-compat"))]
fn one_or_many_assistant(items: Vec<AssistantContent>) -> OneOrMany<AssistantContent> {
    OneOrMany::many(items).unwrap_or_else(|_| OneOrMany::one(AssistantContent::text(String::new())))
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
            match serde_json::to_value(system) {
                Ok(value) => {
                    map.insert("system".to_string(), value);
                }
                Err(err) => {
                    // A swallowed failure here sent the request with no
                    // system prompt at all and nothing observable. Degrade
                    // to the plain-text form (as `GenericShaper` does) so the
                    // prompt is never silently lost; only the per-block
                    // cache_control breakpoints are given up.
                    tracing::warn!(
                        %err,
                        "failed to serialize structured system prompt; falling back to plain text"
                    );
                    let text = match system {
                        SystemPrompt::Text(s) => s.clone(),
                        SystemPrompt::Blocks(blocks) => blocks
                            .iter()
                            .map(|b| b.text.clone())
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                    };
                    map.insert("system".to_string(), serde_json::Value::String(text));
                }
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

#[cfg(all(
    test,
    any(feature = "openai", feature = "deepseek", feature = "openai-compat")
))]
mod tests {
    use super::*;
    use codesmith_agent::models::MessageRequest;
    use rig_core::completion::message::{AssistantContent, ToolCall as RigToolCall, ToolFunction};
    use serde_json::json;

    /// Minimal `MessageRequest` carrying only the fields `shape_messages` and
    /// `shape_max_tokens` consult (`model`, `max_tokens`, `reasoning_effort`).
    fn req_for(model: &str, effort: Option<&str>) -> MessageRequest {
        MessageRequest {
            model: model.to_string(),
            messages: Vec::new(),
            max_tokens: 1024,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: effort.map(str::to_string),
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    fn assistant(items: Vec<AssistantContent>) -> RigMessage {
        RigMessage::Assistant {
            id: None,
            content: one_or_many_assistant(items),
        }
    }

    /// Borrow every `AssistantContent` block of an assistant turn, in order.
    fn assistant_items(msg: &RigMessage) -> Vec<&AssistantContent> {
        match msg {
            RigMessage::Assistant { content, .. } => content.iter().collect(),
            _ => Vec::new(),
        }
    }

    fn tool_call(id: &str, name: &str) -> AssistantContent {
        AssistantContent::ToolCall(RigToolCall::new(
            id.to_string(),
            ToolFunction::new(name.to_string(), json!({})),
        ))
    }

    /// `shape_messages` must strip `reasoning_content` for providers/models
    /// that reject it (#1542) while keeping the surrounding text intact.
    #[test]
    fn shape_messages_strips_reasoning_for_non_replay_provider() {
        let shaper = GenericShaper::new("openai");
        let mut messages = vec![assistant(vec![
            AssistantContent::reasoning("secret thoughts"),
            AssistantContent::text("answer"),
        ])];
        shaper.shape_messages(&mut messages, &req_for("gpt-4", None));
        let items = assistant_items(&messages[0]);
        assert!(
            items.iter().any(|c| matches!(c, AssistantContent::Text(_))),
            "text block must survive the strip"
        );
        assert!(
            !items
                .iter()
                .any(|c| matches!(c, AssistantContent::Reasoning(_))),
            "reasoning must be stripped for a non-replay provider"
        );
    }

    /// A reasoning-only assistant turn becomes empty after the strip and must
    /// be dropped outright — rig would reject an empty `Assistant` message.
    #[test]
    fn shape_messages_drops_reasoning_only_assistant_turn() {
        let shaper = GenericShaper::new("openai");
        let mut messages = vec![
            assistant(vec![AssistantContent::reasoning("only thoughts")]),
            assistant(vec![AssistantContent::text("kept")]),
        ];
        shaper.shape_messages(&mut messages, &req_for("gpt-4", None));
        assert_eq!(messages.len(), 1, "reasoning-only turn must be dropped");
        let items = assistant_items(&messages[0]);
        assert!(items.iter().any(|c| matches!(
            c,
            AssistantContent::Text(t) if t.text() == "kept"
        )));
    }

    /// DeepSeek thinking-mode replay: a tool-call assistant turn lacking
    /// `reasoning_content` must get a placeholder injected (#1739 / #1694).
    #[test]
    fn shape_messages_injects_reasoning_placeholder_on_toolcall_replay() {
        let shaper = GenericShaper::new("deepseek");
        let mut messages = vec![assistant(vec![tool_call("call_1", "search")])];
        shaper.shape_messages(&mut messages, &req_for("deepseek-reasoner", None));
        let items = assistant_items(&messages[0]);
        assert!(
            items
                .iter()
                .any(|c| matches!(c, AssistantContent::ToolCall(_))),
            "tool call must be kept"
        );
        assert!(
            items
                .iter()
                .any(|c| matches!(c, AssistantContent::Reasoning(_))),
            "placeholder reasoning must be injected on a reasoning-less tool-call turn"
        );
    }

    /// When a replay tool-call turn already carries reasoning, the shaper must
    /// keep it and NOT inject a duplicate placeholder. (`Reasoning.content` is
    /// private to rig, so we assert presence + singularity rather than text.)
    #[test]
    fn shape_messages_keeps_reasoning_on_replay_toolcall_turn() {
        let shaper = GenericShaper::new("deepseek");
        let mut messages = vec![assistant(vec![
            AssistantContent::reasoning("real thoughts"),
            tool_call("call_1", "search"),
        ])];
        shaper.shape_messages(&mut messages, &req_for("deepseek-reasoner", None));
        let items = assistant_items(&messages[0]);
        let reasoning_count = items
            .iter()
            .filter(|c| matches!(c, AssistantContent::Reasoning(_)))
            .count();
        assert_eq!(
            reasoning_count, 1,
            "existing reasoning must survive (not stripped), with no duplicate placeholder"
        );
        assert!(
            items
                .iter()
                .any(|c| matches!(c, AssistantContent::ToolCall(_))),
            "tool call must be kept"
        );
    }

    /// `reasoning_effort = "off"` disables replay even for a DeepSeek
    /// reasoning model, so reasoning is stripped like any non-replay turn.
    #[test]
    fn shape_messages_off_effort_disables_replay_and_strips() {
        let shaper = GenericShaper::new("deepseek");
        let mut messages = vec![assistant(vec![
            AssistantContent::reasoning("thoughts"),
            AssistantContent::text("answer"),
        ])];
        shaper.shape_messages(&mut messages, &req_for("deepseek-reasoner", Some("off")));
        let items = assistant_items(&messages[0]);
        assert!(
            !items
                .iter()
                .any(|c| matches!(c, AssistantContent::Reasoning(_))),
            "effort=off must disable replay and strip reasoning"
        );
        assert!(items.iter().any(|c| matches!(c, AssistantContent::Text(_))));
    }

    fn assert_max_tokens(spec: MaxTokensSpec, expected_n: u64, expect_completion: bool) {
        match (spec, expect_completion) {
            (MaxTokensSpec::MaxTokens(n), false) => assert_eq!(n, expected_n),
            (MaxTokensSpec::MaxCompletionTokens(n), true) => assert_eq!(n, expected_n),
            (MaxTokensSpec::MaxTokens(_), true) => {
                panic!("expected MaxCompletionTokens, got MaxTokens")
            }
            (MaxTokensSpec::MaxCompletionTokens(_), false) => {
                panic!("expected MaxTokens, got MaxCompletionTokens")
            }
        }
    }

    /// Xiaomi's API rejects `max_tokens` and requires the
    /// `max_completion_tokens` rename (same value, different key — not a
    /// clamp).
    #[test]
    fn shape_max_tokens_xiaomi_mimo_uses_max_completion_tokens() {
        let shaper = GenericShaper::new("xiaomi-mimo");
        assert_max_tokens(
            shaper.shape_max_tokens(&req_for("mimo-7b", None)),
            1024,
            true,
        );
    }

    /// Every other openai-compat provider keeps the standard `max_tokens` wire
    /// field.
    #[test]
    fn shape_max_tokens_openrouter_uses_max_tokens() {
        let shaper = GenericShaper::new("openrouter");
        assert_max_tokens(
            shaper.shape_max_tokens(&req_for("deepseek/deepseek-chat", None)),
            1024,
            false,
        );
    }

    /// The wire key is resolved once at construction; the per-request path
    /// matches the enum, never the tag string.
    #[test]
    fn max_tokens_key_resolves_at_construction() {
        assert_eq!(
            MaxTokensKey::for_provider("xiaomi-mimo"),
            MaxTokensKey::MaxCompletionTokens
        );
        assert_eq!(
            MaxTokensKey::for_provider("openrouter"),
            MaxTokensKey::MaxTokens
        );
        assert_eq!(
            GenericShaper::new("xiaomi-mimo").max_tokens_key,
            MaxTokensKey::MaxCompletionTokens
        );
    }

    /// The structured system prompt rides `additional_params` as exactly one
    /// `system` key with its per-block `cache_control` intact — the half of
    /// the single-`system`-key invariant this crate owns (rig's side is that
    /// its empty named `system` is skipped; see the `AnthropicShaper` docs).
    #[cfg(feature = "anthropic")]
    #[test]
    fn anthropic_system_blocks_ride_additional_params_with_cache_control() {
        use codesmith_agent::models::{CacheControl, SystemBlock};
        let mut req = req_for("claude", None);
        req.system = Some(SystemPrompt::Blocks(vec![
            SystemBlock {
                block_type: "text".to_string(),
                text: "stable prefix".to_string(),
                cache_control: Some(CacheControl {
                    cache_type: "ephemeral".to_string(),
                }),
            },
            SystemBlock {
                block_type: "text".to_string(),
                text: "tail".to_string(),
                cache_control: None,
            },
        ]));
        let params = AnthropicShaper
            .additional_params(&req)
            .expect("system produces params");
        let map = params.as_object().expect("object");
        assert_eq!(map.keys().filter(|k| *k == "system").count(), 1);
        let blocks = map["system"].as_array().expect("blocks array");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
        assert!(blocks[1].get("cache_control").is_none());
        // And the preamble path stays empty so rig's named `system` is skipped.
        assert!(
            AnthropicShaper
                .system_message(req.system.as_ref().unwrap())
                .is_none()
        );
    }
}
