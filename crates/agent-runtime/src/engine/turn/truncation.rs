//! Output-limit truncation classification (P0-2: tool calls cut off by the
//! provider output cap must never be repaired into executable form).
//!
//! When a provider stops emitting because the output token limit was hit, it
//! reports it on the message-level stop reason (`length` — OpenAI/Anthropic
//! wire value; some providers spell it `max_tokens`). Any tool-call argument
//! JSON that failed to parse in such a turn was almost certainly cut off
//! mid-argument, not malformed by intent. Repairing that fragment (closing
//! its braces, stripping its trailing commas) would fabricate a complete
//! command the model never wrote — for shell-class tools that executes
//! half a sentence, which is a safety issue, not a quality issue. The
//! dispatch-side gate therefore refuses to execute such calls and feeds
//! back a re-send request instead (upstream v0.9.13 #5986 semantics).
//!
//! Two caveats live here rather than at the call site:
//!
//! - **Per-provider exceptions**: a few providers mis-report `length` on
//!   normally-completed turns. For those, the stop reason alone is not
//!   evidence of truncation, so they are exempted via
//!   [`PROVIDERS_REPORTING_LENGTH_AT_NORMAL_COMPLETION`] (empty until a
//!   concrete offender is confirmed — the mechanism is pinned by test).
//! - **Scope**: this classification keys on the provider's *declared* stop
//!   reason only. A stream that died mid-flight without any stop reason is
//!   a different failure (termination proof, P0-3) and must not be treated
//!   as output-limit truncation here.

/// Stop-reason values (normalized to lowercase) that mean "the provider hit
/// its output token limit and stopped emitting". `length` is the OpenAI /
/// Anthropic wire value; `max_tokens` is the spelling some OpenAI-compat
/// providers use (and Gemini's `MAX_TOKENS` normalizes to it).
const OUTPUT_LIMIT_STOP_REASONS: &[&str] = &["length", "max_tokens"];

/// Providers known to report an output-limit stop reason on turns that
/// actually completed normally. For these providers the stop reason is not
/// evidence of truncation, so the P0-2 gate stays open (a mis-gated call
/// would block execution of arguments the model did finish writing).
/// Deliberately empty until a concrete offender is confirmed in the wild —
/// add the `provider_name()` string (lowercase) here when one is.
const PROVIDERS_REPORTING_LENGTH_AT_NORMAL_COMPLETION: &[&str] = &[];

/// Does this stop reason mean the turn was cut off by the output token
/// limit — i.e. tool-call arguments that failed to parse were truncated,
/// not malformed? `provider` is the client's `provider_name()` and selects
/// the mis-report exemptions above.
pub(crate) fn stop_reason_indicates_output_truncation(
    stop_reason: Option<&str>,
    provider: Option<&str>,
) -> bool {
    let Some(reason) = stop_reason.map(str::trim).filter(|r| !r.is_empty()) else {
        return false;
    };
    let normalized = reason.to_ascii_lowercase();
    if !OUTPUT_LIMIT_STOP_REASONS.contains(&normalized.as_str()) {
        return false;
    }
    match provider {
        Some(provider) => {
            let provider = provider.to_ascii_lowercase();
            !PROVIDERS_REPORTING_LENGTH_AT_NORMAL_COMPLETION.contains(&provider.as_str())
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_limit_stop_reasons_classify_as_truncated() {
        for reason in ["length", "max_tokens", "Length", "MAX_TOKENS", " length "] {
            assert!(
                stop_reason_indicates_output_truncation(Some(reason), Some("deepseek")),
                "{reason:?} should classify as output-limit truncation"
            );
        }
    }

    #[test]
    fn normal_stop_reasons_do_not_classify_as_truncated() {
        for reason in [
            "stop",
            "end_turn",
            "tool_use",
            "tool_calls",
            "content_filter",
            "",
        ] {
            assert!(
                !stop_reason_indicates_output_truncation(Some(reason), Some("deepseek")),
                "{reason:?} should not classify as output-limit truncation"
            );
        }
        assert!(!stop_reason_indicates_output_truncation(
            None,
            Some("deepseek")
        ));
    }

    #[test]
    fn exempt_providers_override_the_length_signal() {
        // The exemption mechanism is pinned even while the table is empty:
        // every entry must neutralize the gate, case-insensitively.
        for provider in PROVIDERS_REPORTING_LENGTH_AT_NORMAL_COMPLETION {
            assert!(
                !stop_reason_indicates_output_truncation(Some("length"), Some(provider)),
                "exempt provider {provider:?} must not classify as truncated"
            );
            let upper = provider.to_ascii_uppercase();
            assert!(
                !stop_reason_indicates_output_truncation(Some("length"), Some(&upper)),
                "exemption must match provider names case-insensitively"
            );
        }
    }

    #[test]
    fn missing_provider_still_classifies_on_stop_reason() {
        assert!(stop_reason_indicates_output_truncation(
            Some("length"),
            None
        ));
    }
}
