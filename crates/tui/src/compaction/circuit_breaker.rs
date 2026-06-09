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