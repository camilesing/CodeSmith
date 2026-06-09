//! Partial compaction: directional compression that preserves or sacrifices
//! the prefix cache for targeted budget recovery.
//!
//! Two directions:
//! - **From**: Summarize messages after `pivot_index`, keeping the prefix intact
//!   (preserves V4 prefix cache). The summary sits between prefix and tail.
//! - **UpTo**: Summarize messages before `pivot_index`, removing the prefix
//!   (sacrifices cache for immediate budget relief). The summary goes into
//!   the system prompt.

use crate::client::DeepSeekClient;
use crate::compaction::estimate_tokens;
use crate::models::{ContentBlock, Message, SystemBlock, SystemPrompt};

/// Direction of partial compaction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PartialCompactDirection {
    /// Summarize messages after `pivot_index`; keep prefix (preserve cache).
    From,
    /// Summarize messages before `pivot_index`; remove prefix (sacrifice cache).
    UpTo,
}

/// Request for a partial compaction operation.
#[derive(Debug, Clone)]
pub struct PartialCompactRequest {
    /// Direction of the partial compaction.
    pub direction: PartialCompactDirection,
    /// Pivot index — the boundary between summarize and retain.
    pub pivot_index: usize,
    /// Model to use for summary generation.
    pub model: String,
    /// Custom user feedback to include in the summary.
    pub user_feedback: Option<String>,
}

/// Result of a partial compaction operation.
#[derive(Debug)]
pub struct PartialCompactResult {
    /// Compacted messages.
    pub messages: Vec<Message>,
    /// Summary system prompt (if generated).
    pub summary_prompt: Option<SystemPrompt>,
    /// Indices of messages that were removed/summarized.
    pub removed_indices: Vec<usize>,
}

/// Adjust a pivot index to align with tool_use/tool_result pair boundaries.
///
/// Ensures we don't split a tool_use + tool_result pair at the boundary.
pub fn adjust_pivot_for_tool_pairs(messages: &[Message], pivot: usize) -> usize {
    if pivot == 0 || pivot >= messages.len() {
        return pivot;
    }

    let mut adjusted = pivot;

    // Build tool_use_id → message index maps.
    let mut call_ids: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut result_ids: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (idx, msg) in messages.iter().enumerate() {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    call_ids.insert(id.clone(), idx);
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    result_ids.insert(tool_use_id.clone(), idx);
                }
                _ => {}
            }
        }
    }

    // If pivot lands on a tool_result whose call is in the prefix section,
    // move pivot forward past the result to keep the pair together in From direction.
    for block in &messages[adjusted].content {
        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
            if let Some(&call_idx) = call_ids.get(tool_use_id) {
                if call_idx < adjusted {
                    // The call is before pivot, result is at/after pivot.
                    // For From direction: move pivot back to include the call.
                    // For UpTo direction: move pivot forward to include the result.
                    // Default behavior: move forward to keep pairs together.
                    adjusted = adjusted.saturating_add(1);
                    return adjust_pivot_for_tool_pairs(messages, adjusted);
                }
            }
        }
    }

    // If pivot lands on a tool_use whose result is after pivot,
    // move pivot forward past the result.
    for block in &messages[adjusted].content {
        if let ContentBlock::ToolUse { id, .. } = block {
            if let Some(&result_idx) = result_ids.get(id) {
                if result_idx > adjusted {
                    adjusted = result_idx.saturating_add(1);
                    return adjust_pivot_for_tool_pairs(messages, adjusted);
                }
            }
        }
    }

    adjusted
}

/// Find a good pivot index based on token budget.
///
/// For From direction: find the earliest index where messages[pivot..] tokens
/// are within the budget, preserving as much prefix as possible.
/// For UpTo direction: find the latest index where messages[0..pivot] tokens
/// exceed the budget, removing as little as needed.
pub fn find_pivot_for_budget(
    messages: &[Message],
    direction: PartialCompactDirection,
    target_tokens: usize,
) -> usize {
    match direction {
        PartialCompactDirection::From => {
            // Walk backwards from tail, accumulate tokens until budget exceeded.
            let mut accumulated = 0usize;
            for (idx, msg) in messages.iter().enumerate().rev() {
                accumulated += estimate_tokens_for_message(msg);
                if accumulated > target_tokens {
                    return adjust_pivot_for_tool_pairs(messages, idx.saturating_add(1));
                }
            }
            // Entire conversation fits in budget — no compaction needed.
            0
        }
        PartialCompactDirection::UpTo => {
            // Walk forwards from head, accumulate tokens until budget exceeded.
            let mut accumulated = 0usize;
            for (idx, msg) in messages.iter().enumerate() {
                accumulated += estimate_tokens_for_message(msg);
                if accumulated > target_tokens {
                    // Return pivot after the current message so at least one is removed.
                    let pivot = idx.saturating_add(1);
                    return adjust_pivot_for_tool_pairs(messages, pivot.min(messages.len()));
                }
            }
            // Entire conversation fits in budget.
            messages.len()
        }
    }
}

/// Perform a partial compaction.
///
/// Depending on direction:
/// - **From**: Retains messages `[0..pivot]` as prefix, summarizes `[pivot..]`.
///   Result: prefix messages + summary block + retained tail messages.
/// - **UpTo**: Retains messages `[pivot..]`, summarizes `[0..pivot]`.
///   Result: summary block (in system prompt) + retained tail messages.
pub async fn partial_compact(
    client: &DeepSeekClient,
    messages: &[Message],
    request: &PartialCompactRequest,
    cache_summary: bool,
) -> anyhow::Result<PartialCompactResult> {
    if messages.is_empty() || request.pivot_index >= messages.len() {
        return Ok(PartialCompactResult {
            messages: messages.to_vec(),
            summary_prompt: None,
            removed_indices: vec![],
        });
    }

    let pivot = adjust_pivot_for_tool_pairs(messages, request.pivot_index);

    let (to_summarize, to_retain, removed_indices): (Vec<Message>, Vec<Message>, Vec<usize>) =
        match request.direction {
            PartialCompactDirection::From => {
                let summarize = messages[pivot..].to_vec();
                let retain = messages[..pivot].to_vec();
                let indices = (pivot..messages.len()).collect();
                (summarize, retain, indices)
            }
            PartialCompactDirection::UpTo => {
                let summarize = messages[..pivot].to_vec();
                let retain = messages[pivot..].to_vec();
                let indices = (0..pivot).collect();
                (summarize, retain, indices)
            }
        };

    if to_summarize.is_empty() {
        return Ok(PartialCompactResult {
            messages: messages.to_vec(),
            summary_prompt: None,
            removed_indices: vec![],
        });
    }

    // Generate summary using LLM.
    let summary_text =
        crate::compaction::create_summary(client, &to_summarize, &request.model).await?;

    let summary_block = SystemBlock {
        block_type: "text".to_string(),
        text: format!(
            "## Partial Compaction Summary\n\n\
             {summary_text}\n\n\
             ---\n\n\
             Direction: {} direction partial compaction at message index {pivot}.\n\
             Compacted messages follow:",
            match request.direction {
                PartialCompactDirection::From => "From (prefix preserved)",
                PartialCompactDirection::UpTo => "UpTo (prefix sacrificed)",
            }
        ),
        cache_control: if cache_summary {
            Some(crate::models::CacheControl {
                cache_type: "ephemeral".to_string(),
            })
        } else {
            None
        },
    };

    let summary_prompt = Some(SystemPrompt::Blocks(vec![summary_block]));

    // Build resulting message list.
    let result_messages = match request.direction {
        PartialCompactDirection::From => {
            // Prefix + summary block as a user message + retained tail.
            let mut result = to_retain;
            result.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: format!(
                        "[Context partially compacted. Summary of removed messages:\n\n{summary_text}]"
                    ),
                    cache_control: None,
                }],
            });
            result
        }
        PartialCompactDirection::UpTo => {
            // Summary goes into system prompt, only tail messages remain.
            to_retain
        }
    };

    Ok(PartialCompactResult {
        messages: result_messages,
        summary_prompt,
        removed_indices,
    })
}

/// Conservative token estimate for a single message.
fn estimate_tokens_for_message(msg: &Message) -> usize {
    msg.content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.len() / 4,
            ContentBlock::Thinking { thinking } => thinking.len() / 4,
            ContentBlock::ToolUse { input, .. } => {
                serde_json::to_string(input)
                    .map(|s| s.len() / 4)
                    .unwrap_or(100)
            }
            ContentBlock::ToolResult { content, .. } => content.len() / 4,
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
    fn adjust_pivot_preserves_tool_pairs() {
        let messages = vec![
            msg("user", "start"),         // 0
            tool_use_msg("t1", "read"),    // 1
            tool_result_msg("t1", "ok"),   // 2
            msg("user", "more"),           // 3
            msg("assistant", "reply"),     // 4
        ];

        // Pivot at 2 (tool_result) — its call at 1 would be split.
        let adjusted = adjust_pivot_for_tool_pairs(&messages, 2);
        // Should move past the result to 3.
        assert!(adjusted >= 3, "adjusted pivot should not split tool pairs");
    }

    #[test]
    fn find_pivot_from_preserves_prefix() {
        // Long prefix, short tail.
        let messages = vec![
            msg("user", &"x".repeat(4000)),   // ~1000 tokens
            msg("assistant", &"y".repeat(4000)), // ~1000 tokens
            msg("user", "short"),             // ~1 token
            msg("assistant", "brief"),        // ~1 token
        ];

        // Target: keep 500 tokens in prefix, summarize the rest.
        let pivot = find_pivot_for_budget(&messages, PartialCompactDirection::From, 500);
        // Pivot should be 1 or 2 — prefix is messages[0..pivot].
        assert!(pivot >= 1, "From pivot should preserve some prefix");
        assert!(pivot < messages.len(), "From pivot should leave something to summarize");
    }

    #[test]
    fn find_pivot_up_to_removes_prefix() {
        let messages = vec![
            msg("user", &"x".repeat(4000)),   // ~1000 tokens
            msg("assistant", &"y".repeat(4000)), // ~1000 tokens
            msg("user", "short"),
            msg("assistant", "brief"),
        ];

        // Target: remove messages until we're under 500 tokens.
        let pivot = find_pivot_for_budget(&messages, PartialCompactDirection::UpTo, 500);
        // Pivot should be early — removing prefix until budget fits.
        assert!(pivot >= 1, "UpTo pivot should remove some prefix");
    }

    #[test]
    fn empty_messages_return_empty_result() {
        let messages: Vec<Message> = vec![];
        let pivot = adjust_pivot_for_tool_pairs(&messages, 0);
        assert_eq!(pivot, 0);
    }

    #[test]
    fn pivot_at_boundary_returns_boundary() {
        let messages = vec![
            msg("user", "a"),
            msg("assistant", "b"),
            msg("user", "c"),
            msg("assistant", "d"),
        ];

        let pivot = adjust_pivot_for_tool_pairs(&messages, 2);
        assert_eq!(pivot, 2); // No tool pairs, pivot stays at 2
    }
}