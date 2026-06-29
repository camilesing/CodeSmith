//! Recursive Language Model (RLM) loop — paper-spec Algorithm 1.
//!
//! Implements Zhang, Kraska & Khattab (arXiv:2512.24601, §2 Algorithm 1).
//!
//! Migrated to `codesmith_agent_runtime::rlm`; re-exported here so the
//! historical `crate::rlm::{bridge, prompt, session, turn}` paths — and
//! the `crate::rlm::RlmBridge` / `crate::rlm::session::SessionObjectSnapshot`
//! item paths — keep resolving for TUI-side callers (the `rlm` tool,
//! `runtime_threads`, `tools::spec`, the engine body until it moves, etc.).

pub use codesmith_agent_runtime::rlm::*;
