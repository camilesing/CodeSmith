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

pub use codesmith_context::agent_memory;
pub mod attachments;
pub mod auto_reasoning;
pub mod background_task;
pub use codesmith_sandbox::bash_ast;
pub mod callback_bridge;
pub mod capacity;

pub mod capacity_memory;
pub use codesmith_context::claudemd;
pub mod coherence;
pub use codesmith_sandbox::command_safety;
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
pub use codesmith_sandbox::execpolicy;
pub mod features;
pub mod hooks;
pub mod host_services;
pub use codesmith_context::knowledge;
pub mod lsp_config;
pub mod lsp_diagnostics;
pub mod lsp_registry;
pub mod mailbox;
pub use codesmith_agent::mode;
pub use codesmith_context::memory;
pub use codesmith_context::onboarding;
pub use codesmith_mcp_client::oauth as mcp_oauth;
pub use codesmith_mcp_client::pool as mcp;
pub use codesmith_sandbox::network_policy;
pub mod ops;
pub use codesmith_prompts::prefix_cache;
pub mod pricing;
pub use codesmith_context::project_docs as project_context;
pub use codesmith_prompts::assembly as prompts;
pub use codesmith_prompts::inspect as prompt_inspect;
pub use codesmith_prompts::runtime as prompt_runtime;
pub use codesmith_prompts::sources as prompt_sources;
pub mod purge;
pub mod repl;
pub mod retry_status;
pub mod rlm;
pub mod runtime_ui;
pub use codesmith_sandbox::child_env;
pub use codesmith_sandbox::runtime as sandbox;
pub use sandbox_mode_policy::sandbox_policy_for_mode;
pub mod sandbox_mode_policy;
pub use codesmith_sandbox::sanitization;
pub mod session;
pub mod session_history;
pub use codesmith_context::settings_file;
pub use codesmith_sandbox::shell_dispatcher;
pub mod shell_manager;
pub use codesmith_prompts::skills;
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
pub mod turn_lease;
pub mod user_input;
pub mod utils;
pub mod web_service;
pub mod workflow;
pub use codesmith_context::working_set;
pub use codesmith_context::workspace_discovery;
pub use codesmith_context::workspace_trust;
