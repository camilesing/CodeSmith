//! Re-export of [`codesmith_agent_runtime::test_support`].
//!
//! Canonical home is `codesmith_agent_runtime`. These shared test helpers
//! (`lock_test_env`, `EnvVarGuard`, `assert_byte_identical`, …) are compiled
//! unconditionally in the runtime crate (not `cfg(test)`-gated) so they stay
//! visible to this crate's test build — dependencies are not built with
//! `cfg(test)`, so a `cfg(test)`-gated helper would be invisible cross-crate.
//! This glob shim flattens them so `crate::test_support::<item>` paths keep
//! working.
pub use codesmith_agent_runtime::test_support::*;
