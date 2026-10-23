//! Streaming response state and guardrails.
//!
//! This module owns the local state used while decoding one model stream:
//! streamed tool-use buffers, transparent retry policy, and scrubbers for
//! text that looks like a forged tool-call wrapper.
//
// After the legacy turn-loop retirement the stream-reducer
// config cluster (`*_STREAM_CHUNK_TIMEOUT_SECS`, `stream_chunk_timeout_secs`,
// `ContentBlockKind`, `STREAM_MAX_*`) was orphaned — `HostAgentExecutor::reduce_stream`
// carries its own config. Slice 32 §E deleted that orphan cluster; the scrubbers /
// retry policy (`filter_tool_call_delta`, `should_transparently_retry_stream`,
// `TOOL_CALL_*_MARKERS`) and `ToolUseState` remain live (re-exported via
// `engine/mod.rs`).

use crate::models::ToolCaller;

#[derive(Debug, Clone)]
pub struct ToolUseState {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub caller: Option<ToolCaller>,
    pub input_buffer: String,
}

/// Hard cap on consecutive recoverable stream errors before we surface a turn
/// failure. Bumped 3 → 5 along with the HTTP/2 keepalive defaults
/// (#103) — keepalive should make spurious decode errors rarer, so we can
/// tolerate a longer streak before giving up on the turn.
pub const MAX_STREAM_ERRORS_BEFORE_FAIL: u32 = 5;
/// Cap on transparent stream-level retries — these only happen when the wire
/// dies before any content was streamed, so DeepSeek hasn't billed us and
/// the user hasn't seen anything. Unified with the engine-side retry loop
/// (`stream_with_transparent_retry`): the two sites drifted apart during the
/// turn-loop migration (2 here, 3 there) — that was drift, not policy, so
/// one constant now serves both. Three retries is enough to ride out a
/// flaky edge node without amplifying real outages (#103).
pub const MAX_TRANSPARENT_STREAM_RETRIES: u32 = 3;

/// Decide whether a stream error is eligible for a transparent retry.
///
/// True only when ALL three conditions hold:
/// 1. No content has been received on the current attempt — otherwise DeepSeek
///    has already billed us for output tokens and the user has seen partial
///    deltas; resending would double-bill and desync the UI.
/// 2. We still have transparent-retry budget remaining.
/// 3. The turn has not been cancelled.
///
/// Extracted as a pure function so the four #103 retry cases can be exercised
/// in unit tests without booting the full engine state machine.
pub fn should_transparently_retry_stream(
    any_content_received: bool,
    transparent_attempts: u32,
    cancelled: bool,
) -> bool {
    !any_content_received && transparent_attempts < MAX_TRANSPARENT_STREAM_RETRIES && !cancelled
}

pub const TOOL_CALL_START_MARKERS: [&str; 5] = [
    "[TOOL_CALL]",
    "<codesmith:tool_call",
    "<tool_call",
    "<invoke ",
    "<function_calls>",
];

pub const TOOL_CALL_END_MARKERS: [&str; 5] = [
    "[/TOOL_CALL]",
    "</codesmith:tool_call>",
    "</tool_call>",
    "</invoke>",
    "</function_calls>",
];

/// Compact one-shot notice emitted when a model attempts to forge a tool-call
/// wrapper in plain text instead of using the API tool channel. The visible
/// content is still scrubbed; this exists so the user can see why their text
/// shrank.
pub const FAKE_WRAPPER_NOTICE: &str =
    "Stripped non-API tool-call wrapper from model output (use the API tool channel)";

/// True if `text` contains any of the known fake-wrapper start markers. Used by
/// the streaming loop to decide whether to emit `FAKE_WRAPPER_NOTICE`.
pub fn contains_fake_tool_wrapper(text: &str) -> bool {
    TOOL_CALL_START_MARKERS.iter().any(|m| text.contains(m))
}

fn find_first_marker(text: &str, markers: &[&str]) -> Option<(usize, usize)> {
    markers
        .iter()
        .filter_map(|marker| text.find(marker).map(|idx| (idx, marker.len())))
        .min_by_key(|(idx, _)| *idx)
}

pub fn filter_tool_call_delta(delta: &str, in_tool_call: &mut bool) -> String {
    if delta.is_empty() {
        return String::new();
    }

    // Worst case the output is the whole delta; preallocate instead of
    // growing through repeated reallocation on every stream chunk.
    let mut output = String::with_capacity(delta.len());
    let mut rest = delta;

    loop {
        if *in_tool_call {
            let Some((idx, len)) = find_first_marker(rest, &TOOL_CALL_END_MARKERS) else {
                break;
            };
            rest = &rest[idx + len..];
            *in_tool_call = false;
        } else {
            let Some((idx, len)) = find_first_marker(rest, &TOOL_CALL_START_MARKERS) else {
                output.push_str(rest);
                break;
            };
            output.push_str(&rest[..idx]);
            rest = &rest[idx + len..];
            *in_tool_call = true;
        }
    }

    output
}

/// Stateful, stream-safe form of [`filter_tool_call_delta`].
///
/// The free function scans one chunk at a time, so a marker split across a
/// chunk boundary (`"[TOOL"` + `"_CALL]"`) matched neither chunk and the
/// forged wrapper sailed through to the user. This filter carries the
/// trailing partial-marker suffix between chunks; call [`Self::push`] per
/// delta and [`Self::flush`] at stream end to release any held-back tail.
#[derive(Debug, Default)]
pub struct ToolCallDeltaFilter {
    in_tool_call: bool,
    carry: String,
}

impl ToolCallDeltaFilter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter one streamed delta; returns the visible text for it. A
    /// suffix that could still complete a marker is held back (returned by
    /// the next `push` or by `flush`).
    pub fn push(&mut self, delta: &str) -> String {
        let mut buf = std::mem::take(&mut self.carry);
        buf.push_str(delta);

        let mut output = String::with_capacity(buf.len());
        let mut rest = buf.as_str();
        loop {
            let markers = if self.in_tool_call {
                &TOOL_CALL_END_MARKERS
            } else {
                &TOOL_CALL_START_MARKERS
            };
            match find_first_marker(rest, markers) {
                Some((idx, len)) => {
                    if !self.in_tool_call {
                        output.push_str(&rest[..idx]);
                    }
                    rest = &rest[idx + len..];
                    self.in_tool_call = !self.in_tool_call;
                }
                None => {
                    if self.in_tool_call {
                        // Inside a wrapper: everything is suppressed, but a
                        // partial END marker must carry for the next chunk.
                        let keep = marker_prefix_suffix_len(rest, &TOOL_CALL_END_MARKERS);
                        let split = rest.len().saturating_sub(keep);
                        let (suppressed, tail) = rest.split_at(split);
                        self.carry = tail.to_string();
                        let _ = suppressed;
                    } else {
                        let keep = marker_prefix_suffix_len(rest, &TOOL_CALL_START_MARKERS);
                        let split = rest.len().saturating_sub(keep);
                        let (emit, tail) = rest.split_at(split);
                        output.push_str(emit);
                        self.carry = tail.to_string();
                    }
                    break;
                }
            }
        }
        output
    }

    /// Release any held-back partial-marker tail (call at stream end).
    pub fn flush(&mut self) -> String {
        std::mem::take(&mut self.carry)
    }
}

/// Longest suffix of `text` (shorter than `text`) that is a proper prefix
/// of any marker — the bytes that must carry to the next chunk because
/// they might complete a marker.
fn marker_prefix_suffix_len(text: &str, markers: &[&str]) -> usize {
    let max_marker = markers.iter().map(|m| m.len()).max().unwrap_or(0);
    let max_keep = max_marker.saturating_sub(1).min(text.len());
    (1..=max_keep)
        .rev()
        .find(|keep| {
            let suffix = &text[text.len() - keep..];
            markers
                .iter()
                .any(|m| m.len() > *keep && m.starts_with(suffix))
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod split_marker_tests {
    use super::ToolCallDeltaFilter;

    #[test]
    fn start_marker_split_across_chunks_is_still_scrubbed() {
        // The per-chunk free function let "[TOOL" + "_CALL]" through; the
        // stateful filter must not.
        let mut filter = ToolCallDeltaFilter::new();
        let a = filter.push("hello [TOOL");
        let b = filter.push("_CALL] pwned [/TOOL_CALL] tail");
        let visible = format!("{a}{}{}", b, filter.flush());
        assert!(visible.contains("hello"), "prefix lost: {visible:?}");
        assert!(
            !visible.contains("pwned"),
            "split-marker wrapper leaked: {visible:?}"
        );
        assert!(
            visible.contains("tail"),
            "post-marker text lost: {visible:?}"
        );
    }

    #[test]
    fn end_marker_split_across_chunks_completes_the_suppression() {
        let mut filter = ToolCallDeltaFilter::new();
        let _ = filter.push("[TOOL_CALL] hidden [/TOOL");
        let visible = filter.push("_CALL] after");
        assert!(!visible.contains("hidden"));
        assert!(visible.contains("after"), "{visible:?}");
    }

    #[test]
    fn clean_text_passes_through_and_flush_releases_carry() {
        let mut filter = ToolCallDeltaFilter::new();
        let out = filter.push("plain text with [ bracket");
        assert!(out.contains("plain text"));
        let rest = filter.flush();
        let all = format!("{out}{rest}");
        assert!(all.contains("[ bracket"), "held-back tail lost: {all:?}");
    }
}
