//! Operations submitted by the UI to the core engine.
//!
//! Re-exported from `codesmith_agent_runtime::ops` so the engine and TUI
//! share the canonical `Op` / `CompactMode` types. The implementation lives
//! in the runtime; the TUI only consumes it.
pub use codesmith_agent_runtime::ops::*;
