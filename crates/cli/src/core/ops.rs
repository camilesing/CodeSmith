//! Operations submitted by the UI to the core engine.
//!
//! Re-exported from `codesmith_agent_runtime::ops` so the engine and CLI
//! share the canonical `Op` / `CompactMode` types. The implementation lives
//! in the runtime; the CLI only consumes it.
pub use codesmith_agent_runtime::ops::*;
