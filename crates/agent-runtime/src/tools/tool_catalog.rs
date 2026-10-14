//! Native tool catalog defaults.
//!
//! The default active native tool set — the tools eager-loaded at startup
//! (as opposed to deferred behind tool-search progressive disclosure).
//! Relocated from `crates/tui/src/core/engine/tool_catalog.rs` so the
//! prompt builder (which renders a core-tool taxonomy block from this
//! list) can live in `codesmith-agent-runtime` without a circular
//! dependency back into the TUI engine body.

/// Native tools enabled by default. Tools listed here are marked
/// non-deferred by `crate::engine::tool_catalog::should_default_defer_tool`,
/// so they are eager-loaded *whenever they are registered at all*. Note
/// that registration itself can be config-gated (e.g. `symbol_search` /
/// `find_references` need an enabled code index; shell and task-shell
/// tools need `allow_shell`), in which case the entry here is inert. The
/// prompt builder renders its core-tool taxonomy block from this list —
/// keep it in sync with the enforcing function.
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
    "find_references",
    "git_diff",
    "git_status",
    "grep_files",
    "list_dir",
    "read_file",
    "run_tests",
    "symbol_search",
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
