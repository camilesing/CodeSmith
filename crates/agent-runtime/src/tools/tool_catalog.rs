//! Native tool catalog defaults.
//!
//! The default active native tool set — the tools eager-loaded at startup
//! (as opposed to deferred behind tool-search progressive disclosure).
//! Relocated from `crates/tui/src/core/engine/tool_catalog.rs` so the
//! prompt builder (which renders a core-tool taxonomy block from this
//! list) can live in `codesmith-agent-runtime` without a circular
//! dependency back into the TUI engine body.

/// Native tools enabled by default. These are loaded eagerly and never
/// deferred by `should_default_defer_tool`. The prompt builder's core
/// tool taxonomy block references a subset of these names.
pub const DEFAULT_ACTIVE_NATIVE_TOOLS: &[&str] = &[
    "agent_open",
    "apply_patch",
    "checklist_write",
    "edit_file",
    "exec_interact",
    "exec_shell",
    "exec_shell_interact",
    "exec_shell_wait",
    "exec_wait",
    "fetch_url",
    "file_search",
    "git_diff",
    "git_status",
    "grep_files",
    "list_dir",
    "read_file",
    "run_tests",
    "task_create",
    "task_list",
    "task_read",
    "task_shell_start",
    "task_shell_wait",
    "update_plan",
    "web_search",
    "write_file",
];

/// Accessor kept as a function (rather than importing the constant
/// directly) to mirror the original engine-body API and leave room for
/// the list to become mode- or config-aware without churning call sites.
#[must_use]
pub fn default_active_native_tool_names() -> &'static [&'static str] {
    DEFAULT_ACTIVE_NATIVE_TOOLS
}
