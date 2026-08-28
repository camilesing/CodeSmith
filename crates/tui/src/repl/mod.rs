//! Long-lived Python REPL runtime used by the RLM loop and by inline
//! `` ```repl `` block execution in the agent loop.
//!
//! Migrated to `codesmith_agent_runtime::repl`; re-exported here so the
//! historical `crate::repl::{runtime, sandbox}` paths — and the
//! `crate::repl::PythonRuntime` / `crate::repl::sandbox::has_repl_block`
//! item paths — keep resolving for TUI-side callers (the `rlm` tool, the
//! engine body until it moves, etc.).

pub use codesmith_agent_runtime::repl::*;
