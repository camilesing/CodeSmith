//! Repo-aware working set tracking and prompt context packing.
//!
//! Re-exported from `codesmith_agent_runtime::working_set` so the engine
//! and TUI share the canonical `Workspace` / `WorkingSet` / `WorkingSetEntry`
//! / `WorkingSetConfig` types. The implementation lives in the runtime; the
//! TUI only consumes it.
pub use codesmith_agent_runtime::working_set::*;
