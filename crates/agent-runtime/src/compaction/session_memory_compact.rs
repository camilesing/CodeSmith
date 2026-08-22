//! Session-memory-based compaction: uses MEMORY.md / KoD files as summaries
//! to avoid LLM API calls when available.

use crate::compaction::estimate_tokens;
use crate::models::{ContentBlock, Message, SystemBlock, SystemPrompt};

/// Configuration for session-memory-based compaction.
#[derive(Debug, Clone)]
pub struct SessionMemoryCompactConfig {
    /// Minimum number of recent tokens to retain (default: 10,000).
    pub min_retain_tokens: usize,
    /// Maximum number of recent tokens to retain (default: 40,000).
    pub max_retain_tokens: usize,
    /// Whether this compaction mode is enabled.
    pub enabled: bool,
}

impl Default for SessionMemoryCompactConfig {
    fn default() -> Self {
        Self {
            min_retain_tokens: 10_000,
            max_retain_tokens: 40_000,
            enabled: true,
        }
    }
}

/// Result of a session-memory compaction attempt.
#[derive(Debug)]
pub struct SessionMemoryCompactResult {
    /// Compacted messages (tail retained + summary block).
    pub messages: Vec<Message>,
    /// Summary system prompt derived from memory content.
    pub summary_prompt: Option<SystemPrompt>,
    /// Number of messages removed.
    pub removed_count: usize,
}

/// Check whether session-memory compaction should be used.
///
/// Returns `true` when:
/// 1. The feature is enabled
/// 2. Memory content exists (MEMORY.md / KoD files)
/// 3. The conversation exceeds the min_retain_tokens threshold
pub fn should_use_session_memory_compact(
    memory_content: &str,
    messages: &[Message],
    config: &SessionMemoryCompactConfig,
) -> bool {
    if !config.enabled {
        return false;
    }
    if memory_content.trim().is_empty() {
        return false;
    }
    let total_tokens = estimate_tokens(messages);
    total_tokens > config.min_retain_tokens
}

/// Perform session-memory compaction.
///
/// Uses the memory content (MEMORY.md / KoD) as a summary, retaining only
/// the most recent messages that fit within the token budget. Ensures
/// tool_use/tool_result pairs are not split at the boundary.
pub fn session_memory_compact(
    messages: &[Message],
    memory_content: &str,
    config: &SessionMemoryCompactConfig,
) -> SessionMemoryCompactResult {
    if messages.is_empty() || memory_content.trim().is_empty() {
        return SessionMemoryCompactResult {
            messages: messages.to_vec(),
            summary_prompt: None,
            removed_count: 0,
        };
    }

    // Find the retain boundary: accumulate tokens from the tail until
    // we're within min_retain_tokens..max_retain_tokens.
    let retain_start_idx = calculate_retain_start_index(messages, config);

    if retain_start_idx == 0 {
        // No messages to remove — entire conversation fits in budget.
        return SessionMemoryCompactResult {
            messages: messages.to_vec(),
            summary_prompt: None,
            removed_count: 0,
        };
    }

    // Build summary prompt from memory content.
    let summary_block = SystemBlock {
        block_type: "text".to_string(),
        text: format!(
            "## Session Memory (Auto-Loaded)\n\n\
             The following context was loaded from your persistent session memory. \
             Earlier conversation has been compacted; this summary preserves key facts.\n\n\
             {memory_content}\n\n\
             ---\n\n\
             Pinned messages follow:"
        ),
        cache_control: None,
    };
    let summary_prompt = Some(SystemPrompt::Blocks(vec![summary_block]));

    // Retain messages from retain_start_idx onwards.
    let retained = messages[retain_start_idx..].to_vec();
    let removed_count = retain_start_idx;

    SessionMemoryCompactResult {
        messages: retained,
        summary_prompt,
        removed_count,
    }
}

/// Calculate the start index for messages to retain.
///
/// Walks backwards from the tail, accumulating token estimates, and finds
/// the earliest index where retained tokens fall within the budget.
/// Adjusts to preserve tool_use/tool_result pair integrity.
fn calculate_retain_start_index(
    messages: &[Message],
    config: &SessionMemoryCompactConfig,
) -> usize {
    let mut accumulated_tokens = 0usize;
    let mut candidate_idx = messages.len();

    for (idx, msg) in messages.iter().enumerate().rev() {
        let msg_tokens = estimate_tokens_for_message_conservative(msg);
        accumulated_tokens += msg_tokens;

        if accumulated_tokens > config.max_retain_tokens {
            // We've exceeded the max budget; stop here.
            candidate_idx = idx + 1;
            break;
        }

        if accumulated_tokens >= config.min_retain_tokens {
            // We've reached the minimum budget; this is a good boundary.
            candidate_idx = idx;
            // Continue checking if we can include more without exceeding max.
        }
    }

    // If we never exceeded max, retain everything from candidate_idx.
    if candidate_idx >= messages.len() {
        candidate_idx = 0;
    }

    // Adjust to preserve tool_use/tool_result pairs.
    adjust_index_for_tool_pairs(messages, candidate_idx)
}

/// Adjust a start index to ensure we don't split tool_use/tool_result pairs.
///
/// Retention always keeps a suffix of the transcript. If either side of a
/// tool pair is retained while the other side would be removed, move the
/// boundary back to the earlier side of the pair. Repeat until all known pairs
/// that cross the boundary are fully retained. Orphaned tool results at the
/// boundary are dropped by moving the boundary forward past them.
fn adjust_index_for_tool_pairs(messages: &[Message], start_idx: usize) -> usize {
    if start_idx == 0 || start_idx >= messages.len() {
        return start_idx;
    }

    let mut tool_uses: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut tool_results: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for (idx, msg) in messages.iter().enumerate() {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    tool_uses.entry(id.clone()).or_insert(idx);
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    tool_results.entry(tool_use_id.clone()).or_insert(idx);
                }
                _ => {}
            }
        }
    }

    let mut adjusted = start_idx;

    loop {
        let mut moved = false;
        for (id, use_idx) in &tool_uses {
            let Some(result_idx) = tool_results.get(id) else {
                continue;
            };
            let earliest = (*use_idx).min(*result_idx);
            let latest = (*use_idx).max(*result_idx);
            if earliest < adjusted && latest >= adjusted {
                adjusted = earliest;
                moved = true;
                break;
            }
        }
        if !moved {
            break;
        }
    }

    while adjusted < messages.len()
        && messages[adjusted].content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolResult { tool_use_id, .. }
                    if !tool_uses.contains_key(tool_use_id)
            )
        })
    {
        adjusted += 1;
    }

    adjusted
}

/// Conservative token estimate for a single message.
fn estimate_tokens_for_message_conservative(msg: &Message) -> usize {
    msg.content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.len().div_ceil(3),
            ContentBlock::Thinking { thinking } => thinking.len().div_ceil(3),
            ContentBlock::ToolUse { input, .. } => serde_json::to_string(input)
                .map(|s| s.len().div_ceil(3))
                .unwrap_or(100),
            ContentBlock::ToolResult { content, .. } => content.len().div_ceil(3),
            ContentBlock::Image { .. } => crate::models::IMAGE_BLOCK_ESTIMATED_TOKENS,
            ContentBlock::ServerToolUse { .. }
            | ContentBlock::ToolSearchToolResult { .. }
            | ContentBlock::CodeExecutionToolResult { .. } => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_use_msg(id: &str, name: &str) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({}),
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
    fn should_use_returns_false_when_disabled() {
        let config = SessionMemoryCompactConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(!should_use_session_memory_compact(
            "memory",
            &vec![msg("user", "test")],
            &config
        ));
    }

    #[test]
    fn should_use_returns_false_when_memory_empty() {
        let config = SessionMemoryCompactConfig::default();
        assert!(!should_use_session_memory_compact(
            "",
            &vec![msg("user", "test")],
            &config
        ));
    }

    #[test]
    fn adjust_preserves_tool_pairs_when_result_is_retained() {
        let messages = vec![
            msg("user", "start"),            // 0
            tool_use_msg("t1", "read"),      // 1
            tool_result_msg("t1", "result"), // 2
            msg("user", "more"),             // 3
            msg("assistant", "reply"),       // 4
        ];

        // Start at index 2 (tool_result) — its call (tool_use) is at index 1
        // which would be removed. Adjust should move back to include index 1.
        let adjusted = adjust_index_for_tool_pairs(&messages, 2);
        assert_eq!(adjusted, 1);
    }

    #[test]
    fn adjust_preserves_tool_pairs_when_use_is_retained() {
        let messages = vec![
            msg("user", "start"),
            tool_use_msg("t1", "read"),
            tool_result_msg("t1", "result"),
            msg("assistant", "done"),
        ];

        // Start at index 2 (tool_result) already includes the result but not
        // the use. The boundary should move back to include the whole pair.
        assert_eq!(adjust_index_for_tool_pairs(&messages, 2), 1);
    }

    #[test]
    fn adjust_handles_cascading_tool_pairs() {
        let messages = vec![
            msg("user", "start"),
            tool_use_msg("a", "read"),
            tool_result_msg("a", "result"),
            tool_use_msg("b", "read"),
            tool_result_msg("b", "result"),
            msg("assistant", "done"),
        ];

        assert_eq!(adjust_index_for_tool_pairs(&messages, 4), 3);
        assert_eq!(adjust_index_for_tool_pairs(&messages, 2), 1);
    }

    #[test]
    fn session_memory_compact_retains_recent_messages() {
        // Create messages that total more than max_retain_tokens so some get removed.
        let long_text = "x".repeat(3000); // ~1000 tokens per message
        let config = SessionMemoryCompactConfig {
            min_retain_tokens: 800,
            max_retain_tokens: 2000,
            enabled: true,
        };
        let messages = vec![
            msg("user", &long_text),
            msg("assistant", &long_text),
            msg("user", &long_text),
            msg("assistant", &long_text),
            msg("user", "recent question"),
            msg("assistant", "recent answer"),
        ];

        let result = session_memory_compact(&messages, "memory content", &config);
        assert!(result.removed_count > 0);
        assert!(result.summary_prompt.is_some());
        assert!(result.messages.last().unwrap().role == "assistant");
    }
}
