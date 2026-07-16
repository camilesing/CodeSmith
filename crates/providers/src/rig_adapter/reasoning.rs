//! DeepSeek thinking-mode reasoning protocol quirks for the rig-backed
//! OpenAI / openai-compat factories — the provider-aware wrapper and the
//! per-provider `reasoning_effort` translation, ported from the hand-written
//! TUI client so the rig adapter reaches behavioural parity without the
//! host's concrete client.
//!
//! The base model-name predicates (`requires_reasoning_content`,
//! `should_replay_reasoning_content`, `has_deepseek_r_series_marker`) were
//! lifted to `codesmith_agent::reasoning` (ROADMAP §A slice 42) — this module
//! imports them from there. What stays here is provider-shaping: the
//! provider-name allowlist (`provider_accepts_reasoning_content`), the
//! load-bearing `should_replay_reasoning_content_for_provider` wrapper, and
//! `apply_reasoning_effort` (the `serde_json` `Map` translation rig has no
//! first-class slot for).
//!
//! These functions are keyed on a provider-name `&str` (e.g. `"openrouter"`,
//! `"deepseek"`) — the same string `GenericShaper::new(name)` holds and
//! `ApiProvider::as_str()` yields — rather than the TUI's `ApiProvider` enum,
//! so this crate stays off the `codesmith-agent-runtime` dep edge (ROADMAP §C6).

use codesmith_agent::reasoning::{requires_reasoning_content, should_replay_reasoning_content};
use serde_json::{Map, Value, json};

/// The 9 providers whose API tolerates a `reasoning_content` field on assistant
/// messages even for generic (non-DeepSeek) models. Keyed on `ApiProvider::as_str()`
/// values, which match the `GenericShaper::new(name)` names and `COMPAT_KINDS`.
///
/// Not in the allowlist (field rejected): `openai`, `atlascloud`, `wanjie-ark`,
/// `volcengine`, `moonshot`, `vllm`, `ollama`, `anthropic`.
pub(crate) fn provider_accepts_reasoning_content(name: &str) -> bool {
    matches!(
        name,
        "deepseek"
            | "deepseek-cn"
            | "nvidia-nim"
            | "openrouter"
            | "xiaomi-mimo"
            | "novita"
            | "fireworks"
            | "siliconflow"
            | "sglang"
    )
}

/// Provider-aware replay gate — the load-bearing predicate for both the
/// attach/strip decision and the placeholder injection.
///
/// Truth table:
/// - generic model + provider-rejects-field → `false` (STRIP — #1542 fix)
/// - generic model + provider-accepts-field → `requires_reasoning_content` = `false` (STRIP)
/// - DeepSeek reasoning model on ANY provider → `true` (ATTACH — #1739/#1694 fix)
/// - any model + effort "off"/… → `false` (STRIP)
pub(crate) fn should_replay_reasoning_content_for_provider(
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> bool {
    if !provider_accepts_reasoning_content(provider) && !requires_reasoning_content(model) {
        // Generic non-DeepSeek model on a provider that rejects the field:
        // keep stripping it (preserves the #1542 fix). But a known DeepSeek
        // reasoning model pointed at a DeepSeek-compatible endpoint via the
        // generic `openai` provider still requires reasoning_content replay,
        // or the thinking-mode API returns 400 (#1739 / #1694).
        return false;
    }
    should_replay_reasoning_content(model, effort)
}

// Streaming-path reasoning detection (the former "Gap E") is no longer needed:
// rig's OpenAI / DeepSeek compat layer parses `delta.reasoning_content` into
// `StreamedAssistantContent::ReasoningDelta` natively (see
// `rig-core-0.39.0/src/providers/internal/openai_chat_completions_compatible.rs`
// + `src/streaming.rs`), and `stream::map_rig_stream` maps those onto
// `ContentBlockStart::Thinking` / `ThinkingDelta`. There is no model-level
// gating to do — rig separates reasoning from content for every model, so the
// hand-written client's `is_reasoning_model_for_stream` predicate has no rig
// analogue.

/// Translate CodeSmith's neutral `reasoning_effort` string into the per-provider
/// wire fields (`thinking`, `reasoning_effort`, `chat_template_kwargs`) rig has
/// no first-class slot for. Operates on the `additional_params` map (rig flattens
/// it onto the request body), so `map.insert("thinking", …)` is the rig analogue
/// of the hand-written client's `body["thinking"] = …`.
///
/// No-op when `effort` is `None` (the engine leaves reasoning off → send nothing,
/// matching the hand-written early-return). For the no-op provider arms (e.g.
/// `openai`, `anthropic`), nothing is written — `reasoning_effort` is **not**
/// passed through verbatim, which is the parity fix for providers that reject it.
pub(crate) fn apply_reasoning_effort(
    params: &mut Map<String, Value>,
    effort: Option<&str>,
    provider: &str,
) {
    let Some(effort) = effort else {
        return;
    };
    let normalized = effort.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "off" | "disabled" | "none" | "false" => match provider {
            "deepseek" | "deepseek-cn" | "openrouter" | "xiaomi-mimo" | "novita"
            | "siliconflow" | "sglang" | "volcengine" => {
                params.insert("thinking".to_string(), json!({ "type": "disabled" }));
            }
            "fireworks" => {}
            // vLLM is an OpenAI-protocol server, not an Anthropic-protocol one.
            // For Qwen3 / DeepSeek-R1 / other reasoning models hosted via vLLM,
            // the canonical OpenAI extension to disable thinking is
            // `chat_template_kwargs.enable_thinking`. The old
            // `thinking: {type: disabled}` field is Anthropic-native and
            // silently ignored by vLLM — the model still emits a full
            // reasoning trace, causing 10+ seconds of perceived "freeze"
            // before the first content token (PR #1480 by @h3c-hexin).
            "vllm" => {
                params.insert(
                    "chat_template_kwargs".to_string(),
                    json!({ "enable_thinking": false }),
                );
            }
            "openai" | "atlascloud" | "wanjie-ark" | "moonshot" | "ollama" => {}
            // Anthropic uses /v1/messages and the AnthropicShaper applies its
            // own thinking config; GenericShaper is never used for anthropic.
            "anthropic" => {}
            "nvidia-nim" => {
                params.insert(
                    "chat_template_kwargs".to_string(),
                    json!({ "thinking": false }),
                );
            }
            _ => {}
        },
        "low" | "minimal" | "medium" | "mid" | "high" | "" => match provider {
            // DeepSeek compatibility: low/medium both map to high.
            "deepseek" | "deepseek-cn" | "siliconflow" | "sglang" | "volcengine" => {
                params.insert("reasoning_effort".to_string(), json!("high"));
                params.insert("thinking".to_string(), json!({ "type": "enabled" }));
            }
            // OpenRouter/Novita: pass through the actual user-chosen value.
            // OpenRouter's unified scale is none/minimal/low/medium/high/xhigh;
            // DeepSeek models hosted there accept those directly.
            "openrouter" | "novita" => {
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                params.insert("reasoning_effort".to_string(), json!(value));
                params.insert("thinking".to_string(), json!({ "type": "enabled" }));
            }
            "xiaomi-mimo" => {
                params.insert("thinking".to_string(), json!({ "type": "enabled" }));
            }
            "fireworks" => {
                params.insert("reasoning_effort".to_string(), json!("high"));
            }
            "vllm" => {
                params.insert(
                    "chat_template_kwargs".to_string(),
                    json!({ "enable_thinking": true }),
                );
                // vLLM supports low/medium/high natively — pass through the
                // user-chosen value instead of hard-coding "high".
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                params.insert("reasoning_effort".to_string(), json!(value));
            }
            "openai" | "atlascloud" | "wanjie-ark" | "moonshot" | "ollama" => {}
            "anthropic" => {}
            "nvidia-nim" => {
                params.insert(
                    "chat_template_kwargs".to_string(),
                    json!({ "thinking": true, "reasoning_effort": "high" }),
                );
            }
            _ => {}
        },
        "xhigh" | "max" | "highest" => match provider {
            "deepseek" | "deepseek-cn" | "siliconflow" | "sglang" | "volcengine" => {
                params.insert("reasoning_effort".to_string(), json!("max"));
                params.insert("thinking".to_string(), json!({ "type": "enabled" }));
            }
            "openrouter" | "novita" => {
                params.insert("reasoning_effort".to_string(), json!("xhigh"));
                params.insert("thinking".to_string(), json!({ "type": "enabled" }));
            }
            "xiaomi-mimo" => {
                params.insert("thinking".to_string(), json!({ "type": "enabled" }));
            }
            "fireworks" => {
                params.insert("reasoning_effort".to_string(), json!("max"));
            }
            "vllm" => {
                params.insert(
                    "chat_template_kwargs".to_string(),
                    json!({ "enable_thinking": true }),
                );
                // vLLM only supports none/low/medium/high — downgrade
                // "max" to "high" instead of sending an invalid value.
                params.insert("reasoning_effort".to_string(), json!("high"));
            }
            "openai" | "atlascloud" | "wanjie-ark" | "moonshot" | "ollama" => {}
            "anthropic" => {}
            "nvidia-nim" => {
                params.insert(
                    "chat_template_kwargs".to_string(),
                    json!({ "thinking": true, "reasoning_effort": "max" }),
                );
            }
            _ => {}
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- should_replay_reasoning_content_for_provider ------------------------

    #[test]
    fn replay_truth_table() {
        // generic model + provider-rejects-field → strip
        assert!(!should_replay_reasoning_content_for_provider("openai", "gpt-4o", Some("high")));
        assert!(!should_replay_reasoning_content_for_provider("vllm", "qwen3-235b", Some("high")));
        // generic model + provider-accepts-field → still strip (model is not reasoning)
        assert!(!should_replay_reasoning_content_for_provider(
            "openrouter", "qwen3-235b", Some("high"
        )));
        // DeepSeek reasoning model on ANY provider → attach
        assert!(should_replay_reasoning_content_for_provider("openrouter", "deepseek-chat", Some("high")));
        assert!(should_replay_reasoning_content_for_provider("openai", "deepseek-r1", Some("medium")));
        assert!(should_replay_reasoning_content_for_provider("deepseek", "deepseek-v4-pro", Some("high")));
        // effort off overrides everything
        assert!(!should_replay_reasoning_content_for_provider("deepseek", "deepseek-chat", Some("off")));
        assert!(!should_replay_reasoning_content_for_provider("deepseek", "deepseek-chat", Some("disabled")));
        assert!(!should_replay_reasoning_content_for_provider("deepseek", "deepseek-chat", Some("none")));
        assert!(!should_replay_reasoning_content_for_provider("deepseek", "deepseek-chat", Some("false")));
        // effort None → model-driven
        assert!(should_replay_reasoning_content_for_provider("deepseek", "deepseek-chat", None));
        assert!(!should_replay_reasoning_content_for_provider("openai", "gpt-4o", None));
    }

    // --- apply_reasoning_effort ---------------------------------------------

    fn params_for(effort: Option<&str>, provider: &str) -> Map<String, Value> {
        let mut map = Map::new();
        apply_reasoning_effort(&mut map, effort, provider);
        map
    }

    #[test]
    fn effort_none_is_noop() {
        assert!(params_for(None, "deepseek").is_empty());
        assert!(params_for(None, "openrouter").is_empty());
        assert!(params_for(None, "openai").is_empty());
    }

    #[test]
    fn deepseek_effort_shaping() {
        // low/medium collapse to high for the DeepSeek family.
        let m = params_for(Some("low"), "deepseek");
        assert_eq!(m["reasoning_effort"], json!("high"));
        assert_eq!(m["thinking"], json!({ "type": "enabled" }));
        let m = params_for(Some("medium"), "siliconflow");
        assert_eq!(m["reasoning_effort"], json!("high"));
        // xhigh/max → "max"
        let m = params_for(Some("max"), "sglang");
        assert_eq!(m["reasoning_effort"], json!("max"));
    }

    #[test]
    fn openrouter_passes_through_effort_tier() {
        let low = params_for(Some("low"), "openrouter");
        assert_eq!(low["reasoning_effort"], json!("low"));
        let med = params_for(Some("medium"), "novita");
        assert_eq!(med["reasoning_effort"], json!("medium"));
        let high = params_for(Some("high"), "openrouter");
        assert_eq!(high["reasoning_effort"], json!("high"));
        assert_eq!(high["thinking"], json!({ "type": "enabled" }));
        let xhigh = params_for(Some("xhigh"), "openrouter");
        assert_eq!(xhigh["reasoning_effort"], json!("xhigh"));
    }

    #[test]
    fn xiaomi_mimo_emits_thinking_only() {
        let m = params_for(Some("high"), "xiaomi-mimo");
        assert_eq!(m["thinking"], json!({ "type": "enabled" }));
        assert!(m.get("reasoning_effort").is_none());
    }

    #[test]
    fn nvidia_nim_uses_chat_template_kwargs() {
        let m = params_for(Some("high"), "nvidia-nim");
        assert_eq!(
            m["chat_template_kwargs"],
            json!({ "thinking": true, "reasoning_effort": "high" })
        );
        let off = params_for(Some("off"), "nvidia-nim");
        assert_eq!(off["chat_template_kwargs"], json!({ "thinking": false }));
    }

    #[test]
    fn vllm_uses_enable_thinking() {
        let m = params_for(Some("high"), "vllm");
        assert_eq!(m["chat_template_kwargs"], json!({ "enable_thinking": true }));
        assert_eq!(m["reasoning_effort"], json!("high"));
        let off = params_for(Some("off"), "vllm");
        assert_eq!(off["chat_template_kwargs"], json!({ "enable_thinking": false }));
        // max downgraded to high
        let max = params_for(Some("max"), "vllm");
        assert_eq!(max["reasoning_effort"], json!("high"));
    }

    #[test]
    fn openai_noop_does_not_send_reasoning_effort() {
        // The parity fix: openai's no-op arm must NOT inject reasoning_effort.
        let m = params_for(Some("high"), "openai");
        assert!(m.get("reasoning_effort").is_none());
        assert!(m.get("thinking").is_none());
        assert!(params_for(Some("off"), "openai").is_empty());
    }

    #[test]
    fn off_arm_emits_disabled_thinking_for_deepseek_family() {
        let m = params_for(Some("off"), "deepseek");
        assert_eq!(m["thinking"], json!({ "type": "disabled" }));
        assert!(m.get("reasoning_effort").is_none());
        let m = params_for(Some("disabled"), "openrouter");
        assert_eq!(m["thinking"], json!({ "type": "disabled" }));
    }

    #[test]
    fn fireworks_reasoning_effort_without_thinking() {
        let m = params_for(Some("high"), "fireworks");
        assert_eq!(m["reasoning_effort"], json!("high"));
        assert!(m.get("thinking").is_none());
        assert!(params_for(Some("off"), "fireworks").is_empty());
    }
}
