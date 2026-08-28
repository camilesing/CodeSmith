//! Post-compaction cleanup: reset stale state after a compaction cycle.
//!
//! After compaction removes messages, various session caches and state
//! may reference stale data. This module resets them to ensure the
//! session continues correctly.
//!
//! Migrated from the TUI (`crates/tui/src/compaction/post_compact_cleanup.rs`)
//! as part of the engine-closure extraction — the engine body references
//! this via `crate::compaction::post_compact_cleanup`.

use super::micro_compact::MicroCompactState;
use crate::session::Session;

/// Execute post-compaction cleanup on the session.
///
/// Resets:
/// 1. Micro-compact state (bytes cleared, timestamps)
/// 2. System prompt hash cache (summary changed the prompt)
/// 3. Circuit breaker (fresh start in new context)
/// 4. Forces working set rebuild (removed messages may reference stale paths)
pub fn post_compact_cleanup(session: &mut Session) {
    // 1. Reset micro-compact state
    session.micro_compact_state = MicroCompactState::default();

    // 2. Invalidate system prompt hash cache — the compaction summary
    //    changes the system prompt content, so the cached hash must be
    //    cleared to force re-assembly.
    session.last_system_prompt_hash = None;

    // 3. Reset circuit breaker — new context means new opportunity.
    session.circuit_breaker.reset();

    // 4. Rebuild working set — removed messages may have contributed
    //    path entries that no longer exist in the conversation.
    session
        .working_set
        .lock()
        .expect("working_set poisoned")
        .force_rebuild();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use std::path::PathBuf;

    fn make_test_session() -> Session {
        Session::new(
            "deepseek-v4-flash".to_string(),
            PathBuf::from("/tmp/test"),
            false,
            false,
            PathBuf::from("/tmp/notes"),
            PathBuf::from("/tmp/mcp"),
        )
    }

    #[test]
    fn cleanup_resets_micro_compact_state() {
        let mut session = make_test_session();
        session.micro_compact_state.bytes_cleared = 10000;
        post_compact_cleanup(&mut session);
        assert_eq!(session.micro_compact_state.bytes_cleared, 0);
    }

    #[test]
    fn cleanup_clears_system_prompt_hash() {
        let mut session = make_test_session();
        session.last_system_prompt_hash = Some(99999);
        post_compact_cleanup(&mut session);
        assert!(session.last_system_prompt_hash.is_none());
    }

    #[test]
    fn cleanup_resets_circuit_breaker() {
        let mut session = make_test_session();
        session.circuit_breaker.record_failure();
        session.circuit_breaker.record_failure();
        session.circuit_breaker.record_failure();
        assert!(session.circuit_breaker.is_tripped());

        post_compact_cleanup(&mut session);
        assert!(!session.circuit_breaker.is_tripped());
        assert_eq!(session.circuit_breaker.consecutive_failures(), 0);
    }
}
