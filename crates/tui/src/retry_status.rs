//! Re-export of [`codesmith_agent_runtime::retry_status`] (Phase 6 §6b-1).
//!
//! Canonical home is now `codesmith_agent_runtime`. This glob shim flattens
//! the runtime module's public items so `crate::retry_status::<item>` paths
//! in the TUI keep working until later steps rewire them onto the runtime
//! crate directly.
pub use codesmith_agent_runtime::retry_status::*;

#[cfg(test)]
mod test_helpers {
    /// Test-only serialization guard mirroring the one that used to live in
    /// `retry_status::test_guard`. The original is `#[cfg(test)]` in the
    /// runtime crate, so it is invisible to TUI's test build (dependencies
    /// aren't compiled with `cfg(test)`). TUI tests touch the same global
    /// retry-state cells via the (always-compiled) `snapshot`/`clear`, so they
    /// need a guard local to this test binary to avoid torn reads under
    /// cargo's parallel runner.
    pub fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
pub use test_helpers::test_guard;
