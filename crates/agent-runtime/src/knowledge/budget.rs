//! Budget enforcement for Knowledge On Demand.
//!
//! Limits how many memories are surfaced per turn and per session,
//! mirroring the TypeScript `MAX_SESSION_BYTES`, `MAX_MEMORIES_PER_TURN`,
//! and per-memory line/byte caps.

/// Maximum number of memories surfaced in a single turn.
pub const MAX_MEMORIES_PER_TURN: usize = 5;

/// Maximum lines per surfaced memory file (content beyond this is truncated).
pub const MAX_LINES_PER_MEMORY: usize = 30;

/// Maximum bytes per surfaced memory file.
pub const MAX_BYTES_PER_MEMORY: usize = 10_000;

/// Maximum lines in the MEMORY.md entrypoint before truncation.
pub const MAX_ENTRYPOINT_LINES: usize = 200;

/// Maximum bytes in the MEMORY.md entrypoint before truncation.
pub const MAX_ENTRYPOINT_BYTES: usize = 25_000;

/// Maximum number of memory files to scan per prefetch.
pub const MAX_MEMORY_FILES: usize = 200;

/// Session-wide byte budget for surfaced memories. Once exhausted,
/// no more memories are surfaced in the session.
pub const MAX_SESSION_BYTES: usize = 500_000;

/// Tracks session-wide byte consumption for surfaced memories.
#[derive(Debug, Clone)]
pub struct SessionByteBudget {
    pub max_bytes: usize,
    pub bytes_consumed: usize,
}

impl SessionByteBudget {
    pub fn new() -> Self {
        Self {
            max_bytes: MAX_SESSION_BYTES,
            bytes_consumed: 0,
        }
    }

    /// Check whether there is enough budget remaining for `additional` bytes.
    pub fn can_afford(&self, additional: usize) -> bool {
        self.bytes_consumed + additional <= self.max_bytes
    }

    /// Consume `bytes` from the budget.
    pub fn consume(&mut self, bytes: usize) {
        self.bytes_consumed += bytes;
    }

    /// Remaining bytes in the budget.
    pub fn remaining(&self) -> usize {
        self.max_bytes.saturating_sub(self.bytes_consumed)
    }
}

impl Default for SessionByteBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_can_afford_within_limit() {
        let budget = SessionByteBudget::new();
        assert!(budget.can_afford(10_000));
    }

    #[test]
    fn budget_cannot_afford_over_limit() {
        let mut budget = SessionByteBudget::new();
        budget.consume(MAX_SESSION_BYTES);
        assert!(!budget.can_afford(1));
    }

    #[test]
    fn budget_remaining_decreases() {
        let mut budget = SessionByteBudget::new();
        budget.consume(100_000);
        assert_eq!(budget.remaining(), MAX_SESSION_BYTES - 100_000);
    }
}
