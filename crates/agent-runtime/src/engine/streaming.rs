//! Streaming response state and guardrails.
//!
//! This module owns the local state used while decoding one model stream:
//! streamed tool-use buffers, transparent retry policy, and scrubbers for
//! text that looks like a forged tool-call wrapper.
//
// After slice 20 §E (`handle_deepseek_turn` retirement) the stream-reducer
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
/// failure. Bumped 3 → 5 in v0.6.7 along with the HTTP/2 keepalive defaults
/// (#103) — keepalive should make spurious decode errors rarer, so we can
/// tolerate a longer streak before giving up on the turn.
pub const MAX_STREAM_ERRORS_BEFORE_FAIL: u32 = 5;
/// Cap on transparent stream-level retries — these only happen when the wire
/// dies before any content was streamed, so DeepSeek hasn't billed us and
/// the user hasn't seen anything. Two attempts is enough to ride out a
/// flaky edge node without amplifying real outages (#103).
pub const MAX_TRANSPARENT_STREAM_RETRIES: u32 = 2;

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

    let mut output = String::new();
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
