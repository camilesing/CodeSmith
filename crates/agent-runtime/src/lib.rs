//! Unified agent execution core for CodeSmith.
//!
//! This crate is the shared "execution kernel" consumed by all run-forms
//! (interactive REPL, headless/SDK one-shots, subagents, background agents, and
//! the app-server). It owns the streaming turn loop (`run_turn_once`), the
//! `Engine` / `EngineHandle` / `Op` / `Event` protocol, and the supporting
//! subsystems the loop depends on (compaction, sandbox, MCP, hooks, knowledge,
//! command safety).
//!
//! Migration status: modules are being moved here incrementally from the
//! `codesmith-tui` binary crate. Until the migration completes, the TUI keeps
//! thin re-export shims (`pub use codesmith_agent_runtime::…`) so existing
//! `use crate::<module>` paths keep resolving.

#![forbid(unsafe_code)]
