//! Partial compaction: directional compression that preserves or sacrifices
//! the prefix cache for targeted budget recovery.
//!
//! Two directions:
//! - **From**: Summarize messages after `pivot_index`, keeping the prefix intact
//!   (preserves V4 prefix cache). The summary sits between prefix and tail.
//! - **UpTo**: Summarize messages before `pivot_index`, removing the prefix
//!   (sacrifices cache for immediate budget relief). The summary goes into
//!   the system prompt.

use crate::llm_client::LlmClient;
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

    // Build tool_use_id → (earliest side, latest side). A pair spans the
    // compaction boundary whenever its two sides land on opposite sides of
    // `pivot`; the pivot then moves forward past the pair so both sides stay
    // together (in the summarized prefix for UpTo, in the kept prefix for
    // From). Scanning every pair is required: inspecting only the message at
    // `pivot` misses a call/result straddling it from pivot-1 / pivot+1, and
    // one side would then be summarized away from the other.
    let mut pair_bounds: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for (idx, msg) in messages.iter().enumerate() {
        for block in &msg.content {
            let id = match block {
                ContentBlock::ToolUse { id, .. } => id,
                ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id,
                _ => continue,
            };
            let bounds = pair_bounds.entry(id.clone()).or_insert((idx, idx));
            bounds.0 = bounds.0.min(idx);
            bounds.1 = bounds.1.max(idx);
        }
    }

    let mut adjusted = pivot;
    loop {
        let Some((_, (_, latest))) = pair_bounds
            .iter()
            .find(|(_, (earliest, latest))| *earliest < adjusted && *latest >= adjusted)
        else {
            break;
        };
        adjusted = latest.saturating_add(1);
        if adjusted >= messages.len() {
            // Clamp to a sliceable bound (`perform_partial_compact` slices
            // `messages[pivot..]`).
            return messages.len();
        }
    }

    adjusted
}

/// Find a good pivot index based on token budget.
///
/// For From direction: find the latest index whose RETAINED prefix
/// `messages[..pivot]` stays within the budget — the engine invokes this
/// during overflow recovery and afterwards requires the whole
/// post-compaction input to fit the budget, so the kept prefix (not the
/// summarized suffix) is what must be bounded. For UpTo direction: find
/// the smallest pivot whose RETAINED tail `messages[pivot..]` fits the
/// budget — accumulating from the tail bounds what remains, not what is
/// thrown away (a head-accumulation bound could remove only
/// ~`target_tokens` worth and leave a retained tail far above budget,
/// failing the engine's post-compaction check after the summary call was
/// already spent).
pub fn find_pivot_for_budget(
    messages: &[Message],
    direction: PartialCompactDirection,
    target_tokens: usize,
) -> usize {
    match direction {
        PartialCompactDirection::From => {
            // Accumulate from the head and stop before the KEPT prefix
            // exceeds the budget (the old backwards walk bounded the
            // summarized suffix, so a front-heavy conversation kept a
            // prefix above budget and wasted the summary round-trip on a
            // post-check that could only fail).
            let mut accumulated = 0usize;
            for (idx, msg) in messages.iter().enumerate() {
                accumulated += estimate_tokens_for_message(msg);
                if accumulated > target_tokens {
                    return adjust_pivot_for_tool_pairs(messages, idx);
                }
            }
            // Entire conversation fits in budget — no compaction needed. For
            // `From` the no-op pivot is `messages.len()` (summarizing
            // nothing); returning 0 would summarize the ENTIRE conversation
            // including the prefix, destroying the prefix cache this
            // direction exists to preserve.
            messages.len()
        }
        PartialCompactDirection::UpTo => {
            let mut accumulated = 0usize;
            let mut pivot = messages.len();
            for (idx, msg) in messages.iter().enumerate().rev() {
                accumulated += estimate_tokens_for_message(msg);
                if accumulated > target_tokens {
                    // Pivot after the current message so at least one is removed.
                    pivot = idx.saturating_add(1);
                    break;
                }
            }
            adjust_pivot_for_tool_pairs(messages, pivot.min(messages.len()))
        }
    }
}

/// Perform a partial compaction.
///
/// Depending on direction:
/// - **From**: Retains messages `[0..pivot]` as prefix, summarizes
///   `[pivot..]` into a user message carrying the summary (the system
///   prompt is untouched so the preserved prefix cache survives).
/// - **UpTo**: Retains messages `[pivot..]`, summarizes `[0..pivot]`
///   into a system-prompt block.
pub async fn partial_compact(
    client: &dyn LlmClient,
    messages: &[Message],
    request: &PartialCompactRequest,
    cache_summary: bool,
) -> anyhow::Result<PartialCompactResult> {
    if messages.is_empty() || request.pivot_index >= messages.len() {
        // Budget-recovery callers (engine overflow retry) treat the
        // unchanged result as an attempt; without a log the wasted attempt
        // is invisible.
        tracing::warn!(
            target: "compaction",
            pivot = request.pivot_index,
            len = messages.len(),
            "partial compaction no-op: pivot at or past the end of the message list"
        );
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
        // The pair sweep cascaded the pivot to the very end (the tail is
        // one call/result pair): the From direction has nothing left to
        // summarize, and the budget-recovery request silently no-ops.
        tracing::warn!(
            target: "compaction",
            pivot,
            len = messages.len(),
            "partial compaction no-op: pair sweep left no summarizable messages"
        );
        return Ok(PartialCompactResult {
            messages: messages.to_vec(),
            summary_prompt: None,
            removed_indices: vec![],
        });
    }

    // Generate summary using LLM. `user_feedback` (the engine's overflow
    // reason) is prepended to the summarized input — create_summary has no
    // parameter for it and it was silently dropped, contradicting the
    // field's documented purpose.
    let mut summarize_input = to_summarize.clone();
    if let Some(feedback) = request
        .user_feedback
        .as_deref()
        .filter(|feedback| !feedback.trim().is_empty())
    {
        summarize_input.insert(
            0,
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: format!("[caller feedback: {feedback}]"),
                    cache_control: None,
                }],
            },
        );
    }
    let summary_text = crate::compaction::create_summary(client, &summarize_input, &request.model)
        .await?
        .text;

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

    // `From` already embeds the summary in the injected user message; also
    // returning a system-prompt block would land the summary text twice
    // (the caller applies both), and rewriting the system prompt adds a
    // fresh ephemeral cache breakpoint that invalidates the prefix cache
    // `From` exists to preserve.
    let summary_prompt = match request.direction {
        PartialCompactDirection::From => None,
        PartialCompactDirection::UpTo => Some(SystemPrompt::Blocks(vec![summary_block])),
    };

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
///
/// Routes through the shared [`crate::tokenizer`] counter, mirroring the
/// estimation helpers in `compaction::mod` (the historical bytes/4
/// arithmetic under-counted CJK text by ~3x).
fn estimate_tokens_for_message(msg: &Message) -> usize {
    let counter = crate::tokenizer::default_counter();
    msg.content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => counter.count_text(text),
            ContentBlock::Thinking { thinking } => counter.count_text(thinking),
            ContentBlock::ToolUse { input, .. } => serde_json::to_string(input)
                .map(|s| counter.count_text(&s))
                .unwrap_or(100),
            ContentBlock::ToolResult {
                content,
                content_blocks,
                ..
            } => {
                // `content_blocks` can carry the substantial payload while
                // `content` is only a short fallback string — ignoring the
                // blocks under-counted tool-result-heavy history and let
                // the pivot be trusted for cache/budget decisions it could
                // not actually satisfy. Image blocks cost the fixed
                // vision estimate, not their (tiny) JSON serialization.
                let mut tokens = counter.count_text(content);
                if let Some(blocks) = content_blocks {
                    for block in blocks {
                        tokens += if block.get("type").and_then(serde_json::Value::as_str)
                            == Some("image")
                        {
                            crate::models::IMAGE_BLOCK_ESTIMATED_TOKENS
                        } else {
                            serde_json::to_string(block)
                                .map(|s| counter.count_text(&s))
                                .unwrap_or(100)
                        };
                    }
                }
                tokens
            }
            ContentBlock::Image { .. } => crate::models::IMAGE_BLOCK_ESTIMATED_TOKENS,
            // These blocks are dropped at the wire layer
            // (crates/providers/src/rig_adapter/convert.rs) before the
            // request is serialized, so 0 is deliberate for request-size
            // estimation.
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
            msg("user", "start"),        // 0
            tool_use_msg("t1", "read"),  // 1
            tool_result_msg("t1", "ok"), // 2
            msg("user", "more"),         // 3
            msg("assistant", "reply"),   // 4
        ];

        // Pivot at 2 (tool_result) — its call at 1 would be split.
        let adjusted = adjust_pivot_for_tool_pairs(&messages, 2);
        // Should move past the result to 3.
        assert!(adjusted >= 3, "adjusted pivot should not split tool pairs");
    }

    #[test]
    fn find_pivot_from_bounds_the_kept_prefix() {
        // Long prefix, short tail. Each long message is ~1000+ tokens
        // under the tokenizer-based estimator.
        let messages = vec![
            msg("user", &"x".repeat(4000)),
            msg("assistant", &"y".repeat(4000)),
            msg("user", "short"),
            msg("assistant", "brief"),
        ];

        // Budget 1500: the kept prefix is [msg0] — adding msg1 (~2000
        // total) would exceed it. The RETAINED prefix, not the summarized
        // suffix, is what must fit the budget (the old tail-walk bounded
        // the suffix and let a front-heavy conversation keep a prefix far
        // above budget, wasting the summary round-trip on a post-check
        // that could only fail).
        let pivot = find_pivot_for_budget(&messages, PartialCompactDirection::From, 1500);
        assert_eq!(pivot, 1, "prefix keeps only what fits the budget");
        assert!(
            pivot < messages.len(),
            "From pivot should leave something to summarize"
        );

        // Budget smaller than any single message: nothing can be kept —
        // pivot 0 signals infeasibility so the caller can escalate to
        // UpTo/Full instead of a doomed From attempt.
        let pivot = find_pivot_for_budget(&messages, PartialCompactDirection::From, 500);
        assert_eq!(pivot, 0, "no prefix fits — signal infeasibility");
    }

    #[test]
    fn find_pivot_from_fits_case_is_a_no_op() {
        // Everything fits: `From` must return messages.len() (summarize
        // nothing). Returning 0 summarized the ENTIRE conversation
        // including the prefix, destroying the prefix cache this
        // direction exists to preserve.
        let messages = vec![msg("user", "hi"), msg("assistant", "hello")];
        let pivot = find_pivot_for_budget(&messages, PartialCompactDirection::From, 100_000);
        assert_eq!(pivot, messages.len());
    }

    #[test]
    fn find_pivot_up_to_bounds_the_retained_tail() {
        // The retained tail must fit the budget. The old head-accumulation
        // bounded what was REMOVED: [small, huge, small] with target 500
        // removed only ~500 tokens of head and left the huge message
        // retained (post-compaction check still failing, summary wasted).
        let messages = vec![
            msg("user", &"a".repeat(100)),
            msg("assistant", &"b".repeat(2000)),
            msg("user", &"c".repeat(100)),
            msg("assistant", &"d".repeat(100)),
        ];
        let pivot = find_pivot_for_budget(&messages, PartialCompactDirection::UpTo, 500);
        // The 2000-token message must be on the removed side.
        assert!(pivot > 1, "pivot {pivot} must exclude the huge message");
    }

    #[test]
    fn find_pivot_up_to_removes_prefix() {
        let messages = vec![
            msg("user", &"x".repeat(4000)),      // ~1000 tokens
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

    #[test]
    fn adjust_pivot_catches_pairs_straddling_the_boundary() {
        // call at 1, plain message at 2, result at 3 — a pivot at 2 used to
        // slip through (only the message *at* the pivot was inspected) and
        // summarizing [0..2) would swallow the call while the result stayed.
        let messages = vec![
            msg("user", "start"),        // 0
            tool_use_msg("t1", "read"),  // 1
            msg("assistant", "note"),    // 2
            tool_result_msg("t1", "ok"), // 3
            msg("user", "more"),         // 4
        ];
        let adjusted = adjust_pivot_for_tool_pairs(&messages, 2);
        assert_eq!(adjusted, 4, "pivot must move past the whole pair");
    }

    #[test]
    fn adjust_pivot_cascades_across_overlapping_pairs() {
        // Two calls share the boundary region: moving past t1's result makes
        // t2's pair straddle the new boundary, so the sweep must continue.
        let messages = vec![
            tool_use_msg("t1", "read"),  // 0
            tool_use_msg("t2", "read"),  // 1
            tool_result_msg("t1", "ok"), // 2
            tool_result_msg("t2", "ok"), // 3
            msg("user", "more"),         // 4
        ];
        let adjusted = adjust_pivot_for_tool_pairs(&messages, 2);
        assert_eq!(adjusted, 4, "cascade must clear every straddling pair");
    }
}
