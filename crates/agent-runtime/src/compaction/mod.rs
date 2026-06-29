//! Configuration types and token-estimation helpers for conversation compaction.
//!
//! [`CompactionConfig`] and its supporting constants ([`DEFAULT_TEXT_MODEL`],
//! [`MINIMUM_AUTO_COMPACTION_TOKENS`]) are shared between the engine and the
//! TUI's compaction pipeline, so they live here in the terminal-agnostic
//! runtime. The TUI re-exports them at the historical `crate::compaction` and
//! `crate::config` paths for backwards compatibility.
//!
//! The compaction *state* submodules (`circuit_breaker`, `micro_compact`,
//! `responsive_compact`, `session_memory_compact`), the
//! [`post_compact_cleanup`] helper, and the pure token-estimation helpers
//! ([`estimate_tokens`] et al.) also live here. The heavy compaction
//! *implementation* (summary building, partial/micro compaction
//! orchestration, `should_compact`, etc.) still lives in the TUI for now and
//! re-exports these via `crate::compaction`.

use crate::models::{ContentBlock, Message, SystemPrompt};

pub mod circuit_breaker;
pub mod micro_compact;
pub mod post_compact_cleanup;
pub mod responsive_compact;
pub mod session_memory_compact;

/// Default text model used as a fallback when a caller does not supply one.
///
/// Centralised here so that both the runtime and the TUI reference the same
/// value; the TUI re-exports it from `crate::config` for backwards
/// compatibility.
pub const DEFAULT_TEXT_MODEL: &str = "deepseek-v4-pro";

/// Configuration for conversation compaction behavior.
///
/// v0.8.11 simplified this from the prior token-OR-message-count trigger
/// to a token-only trigger gated by an absolute floor. The
/// `message_threshold` field was removed: its only purpose was to fire
/// compaction on long sessions of small messages, which is exactly the
/// case where rewriting the V4 prefix cache is least valuable. Token
/// budget is the right signal; message count was a 128K-era heuristic.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionConfig {
    pub enabled: bool,
    pub token_threshold: usize,
    pub model: String,
    pub cache_summary: bool,
    /// Hard floor — `should_compact` returns `false` when total session
    /// tokens fall below this number, regardless of `enabled` or
    /// `token_threshold`. Defaults to [`MINIMUM_AUTO_COMPACTION_TOKENS`]
    /// (0) so sub-500K providers can compact before provider rejection.
    /// Tests or explicit callers can set this when they need a later floor.
    pub auto_floor_tokens: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            // ON BY DEFAULT since v0.8.6 (#402 P0 survivability) — but the
            // engine-level `auto_compact` setting was flipped OFF in v0.8.11
            // (#665) so this default is mostly a fallback for code paths
            // that build a `CompactionConfig` without going through
            // `compaction_threshold_for_model_and_effort`. Real per-model
            // values are still derived through that helper.
            enabled: true,
            // Provider-neutral default: near the effective context window after
            // reserving summary output and a safety buffer. Real call sites
            // override this via `compaction_threshold_for_model_and_effort`.
            token_threshold: 967_000,
            model: DEFAULT_TEXT_MODEL.to_string(),
            cache_summary: true,
            auto_floor_tokens: MINIMUM_AUTO_COMPACTION_TOKENS,
        }
    }
}

/// Hard floor for automatic compaction in provider-neutral mode.
///
/// Automatic compaction now follows the model's effective context window instead
/// of a DeepSeek V4-specific cache floor. Keeping the default at zero ensures
/// 128K/200K providers can compact before they hit provider hard limits. Users
/// and tests can still set `auto_floor_tokens` when they intentionally want a
/// later trigger.
pub const MINIMUM_AUTO_COMPACTION_TOKENS: usize = 0;

// === Token-estimation helpers (moved from tui compaction/mod.rs) ===
//
// Pure functions over API message types. Kept `pub` so the TUI's heavy
// compaction engine can re-export and call them unqualified; they are the
// single source of truth for the rough char/token heuristic.

pub fn estimate_tokens_for_message(message: &Message, include_thinking: bool) -> usize {
    message
        .content
        .iter()
        .map(|c| match c {
            ContentBlock::Text { text, .. } => text.len() / 4,
            // Historical reasoning blocks are UI/session metadata for DeepSeek.
            // Only current-turn tool-call reasoning is sent back to the API.
            ContentBlock::Thinking { thinking } if include_thinking => thinking.len() / 4,
            ContentBlock::Thinking { .. } => 0,
            ContentBlock::ToolUse { input, .. } => serde_json::to_string(input)
                .map(|s| s.len() / 4)
                .unwrap_or(100),
            ContentBlock::ToolResult { content, .. } => content.len() / 4,
            ContentBlock::ServerToolUse { .. }
            | ContentBlock::ToolSearchToolResult { .. }
            | ContentBlock::CodeExecutionToolResult { .. } => 0,
        })
        .sum::<usize>()
}

pub fn estimate_tokens(messages: &[Message]) -> usize {
    // Rough estimate: ~4 chars per token. DeepSeek thinking-mode rule: any
    // assistant message with tool_calls keeps its reasoning_content forever
    // (replayed in all subsequent requests). Final text-only answers drop it.
    messages
        .iter()
        .map(|message| estimate_tokens_for_message(message, message_has_tool_use(message)))
        .sum()
}

pub fn message_has_tool_use(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
}

pub fn estimate_text_tokens_conservative(text: &str) -> usize {
    text.chars().count().div_ceil(3)
}

pub fn estimate_system_tokens_conservative(system: Option<&SystemPrompt>) -> usize {
    match system {
        Some(SystemPrompt::Text(text)) => estimate_text_tokens_conservative(text),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| estimate_text_tokens_conservative(&block.text))
            .sum(),
        None => 0,
    }
}

/// Conservative estimate for full request input tokens (messages + system + framing).
#[must_use]
pub fn estimate_input_tokens_conservative(
    messages: &[Message],
    system: Option<&SystemPrompt>,
) -> usize {
    let message_tokens = estimate_tokens(messages).saturating_mul(3).div_ceil(2);
    let system_tokens = estimate_system_tokens_conservative(system);
    let framing_overhead = messages.len().saturating_mul(12).saturating_add(48);
    message_tokens
        .saturating_add(system_tokens)
        .saturating_add(framing_overhead)
}
