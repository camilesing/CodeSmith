//! Repo-aware working set tracking and prompt context packing.
//!
//! Re-exported from `codesmith_agent_runtime::working_set` so the engine
//! and CLI share the canonical `Workspace` / `WorkingSet` / `WorkingSetEntry`
//! / `WorkingSetConfig` types. The implementation lives in the runtime; the
//! CLI only consumes it.
pub use codesmith_agent_runtime::working_set::*;
