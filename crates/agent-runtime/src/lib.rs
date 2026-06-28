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

// `deny` (not `forbid`): the execution core legitimately uses `unsafe` for
// `std::env` mutation in `child_env` (documented single-threaded pre-spawn
// pattern) and starlark trait impls in `execpolicy::parser`. Each is gated
// with a file-local `#![allow(unsafe_code)]` + safety note.
#![deny(unsafe_code)]

// Re-export the agent "primitives" layer so modules moved into this crate can
// keep referencing `crate::models` / `crate::llm_client` / `crate::retry`
// without hard-coding `codesmith_agent::` paths.
pub use codesmith_agent::{llm_client, models, retry};

pub mod background_task;
pub mod capacity;
pub mod capacity_memory;
pub mod child_env;
pub mod coherence;
pub mod command_safety;
pub mod compaction;
pub mod cost_status;
pub mod cycle_manager;
pub mod dependencies;
pub mod error_taxonomy;
pub mod events;
pub mod execpolicy;
pub mod features;
pub mod hooks;
pub mod knowledge;
pub mod mailbox;
pub mod mcp;
pub mod mode;
pub mod network_policy;
pub mod ops;
pub mod prefix_cache;
pub mod pricing;
pub mod prompt_runtime;
pub mod prompt_zones;
pub mod retry_status;
pub mod runtime_ui;
pub mod snapshot;
pub mod subagent;
pub mod team;
pub mod test_support;
pub mod tool_dispatch;
pub mod user_input;
pub mod utils;
pub mod working_set;
pub mod workspace_discovery;
pub mod workspace_trust;
