//! DeepSeek thinking-mode reasoning predicates — the single source of truth.
//!
//! These model-name heuristics decide whether a model belongs to the
//! DeepSeek thinking-mode family (and thus requires `reasoning_content`
//! replay on tool-call turns, or the API returns 400 — #1739 / #1694).
//!
//! Historically the same three predicates were duplicated across the rig
//! adapter (`codesmith-providers` `rig_adapter/reasoning`) and the
//! inspect/warmup path (`codesmith-agent-runtime` `prompt_inspect`).
//! They were lifted here (ROADMAP §A slice 42) so both consumers share one
//! definition. The provider-aware wrapper
//! (`should_replay_reasoning_content_for_provider`) and the per-provider
//! `reasoning_effort` translation (`apply_reasoning_effort`) stay in
//! `codesmith-providers` — they are provider-shaping concerns, not model-name
//! heuristics.
//!
//! All three functions are pure `&str`/`Option<&str>` predicates with no
//! crate dependencies, so this module introduces no new dep edge for the
//! framework core.

/// Model-name heuristic: does this model id belong to the DeepSeek thinking-mode
/// family (and thus require `reasoning_content` replay on tool-call turns, or the
/// API returns 400 — #1739 / #1694)?
///
/// Matches (case-insensitive): `deepseek-v4`, `deepseek-chat*`, `deepseek-reasoner*`,
/// `*reasoner*`, `*-reasoning`, `*-thinking`, and `deepseek-r<digit>` (r1, r1-distill,
/// …). Does **not** match `deepseek-v3` / plain `deepseek` — those are non-reasoning.
pub fn requires_reasoning_content(model: &str) -> bool {
    let lower = model.to_lowercase();
    // V4-family direct model IDs.
    lower.contains("deepseek-v4")
        // Public DeepSeek API aliases routed server-side to the V4 family.
        // `deepseek-chat` resolves to `deepseek-v4-flash` and `deepseek-reasoner`
        // resolves to `deepseek-v4-pro`; both have thinking mode enabled by
        // default, so any assistant message carrying tool_calls must replay
        // `reasoning_content` on subsequent turns or the API returns 400.
        || lower.starts_with("deepseek-chat")
        || lower.starts_with("deepseek-reasoner")
        // Generic reasoning markers used by custom/proxied deployments.
        || lower.contains("reasoner")
        || lower.contains("-reasoning")
        || lower.contains("-thinking")
        || has_deepseek_r_series_marker(&lower)
}

fn has_deepseek_r_series_marker(model_lower: &str) -> bool {
    const PREFIX: &str = "deepseek-r";
    model_lower.match_indices(PREFIX).any(|(idx, _)| {
        model_lower[idx + PREFIX.len()..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
    })
}

/// Should the outgoing request re-attach `reasoning_content` to assistant
/// tool-call turns for this model + effort? Honours the `off`/`disabled`/
/// `none`/`false` effort override. Model-only (ignores provider).
pub fn should_replay_reasoning_content(model: &str, effort: Option<&str>) -> bool {
    if effort
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "disabled" | "none" | "false"
            )
        })
        .unwrap_or(false)
    {
        return false;
    }

    requires_reasoning_content(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- requires_reasoning_content: positives ------------------------------

    #[test]
    fn requires_reasoning_content_matches_deepseek_family() {
        assert!(requires_reasoning_content("deepseek-v4-flash"));
        assert!(requires_reasoning_content("deepseek-v4-pro"));
        assert!(requires_reasoning_content("deepseek-chat"));
        assert!(requires_reasoning_content("DeepSeek-Chat"));
        assert!(requires_reasoning_content("DEEPSEEK-REASONER"));
        assert!(requires_reasoning_content("deepseek-reasoner"));
        assert!(requires_reasoning_content("deepseek-r1"));
        assert!(requires_reasoning_content("deepseek-r1-distill-qwen-32b"));
        assert!(requires_reasoning_content("qwen3-reasoner"));
        assert!(requires_reasoning_content("gpt-5-reasoning"));
        assert!(requires_reasoning_content("kimi-thinking"));
    }

    #[test]
    fn explicit_v4_ids_still_require_reasoning_content() {
        // Direct V4 IDs continue to match (regression guard for the existing
        // `lower.contains("deepseek-v4")` branch).
        assert!(requires_reasoning_content("deepseek-v4-flash"));
        assert!(requires_reasoning_content("deepseek-v4-pro"));
    }

    #[test]
    fn alias_prefix_handles_suffixed_variants() {
        // OpenRouter / proxy deployments occasionally suffix the canonical
        // alias (e.g. `deepseek-chat:free`). Those routes still hit V4
        // server-side, so they must continue to require reasoning_content.
        assert!(requires_reasoning_content("deepseek-chat:free"));
        assert!(requires_reasoning_content("deepseek-reasoner-2025-05"));
    }

    #[test]
    fn reasoning_alias_remains_reasoning_when_suffixed() {
        // `deepseek-reasoner-v2` still matches (starts_with "deepseek-reasoner")
        // — it's a reasoning model. Documented here to pin the heuristic.
        assert!(requires_reasoning_content("deepseek-reasoner-v2"));
    }

    // --- requires_reasoning_content: negatives ------------------------------

    #[test]
    fn requires_reasoning_content_rejects_non_reasoning() {
        assert!(!requires_reasoning_content("deepseek-v3"));
        assert!(!requires_reasoning_content("deepseek-v3.1"));
        assert!(!requires_reasoning_content("deepseek"));
        assert!(!requires_reasoning_content("qwen3-235b"));
        assert!(!requires_reasoning_content("gpt-4o"));
    }

    #[test]
    fn non_thinking_aliases_remain_excluded() {
        // Legacy non-thinking IDs and unrelated provider models must not be
        // misclassified, otherwise we would force a placeholder
        // `reasoning_content` on providers that reject the field.
        assert!(!requires_reasoning_content("deepseek-coder"));
        assert!(!requires_reasoning_content("qwen3-coder"));
        assert!(!requires_reasoning_content("claude-sonnet-4-6"));
    }

    // --- has_deepseek_r_series_marker ---------------------------------------

    #[test]
    fn r_series_marker_requires_trailing_digit() {
        // `deepseek-r` alone (no digit) must not match — it's a prefix of
        // `deepseek-reasoner`, already caught by the starts_with arm.
        assert!(!has_deepseek_r_series_marker("deepseek-r"));
        assert!(has_deepseek_r_series_marker("deepseek-r1"));
        assert!(has_deepseek_r_series_marker("deepseek-r1-distill"));
    }

    // --- should_replay_reasoning_content ------------------------------------

    #[test]
    fn explicit_reasoning_off_overrides_alias_detection() {
        // `reasoning_effort = "off"` is the documented escape hatch: even when
        // the model is in the thinking family, the user can opt out and the
        // sanitizer must respect that choice.
        assert!(!should_replay_reasoning_content(
            "deepseek-chat",
            Some("off")
        ));
        assert!(!should_replay_reasoning_content(
            "deepseek-reasoner",
            Some("disabled")
        ));
        assert!(!should_replay_reasoning_content(
            "deepseek-chat",
            Some("none")
        ));
        assert!(!should_replay_reasoning_content(
            "deepseek-chat",
            Some("false")
        ));
        // Without an explicit override, alias models still trigger replay.
        assert!(should_replay_reasoning_content("deepseek-chat", None));
        assert!(should_replay_reasoning_content(
            "deepseek-reasoner",
            Some("medium")
        ));
    }

    #[test]
    fn replay_is_model_driven_when_effort_unset() {
        assert!(should_replay_reasoning_content("deepseek-v4-pro", None));
        assert!(!should_replay_reasoning_content("gpt-4o", None));
    }
}
