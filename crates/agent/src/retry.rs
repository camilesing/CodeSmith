//! Resolved retry policy with exponential backoff.
//!
//! Extracted from `tui::config` in Phase 6 §6a so the `LlmClient` retry loop
//! (now in [`crate::llm_client`]) can depend on it without a host dependency.

/// Hard safety ceiling on any computed backoff delay. A configured
/// `max_delay` above this cap is clamped down to it: retry sleeps beyond
/// five minutes are almost certainly a misconfiguration, and the caller's
/// total-timeout budget (if any) should be the real wall-clock bound.
const HARD_DELAY_CAP_SECS: f64 = 300.0;

/// Resolved retry policy with defaults applied.
///
/// [`RetryPolicy::delay_for_attempt`] clamps the computed delay to
/// `max_delay` and to the `HARD_DELAY_CAP_SECS` (300 s) safety ceiling, so
/// even a misconfigured large `max_delay` cannot park the client between
/// attempts.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub initial_delay: f64,
    pub max_delay: f64,
    pub exponential_base: f64,
}

impl RetryPolicy {
    /// Compute the backoff delay for a retry attempt.
    ///
    /// `attempt` is 0-indexed and counts retries already performed:
    /// attempt 0 yields exactly `initial_delay` (`powi(0) == 1.0`) and each
    /// subsequent attempt multiplies by `exponential_base`. This method
    /// ignores `enabled` and `max_retries`; bounding the number of attempts
    /// and honoring `enabled` is the caller's responsibility.
    #[must_use]
    #[allow(dead_code)] // used by runtime_api; will be wired into client retry loop
    #[allow(clippy::manual_clamp)] // NaN-dropping min/max is intentional, see below
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let exponent = i32::try_from(attempt).unwrap_or(i32::MAX);
        let delay = self.initial_delay * self.exponential_base.powi(exponent);
        let delay = delay.min(self.max_delay);
        // `min`/`max` (unlike `clamp`) drop NaN operands, so a misconfigured
        // NaN policy degrades to a bounded delay instead of panicking
        // `Duration::from_secs_f64`.
        let delay = delay.min(HARD_DELAY_CAP_SECS).max(0.0);
        std::time::Duration::from_secs_f64(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nan_policy_degrades_to_bounded_delay() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 3,
            initial_delay: f64::NAN,
            max_delay: f64::NAN,
            exponential_base: f64::NAN,
        };
        for attempt in 0..=3 {
            let secs = policy.delay_for_attempt(attempt).as_secs_f64();
            assert!(secs.is_finite() && (0.0..=300.0).contains(&secs));
        }
    }

    #[test]
    fn negative_misconfiguration_clamps_to_zero() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 1,
            initial_delay: -5.0,
            max_delay: -1.0,
            exponential_base: 2.0,
        };
        assert_eq!(policy.delay_for_attempt(0), std::time::Duration::ZERO);
    }
}
