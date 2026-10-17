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
//! `codesmith-tui` binary crate. Until the migration completes, the CLI keeps
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

/// Serialize tests that publish/read the ambient (process-default)
/// policy store (`codesmith_agent::policy::publish_policies` replaces
/// the whole map, so a publish in one module's tests races another's).
/// Modules expose their own `policy_test_lock()` delegating here — the
/// per-module locks this replaces were a latent cross-module flake.
#[cfg(test)]
pub(crate) fn policy_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    match LOCK.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub mod agent_memory;
pub mod artifacts;
pub mod auto_reasoning;
pub mod background_task;
pub mod bash_ast;
pub mod callback_bridge;
pub mod capacity;

pub mod capacity_memory;
pub mod child_env;
pub mod claudemd;
pub mod coherence;
pub mod command_safety;
pub mod compaction;
pub mod config_types;
pub mod cost_status;
pub mod cycle_manager;
pub mod dependencies;
pub mod engine;
pub mod engine_config;
pub mod error_taxonomy;
pub mod events;
pub mod evolution_log;
pub mod execpolicy;
pub mod features;
pub mod hooks;
pub mod host_services;
pub mod knowledge;
pub mod lsp_config;
pub mod lsp_diagnostics;
pub mod lsp_registry;
pub mod mailbox;
pub mod mcp;
pub mod mcp_oauth;
pub mod memory;
pub mod mode;
pub mod network_policy;
pub mod onboarding;
pub mod ops;
pub mod prefix_cache;
pub mod pricing;
pub mod project_context;
pub mod projections;
pub mod prompt_inspect;
pub mod prompt_runtime;
pub mod prompt_sources;
pub mod prompt_zones;
pub mod prompts;
pub mod purge;
pub mod repl;
pub mod retry_status;
pub mod rlm;
pub mod runtime_ui;
pub mod sandbox;
pub mod sanitization;
pub mod session;
pub mod session_history;
pub mod session_store;
pub mod settings_file;
pub mod shell_dispatcher;
pub mod shell_manager;
pub mod skills;
pub mod slop_ledger;
pub mod snapshot;
pub mod subagent;
pub mod team;
pub mod telemetry;
pub mod test_support;
pub mod tokenizer;
pub mod tool_dispatch;
pub mod tool_parser;
pub mod tool_state;
pub mod tools;
pub mod turn;
pub mod user_input;
pub mod utils;
pub mod working_set;
pub mod workspace_discovery;
pub mod workspace_trust;
