//! Agent-specific persistent memory support (shim).
//!
//! The implementation lives in `codesmith_agent_runtime::agent_memory`; this
//! module re-exports it so the rest of the TUI keeps resolving
//! `crate::agent_memory::{...}` (including the `paths` / `prompt` / `snapshot`
//! submodules and the `AgentMemoryScope` / `AgentMemoryMetadata` types
//! re-exported from `crate::subagent` inside the runtime).

pub use codesmith_agent_runtime::agent_memory::*;
