//! Responsive compaction: recovery when API returns prompt-too-long errors.
//!
//! When the model API rejects a request because the context is too long,
//! responsive compaction attempts recovery through a cascade of increasingly
//! aggressive strategies before falling back to trimming oldest messages.

/// State tracker for responsive compaction attempts.
#[derive(Debug, Clone, Default)]
pub struct ResponsiveCompactState {
    /// Consecutive prompt-too-long errors encountered this turn.
    pub consecutive_overflows: u32,
    /// Total responsive compaction attempts made this session.
    pub attempts: u32,
    /// Maximum attempts before giving up (default: 3).
    pub max_attempts: u32,
}

impl ResponsiveCompactState {
    /// Create state with default settings.
    pub fn new() -> Self {
        Self {
            consecutive_overflows: 0,
            attempts: 0,
            max_attempts: 3,
        }
    }

    /// Record a prompt-too-long overflow event.
    pub fn record_overflow(&mut self) {
        self.consecutive_overflows += 1;
        self.attempts += 1;
    }

    /// Reset after successful recovery.
    pub fn reset(&mut self) {
        self.consecutive_overflows = 0;
    }

    /// Check if we've exhausted our recovery attempts.
    pub fn is_exhausted(&self) -> bool {
        self.attempts >= self.max_attempts
    }
}

/// The action to take for a prompt-too-long recovery.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResponsiveCompactAction {
    /// Try micro-compaction first (cheapest, no API call).
    MicroCompact,
    /// Try partial compaction preserving the prefix cache (From direction).
    PartialFrom,
    /// Try partial compaction sacrificing the prefix cache (UpTo direction).
    PartialUpTo,
    /// Try full compaction.
    FullCompact,
    /// Cannot recover — give up.
    Fail,
}

/// Determine the next recovery action based on state and overflow count.
///
/// Recovery cascade:
/// 1. MicroCompact (clear tool results, no API call)
/// 2. PartialFrom (summarize tail, preserve prefix cache)
/// 3. PartialUpTo (summarize head, sacrifice cache)
/// 4. FullCompact (full LLM summary)
/// 5. Fail (exhausted all attempts)
pub fn next_recovery_action(
    state: &ResponsiveCompactState,
    attempt_number: u32,
) -> ResponsiveCompactAction {
    if state.is_exhausted() {
        return ResponsiveCompactAction::Fail;
    }

    match attempt_number {
        0 => ResponsiveCompactAction::MicroCompact,
        1 => ResponsiveCompactAction::PartialFrom,
        2 => ResponsiveCompactAction::PartialUpTo,
        3 => ResponsiveCompactAction::FullCompact,
        _ => ResponsiveCompactAction::Fail,
    }
}

/// Check if an API error message indicates a context-length / prompt-too-long problem.
pub fn is_prompt_too_long_error(error_message: &str) -> bool {
    let lower = error_message.to_lowercase();
    lower.contains("context")
        || lower.contains("token")
        || lower.contains("prompt is too long")
        || lower.contains("requested")
        || lower.contains("maximum")
        || lower.contains("too long for the current model")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_tracks_overflows() {
        let mut state = ResponsiveCompactState::new();
        assert_eq!(state.consecutive_overflows, 0);
        assert!(!state.is_exhausted());

        state.record_overflow();
        assert_eq!(state.consecutive_overflows, 1);
        assert!(!state.is_exhausted());

        state.record_overflow();
        state.record_overflow();
        assert_eq!(state.attempts, 3);
        assert!(state.is_exhausted());
    }

    #[test]
    fn recovery_cascade_follows_order() {
        let state = ResponsiveCompactState::new();

        assert_eq!(next_recovery_action(&state, 0), ResponsiveCompactAction::MicroCompact);
        assert_eq!(next_recovery_action(&state, 1), ResponsiveCompactAction::PartialFrom);
        assert_eq!(next_recovery_action(&state, 2), ResponsiveCompactAction::PartialUpTo);
        assert_eq!(next_recovery_action(&state, 3), ResponsiveCompactAction::FullCompact);
        assert_eq!(next_recovery_action(&state, 4), ResponsiveCompactAction::Fail);
    }

    #[test]
    fn exhausted_state_returns_fail() {
        let mut state = ResponsiveCompactState::new();
        state.record_overflow();
        state.record_overflow();
        state.record_overflow();

        assert_eq!(
            next_recovery_action(&state, 0),
            ResponsiveCompactAction::Fail
        );
    }

    #[test]
    fn reset_clears_overflows() {
        let mut state = ResponsiveCompactState::new();
        state.record_overflow();
        state.record_overflow();
        state.reset();
        assert_eq!(state.consecutive_overflows, 0);
        // attempts persist (cumulative session tracking)
        assert_eq!(state.attempts, 2);
    }

    #[test]
    fn detects_prompt_too_long_errors() {
        assert!(is_prompt_too_long_error("prompt is too long for the current model"));
        assert!(is_prompt_too_long_error("maximum context length is 1000000 tokens"));
        assert!(is_prompt_too_long_error("You requested 1000001 tokens but the maximum is 1000000"));
        assert!(is_prompt_too_long_error("request exceeds context window"));

        assert!(!is_prompt_too_long_error("401 Unauthorized: Invalid API key"));
        assert!(!is_prompt_too_long_error("503 Service Unavailable"));
    }
}