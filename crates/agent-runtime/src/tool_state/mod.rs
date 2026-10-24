//! Shared state types for model-visible tools.
//!
//! State types (plain data structs + Shared* aliases) are extracted here
//! so `EngineConfig` can reference them without a host dependency. Tool
//! implementations (`impl ToolSpec`) stay in the tool crates
//! (`codesmith-tool-impls` / the CLI).

pub mod goal;
pub mod plan;
pub mod plan_mode;
pub mod task_v2;
pub mod team;
pub mod todo;
pub mod worktree;

use std::collections::HashSet;

/// Collect the ids of tool calls that actually executed successfully: a
/// user-role `ToolResult` for the id whose `is_error` is explicitly
/// `Some(false)`. Unmatched ids (a call without a result, e.g. an
/// interrupted turn), errored calls, and legacy results that omitted the
/// flag on failed reads are absent — transcript projections must fold
/// only the calls that mutated live state. Same id→`is_error` policy as
/// `session.rs`'s `rebuild_recent_read_files_from_messages` (a `None`
/// flag counts as not-executed, not as success).
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
                // Require an explicit `Some(false)`, matching
                // `rebuild_recent_read_files_from_messages`: legacy
                // transcripts that omitted the flag on FAILED reads must
                // not fold back in as executed mutations.
                && matches!(is_error, Some(false))
            {
                executed.insert(tool_use_id.as_str());
            }
        }
    }
    executed
}
