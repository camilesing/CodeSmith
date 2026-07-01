//! Micro-compaction: lightweight tool-result clearing without API calls.
//!
//! Clears content from old tool results (file reads, shell output, grep, etc.)
//! while preserving message structure. Triggered by time gaps (>=60 min since
//! last assistant message) or byte thresholds. Not triggered in subagents.

use std::time::Instant;

use crate::models::{ContentBlock, Message};

/// Tool names whose results can be micro-compacted (content cleared).
const MICRO_COMPACT_TOOL_NAMES: &[&str] = &[
    "file_read",
    "exec_shell",
    "exec_shell_wait",
    "exec_shell_interact",
    "grep",
    "glob",
    "web_search",
    "web_fetch",
    "write_file",
    "apply_patch",
    "edit_file",
];

/// Placeholder text replacing cleared tool results.
const CLEARED_PLACEHOLDER: &str = "[tool result cleared for context economy]";

/// Minimum time gap (seconds) since last assistant message to trigger
/// time-based micro-compaction. Matches TS's 60-minute threshold.
const TIME_TRIGGER_GAP_SECS: u64 = 3600;

/// Per-session micro-compaction state.
#[derive(Debug, Clone)]
pub struct MicroCompactState {
    /// Timestamp of the last assistant message in the session.
    /// Used for time-based trigger (>=60 min gap).
    pub last_assistant_message_at: Option<Instant>,
    /// Bytes cleared by micro-compaction since last reset.
    pub bytes_cleared: usize,
    /// Byte threshold for cache-trigger micro-compaction.
    pub cache_trigger_threshold: usize,
}

impl Default for MicroCompactState {
    fn default() -> Self {
        Self {
            last_assistant_message_at: None,
            bytes_cleared: 0,
            // Default threshold: clear when accumulated tool results exceed 32KB.
            // This is a conservative starting point; can be tuned per model.
            cache_trigger_threshold: 32 * 1024,
        }
    }
}

/// Check whether a tool name is micro-compactable.
pub fn is_micro_compactable_tool(name: &str) -> bool {
    MICRO_COMPACT_TOOL_NAMES.contains(&name)
}

/// Determine whether micro-compaction should be triggered.
///
/// Triggers when:
/// 1. Time trigger: >=60 minutes since last assistant message
/// 2. Cache trigger: accumulated bytes_cleared exceeds threshold
///
/// Does NOT trigger in subagents (is_subagent=true).
pub fn should_trigger_micro_compact(
    messages: &[Message],
    state: &MicroCompactState,
    is_subagent: bool,
) -> bool {
    if is_subagent {
        return false;
    }

    // Time trigger: large gap since last assistant message means cache is stale.
    if let Some(last_at) = state.last_assistant_message_at {
        if last_at.elapsed().as_secs() >= TIME_TRIGGER_GAP_SECS {
            return true;
        }
    }

    // Cache trigger: accumulated tool result bytes exceed threshold.
    if state.bytes_cleared >= state.cache_trigger_threshold {
        return true;
    }

    // Check if there are enough micro-compactable tool results to warrant clearing.
    let compactable_count = count_compactable_tool_results(messages);
    compactable_count > 0 && estimate_compactable_bytes(messages) >= state.cache_trigger_threshold
}

/// Count how many tool results in messages are from micro-compactable tools.
fn count_compactable_tool_results(messages: &[Message]) -> usize {
    let tool_use_names = collect_tool_use_names(messages);

    messages
        .iter()
        .flat_map(|msg| msg.content.iter())
        .filter(|block| {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                tool_use_names
                    .get(tool_use_id)
                    .is_some_and(|name| is_micro_compactable_tool(name))
            } else {
                false
            }
        })
        .count()
}

/// Estimate total bytes in micro-compactable tool results.
fn estimate_compactable_bytes(messages: &[Message]) -> usize {
    let tool_use_names = collect_tool_use_names(messages);

    messages
        .iter()
        .flat_map(|msg| msg.content.iter())
        .filter_map(|block| {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = block
            {
                if tool_use_names
                    .get(tool_use_id)
                    .is_some_and(|name| is_micro_compactable_tool(name))
                {
                    Some(content.len())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .sum()
}

/// Build a map of tool_use_id -> tool_name from messages.
fn collect_tool_use_names(messages: &[Message]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolUse { id, name, .. } = block {
                map.insert(id.clone(), name.clone());
            }
        }
    }
    map
}

/// Apply micro-compaction to messages.
///
/// Scans for tool results from compactable tools and replaces their content
/// with a placeholder, preserving message structure and tool_use/tool_result pairs.
///
/// Returns the number of bytes cleared.
pub fn micro_compact_messages(messages: &mut [Message], state: &mut MicroCompactState) -> usize {
    let tool_use_names = collect_tool_use_names(messages);
    let _total_bytes_cleared: usize = 0;
    let mut bytes_cleared = 0usize;

    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                content_blocks,
                ..
            } = block
            {
                let Some(tool_name) = tool_use_names.get(tool_use_id) else {
                    continue;
                };
                if !is_micro_compactable_tool(tool_name) {
                    continue;
                }
                // Don't clear already-cleared results
                if content == CLEARED_PLACEHOLDER {
                    continue;
                }

                let original_len = content.len();
                if original_len <= CLEARED_PLACEHOLDER.len() {
                    continue; // Too small to benefit from clearing
                }

                bytes_cleared += original_len.saturating_sub(CLEARED_PLACEHOLDER.len());
                *content = CLEARED_PLACEHOLDER.to_string();
                *content_blocks = None;
            }
        }
    }

    state.bytes_cleared += bytes_cleared;
    bytes_cleared
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_use_msg(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
            }],
        }
    }

    fn tool_result_msg(id: &str, content: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    #[test]
    fn is_micro_compactable_tool_matches_known_tools() {
        assert!(is_micro_compactable_tool("file_read"));
        assert!(is_micro_compactable_tool("exec_shell"));
        assert!(is_micro_compactable_tool("grep"));
        assert!(is_micro_compactable_tool("glob"));
        assert!(is_micro_compactable_tool("web_search"));
        assert!(is_micro_compactable_tool("web_fetch"));
        assert!(!is_micro_compactable_tool("agent_open"));
        assert!(!is_micro_compactable_tool("unknown_tool"));
    }

    #[test]
    fn should_trigger_returns_false_for_subagent() {
        let state = MicroCompactState::default();
        let messages = vec![msg("user", "test")];
        assert!(!should_trigger_micro_compact(&messages, &state, true));
    }

    #[test]
    fn should_trigger_time_based() {
        let mut state = MicroCompactState::default();
        // Simulate 61 minutes since last assistant message
        state.last_assistant_message_at =
            Some(Instant::now() - std::time::Duration::from_secs(TIME_TRIGGER_GAP_SECS + 60));
        let messages = vec![msg("user", "test")];
        assert!(should_trigger_micro_compact(&messages, &state, false));
    }

    #[test]
    fn micro_compact_clears_compactable_tool_results() {
        let mut messages = vec![
            tool_use_msg("call-1", "file_read", json!({"path": "Cargo.toml"})),
            tool_result_msg("call-1", &"x".repeat(500)),
            msg("user", "question"),
            msg("assistant", "answer"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state);

        assert!(cleared > 0);
        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, CLEARED_PLACEHOLDER);
    }

    #[test]
    fn micro_compact_preserves_non_compactable_results() {
        let mut messages = vec![
            tool_use_msg("call-1", "agent_open", json!({"prompt": "test"})),
            tool_result_msg("call-1", "agent response content"),
            msg("user", "question"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state);
        assert_eq!(cleared, 0);

        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, "agent response content");
    }

    #[test]
    fn micro_compact_skips_small_results() {
        let mut messages = vec![
            tool_use_msg("call-1", "file_read", json!({"path": "a.rs"})),
            tool_result_msg("call-1", "ok"), // shorter than placeholder
            msg("user", "question"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state);
        assert_eq!(cleared, 0);
    }

    #[test]
    fn micro_compact_skips_already_cleared() {
        let mut messages = vec![
            tool_use_msg("call-1", "file_read", json!({"path": "a.rs"})),
            tool_result_msg("call-1", CLEARED_PLACEHOLDER),
            msg("user", "question"),
        ];
        let mut state = MicroCompactState::default();

        let cleared = micro_compact_messages(&mut messages, &mut state);
        assert_eq!(cleared, 0);
    }
}
