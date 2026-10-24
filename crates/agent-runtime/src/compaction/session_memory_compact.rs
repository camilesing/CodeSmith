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
/// 3. The conversation exceeds the `max_retain_tokens` budget — the
///    threshold at which `calculate_retain_start_index` actually produces
///    a non-zero removal boundary. (Gating on `min_retain_tokens` instead
///    would let callers invoke a guaranteed no-op compaction for any total
///    in the min..max window and misread `removed_count == 0` as success.)
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
    total_tokens > config.max_retain_tokens
}

/// Session-memory budget for the inlined MEMORY.md / KoD content. The
/// memory text rides inside the summary system prompt, so it consumes the
/// very context window this compaction protects; beyond this cap the tail
/// plus summary could jointly exceed the model's limit.
const MAX_MEMORY_CONTENT_TOKENS: usize = 4_000;

/// Perform session-memory compaction.
///
/// Uses the memory content (MEMORY.md / KoD) as a summary, retaining only
/// the most recent messages that fit within the token budget. Ensures
/// tool_use/tool_result pairs are not split at the boundary.
///
/// Safe to call unconditionally: `config.enabled` is re-checked here so a
/// caller that bypasses (or races ahead of)
/// [`should_use_session_memory_compact`] cannot compact through a mode
/// that was explicitly turned off.
pub fn session_memory_compact(
    messages: &[Message],
    memory_content: &str,
    config: &SessionMemoryCompactConfig,
) -> SessionMemoryCompactResult {
    if !config.enabled || messages.is_empty() || memory_content.trim().is_empty() {
        return SessionMemoryCompactResult {
            messages: messages.to_vec(),
            summary_prompt: None,
            removed_count: 0,
        };
    }

    // Bound the memory content's contribution and count it against the
    // retention budget: the summary rides in the system prompt of every
    // post-compaction request.
    let memory_text = clamp_memory_content(memory_content, MAX_MEMORY_CONTENT_TOKENS);
    let memory_tokens = memory_text.len().div_ceil(3);
    let effective_config = SessionMemoryCompactConfig {
        max_retain_tokens: config
            .max_retain_tokens
            .saturating_sub(memory_tokens)
            .max(1),
        ..config.clone()
    };

    // Find the retain boundary: accumulate tokens from the tail until
    // we're within min_retain_tokens..max_retain_tokens.
    let retain_start_idx = calculate_retain_start_index(messages, &effective_config);

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
             {memory_text}\n\n\
             ---\n\n\
             Pinned messages follow:"
        ),
        cache_control: None,
    };
    let summary_prompt = Some(SystemPrompt::Blocks(vec![summary_block]));

    // Retain messages from retain_start_idx onwards. Tool_result blocks
    // whose tool_use lives on the removed side of the boundary are dropped
    // at the BLOCK level: skipping whole messages (the old forward-skip in
    // the boundary computation) threw away sibling user text and valid
    // tool_use blocks, and could orphan retained results whose use lived
    // in a skipped message — a transcript shape strict APIs reject.
    let retained_tool_uses: std::collections::HashSet<&str> = messages[retain_start_idx..]
        .iter()
        .flat_map(|msg| msg.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let retained: Vec<Message> = messages[retain_start_idx..]
        .iter()
        .filter_map(|msg| {
            let content: Vec<ContentBlock> = msg
                .content
                .clone()
                .into_iter()
                .filter(|block| match block {
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        retained_tool_uses.contains(tool_use_id.as_str())
                    }
                    _ => true,
                })
                .collect();
            if content.is_empty() {
                None
            } else {
                Some(Message {
                    role: msg.role.clone(),
                    content,
                })
            }
        })
        .collect();
    let removed_count = messages.len() - retained.len();

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
    // `None` = no boundary found (everything fits — retain all, index 0).
    // A boundary found at `idx + 1 == messages.len()` (the tail-most
    // message alone exceeds max) is a DIFFERENT state and must not be
    // conflated with it: the old `messages.len()` sentinel reset both to 0,
    // silently retaining the entire oversized transcript exactly when the
    // conversation was most over budget.
    let mut candidate_idx: Option<usize> = None;

    for (idx, msg) in messages.iter().enumerate().rev() {
        let msg_tokens = estimate_tokens_for_message_conservative(msg);
        accumulated_tokens += msg_tokens;

        if accumulated_tokens > config.max_retain_tokens {
            // We've exceeded the max budget; stop here.
            candidate_idx = Some(idx + 1);
            break;
        }

        if accumulated_tokens >= config.min_retain_tokens {
            // We've reached the minimum budget; this is a good boundary.
            candidate_idx = Some(idx);
            // Continue checking if we can include more without exceeding max.
        }
    }

    // A boundary of `messages.len()` means the tail message ALONE exceeds
    // the budget: clamp to keep that one message. Dropping it (the old
    // behavior) emptied the entire conversation exactly when it was most
    // over budget — one over-budget tail message beats an empty transcript.
    let boundary = candidate_idx
        .unwrap_or(0)
        .min(messages.len().saturating_sub(1));

    // Adjust to preserve tool_use/tool_result pairs.
    let adjusted = adjust_index_for_tool_pairs(messages, boundary);
    if adjusted < boundary {
        // Budget cap on the pullback: unbounded, it can walk to the head —
        // a retained suffix far past `max_retain_tokens`, or index 0 where
        // `session_memory_compact` no-ops and every retry repeats the same
        // no-op. The block-level orphan filter at the split already
        // preserves API validity at the original boundary, so the budget
        // wins over the pullback.
        let adjusted_suffix_tokens: usize = messages[adjusted..]
            .iter()
            .map(estimate_tokens_for_message_conservative)
            .sum();
        if adjusted_suffix_tokens > config.max_retain_tokens {
            return boundary;
        }
    }
    adjusted
}

/// Adjust a start index to ensure we don't split tool_use/tool_result pairs.
///
/// Retention always keeps a suffix of the transcript. If either side of a
/// tool pair is retained while the other side would be removed, move the
/// boundary back to the earlier side of the pair. Repeat until all known pairs
/// that cross the boundary are fully retained. Orphaned tool_result blocks
/// (their tool_use lives on the removed side) are filtered at the split in
/// [`session_memory_compact`] — not by moving the boundary forward, which
/// used to drop whole multi-block messages.
fn adjust_index_for_tool_pairs(messages: &[Message], start_idx: usize) -> usize {
    if start_idx == 0 || start_idx >= messages.len() {
        return start_idx;
    }

    // Borrowed keys: cloning every tool id String per block was pure
    // allocation overhead on long transcripts.
    let mut tool_uses: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut tool_results: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();

    for (idx, msg) in messages.iter().enumerate() {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    tool_uses.entry(id.as_str()).or_insert(idx);
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    tool_results.entry(tool_use_id.as_str()).or_insert(idx);
                }
                _ => {}
            }
        }
    }

    let mut adjusted = start_idx;

    // Each pass moves the boundary to the EARLIEST crossing pair in one
    // step (the old first-hit-and-restart form re-scanned all pairs per
    // move). Passes now equal the number of distinct boundary positions,
    // not the number of crossing pairs. Note: this cannot be collapsed to
    // the single-pass "min earliest over pairs with latest >= start_idx" —
    // a chain (a pair entirely left of the first move can cross the moved
    // boundary) needs the iteration.
    loop {
        let mut next = adjusted;
        for (id, use_idx) in &tool_uses {
            let Some(&result_idx) = tool_results.get(id) else {
                continue;
            };
            let earliest = (*use_idx).min(result_idx);
            let latest = (*use_idx).max(result_idx);
            if earliest < adjusted && latest >= adjusted {
                next = next.min(earliest);
            }
        }
        if next >= adjusted {
            break;
        }
        adjusted = next;
    }

    adjusted
}

/// Token estimate for a single message — the SAME accounting the gate
/// (`crate::compaction::estimate_tokens`) uses, so a transcript the gate
/// calls oversized cannot look in-budget to the boundary walk. The old
/// local `len().div_ceil(3)` counter disagreed with the gate's
/// tokenizer-based counts and ignored `content_blocks` / server-tool
/// blocks, letting the retained tail materially exceed
/// `max_retain_tokens`.
fn estimate_tokens_for_message_conservative(msg: &Message) -> usize {
    crate::compaction::estimate_tokens_for_message(
        msg,
        crate::compaction::message_has_tool_use(msg),
    )
}

/// Truncate `text` to roughly `max_tokens` without splitting a UTF-8
/// character, marking the cut.
fn clamp_memory_content(text: &str, max_tokens: usize) -> String {
    let max_chars = max_tokens.saturating_mul(3);
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n…[memory truncated at {max_tokens} tokens]",
        &text[..end]
    )
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
    fn tail_message_alone_over_budget_still_retains_the_tail() {
        // The old boundary walk returned `messages.len()` when the tail
        // message alone exceeded the budget, dropping the ENTIRE
        // conversation. The clamp must keep at least the last message.
        let messages = vec![msg("user", "early"), msg("assistant", &"x".repeat(200_000))];
        let config = SessionMemoryCompactConfig {
            enabled: true,
            min_retain_tokens: 10,
            max_retain_tokens: 1_000,
        };
        let result = session_memory_compact(&messages, "memory", &config);
        assert!(
            !result.messages.is_empty(),
            "entire conversation dropped; must retain the tail message"
        );
        assert!(
            result
                .messages
                .last()
                .is_some_and(|m| m.role == "assistant")
        );
    }

    #[test]
    fn should_use_returns_false_when_disabled() {
        let config = SessionMemoryCompactConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(!should_use_session_memory_compact(
            "memory",
            &[msg("user", "test")],
            &config
        ));
    }

    #[test]
    fn should_use_returns_false_when_memory_empty() {
        let config = SessionMemoryCompactConfig::default();
        assert!(!should_use_session_memory_compact(
            "",
            &[msg("user", "test")],
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
    fn pair_pullback_is_capped_by_the_retain_budget() {
        // A tool_use near the head whose result sits in the retained tail
        // used to walk the boundary back unbounded — past
        // `max_retain_tokens` or to 0 (where the compaction no-ops and
        // every retry repeats it). The budget must win: the boundary falls
        // back and the block-level orphan filter handles the crossing pair.
        let messages = vec![
            msg("user", "start"),                  // 0
            tool_use_msg("t1", "read"),            // 1 — pair head, far from boundary
            msg("assistant", &"h".repeat(40_000)), // 2 — huge middle message
            tool_result_msg("t1", "result"),       // 3 — walk lands the boundary here
            msg("assistant", "final"),             // 4
        ];
        let config = SessionMemoryCompactConfig {
            enabled: true,
            min_retain_tokens: 10,
            max_retain_tokens: 1_000,
        };
        let result = session_memory_compact(&messages, "memory", &config);
        // The pair pullback to index 1 would retain the 40K middle message;
        // the budget cap must keep the retained suffix small.
        let retained_tokens: usize = result
            .messages
            .iter()
            .map(estimate_tokens_for_message_conservative)
            .sum();
        assert!(
            retained_tokens <= config.max_retain_tokens,
            "pullback must not push the retained suffix past the budget (got {retained_tokens} tokens, {} messages)",
            result.messages.len()
        );
        assert!(
            !result.messages.is_empty(),
            "budget cap must not empty the transcript either"
        );
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
