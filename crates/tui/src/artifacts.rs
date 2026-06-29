//! Session-scoped artifact metadata (re-export shim).
//!
//! Canonical home is now `codesmith_agent_runtime::artifacts`; this glob shim
//! flattens the runtime module's public items so historical
//! `crate::artifacts::<item>` paths keep resolving. The test-session-root
//! override helpers (`set_test_artifact_sessions_root`,
//! `TEST_ARTIFACT_SESSIONS_GUARD`) are compiled unconditionally in the runtime
//! crate (see `codesmith_agent_runtime::test_support` for the same convention)
//! so this re-export also reaches them from TUI test code.
pub use codesmith_agent_runtime::artifacts::*;
