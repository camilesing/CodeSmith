//! Tool system modules and re-exports.

// Tools run inside the TUI alt-screen runtime. Raw `print!` / `eprintln!`
// inside this module tree leaks into ratatui's diff-renderer buffer and
// produces the "scroll demon" regression (#1085 / v0.8.27 follow-up).
// Route status/error reporting through `tracing::*` instead — the
// `runtime_log` subscriber captures it to `~/.deepseek/logs/`.
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

pub mod agent_memory;
pub use codesmith_tool_impls::tools::apply_patch;
pub mod approval_cache;
pub mod arg_repair;
pub mod automation;
pub mod cargo_failure_summary;
pub use codesmith_tool_impls::tools::diagnostics;
pub mod diff_format;
pub mod file;
pub use codesmith_tool_impls::tools::file_search;
pub use codesmith_tool_impls::tools::finance;

pub use codesmith_tool_impls::tools::fetch_url;
pub use codesmith_tool_impls::tools::fim;
pub use codesmith_tool_impls::tools::git;
pub use codesmith_tool_impls::tools::git_history;
pub mod github;
pub mod goal;
pub use codesmith_tool_impls::tools::handle;
pub use codesmith_tool_impls::tools::image_ocr;
pub mod js_execution;
pub use codesmith_tool_impls::tools::knowledge_recall;
pub mod large_output_router;
pub use codesmith_tool_impls::tools::notify;
pub use codesmith_tool_impls::tools::pandoc;
pub use codesmith_tool_impls::tools::parallel;
pub use codesmith_tool_impls::tools::plan;
pub use codesmith_tool_impls::tools::plan_file;
pub use codesmith_tool_impls::tools::plan_mode;
pub mod plugin;
pub use codesmith_tool_impls::tools::project;
pub use codesmith_tool_impls::tools::recall_archive;
pub mod registry;
pub use codesmith_tool_impls::tools::remember;
pub use codesmith_tool_impls::tools::revert_turn;
pub use codesmith_tool_impls::tools::review;
pub use codesmith_tool_impls::tools::rlm;
pub mod schema_sanitize;
pub use codesmith_tool_impls::tools::search;
pub use codesmith_tool_impls::tools::symbols;
pub mod shell;
mod shell_output;
pub mod skill;
// `spec` (ToolSpec, ToolContext, RuntimeToolServices, …) physically lives in
// `codesmith_agent_runtime::tools::spec`. Historical `crate::tools::spec::X`
// paths keep resolving through this module re-export.
pub use codesmith_agent_runtime::tools::spec;
pub mod subagent;
pub mod task_v2;
pub mod tasks;
pub mod team;
pub use codesmith_tool_impls::tools::test_runner;
pub use codesmith_tool_impls::tools::todo;
pub use codesmith_tool_impls::tools::tool_result_retrieval;
pub mod truncate;
pub use codesmith_tool_impls::tools::user_input;
pub use codesmith_tool_impls::tools::validate_data;
pub use codesmith_tool_impls::tools::web_run;
pub mod web_search;
pub use codesmith_tool_impls::tools::worktree;

pub use registry::{ToolRegistry, ToolRegistryBuilder, ToolRegistryPluginExt};
pub use review::ReviewOutput;
pub use spec::ToolContext;
pub use user_input::UserInputResponse;
