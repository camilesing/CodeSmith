//! Circuit breaker for auto-compaction operations.
//!
//! Prevents infinite retry loops by tripping after consecutive failures.
//! Manual `/compact` bypasses the breaker (explicit user agency).

use std::time::{Duration, Instant};

/// Default threshold for tripping the circuit breaker.
const DEFAULT_TRIP_THRESHOLD: u32 = 3;

/// Default recovery timeout before half-open state.
const DEFAULT_RECOVERY_TIMEOUT_SECS: u64 = 300; // 5 minutes

/// Circuit breaker for compaction operations.
///
/// After `trip_threshold` consecutive failures, the breaker trips (opens)
/// and refuses further auto-compaction attempts until `recovery_timeout`
/// elapses, at which point it enters half-open state (allows one attempt).
/// A successful attempt resets the breaker; a failed one re-trips it.
///
/// Manual `/compact` bypasses the breaker via [`CompactionCircuitBreaker::force_attempt`].
#[derive(Debug, Clone)]
pub struct CompactionCircuitBreaker {
    /// Consecutive failures accumulated.
    consecutive_failures: u32,
    /// Threshold at which the breaker trips.
    trip_threshold: u32,
    /// Whether the breaker is currently tripped (open).
    is_tripped: bool,
    /// When the breaker tripped.
    tripped_at: Option<Instant>,
    /// Timeout before entering half-open state.
    recovery_timeout: Duration,
}

impl Default for CompactionCircuitBreaker {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            trip_threshold: DEFAULT_TRIP_THRESHOLD,
            is_tripped: false,
            tripped_at: None,
            recovery_timeout: Duration::from_secs(DEFAULT_RECOVERY_TIMEOUT_SECS),
        }
    }
}

impl CompactionCircuitBreaker {
    /// Create a new circuit breaker with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a circuit breaker with custom settings.
    pub fn with_config(trip_threshold: u32, recovery_timeout: Duration) -> Self {
        Self {
            consecutive_failures: 0,
            trip_threshold,
            is_tripped: false,
            tripped_at: None,
            recovery_timeout,
        }
    }

    /// Check whether an auto-compaction attempt should proceed.
    ///
    /// Returns `true` when:
    /// - The breaker is closed (not tripped)
    /// - The breaker is half-open (tripped but recovery_timeout elapsed)
    ///
    /// Returns `false` when:
    /// - The breaker is open (tripped and recovery_timeout not yet elapsed)
    pub fn should_attempt(&mut self) -> bool {
        if !self.is_tripped {
            return true;
        }

        let Some(tripped_at) = self.tripped_at else {
            // Inconsistent state — reset and allow.
            self.reset();
            return true;
        };

        if tripped_at.elapsed() >= self.recovery_timeout {
            // Half-open: allow one probe attempt.
            true
        } else {
            false
        }
    }

    /// Check whether a manual `/compact` attempt should proceed.
    ///
    /// Always returns `true` — manual compaction bypasses the breaker
    /// (explicit user agency overrides automatic safety).
    pub fn force_attempt(&self) -> bool {
        true
    }

    /// Record a successful compaction attempt.
    ///
    /// Resets consecutive_failures and closes the breaker.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.is_tripped = false;
        self.tripped_at = None;
    }

    /// Record a failed compaction attempt.
    ///
    /// Increments consecutive_failures. If it reaches `trip_threshold`,
    /// the breaker trips (opens).
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.trip_threshold {
            self.is_tripped = true;
            self.tripped_at = Some(Instant::now());
        }
    }

    /// Whether the breaker is currently tripped (open).
    pub fn is_tripped(&self) -> bool {
        self.is_tripped
    }

    /// Current consecutive failure count.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Fully reset the breaker state.
    ///
    /// Called during post-compaction cleanup to give a fresh start.
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.is_tripped = false;
        self.tripped_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_breaker_allows_attempts() {
        let mut breaker = CompactionCircuitBreaker::new();
        assert!(breaker.should_attempt());
        assert!(!breaker.is_tripped());
    }

    #[test]
    fn breaker_trips_after_threshold_failures() {
        let mut breaker = CompactionCircuitBreaker::new();
        assert!(breaker.should_attempt());

        breaker.record_failure();
        assert!(!breaker.is_tripped());
        assert!(breaker.should_attempt());

        breaker.record_failure();
        assert!(!breaker.is_tripped());
        assert!(breaker.should_attempt());

        breaker.record_failure(); // 3rd failure → trip
        assert!(breaker.is_tripped());
        assert!(!breaker.should_attempt());
    }

    #[test]
    fn success_resets_breaker() {
        let mut breaker = CompactionCircuitBreaker::new();
        breaker.record_failure();
        breaker.record_failure();

        breaker.record_success();
        assert!(!breaker.is_tripped());
        assert_eq!(breaker.consecutive_failures(), 0);
        assert!(breaker.should_attempt());
    }

    #[test]
    fn force_attempt_always_succeeds() {
        let mut breaker = CompactionCircuitBreaker::new();
        breaker.record_failure();
        breaker.record_failure();
        breaker.record_failure();
        assert!(breaker.is_tripped());

        // Manual compact bypasses breaker
        assert!(breaker.force_attempt());
    }

    #[test]
    fn half_open_after_recovery_timeout() {
        let mut breaker = CompactionCircuitBreaker::with_config(1, Duration::from_millis(100));
        breaker.record_failure(); // trip immediately (threshold=1)

        assert!(!breaker.should_attempt()); // still in timeout

        // Wait for recovery timeout
        std::thread::sleep(Duration::from_millis(150));
        assert!(breaker.should_attempt()); // half-open
    }

    #[test]
    fn half_open_failure_re_trips() {
        let mut breaker = CompactionCircuitBreaker::with_config(1, Duration::from_millis(100));
        breaker.record_failure();
        std::thread::sleep(Duration::from_millis(150));

        assert!(breaker.should_attempt()); // half-open, allow probe
        breaker.record_failure(); // probe fails → re-trip
        assert!(breaker.is_tripped());
        assert!(!breaker.should_attempt());
    }

    #[test]
    fn half_open_success_closes() {
        let mut breaker = CompactionCircuitBreaker::with_config(1, Duration::from_millis(100));
        breaker.record_failure();
        std::thread::sleep(Duration::from_millis(150));

        assert!(breaker.should_attempt()); // half-open
        breaker.record_success(); // probe succeeds → close
        assert!(!breaker.is_tripped());
        assert!(breaker.should_attempt());
    }

    #[test]
    fn reset_clears_all_state() {
        let mut breaker = CompactionCircuitBreaker::new();
        breaker.record_failure();
        breaker.record_failure();
        breaker.record_failure();
        assert!(breaker.is_tripped());

        breaker.reset();
        assert!(!breaker.is_tripped());
        assert_eq!(breaker.consecutive_failures(), 0);
        assert!(breaker.should_attempt());
    }
}

/// Model-traffic steps tolerated between a completed auto-compaction and
/// the next threshold trigger before it counts as a "rapid refill" (P2-5:
/// a context that refills within a handful of tool rounds isn't drifting,
/// it has a too-large file or tool output pouring into it).
pub const RAPID_REFILL_STEP_WINDOW: u32 = 3;

/// Consecutive rapid refills before the detector trips and auto-compaction
/// is suspended with an actionable message. Three matches the compaction
/// failure breaker — the mirror article's rapid-refill detector uses the
/// same number.
pub const RAPID_REFILL_MAX_STREAK: u32 = 3;

/// The actionable message surfaced when the rapid-refill streak trips.
pub const RAPID_REFILL_MESSAGE: &str = "Context refilled within a few tool rounds of the last compaction, 3 times in a row — a file or tool output is probably too large. Read it back in chunks (read_file start_line/end_line) or retrieve a slice (retrieve_tool_result mode=lines), or start a new session; auto-compaction is paused for this turn.";

/// Rapid-refill detector (P2-5): catches the failure mode where
/// auto-compaction *succeeds* but is futile — the context refills to the
/// threshold within [`RAPID_REFILL_STEP_WINDOW`] steps again and again.
/// Compounding summaries in that loop burn tokens and progressively
/// destroy the transcript, so after [`RAPID_REFILL_MAX_STREAK`]
/// consecutive rapid refills the detector trips and the caller surfaces
/// [`RAPID_REFILL_MESSAGE`] instead of compacting again. A trigger that
/// arrives after a healthy interval resets the streak.
#[derive(Debug, Clone, Default)]
pub struct RapidRefillDetector {
    /// Step index of the last completed compaction.
    last_compaction_step: Option<u32>,
    /// Consecutive within-window refills.
    streak: u32,
}

impl RapidRefillDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that an auto-compaction completed at `step`.
    pub fn record_compaction(&mut self, step: u32) {
        self.last_compaction_step = Some(step);
    }

    /// A compaction trigger fired at `step`. Returns `Some(message)` when
    /// the refill streak tripped — the caller should skip the compaction
    /// and surface the message. A trigger with no prior compaction, or
    /// after a healthy interval (>= [`RAPID_REFILL_STEP_WINDOW`] steps),
    /// resets the streak: it's ordinary context growth, not a leak.
    pub fn record_trigger(&mut self, step: u32) -> Option<&'static str> {
        let last = self.last_compaction_step?;
        if step.saturating_sub(last) > RAPID_REFILL_STEP_WINDOW {
            self.streak = 0;
            return None;
        }
        self.streak += 1;
        if self.streak >= RAPID_REFILL_MAX_STREAK {
            Some(RAPID_REFILL_MESSAGE)
        } else {
            None
        }
    }

    /// Fully reset (post-compaction cleanup gives a fresh start).
    pub fn reset(&mut self) {
        self.last_compaction_step = None;
        self.streak = 0;
    }

    /// Current consecutive refill count (tests / observability).
    pub fn streak(&self) -> u32 {
        self.streak
    }
}

#[cfg(test)]
mod refill_tests {
    use super::*;

    #[test]
    fn no_prior_compaction_never_trips() {
        let mut det = RapidRefillDetector::new();
        assert!(det.record_trigger(0).is_none());
        assert!(det.record_trigger(100).is_none());
    }

    #[test]
    fn three_rapid_refills_trip_with_message() {
        let mut det = RapidRefillDetector::new();
        det.record_compaction(10);
        assert!(det.record_trigger(12).is_none(), "1st refill counts");
        assert_eq!(det.streak(), 1);
        det.record_compaction(12);
        assert!(det.record_trigger(14).is_none(), "2nd refill counts");
        det.record_compaction(14);
        assert_eq!(
            det.record_trigger(16),
            Some(RAPID_REFILL_MESSAGE),
            "3rd rapid refill trips"
        );
    }

    #[test]
    fn healthy_interval_resets_the_streak() {
        let mut det = RapidRefillDetector::new();
        det.record_compaction(10);
        assert!(det.record_trigger(12).is_none());
        // Refill after a long, healthy interval — ordinary growth.
        det.record_compaction(12);
        assert!(det.record_trigger(50).is_none());
        assert_eq!(det.streak(), 0, "healthy interval resets the streak");
    }

    #[test]
    fn window_boundary_is_inclusive() {
        // Exactly RAPID_REFILL_STEP_WINDOW steps after the compaction
        // still counts as rapid (>= threshold-within-window semantics).
        let mut det = RapidRefillDetector::new();
        det.record_compaction(10);
        assert!(
            det.record_trigger(10 + RAPID_REFILL_STEP_WINDOW).is_none()
        );
        assert_eq!(det.streak(), 1, "boundary refill counts as rapid");
        // One step beyond the window does not.
        det.record_compaction(30);
        assert!(
            det.record_trigger(30 + RAPID_REFILL_STEP_WINDOW + 1).is_none()
        );
        assert_eq!(det.streak(), 0, "beyond-window refill resets");
    }

    #[test]
    fn reset_clears_state() {
        let mut det = RapidRefillDetector::new();
        det.record_compaction(10);
        assert!(det.record_trigger(12).is_none());
        det.reset();
        assert!(det.record_trigger(13).is_none(), "no anchor after reset");
        assert_eq!(det.streak(), 0);
    }
}
