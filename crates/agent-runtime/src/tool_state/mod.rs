//! Shared state types for model-visible tools.
//!
//! State types (plain data structs + Shared* aliases) are extracted here
//! so `EngineConfig` can reference them without a tui dependency. Tool
//! implementations (`impl ToolSpec`) stay in tui.

pub mod goal;
pub mod plan;
pub mod plan_mode;
pub mod task_v2;
pub mod team;
pub mod todo;
pub mod worktree;

use std::collections::HashSet;

/// Collect the ids of tool calls that actually executed successfully: a
/// user-role `ToolResult` for the id whose `is_error` is not `Some(true)`.
/// Unmatched ids (a call without a result, e.g. an interrupted turn) and
/// errored calls are absent — transcript projections must fold only the
/// calls that mutated live state. Same id→`is_error` correlation as
/// `session.rs`'s `rebuild_recent_read_files_from_messages` and
/// `capacity_flow.rs`.
pub(crate) fn executed_tool_call_ids(
    messages: &[codesmith_agent::models::Message],
) -> HashSet<&str> {
    use codesmith_agent::models::ContentBlock;

    let mut executed: HashSet<&str> = HashSet::new();
    for message in messages {
        if message.role != "user" {
            continue;
        }
        for block in &message.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } = block
                && !matches!(is_error, Some(true))
            {
                executed.insert(tool_use_id.as_str());
            }
        }
    }
    executed
}
