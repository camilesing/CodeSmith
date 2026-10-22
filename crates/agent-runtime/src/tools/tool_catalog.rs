//! Native tool catalog defaults.
//!
//! The default active native tool set — the tools eager-loaded at startup
//! (as opposed to deferred behind tool-search progressive disclosure).
//! Relocated from `crates/tui/src/core/engine/tool_catalog.rs` so the
//! prompt builder (which renders a core-tool taxonomy block from this
//! list) can live in `codesmith-agent-runtime` without a circular
//! dependency back into the TUI engine body.

/// Canonical name of the sub-agent release tool (`AgentCloseTool`).
pub const AGENT_CLOSE_NAME: &str = "agent_close";
/// Canonical name of the sub-agent follow-up/harvest tool (`AgentEvalTool`).
pub const AGENT_EVAL_NAME: &str = "agent_eval";
/// Canonical name of the sub-agent opener tool (`AgentOpenTool`).
pub const AGENT_OPEN_NAME: &str = "agent_open";
/// Canonical name of the patch-application tool (`ApplyPatchTool`).
pub const APPLY_PATCH_NAME: &str = "apply_patch";
/// Canonical name of the durable checklist write tool (`TodoTool::checklist`).
pub const CHECKLIST_WRITE_NAME: &str = "checklist_write";
/// Canonical name of the file edit tool (`EditFileTool`).
pub const EDIT_FILE_NAME: &str = "edit_file";
/// Canonical name of the legacy interact alias (`ShellInteractTool`).
pub const EXEC_INTERACT_NAME: &str = "exec_interact";
/// Canonical name of the foreground shell tool (`ExecShellTool`).
pub const EXEC_SHELL_NAME: &str = "exec_shell";
/// Canonical name of the interactive shell tool (`ShellInteractTool`).
pub const EXEC_SHELL_INTERACT_NAME: &str = "exec_shell_interact";
/// Canonical name of the shell wait/poll tool (`ShellWaitTool`).
pub const EXEC_SHELL_WAIT_NAME: &str = "exec_shell_wait";
/// Canonical name of the legacy wait alias (`ShellWaitTool`).
pub const EXEC_WAIT_NAME: &str = "exec_wait";
/// Canonical name of the URL fetch tool (`FetchUrlTool`).
pub const FETCH_URL_NAME: &str = "fetch_url";
/// Canonical name of the fuzzy file-search tool (`FileSearchTool`).
pub const FILE_SEARCH_NAME: &str = "file_search";
/// Canonical name of the code-index references tool (`FindReferencesTool`).
pub const FIND_REFERENCES_NAME: &str = "find_references";
/// Canonical name of the git diff tool (`GitDiffTool`).
pub const GIT_DIFF_NAME: &str = "git_diff";
/// Canonical name of the git status tool (`GitStatusTool`).
pub const GIT_STATUS_NAME: &str = "git_status";
/// Canonical name of the content-search tool (`GrepFilesTool`).
pub const GREP_FILES_NAME: &str = "grep_files";
/// Canonical name of the directory listing tool (`ListDirTool`).
pub const LIST_DIR_NAME: &str = "list_dir";
/// Canonical name of the file read tool (`ReadFileTool`).
pub const READ_FILE_NAME: &str = "read_file";
/// Canonical name of the test runner tool (`RunTestsTool`).
pub const RUN_TESTS_NAME: &str = "run_tests";
/// Canonical name of the code-index symbol tool (`SymbolSearchTool`).
pub const SYMBOL_SEARCH_NAME: &str = "symbol_search";
/// Canonical name of the durable task create tool (`TaskCreateTool`).
pub const TASK_CREATE_NAME: &str = "task_create";
/// Canonical name of the durable task list tool (`TaskListTool`).
pub const TASK_LIST_NAME: &str = "task_list";
/// Canonical name of the durable task read tool (`TaskReadTool`).
pub const TASK_READ_NAME: &str = "task_read";
/// Canonical name of the background shell start tool (`TaskShellStartTool`).
pub const TASK_SHELL_START_NAME: &str = "task_shell_start";
/// Canonical name of the background shell wait tool (`TaskShellWaitTool`).
pub const TASK_SHELL_WAIT_NAME: &str = "task_shell_wait";
/// Canonical name of the plan update tool (`UpdatePlanTool`).
pub const UPDATE_PLAN_NAME: &str = "update_plan";
/// Canonical name of the web search tool (`WebSearchTool`).
pub const WEB_SEARCH_NAME: &str = "web_search";
/// Canonical name of the file write tool (`WriteFileTool`).
pub const WRITE_FILE_NAME: &str = "write_file";

/// The native tools the prompt builder advertises as the core surface (its
/// core-tool taxonomy block is rendered from this list).
///
/// Deferral is **not** derived from this list — it is declared per tool
/// ([`crate::tools::spec::ToolSpec::defer_loading`], propagated into the
/// catalog by the registry): a tool is withheld from the request only because
/// its own definition says so, so an undeclared tool is resident and this list
/// going stale cannot silently hide anything. `[tools] always_load` overrides a
/// declaration; the YOLO mode override is MCP-only.
///
/// Built from the `*_NAME` constants above so the list and the tool
/// construction/`name()` sites share one spelling — a bare literal that drifts
/// would advertise a tool the model does not have, or miss one it does.
pub const DEFAULT_ACTIVE_NATIVE_TOOLS: &[&str] = &[
    AGENT_CLOSE_NAME,
    AGENT_EVAL_NAME,
    AGENT_OPEN_NAME,
    APPLY_PATCH_NAME,
    CHECKLIST_WRITE_NAME,
    EDIT_FILE_NAME,
    EXEC_INTERACT_NAME,
    EXEC_SHELL_NAME,
    EXEC_SHELL_INTERACT_NAME,
    EXEC_SHELL_WAIT_NAME,
    EXEC_WAIT_NAME,
    FETCH_URL_NAME,
    FILE_SEARCH_NAME,
    FIND_REFERENCES_NAME,
    GIT_DIFF_NAME,
    GIT_STATUS_NAME,
    GREP_FILES_NAME,
    LIST_DIR_NAME,
    READ_FILE_NAME,
    RUN_TESTS_NAME,
    SYMBOL_SEARCH_NAME,
    TASK_CREATE_NAME,
    TASK_LIST_NAME,
    TASK_READ_NAME,
    TASK_SHELL_START_NAME,
    TASK_SHELL_WAIT_NAME,
    UPDATE_PLAN_NAME,
    WEB_SEARCH_NAME,
    WRITE_FILE_NAME,
];

/// Accessor kept as a function (rather than importing the constant
/// directly) to mirror the original engine-body API and leave room for
/// the list to become mode- or config-aware without churning call sites.
#[must_use]
pub fn default_active_native_tool_names() -> &'static [&'static str] {
    DEFAULT_ACTIVE_NATIVE_TOOLS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_active_native_tools_are_sorted_and_unique() {
        assert!(
            DEFAULT_ACTIVE_NATIVE_TOOLS.windows(2).all(|w| w[0] < w[1]),
            "catalog must stay sorted (prefix-cache stability) and duplicate-free"
        );
    }

    /// The deferral rule's shape, at the level this crate can see it.
    ///
    /// The declarations themselves are pinned over the real registry in
    /// `codesmith-cli` (`deferred_tools_are_exactly_the_reviewed_niche_set`) —
    /// that test is the review artifact for *which* tools opt in. Here: the two
    /// overrides above a declaration, and the control tool's infrastructure
    /// status.
    #[test]
    fn deferral_overrides_and_control_tool_status() {
        use std::collections::HashSet;

        let tool = |name: &str, deferred: Option<bool>| crate::models::Tool {
            tool_type: Some("custom".to_string()),
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
            output_schema: None,
            allowed_callers: None,
            defer_loading: deferred,
            input_examples: None,
            strict: None,
            cache_control: None,
        };

        // `[tools] always_load` beats a declaration; others keep theirs.
        let mut catalog = vec![
            tool("niche_tool", Some(true)),
            tool("other_niche_tool", Some(true)),
            tool("read_file", None),
        ];
        let always_load = HashSet::from(["niche_tool".to_string()]);
        crate::engine::apply_native_deferral_overrides(&mut catalog, &always_load);
        assert_eq!(catalog[0].defer_loading, Some(false), "always_load wins");
        assert_eq!(catalog[1].defer_loading, Some(true), "declaration stands");
        assert_eq!(catalog[2].defer_loading, None, "undeclared stays resident");

        // YOLO sends the MCP surface resident; every other mode keeps the
        // adapter's declaration.
        let mut mcp = vec![tool("mcp__srv__act", Some(true))];
        crate::engine::apply_mcp_tool_deferral(&mut mcp, crate::mode::AppMode::Yolo);
        assert_eq!(
            mcp[0].defer_loading,
            Some(false),
            "YOLO clears MCP deferral"
        );
        let mut mcp = vec![tool("mcp__srv__act", Some(true))];
        crate::engine::apply_mcp_tool_deferral(&mut mcp, crate::mode::AppMode::Agent);
        assert_eq!(mcp[0].defer_loading, Some(true), "Agent keeps it");

        // The search control tool is dispatch infrastructure: tool selection
        // never filters it and the active-set seeding always keeps it.
        assert!(crate::engine::is_tool_search_tool(
            crate::engine::TOOL_SEARCH_NAME
        ));
    }
}
