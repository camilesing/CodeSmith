//! Re-export of [`codesmith_agent_runtime::test_support`] (Phase 6 §6b-2).
//!
//! Canonical home is now `codesmith_agent_runtime`. These shared test
//! helpers (`lock_test_env`, `EnvVarGuard`, `assert_byte_identical`, …) are
//! compiled unconditionally in the runtime crate (not `cfg(test)`-gated) so
//! they remain visible to the TUI's test build — dependencies are not built
//! with `cfg(test)`, so a `cfg(test)`-gated helper would be invisible
//! cross-crate. This glob shim flattens them so `crate::test_support::<item>`
//! paths in the TUI keep working.
pub use codesmith_agent_runtime::test_support::*;
