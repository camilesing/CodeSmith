//! Resolved retry policy with exponential backoff.
//!
//! Extracted from `tui::config` in Phase 6 §6a so the `LlmClient` retry loop
//! (now in [`crate::llm_client`]) can depend on it without a host dependency.
//! The backoff math lives on
//! [`RetryConfig::delay_for_attempt`](crate::llm_client::RetryConfig::delay_for_attempt);
//! this type is only the config-file `[retry]`-table shape the host resolves
//! and converts with `From<RetryPolicy> for RetryConfig`.

/// Resolved retry policy with defaults applied.
///
/// The delay math lives on [`RetryConfig`](crate::llm_client::RetryConfig),
/// which clamps each computed delay to `max_delay` and to a hard 300 s
/// safety ceiling, so even a misconfigured large `max_delay` cannot park
/// the client between attempts.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub initial_delay: f64,
    pub max_delay: f64,
    pub exponential_base: f64,
}
