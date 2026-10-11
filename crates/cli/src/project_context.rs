//! Project context loading for CodeSmith.
//!
//! Re-exported from `codesmith_agent_runtime::project_context` so the engine
//! and TUI share the canonical `ProjectContext` type and loaders
//! (`load_project_context`, `load_project_context_with_parents`,
//! `create_default_agents_md`, `merge_contexts`, etc.). The implementation
//! lives in the runtime; the TUI only consumes it.
pub use codesmith_agent_runtime::project_context::*;
