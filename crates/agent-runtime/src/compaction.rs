//! Configuration types for conversation compaction.
//!
//! [`CompactionConfig`] and its supporting constants ([`DEFAULT_TEXT_MODEL`],
//! [`MINIMUM_AUTO_COMPACTION_TOKENS`]) are shared between the engine and the
//! TUI's compaction pipeline, so they live here in the terminal-agnostic
//! runtime. The TUI re-exports them at the historical `crate::compaction` and
//! `crate::config` paths for backwards compatibility. The compaction
//! *implementation* (summary building, partial/micro compaction, etc.) still
//! lives in the TUI for now.

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
