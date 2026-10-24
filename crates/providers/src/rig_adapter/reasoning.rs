//! DeepSeek thinking-mode reasoning protocol quirks for the rig-backed
//! OpenAI / openai-compat factories — the provider-aware wrapper and the
//! per-provider `reasoning_effort` translation, ported from the retired
//! hand-written client so the rig adapter reaches behavioural parity without
//! the host's concrete client.
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
//! `ApiProvider::as_str()` yields — rather than agent-runtime's `ApiProvider` enum,
//! so this crate stays off the `codesmith-agent-runtime` dep edge (ROADMAP §C6).

use codesmith_agent::reasoning::{
    requires_reasoning_content, reasoning_scope, should_replay_reasoning_content, ReasoningScope,
};
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

/// The known providers from the reject list above — unlike an unrecognized
/// (custom) name, these are confirmed to reject the `reasoning_content` field,
/// so even a reasoning-looking model id must not ship it (#224).
pub(crate) fn provider_rejects_reasoning_content(name: &str) -> bool {
    matches!(
        name,
        "openai"
            | "atlascloud"
            | "wanjie-ark"
            | "volcengine"
            | "moonshot"
            | "vllm"
            | "ollama"
            | "anthropic"
    )
}

/// Whether `model` matched a DeepSeek-scoped arm of
/// [`requires_reasoning_content`] rather than its generic
/// `*reasoner*` / `*-reasoning` / `*-thinking` globs. Resolved via the
/// upstream [`reasoning_scope`] so the exactness is shared, not re-derived
/// by substring: `contains("deepseek")` was broader than the DeepSeek arms
/// and let glob-only ids (e.g. `acme-deepseek-turbo-reasoner`) escape the
/// #224 strip on providers confirmed to reject the field.
fn deepseek_scoped(model: &str) -> bool {
    reasoning_scope(model) == ReasoningScope::DeepSeek
}

/// Provider-aware replay gate — the load-bearing predicate for both the
/// attach/strip decision and the placeholder injection.
///
/// Truth table:
/// - generic model + provider-rejects-field → `false` (STRIP — #1542 fix)
/// - generic model + provider-accepts-field → `requires_reasoning_content` = `false` (STRIP)
/// - glob-only model (`*reasoner*`/`*-reasoning`/`*-thinking`) + known
///   field-rejecting provider → `false` (STRIP — #224; the generic globs are
///   for custom deployments, which unknown provider names represent)
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
    if provider_rejects_reasoning_content(provider) && !deepseek_scoped(model) {
        // #224: the generic name globs are for custom/proxied deployments,
        // which an unrecognized provider name already covers. On a provider
        // known to reject the field they would only ship a 400-triggering
        // `reasoning_content`.
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
///
/// `passthrough` is the `CODESMITH_REASONING_PASSTHROUGH` opt-in, resolved once
/// by the caller (the shaper) at construction so this function stays
/// deterministic and testable.
pub(crate) fn apply_reasoning_effort(
    params: &mut Map<String, Value>,
    effort: Option<&str>,
    provider: &str,
    passthrough: bool,
) {
    let Some(effort) = effort else {
        return;
    };
    let normalized = effort.trim().to_ascii_lowercase();

    // Third-party OpenAI-compatible gateways (Zhipu GLM, …) accept the
    // `reasoning_effort` / `thinking` fields even though the official OpenAI
    // endpoint rejects them. `CODESMITH_REASONING_PASSTHROUGH=1` opts the
    // generic `openai` provider arm into verbatim forwarding so those
    // gateways get thinking control too. Empty effort is excluded: the
    // non-passthrough openai arm sends nothing for it, and forwarding ""
    // verbatim earns a gateway 400 (the failure mode this adapter works
    // around) — the deepseek family's deliberate ""→high mapping below is
    // untouched.
    if provider == "openai" && !normalized.is_empty() && passthrough {
        match normalized.as_str() {
            "off" | "disabled" | "none" | "false" => {
                params.insert(
                    "thinking".to_string(),
                    serde_json::json!({ "type": "disabled" }),
                );
            }
            "xhigh" | "max" | "highest" => {
                params.insert("reasoning_effort".to_string(), serde_json::json!("high"));
            }
            // Known tiers forward verbatim; anything else (a typo like
            // "ultra") is a no-op, sharing the policy of the non-passthrough
            // match below — forwarding it verbatim earns the gateway 400
            // this adapter exists to work around.
            "low" | "minimal" | "medium" | "mid" | "high" => {
                params.insert(
                    "reasoning_effort".to_string(),
                    serde_json::json!(normalized),
                );
            }
            _ => {}
        }
        return;
    }

    // "" means "send nothing" everywhere except the DeepSeek family, whose
    // deliberate ""→high mapping is covered by tests. Anywhere else the
    // tier arms below would force thinking on at the highest tier when the
    // engine meant it off.
    if normalized.is_empty()
        && !matches!(
            provider,
            "deepseek" | "deepseek-cn" | "siliconflow" | "sglang" | "volcengine"
        )
    {
        return;
    }

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
                merge_chat_template_kwargs(params, "enable_thinking", json!(false));
            }
            "openai" | "atlascloud" | "wanjie-ark" | "moonshot" | "ollama" => {}
            // Anthropic uses /v1/messages and the AnthropicShaper applies its
            // own thinking config; GenericShaper is never used for anthropic.
            "anthropic" => {}
            "nvidia-nim" => {
                merge_chat_template_kwargs(params, "thinking", json!(false));
            }
            _ => tracing::debug!(provider, "no reasoning-effort shaping for provider"),
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
                merge_chat_template_kwargs(params, "enable_thinking", json!(true));
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
                merge_chat_template_kwargs(params, "thinking", json!(true));
                merge_chat_template_kwargs(params, "reasoning_effort", json!("high"));
            }
            _ => tracing::debug!(provider, "no reasoning-effort shaping for provider"),
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
                merge_chat_template_kwargs(params, "enable_thinking", json!(true));
                // vLLM only supports none/low/medium/high — downgrade
                // "max" to "high" instead of sending an invalid value.
                params.insert("reasoning_effort".to_string(), json!("high"));
            }
            "openai" | "atlascloud" | "wanjie-ark" | "moonshot" | "ollama" => {}
            "anthropic" => {}
            "nvidia-nim" => {
                merge_chat_template_kwargs(params, "thinking", json!(true));
                merge_chat_template_kwargs(params, "reasoning_effort", json!("max"));
            }
            _ => tracing::debug!(provider, "no reasoning-effort shaping for provider"),
        },
        other => tracing::warn!(
            effort = other,
            "unrecognized reasoning_effort value; no reasoning params sent"
        ),
    }
}

/// Merge `key: value` into the `chat_template_kwargs` object of `params`,
/// preserving keys already present — `additional_params` is a shared,
/// caller-populated escape hatch, and a whole-object `insert` would silently
/// drop coexisting template kwargs.
fn merge_chat_template_kwargs(params: &mut Map<String, Value>, key: &str, value: Value) {
    let entry = params
        .entry("chat_template_kwargs".to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        // A non-object value (string/number/bool/null) is already a broken
        // request; replacing it keeps the thinking control effective instead
        // of silently dropping it (the vLLM "freeze" PR #1480 fixed).
        tracing::warn!(
            "chat_template_kwargs in additional_params is not a JSON object; \
             replacing it so thinking control is not silently dropped"
        );
        *entry = json!({});
    }
    if let Some(obj) = entry.as_object_mut() {
        obj.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- should_replay_reasoning_content_for_provider ------------------------

    #[test]
    fn replay_truth_table() {
        // generic model + provider-rejects-field → strip
        assert!(!should_replay_reasoning_content_for_provider(
            "openai",
            "gpt-4o",
            Some("high")
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            "vllm",
            "qwen3-235b",
            Some("high")
        ));
        // generic model + provider-accepts-field → still strip (model is not reasoning)
        assert!(!should_replay_reasoning_content_for_provider(
            "openrouter",
            "qwen3-235b",
            Some("high")
        ));
        // DeepSeek reasoning model on ANY provider → attach
        assert!(should_replay_reasoning_content_for_provider(
            "openrouter",
            "deepseek-chat",
            Some("high")
        ));
        assert!(should_replay_reasoning_content_for_provider(
            "openai",
            "deepseek-r1",
            Some("medium")
        ));
        assert!(should_replay_reasoning_content_for_provider(
            "deepseek",
            "deepseek-v4-pro",
            Some("high")
        ));
        // glob-only model on a known field-rejecting provider → strip (#224);
        // the generic globs are for custom deployments, which an unknown
        // provider name still represents
        assert!(!should_replay_reasoning_content_for_provider(
            "vllm",
            "qwen3-reasoner",
            Some("high")
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            "openai",
            "kimi-thinking",
            None
        ));
        assert!(should_replay_reasoning_content_for_provider(
            "acme-llm",
            "kimi-thinking",
            None
        ));
        // effort off overrides everything
        assert!(!should_replay_reasoning_content_for_provider(
            "deepseek",
            "deepseek-chat",
            Some("off")
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            "deepseek",
            "deepseek-chat",
            Some("disabled")
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            "deepseek",
            "deepseek-chat",
            Some("none")
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            "deepseek",
            "deepseek-chat",
            Some("false")
        ));
        // effort None → model-driven
        assert!(should_replay_reasoning_content_for_provider(
            "deepseek",
            "deepseek-chat",
            None
        ));
        assert!(!should_replay_reasoning_content_for_provider(
            "openai", "gpt-4o", None
        ));
    }

    // --- apply_reasoning_effort ---------------------------------------------

    fn params_for(effort: Option<&str>, provider: &str) -> Map<String, Value> {
        let mut map = Map::new();
        apply_reasoning_effort(&mut map, effort, provider, false);
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
        assert_eq!(
            m["chat_template_kwargs"],
            json!({ "enable_thinking": true })
        );
        assert_eq!(m["reasoning_effort"], json!("high"));
        let off = params_for(Some("off"), "vllm");
        assert_eq!(
            off["chat_template_kwargs"],
            json!({ "enable_thinking": false })
        );
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
    fn openai_passthrough_forwards_effort_verbatim() {
        let mut map = Map::new();
        apply_reasoning_effort(&mut map, Some("high"), "openai", true);
        assert_eq!(map["reasoning_effort"], json!("high"));
        let mut map = Map::new();
        apply_reasoning_effort(&mut map, Some("off"), "openai", true);
        assert_eq!(map["thinking"], json!({ "type": "disabled" }));
        // Empty effort stays excluded from the passthrough arm.
        let mut map = Map::new();
        apply_reasoning_effort(&mut map, Some(""), "openai", true);
        assert!(map.is_empty());
    }

    #[test]
    fn allowlisted_providers_shape_every_effort_tier() {
        // Drift guard: a provider allowlisted for `reasoning_content` replay
        // that loses its arm in one of the effort tiers below silently gets
        // no thinking control (the `_ => {}` fallthrough). Every allowlisted
        // name must keep producing params for each tier it shapes.
        for provider in [
            "deepseek",
            "deepseek-cn",
            "nvidia-nim",
            "openrouter",
            "xiaomi-mimo",
            "novita",
            "fireworks",
            "siliconflow",
            "sglang",
        ] {
            assert!(
                !params_for(Some("high"), provider).is_empty(),
                "{provider} must have an explicit effort arm"
            );
            assert!(
                !params_for(Some("max"), provider).is_empty(),
                "{provider} must have an explicit xhigh arm"
            );
        }
        // `off` deliberately sends nothing for fireworks; every other
        // allowlisted provider must disable thinking explicitly.
        for provider in [
            "deepseek",
            "deepseek-cn",
            "nvidia-nim",
            "openrouter",
            "xiaomi-mimo",
            "novita",
            "siliconflow",
            "sglang",
        ] {
            assert!(
                !params_for(Some("off"), provider).is_empty(),
                "{provider} must have an explicit off arm"
            );
        }
    }

    #[test]
    fn empty_effort_sends_nothing_for_openai() {
        // "" must mean "send nothing" for openai (the passthrough arm is
        // env-gated and likewise excludes it); only the deepseek family
        // deliberately maps "" to high.
        assert!(params_for(Some(""), "openai").is_empty());
        assert!(params_for(Some("   "), "openai").is_empty());
        assert!(!params_for(Some(""), "deepseek").is_empty());
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
