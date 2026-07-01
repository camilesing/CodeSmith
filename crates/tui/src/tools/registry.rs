//! Tool registry for managing and executing tools.
//!
//! This module is a thin shim over
//! [`codesmith_agent_runtime::tools::registry`]: the portable `ToolRegistry`
//! core, its portable methods, and the fail-closed construction helpers were
//! physically migrated there (orphan rule: `ToolRegistry`'s inherent impls
//! must live in the defining crate). What remains here is the TUI-coupled
//! surface that the agent-runtime crate must not depend on:
//!
//! - `ToolRegistryBuilder` — wires concrete tool impls and an `LlmClientHandle`.
//! - `McpToolAdapter` — adapts `crate::mcp` tools to `ToolSpec`.
//! - `ToolRegistryPluginExt` — `apply_overrides` / `load_plugins`, which reach
//!   into `crate::config::ToolOverride` and `crate::tools::plugin`.
//! - The unit tests.

use std::collections::HashMap;
use std::sync::Arc;

use std::path::Path;

use serde_json::Value;

use crate::llm_client::LlmClientHandle;

use super::spec::{ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec};

pub use codesmith_agent_runtime::tools::registry::{
    MAX_TOOL_NAME_LEN, ToolRegistry, is_valid_tool_name, sanitize_tool_name,
};

/// TUI-coupled plugin/override operations on a [`ToolRegistry`].
///
/// `apply_overrides` and `load_plugins` reach into `crate::config::ToolOverride`
/// and `crate::tools::plugin`, which are TUI-only concerns, so they live behind
/// an extension trait rather than on the agent-runtime core type (whose crate
/// must not depend on TUI config/plugin loading). Bring the trait into scope to
/// call the methods: `use crate::tools::ToolRegistryPluginExt;`.
pub trait ToolRegistryPluginExt {
    /// Apply config.toml tool overrides to this registry.
    ///
    /// For each entry in `overrides`:
    /// - `Disabled` removes the tool.
    /// - `Script` / `Command` replaces the tool with the user's implementation.
    ///
    /// `plugin_dir` is used as the base for relative script paths.
    fn apply_overrides(
        &mut self,
        overrides: &HashMap<String, crate::config::ToolOverride>,
        plugin_dir: &Path,
    );

    /// Load and register plugin tools from a directory.
    ///
    /// Each script with valid frontmatter (`# name:`, `# description:`, etc.)
    /// becomes a registered `ScriptPluginTool`. Tools whose name matches an
    /// already-registered tool will overwrite it.
    fn load_plugins(&mut self, plugin_dir: &Path);
}

impl ToolRegistryPluginExt for ToolRegistry {
    fn apply_overrides(
        &mut self,
        overrides: &std::collections::HashMap<String, crate::config::ToolOverride>,
        plugin_dir: &Path,
    ) {
        for (tool_name, override_cfg) in overrides {
            match override_cfg {
                crate::config::ToolOverride::Disabled => {
                    if self.remove_tool(tool_name) {
                        tracing::info!("Tool '{}' disabled via config override", tool_name);
                    } else {
                        tracing::warn!("Cannot disable tool '{}': not registered", tool_name);
                    }
                }
                _ => {
                    // Script and Command overrides create replacement tools.
                    use crate::tools::plugin::tool_from_override;
                    match tool_from_override(tool_name, override_cfg, plugin_dir) {
                        Some(replacement) => {
                            self.register(replacement);
                            tracing::info!("Tool '{}' replaced via config override", tool_name);
                        }
                        None => {
                            if self.remove_tool(tool_name) {
                                tracing::warn!(
                                    "Tool '{}' override did not create a replacement; removed the original tool to avoid override fallthrough",
                                    tool_name
                                );
                            } else {
                                tracing::warn!(
                                    "Tool '{}' override did not create a replacement and no registered tool existed",
                                    tool_name
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn load_plugins(&mut self, plugin_dir: &Path) {
        if !plugin_dir.exists() {
            tracing::debug!(
                "Plugin directory {} does not exist, skipping",
                plugin_dir.display()
            );
            return;
        }
        let plugins = crate::tools::plugin::load_plugin_tools(plugin_dir);
        let count = plugins.len();
        for tool in plugins {
            self.register(tool);
        }
        if count > 0 {
            tracing::info!(
                "Loaded {count} plugin tool(s) from {}",
                plugin_dir.display()
            );
        }
    }
}

/// Builder for constructing a `ToolRegistry` with common tools.
pub struct ToolRegistryBuilder {
    tools: Vec<Arc<dyn ToolSpec>>,
}

impl ToolRegistryBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Add a custom tool.
    #[must_use]
    pub fn with_tool(mut self, tool: Arc<dyn ToolSpec>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Include file tools (read, write, edit, list).
    #[must_use]
    pub fn with_file_tools(self) -> Self {
        use super::file::{EditFileTool, ListDirTool, ReadFileTool, WriteFileTool};
        self.with_tool(Arc::new(ReadFileTool))
            .with_tool(Arc::new(WriteFileTool))
            .with_tool(Arc::new(EditFileTool))
            .with_tool(Arc::new(ListDirTool))
    }

    /// Include scoped Agent Memory tools. These are constrained by
    /// `ToolContext.agent_memory_dir` and do not grant general workspace writes.
    #[must_use]
    pub fn with_agent_memory_tools(self) -> Self {
        use super::agent_memory::{AgentMemoryEditTool, AgentMemoryReadTool, AgentMemoryWriteTool};
        self.with_tool(Arc::new(AgentMemoryReadTool))
            .with_tool(Arc::new(AgentMemoryWriteTool))
            .with_tool(Arc::new(AgentMemoryEditTool))
    }

    /// Include only read-only file tools (read, list).
    #[must_use]
    pub fn with_read_only_file_tools(self) -> Self {
        use super::file::{ListDirTool, ReadFileTool};
        self.with_tool(Arc::new(ReadFileTool))
            .with_tool(Arc::new(ListDirTool))
            .with_tool(Arc::new(
                super::tool_result_retrieval::RetrieveToolResultTool,
            ))
    }

    /// Include shell execution tool.
    #[must_use]
    pub fn with_shell_tools(self) -> Self {
        use super::shell::{ExecShellTool, ShellCancelTool, ShellInteractTool, ShellWaitTool};
        self.with_tool(Arc::new(ExecShellTool))
            .with_tool(Arc::new(ShellWaitTool::new("exec_shell_wait")))
            .with_tool(Arc::new(ShellInteractTool::new("exec_shell_interact")))
            .with_tool(Arc::new(ShellCancelTool))
            .with_tool(Arc::new(ShellWaitTool::new("exec_wait")))
            .with_tool(Arc::new(ShellInteractTool::new("exec_interact")))
    }

    /// Include search tools (`grep_files`).
    #[must_use]
    pub fn with_search_tools(self) -> Self {
        use super::file_search::FileSearchTool;
        use super::search::GrepFilesTool;
        self.with_tool(Arc::new(GrepFilesTool))
            .with_tool(Arc::new(FileSearchTool))
    }

    /// Include git inspection tools (`git_status`, `git_diff`).
    #[must_use]
    pub fn with_git_tools(self) -> Self {
        use super::git::{GitDiffTool, GitStatusTool};
        self.with_tool(Arc::new(GitStatusTool))
            .with_tool(Arc::new(GitDiffTool))
    }

    /// Include git history tools (`git_log`, `git_show`, `git_blame`).
    #[must_use]
    pub fn with_git_history_tools(self) -> Self {
        use super::git_history::{GitBlameTool, GitLogTool, GitShowTool};
        self.with_tool(Arc::new(GitLogTool))
            .with_tool(Arc::new(GitShowTool))
            .with_tool(Arc::new(GitBlameTool))
    }

    /// Include workspace diagnostics tool.
    #[must_use]
    pub fn with_diagnostics_tool(self) -> Self {
        use super::diagnostics::DiagnosticsTool;
        self.with_tool(Arc::new(DiagnosticsTool))
    }

    /// Include the `pandoc_convert` tool only when the `pandoc`
    /// binary is present on this host. Same probe-then-decide
    /// pattern v0.8.31 introduced for Python — when pandoc is
    /// missing the tool is not registered, so the model never
    /// sees a binary it can't actually use.
    #[must_use]
    pub fn with_pandoc_tools(self) -> Self {
        if crate::dependencies::resolve_pandoc().is_some() {
            use super::pandoc::PandocConvertTool;
            self.with_tool(Arc::new(PandocConvertTool))
        } else {
            self
        }
    }

    /// Include the `image_ocr` tool only when a local OCR backend is present.
    /// macOS uses the built-in Vision framework, while other platforms use
    /// Tesseract when installed.
    #[must_use]
    pub fn with_image_ocr_tools(self) -> Self {
        if super::image_ocr::ocr_available() {
            use super::image_ocr::ImageOcrTool;
            self.with_tool(Arc::new(ImageOcrTool))
        } else {
            self
        }
    }

    /// Include the `load_skill` tool (#434) so the model can pull a
    /// SKILL.md body + companion file list into context with one
    /// call instead of `read_file` + `list_dir` against the path
    /// shown in the system prompt's `## Skills` section.
    #[must_use]
    pub fn with_skill_tools(self) -> Self {
        use super::skill::LoadSkillTool;
        self.with_tool(Arc::new(LoadSkillTool))
    }

    /// Include project mapping tools.
    #[must_use]
    pub fn with_project_tools(self) -> Self {
        use super::project::ProjectMapTool;
        self.with_tool(Arc::new(ProjectMapTool))
    }

    /// Include cargo test runner tool.
    #[must_use]
    pub fn with_test_runner_tool(self) -> Self {
        use super::test_runner::RunTestsTool;
        self.with_tool(Arc::new(RunTestsTool))
    }

    /// Include structured data validation tool (`validate_data`).
    #[must_use]
    pub fn with_validation_tools(self) -> Self {
        use super::validate_data::ValidateDataTool;
        self.with_tool(Arc::new(ValidateDataTool))
    }

    /// Include retrieval for spilled historical tool results.
    #[must_use]
    pub fn with_tool_result_retrieval_tool(self) -> Self {
        use super::tool_result_retrieval::RetrieveToolResultTool;
        self.with_tool(Arc::new(RetrieveToolResultTool))
    }

    /// Include durable task, gate, PR-attempt, GitHub, and automation tools.
    ///
    /// Shell-related task tools (`task_shell_start`, `task_shell_wait`) are
    /// *not* included here — use [`with_runtime_task_shell_tools`] to register
    /// them when `allow_shell` is true.
    #[must_use]
    pub fn with_runtime_task_tools(self) -> Self {
        use super::automation::{
            AutomationCreateTool, AutomationDeleteTool, AutomationListTool, AutomationPauseTool,
            AutomationReadTool, AutomationResumeTool, AutomationRunTool, AutomationUpdateTool,
        };
        use super::github::{
            GithubCloseIssueTool, GithubClosePrTool, GithubCommentTool, GithubIssueContextTool,
            GithubPrContextTool,
        };
        use super::tasks::{
            PrAttemptListTool, PrAttemptPreflightTool, PrAttemptReadTool, PrAttemptRecordTool,
            TaskCancelTool, TaskCreateTool, TaskGateRunTool, TaskListTool, TaskReadTool,
            TaskStopTool,
        };

        self.with_tool(Arc::new(TaskCreateTool))
            .with_tool(Arc::new(TaskListTool))
            .with_tool(Arc::new(TaskReadTool))
            .with_tool(Arc::new(TaskCancelTool))
            .with_tool(Arc::new(TaskStopTool))
            .with_tool(Arc::new(TaskGateRunTool))
            .with_tool(Arc::new(GithubIssueContextTool))
            .with_tool(Arc::new(GithubPrContextTool))
            .with_tool(Arc::new(PrAttemptRecordTool))
            .with_tool(Arc::new(PrAttemptListTool))
            .with_tool(Arc::new(PrAttemptReadTool))
            .with_tool(Arc::new(PrAttemptPreflightTool))
            .with_tool(Arc::new(AutomationCreateTool))
            .with_tool(Arc::new(AutomationListTool))
            .with_tool(Arc::new(AutomationReadTool))
            .with_tool(Arc::new(AutomationUpdateTool))
            .with_tool(Arc::new(AutomationPauseTool))
            .with_tool(Arc::new(AutomationResumeTool))
            .with_tool(Arc::new(AutomationDeleteTool))
            .with_tool(Arc::new(AutomationRunTool))
            .with_tool(Arc::new(GithubCommentTool))
            .with_tool(Arc::new(GithubCloseIssueTool))
            .with_tool(Arc::new(GithubClosePrTool))
    }

    /// Include the unified stop tool for background tasks, workers, teammates,
    /// shell jobs, and durable tasks.
    #[must_use]
    pub fn with_task_stop_tool(self) -> Self {
        use super::tasks::TaskStopTool;
        self.with_tool(Arc::new(TaskStopTool))
    }

    /// Include shell-related task tools (`task_shell_start`, `task_shell_wait`).
    ///
    /// These are gated behind `allow_shell` because `task_shell_start`
    /// delegates directly to `ExecShellTool`, providing the same shell
    /// execution capability as `exec_shell`.
    #[must_use]
    pub fn with_runtime_task_shell_tools(self) -> Self {
        use super::tasks::{TaskShellStartTool, TaskShellWaitTool};
        self.with_tool(Arc::new(TaskShellStartTool))
            .with_tool(Arc::new(TaskShellWaitTool))
    }

    /// Include only read-only durable task, PR-attempt, GitHub, and automation
    /// inspection tools. Plan mode uses this surface so it can observe state
    /// without starting work, changing remotes, or mutating automation config.
    #[must_use]
    pub fn with_runtime_read_only_task_tools(self) -> Self {
        use super::automation::{AutomationListTool, AutomationReadTool};
        use super::github::{GithubIssueContextTool, GithubPrContextTool};
        use super::tasks::{PrAttemptListTool, PrAttemptReadTool, TaskListTool, TaskReadTool};

        self.with_tool(Arc::new(TaskListTool))
            .with_tool(Arc::new(TaskReadTool))
            .with_tool(Arc::new(GithubIssueContextTool))
            .with_tool(Arc::new(GithubPrContextTool))
            .with_tool(Arc::new(PrAttemptListTool))
            .with_tool(Arc::new(PrAttemptReadTool))
            .with_tool(Arc::new(AutomationListTool))
            .with_tool(Arc::new(AutomationReadTool))
    }

    /// Include web search tools.
    #[must_use]
    pub fn with_web_tools(self) -> Self {
        use super::fetch_url::FetchUrlTool;
        use super::finance::FinanceTool;
        use super::web_run::WebRunTool;
        use super::web_search::WebSearchTool;
        self.with_tool(Arc::new(WebSearchTool))
            .with_tool(Arc::new(FetchUrlTool))
            .with_tool(Arc::new(FinanceTool::new()))
            .with_tool(Arc::new(WebRunTool))
    }

    /// Register the `image_analyze` vision tool.
    /// Only registered when `[vision_model]` is configured in config.toml.
    #[must_use]
    pub fn with_vision_tools(self, config: crate::config::VisionModelConfig) -> Self {
        use crate::vision::tools::ImageAnalyzeTool;
        self.with_tool(Arc::new(ImageAnalyzeTool::new(config)))
    }

    /// Previously registered the OpenAI-style `multi_tool_use.parallel`
    /// meta-tool. DeepSeek-V4 has native parallel tool calls (multiple
    /// `tool_calls` entries in one assistant turn) and the meta-tool name
    /// triggered the model to hallucinate OpenAI-internal XML wrappers
    /// (`<multi_tool_use.parallel><tool_name>…</tool_name>…`) instead of
    /// emitting native calls. Kept as a no-op so existing callers compile;
    /// the engine's compatibility dispatcher still handles legacy emissions.
    #[must_use]
    pub fn with_parallel_tool(self) -> Self {
        self
    }

    /// Include request_user_input tool.
    #[must_use]
    pub fn with_user_input_tool(self) -> Self {
        use super::user_input::RequestUserInputTool;
        self.with_tool(Arc::new(RequestUserInputTool))
    }

    /// Include patch tools (`apply_patch`).
    #[must_use]
    pub fn with_patch_tools(self) -> Self {
        use super::apply_patch::ApplyPatchTool;
        self.with_tool(Arc::new(ApplyPatchTool))
    }

    /// Include the `revert_turn` tool. Approval-gated since it mutates
    /// the workspace; the model uses it when the user asks to "undo my
    /// last edit". Backed by the per-workspace snapshot side-repo
    /// (`crate::snapshot`).
    #[must_use]
    pub fn with_revert_turn_tool(self) -> Self {
        use super::revert_turn::RevertTurnTool;
        self.with_tool(Arc::new(RevertTurnTool))
    }

    /// Include persistent RLM session tools.
    #[must_use]
    pub fn with_rlm_tool(self, client: Option<LlmClientHandle>, _root_model: String) -> Self {
        use super::rlm::{
            RlmCloseTool, RlmConfigureTool, RlmEvalTool, RlmOpenTool, RlmSessionObjectsTool,
        };
        self.with_tool(Arc::new(RlmSessionObjectsTool))
            .with_tool(Arc::new(RlmOpenTool))
            .with_tool(Arc::new(RlmEvalTool::new(client)))
            .with_tool(Arc::new(RlmConfigureTool))
            .with_tool(Arc::new(RlmCloseTool))
    }

    /// Include `handle_read`, the bounded projection reader for symbolic
    /// `var_handle` payloads.
    #[must_use]
    pub fn with_handle_tools(self) -> Self {
        use super::handle::HandleReadTool;
        self.with_tool(Arc::new(HandleReadTool))
    }

    /// Include the review tool.
    #[must_use]
    pub fn with_review_tool(self, client: Option<LlmClientHandle>, model: String) -> Self {
        use super::review::ReviewTool;
        self.with_tool(Arc::new(ReviewTool::new(client, model)))
    }

    /// Include the `recall_archive` tool — searches prior cycle archives
    /// produced by the checkpoint-restart system (issue #127).
    #[must_use]
    pub fn with_recall_archive_tool(self) -> Self {
        use super::recall_archive::RecallArchiveTool;
        self.with_tool(Arc::new(RecallArchiveTool))
    }

    /// Include note tool.
    #[must_use]
    pub fn with_note_tool(self) -> Self {
        use super::shell::NoteTool;
        self.with_tool(Arc::new(NoteTool))
    }

    /// Include the FIM (Fill-in-the-Middle) edit tool.
    #[must_use]
    pub fn with_fim_tool(self, client: Option<LlmClientHandle>, model: String) -> Self {
        use super::fim::FimEditTool;
        self.with_tool(Arc::new(FimEditTool::new(client, model)))
    }

    /// Include the `remember` tool — model-callable bullet-add into the
    /// user memory file (#489). Only register when the user has opted
    /// in to the memory feature; without that, the tool would surface
    /// in the model's catalog but always fail with "memory disabled".
    #[must_use]
    pub fn with_remember_tool(self) -> Self {
        use super::remember::RememberTool;
        self.with_tool(Arc::new(RememberTool))
    }

    /// Include KoD knowledge tools — `knowledge_recall` for explicit
    /// memory search. Only register when KoD is enabled.
    #[must_use]
    pub fn with_knowledge_tools(self) -> Self {
        use super::knowledge_recall::KnowledgeRetrievalTool;
        self.with_tool(Arc::new(KnowledgeRetrievalTool))
    }

    /// Include the slop ledger tools (#2127) — durable tracking of
    /// unresolved architectural residue: append, query, update, export.
    /// Registered unconditionally; the ledger JSON file is auto-created
    /// on first append.
    #[must_use]
    pub fn with_slop_ledger_tools(self) -> Self {
        use crate::slop_ledger::{
            SlopLedgerAppendTool, SlopLedgerExportTool, SlopLedgerQueryTool, SlopLedgerUpdateTool,
        };
        self.with_tool(Arc::new(SlopLedgerAppendTool))
            .with_tool(Arc::new(SlopLedgerQueryTool))
            .with_tool(Arc::new(SlopLedgerUpdateTool))
            .with_tool(Arc::new(SlopLedgerExportTool))
    }

    /// Read-only subset of slop ledger tools (#2127) for plan mode:
    /// only query and export — no append or update.
    #[must_use]
    pub fn with_slop_ledger_read_only_tools(self) -> Self {
        use crate::slop_ledger::{SlopLedgerExportTool, SlopLedgerQueryTool};
        self.with_tool(Arc::new(SlopLedgerQueryTool))
            .with_tool(Arc::new(SlopLedgerExportTool))
    }

    /// Include the `notify` tool — model-callable desktop notification
    /// (#1322). Routes through the existing `tui::notifications` OSC 9 /
    /// BEL pipeline so the user's `[notifications].method` config is
    /// honoured automatically (including `off`). Always safe to register
    /// because the tool has no side effects beyond a single terminal
    /// escape write.
    #[must_use]
    pub fn with_notify_tool(self) -> Self {
        use super::notify::NotifyTool;
        self.with_tool(Arc::new(NotifyTool))
    }

    /// Include MCP tools from a connected pool as first-class registry
    /// citizens. Each MCP tool is wrapped in a lightweight adapter that
    /// implements `ToolSpec`, so the unified `ToolRegistryBuilder` flow
    /// handles them alongside native tools.
    ///
    /// MCP tools are marked `defer_loading` by default (except discovery
    /// helpers) to keep the model-visible catalog compact.
    #[must_use]
    pub fn with_mcp_tools(
        mut self,
        mcp_pool: std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>,
    ) -> Self {
        // Snapshot the current tool list from the pool (non-blocking).
        // The adapter lazily resolves at execution time via the pool.
        if let Ok(pool) = mcp_pool.try_lock() {
            for (name, tool) in pool.all_tools() {
                let adapter = Arc::new(McpToolAdapter {
                    name: name.clone(),
                    description: crate::mcp::truncate_mcp_description(
                        tool.description.as_deref().unwrap_or(&name),
                    ),
                    tool: tool.clone(),
                    pool: mcp_pool.clone(),
                });
                self.tools.push(adapter);
            }
        }
        self
    }

    /// Include all agent tools (file tools + shell + note + search + patch).
    #[must_use]
    pub fn with_agent_tools(self, allow_shell: bool) -> Self {
        let builder = self
            .with_file_tools()
            .with_agent_memory_tools()
            .with_note_tool()
            .with_search_tools()
            .with_web_tools()
            .with_user_input_tool()
            .with_parallel_tool()
            .with_patch_tools()
            .with_git_tools()
            .with_git_history_tools()
            .with_diagnostics_tool()
            .with_project_tools()
            .with_skill_tools()
            .with_test_runner_tool()
            .with_validation_tools()
            .with_tool_result_retrieval_tool()
            .with_handle_tools()
            .with_runtime_task_tools()
            .with_revert_turn_tool()
            .with_pandoc_tools()
            .with_image_ocr_tools();

        if allow_shell {
            builder.with_shell_tools().with_runtime_task_shell_tools()
        } else {
            builder
        }
    }

    /// Include the full agent tool surface: every tool family the parent gets
    /// in Agent mode, including review, RLM, and the sub-agent management
    /// family (so children can recurse). Used by both the parent's Agent-mode
    /// registry build (`core/engine.rs`) and by every sub-agent
    /// (`subagent::SubAgentToolRegistry`) — keeping them in lockstep.
    ///
    /// `allow_shell` mirrors the session's shell permission. `manager` and
    /// `runtime` are the sub-agent runtime — children pass through their own
    /// runtime so grandchildren can spawn within the same depth/cancellation
    /// envelope.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_full_agent_surface(
        self,
        client: Option<LlmClientHandle>,
        model: String,
        manager: super::subagent::SharedSubAgentManager,
        runtime: super::subagent::SubAgentRuntime,
        allow_shell: bool,
        todo_list: super::todo::SharedTodoList,
        plan_state: super::plan::SharedPlanState,
    ) -> Self {
        self.with_agent_tools(allow_shell)
            .with_todo_tool(todo_list)
            .with_plan_tool(plan_state)
            .with_review_tool(client.clone(), model.clone())
            .with_rlm_tool(client, model)
            .with_recall_archive_tool()
            .with_subagent_tools(manager, runtime)
    }

    /// Include the todo tool with a shared `TodoList`.
    #[must_use]
    pub fn with_todo_tool(self, todo_list: super::todo::SharedTodoList) -> Self {
        use super::todo::{TodoAddTool, TodoListTool, TodoUpdateTool, TodoWriteTool};
        self.with_tool(Arc::new(TodoWriteTool::checklist(todo_list.clone())))
            .with_tool(Arc::new(TodoAddTool::checklist(todo_list.clone())))
            .with_tool(Arc::new(TodoUpdateTool::checklist(todo_list.clone())))
            .with_tool(Arc::new(TodoListTool::checklist(todo_list.clone())))
            .with_tool(Arc::new(TodoWriteTool::new(todo_list.clone())))
            .with_tool(Arc::new(TodoAddTool::new(todo_list.clone())))
            .with_tool(Arc::new(TodoUpdateTool::new(todo_list.clone())))
            .with_tool(Arc::new(TodoListTool::new(todo_list)))
    }

    /// Include the plan tool with a shared `PlanState`.
    #[must_use]
    pub fn with_plan_tool(self, plan_state: super::plan::SharedPlanState) -> Self {
        use super::plan::UpdatePlanTool;
        self.with_tool(Arc::new(UpdatePlanTool::new(plan_state)))
    }

    /// Include plan mode tools (enter_plan_mode, exit_plan_mode, write_plan_file).
    #[must_use]
    pub fn with_plan_mode_tools(
        self,
        plan_mode_state: super::plan_mode::SharedPlanModeState,
        plan_state: super::plan::SharedPlanState,
    ) -> Self {
        use super::plan_mode::{EnterPlanModeTool, ExitPlanModeTool, WritePlanFileTool};
        self.with_tool(Arc::new(EnterPlanModeTool::new(
            plan_mode_state.clone(),
            plan_state.clone(),
        )))
        .with_tool(Arc::new(ExitPlanModeTool::new(
            plan_mode_state.clone(),
            plan_state.clone(),
        )))
        .with_tool(Arc::new(WritePlanFileTool::new(
            plan_mode_state,
            plan_state,
        )))
    }

    /// Include plan mode tools in read-only subset for plan mode registry
    /// (enter_plan_mode and exit_plan_mode only — write_plan_file is added
    /// separately when plan mode is active).
    #[must_use]
    pub fn with_plan_mode_tools_read_only(
        self,
        plan_mode_state: super::plan_mode::SharedPlanModeState,
        plan_state: super::plan::SharedPlanState,
    ) -> Self {
        use super::plan_mode::{EnterPlanModeTool, ExitPlanModeTool};
        self.with_tool(Arc::new(EnterPlanModeTool::new(
            plan_mode_state.clone(),
            plan_state.clone(),
        )))
        .with_tool(Arc::new(ExitPlanModeTool::new(plan_mode_state, plan_state)))
    }

    /// Include Task V2 tools (task_create_v2, task_update_v2, task_get_v2, task_list_v2).
    #[must_use]
    pub fn with_task_v2_tools(self, manager: super::task_v2::SharedTaskV2Manager) -> Self {
        use super::task_v2::{TaskV2CreateTool, TaskV2GetTool, TaskV2ListTool, TaskV2UpdateTool};
        self.with_tool(Arc::new(TaskV2CreateTool::new(manager.clone())))
            .with_tool(Arc::new(TaskV2UpdateTool::new(manager.clone())))
            .with_tool(Arc::new(TaskV2GetTool::new(manager.clone())))
            .with_tool(Arc::new(TaskV2ListTool::new(manager)))
    }

    /// Include read-only Task V2 tools (task_get_v2, task_list_v2 only).
    #[must_use]
    pub fn with_task_v2_read_only_tools(
        self,
        manager: super::task_v2::SharedTaskV2Manager,
    ) -> Self {
        use super::task_v2::{TaskV2GetTool, TaskV2ListTool};
        self.with_tool(Arc::new(TaskV2GetTool::new(manager.clone())))
            .with_tool(Arc::new(TaskV2ListTool::new(manager)))
    }

    /// Include runtime goal tools (`create_goal`, `get_goal`, `update_goal`).
    #[must_use]
    pub fn with_goal_tools(self, goal_state: super::goal::SharedGoalState) -> Self {
        use super::goal::{CreateGoalTool, GetGoalTool, UpdateGoalTool};
        self.with_tool(Arc::new(CreateGoalTool::new(goal_state.clone())))
            .with_tool(Arc::new(GetGoalTool::new(goal_state.clone())))
            .with_tool(Arc::new(UpdateGoalTool::new(goal_state)))
    }

    /// Include worktree isolation tools (`enter_worktree`, `exit_worktree`).
    #[must_use]
    pub fn with_worktree_tools(
        self,
        worktree_state: super::worktree::SharedWorktreeSessionState,
    ) -> Self {
        use super::worktree::{EnterWorktreeTool, ExitWorktreeTool};
        self.with_tool(Arc::new(EnterWorktreeTool::new(worktree_state.clone())))
            .with_tool(Arc::new(ExitWorktreeTool::new(worktree_state)))
    }

    /// Include sub-agent management tools.
    #[must_use]
    pub fn with_subagent_tools(
        self,
        manager: super::subagent::SharedSubAgentManager,
        runtime: super::subagent::SubAgentRuntime,
    ) -> Self {
        use super::subagent::{
            AgentCloseTool, AgentEvalTool, AgentOpenTool, AgentSpawnTool, SubagentRunTool,
            ToolAgentTool,
        };

        self.with_tool(Arc::new(AgentOpenTool::new(
            manager.clone(),
            runtime.clone(),
        )))
        .with_tool(Arc::new(AgentSpawnTool::new(
            manager.clone(),
            runtime.clone(),
        )))
        .with_tool(Arc::new(AgentEvalTool::new(manager.clone())))
        .with_tool(Arc::new(ToolAgentTool::new(
            manager.clone(),
            runtime.clone(),
        )))
        .with_tool(Arc::new(SubagentRunTool::new(
            manager.clone(),
            runtime.clone(),
        )))
        .with_tool(Arc::new(AgentCloseTool::new(manager)))
    }

    /// Include Task V2 tools when a manager is available.
    /// Returns self unchanged if manager is None.
    #[must_use]
    pub fn with_task_v2_tools_if_available(
        self,
        manager: Option<super::task_v2::SharedTaskV2Manager>,
    ) -> Self {
        if let Some(m) = manager {
            self.with_task_v2_tools(m)
        } else {
            self
        }
    }

    /// Include read-only Task V2 tools when a manager is available.
    /// Returns self unchanged if manager is None.
    #[must_use]
    pub fn with_task_v2_read_only_tools_if_available(
        self,
        manager: Option<super::task_v2::SharedTaskV2Manager>,
    ) -> Self {
        if let Some(m) = manager {
            self.with_task_v2_read_only_tools(m)
        } else {
            self
        }
    }

    /// Include Agent Teams tools (team_create, team_delete, send_message).
    #[must_use]
    pub fn with_team_tools(self, team_context: super::team::SharedTeamContext) -> Self {
        use super::team::{SendMessageTool, TeamCreateTool, TeamDeleteTool};
        self.with_tool(Arc::new(TeamCreateTool::new(team_context.clone())))
            .with_tool(Arc::new(TeamDeleteTool::new(team_context.clone())))
            .with_tool(Arc::new(SendMessageTool::new(team_context)))
    }

    /// Include Agent Teams tools when team context is available.
    #[must_use]
    pub fn with_team_tools_if_available(
        self,
        team_context: Option<super::team::SharedTeamContext>,
    ) -> Self {
        if let Some(tc) = team_context {
            self.with_team_tools(tc)
        } else {
            self
        }
    }

    /// Include only the send_message tool from the team module.
    /// Used by coordinator mode, which needs messaging but not
    /// team_create/team_delete.
    #[must_use]
    pub fn with_send_message_tool(self, team_context: super::team::SharedTeamContext) -> Self {
        use super::team::SendMessageTool;
        self.with_tool(Arc::new(SendMessageTool::new(team_context)))
    }

    /// Build the registry with the given context.
    #[must_use]
    pub fn build(self, context: ToolContext) -> ToolRegistry {
        let mut registry = ToolRegistry::new(context);
        registry.register_all(self.tools);
        registry
    }
}

impl Default for ToolRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
struct McpToolAdapter {
    name: String,
    description: String,
    tool: crate::mcp::McpTool,
    pool: std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>,
}

#[async_trait::async_trait]
impl ToolSpec for McpToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.tool.input_schema.clone()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Conservatively treat MCP tools as requiring approval and
        // network access unless they're known discovery helpers.
        let name_lower = self.name.to_lowercase();
        if name_lower.contains("list_mcp")
            || name_lower.contains("read_mcp")
            || name_lower.contains("mcp_read")
            || name_lower.contains("mcp_get_prompt")
        {
            vec![ToolCapability::ReadOnly]
        } else {
            vec![ToolCapability::Network, ToolCapability::RequiresApproval]
        }
    }

    fn defer_loading(&self) -> bool {
        // Discovery helpers stay loaded; everything else is deferred.
        let keep_loaded = matches!(
            self.name.as_str(),
            "list_mcp_resources"
                | "list_mcp_resource_templates"
                | "mcp_read_resource"
                | "read_mcp_resource"
                | "mcp_get_prompt"
        );
        !keep_loaded
    }

    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        let mut pool = self.pool.lock().await;
        let result = pool
            .call_tool(&self.name, input)
            .await
            .map_err(|e| ToolError::execution_failed(format!("MCP tool failed: {e}")))?;
        let content = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
        Ok(ToolResult::success(content))
    }
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use crate::config::ToolOverride;
    use crate::tools::ToolRegistryBuilder;
    use crate::tools::spec::{
        ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
    };

    use super::{
        MAX_TOOL_NAME_LEN, ToolRegistry, ToolRegistryPluginExt, is_valid_tool_name,
        sanitize_tool_name,
    };

    /// A simple test tool for unit testing
    struct TestTool {
        name: String,
        description: String,
    }

    #[async_trait::async_trait]
    impl ToolSpec for TestTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                },
                "required": ["message"]
            })
        }

        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }

        async fn execute(
            &self,
            input: Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let message = required_str(&input, "message")?;
            Ok(ToolResult::success(format!("Echo: {message}")))
        }
    }

    fn make_test_tool(name: &str) -> Arc<TestTool> {
        Arc::new(TestTool {
            name: name.to_string(),
            description: "A test tool".to_string(),
        })
    }

    #[test]
    fn test_registry_register_and_get() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        let tool = make_test_tool("test_tool");
        registry.register(tool);

        assert!(registry.contains("test_tool"));
        assert!(!registry.contains("nonexistent"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn apply_overrides_removes_original_when_replacement_is_missing() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistryBuilder::new()
            .with_read_only_file_tools()
            .build(ctx);

        assert!(registry.contains("read_file"));
        assert!(registry.contains("list_dir"));

        let mut overrides = HashMap::new();
        overrides.insert(
            "read_file".to_string(),
            ToolOverride::Script {
                path: "missing-wrapper.sh".to_string(),
                args: None,
            },
        );

        registry.apply_overrides(&overrides, tmp.path());

        assert!(!registry.contains("read_file"));
        assert!(registry.contains("list_dir"));
    }

    #[test]
    fn test_registry_names() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("tool_a"));
        registry.register(make_test_tool("tool_b"));

        let names = registry.names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"tool_a"));
        assert!(names.contains(&"tool_b"));
    }

    #[test]
    fn test_registry_to_api_tools() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("my_tool"));

        let api_tools = registry.to_api_tools();
        assert_eq!(api_tools.len(), 1);
        assert_eq!(api_tools[0].name, "my_tool");
        assert_eq!(api_tools[0].description, "A test tool");
        assert!(api_tools[0].output_schema.is_some());
    }

    #[test]
    fn api_tools_with_cache_marks_last_tool_ephemeral() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("tool_a"));
        registry.register(make_test_tool("tool_b"));

        let api_tools = registry.to_api_tools_with_cache(true);
        assert_eq!(api_tools.len(), 2);
        assert!(api_tools[0].cache_control.is_none());
        assert_eq!(
            api_tools[1]
                .cache_control
                .as_ref()
                .map(|c| c.cache_type.as_str()),
            Some("ephemeral")
        );
    }

    /// Tool whose `description()` advances through a script of pre-built
    /// strings, one per call. Used to demonstrate that the api-tools cache
    /// pins the description bytes on first read instead of re-sampling them
    /// each turn (#263 follow-up; mirrors reference-cc's `getToolSchemaCache`).
    struct VaryingDescriptionTool {
        name: String,
        descriptions: Vec<String>,
        next: std::sync::atomic::AtomicUsize,
    }

    impl VaryingDescriptionTool {
        fn new(name: &str, descriptions: &[&str]) -> Self {
            Self {
                name: name.to_string(),
                descriptions: descriptions.iter().map(|s| (*s).to_string()).collect(),
                next: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolSpec for VaryingDescriptionTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            let idx = self
                .next
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .min(self.descriptions.len() - 1);
            &self.descriptions[idx]
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object", "properties": {}, "required": []})
        }

        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }

        async fn execute(
            &self,
            _input: Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success("ok".to_string()))
        }
    }

    #[test]
    fn to_api_tools_pins_description_bytes_across_calls() {
        // Regression for the cache-stability follow-up: an MCP adapter that
        // returns a different `description()` on reconnect (or any other
        // tool whose description isn't a `&'static str`) would otherwise
        // rewrite the catalog bytes mid-session and miss the prefix cache.
        // The registry pins the first call's value until it's mutated.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);
        registry.register(Arc::new(VaryingDescriptionTool::new(
            "varying",
            &["first description", "second description"],
        )));

        let first = registry.to_api_tools();
        let second = registry.to_api_tools();

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].description, "first description");
        assert_eq!(
            first, second,
            "api-tools catalog must be byte-identical across reads with no mutation in between"
        );
    }

    #[test]
    fn register_invalidates_api_tools_cache() {
        // Counter-test: when a real change happens (a new tool registers,
        // an existing one is removed, or `clear` is called), the cache must
        // be discarded so the next read reflects the live registry.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);
        registry.register(Arc::new(VaryingDescriptionTool::new(
            "varying",
            &["first description", "second description"],
        )));

        let before = registry.to_api_tools();
        assert_eq!(before.len(), 1);

        registry.register(make_test_tool("late_arrival"));

        let after = registry.to_api_tools();
        assert_eq!(after.len(), 2, "cache must rebuild after register");
        assert!(after.iter().any(|t| t.name == "varying"));
        assert!(after.iter().any(|t| t.name == "late_arrival"));
        // The varying tool's description advances on cache rebuild — the
        // first read above sampled `first description`; this rebuild samples
        // `second description`. The point is just that the bytes *can*
        // change after a real mutation, not that they always do.
        let varying_after = after
            .iter()
            .find(|t| t.name == "varying")
            .expect("varying tool present");
        assert_eq!(varying_after.description, "second description");
    }

    #[test]
    fn remove_and_clear_invalidate_api_tools_cache() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);
        registry.register(make_test_tool("alpha"));
        registry.register(make_test_tool("beta"));

        let before = registry.to_api_tools();
        assert_eq!(before.len(), 2);

        let _ = registry.remove("alpha");
        let after_remove = registry.to_api_tools();
        assert_eq!(after_remove.len(), 1);
        assert_eq!(after_remove[0].name, "beta");

        registry.clear();
        let after_clear = registry.to_api_tools();
        assert!(after_clear.is_empty(), "cache must clear with the registry");
    }

    #[test]
    fn to_api_tools_emits_alphabetical_order_regardless_of_registration_order() {
        // Regression for #263: HashMap iteration is non-deterministic across
        // process launches, which busts DeepSeek's KV prefix cache for every
        // cross-session resume. `to_api_tools` must emit by name regardless
        // of registration order so two consecutive calls (and two distinct
        // launches) produce byte-identical output.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let order_a = {
            let mut registry = ToolRegistry::new(ctx.clone());
            registry.register(make_test_tool("zebra"));
            registry.register(make_test_tool("alpha"));
            registry.register(make_test_tool("mango"));
            registry
                .to_api_tools()
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>()
        };

        let order_b = {
            let mut registry = ToolRegistry::new(ctx.clone());
            registry.register(make_test_tool("alpha"));
            registry.register(make_test_tool("mango"));
            registry.register(make_test_tool("zebra"));
            registry
                .to_api_tools()
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(order_a, vec!["alpha", "mango", "zebra"]);
        assert_eq!(order_a, order_b);
    }

    #[test]
    fn test_registry_remove() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("removable"));
        assert!(registry.contains("removable"));

        let _ = registry.remove("removable");
        assert!(!registry.contains("removable"));
    }

    #[test]
    fn test_registry_clear() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("tool1"));
        registry.register(make_test_tool("tool2"));
        assert_eq!(registry.len(), 2);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn test_registry_execute() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("echo"));

        let result = registry
            .execute("echo", json!({"message": "hello"}))
            .await
            .expect("execute");

        assert_eq!(result, "Echo: hello");
    }

    #[tokio::test]
    async fn test_registry_execute_unknown_tool() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let registry = ToolRegistry::new(ctx);

        let result = registry.execute("nonexistent", json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_basic() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let registry = ToolRegistryBuilder::new()
            .with_tool(make_test_tool("custom"))
            .build(ctx);

        assert!(registry.contains("custom"));
    }

    #[test]
    fn test_filter_by_capability() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("readonly_tool"));

        let readonly = registry.filter_by_capability(ToolCapability::ReadOnly);
        assert_eq!(readonly.len(), 1);

        let writes = registry.filter_by_capability(ToolCapability::WritesFiles);
        assert_eq!(writes.len(), 0);
    }

    #[test]
    fn test_read_only_tools() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(make_test_tool("reader"));

        let readonly = registry.read_only_tools();
        assert_eq!(readonly.len(), 1);
        assert_eq!(readonly[0].name(), "reader");
    }

    #[test]
    fn test_builder_with_web_tools_includes_finance() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let registry = ToolRegistryBuilder::new().with_web_tools().build(ctx);

        assert!(registry.contains("finance"));
    }

    #[test]
    fn test_builder_with_agent_tools_includes_finance() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let registry = ToolRegistryBuilder::new()
            .with_agent_tools(false)
            .build(ctx);

        assert!(registry.contains("finance"));
    }

    #[test]
    fn agent_tools_with_allow_shell_false_excludes_shell_tools() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let registry = ToolRegistryBuilder::new()
            .with_agent_tools(false)
            .build(ctx);

        assert!(
            !registry.contains("exec_shell"),
            "exec_shell should be excluded when allow_shell is false"
        );
        assert!(
            !registry.contains("task_shell_start"),
            "task_shell_start should be excluded when allow_shell is false"
        );
        assert!(
            !registry.contains("task_shell_wait"),
            "task_shell_wait should be excluded when allow_shell is false"
        );
    }

    #[test]
    fn agent_tools_with_allow_shell_true_includes_shell_tools() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let registry = ToolRegistryBuilder::new().with_agent_tools(true).build(ctx);

        assert!(
            registry.contains("exec_shell"),
            "exec_shell should be included when allow_shell is true"
        );
        assert!(
            registry.contains("task_shell_start"),
            "task_shell_start should be included when allow_shell is true"
        );
        assert!(
            registry.contains("task_shell_wait"),
            "task_shell_wait should be included when allow_shell is true"
        );
    }

    // === Fail-closed buildTool tests ===

    /// A configurable tool whose name/schema can be malformed, used to
    /// exercise the `build_tool` chokepoint.
    struct MalformedTool {
        name: String,
        schema: Value,
    }

    #[async_trait::async_trait]
    impl ToolSpec for MalformedTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "A malformed test tool"
        }
        fn input_schema(&self) -> Value {
            self.schema.clone()
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            _input: Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success("real tool ran"))
        }
    }

    #[test]
    fn build_tool_passes_valid_tool_through_unchanged() {
        // A well-formed tool must reach the registry untouched: it stays
        // executable and keeps its real schema.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(Arc::new(MalformedTool {
            name: "valid_name".to_string(),
            schema: json!({"type": "object", "properties": {}}),
        }));

        let tool = registry.get("valid_name").expect("valid tool registered");
        assert_eq!(
            tool.input_schema(),
            json!({"type": "object", "properties": {}}),
            "valid tool schema must not be replaced by the stub schema"
        );
    }

    #[test]
    fn build_tool_substitutes_stub_for_invalid_name() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        // A space in the name breaks the API tool-name contract; the stub
        // is keyed under the sanitised name instead.
        registry.register(Arc::new(MalformedTool {
            name: "bad name".to_string(),
            schema: json!({"type": "object", "properties": {}}),
        }));

        assert!(
            !registry.contains("bad name"),
            "original invalid name must not be reachable"
        );
        assert!(
            registry.contains("bad_name"),
            "sanitised name should key the fail-closed stub"
        );

        // The catalog stays API-legal: stub name has no whitespace.
        let api_tools = registry.to_api_tools();
        assert_eq!(api_tools.len(), 1);
        assert_eq!(api_tools[0].name, "bad_name");
        assert!(
            api_tools[0]
                .input_schema
                .get("type")
                .and_then(Value::as_str)
                == Some("object"),
            "stub must advertise an object schema"
        );
    }

    #[test]
    fn build_tool_substitutes_stub_for_non_object_schema() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(Arc::new(MalformedTool {
            name: "broken_schema".to_string(),
            schema: json!("not an object"),
        }));

        let api_tools = registry.to_api_tools();
        assert_eq!(api_tools.len(), 1);
        // The stub overrides the broken schema with a permissive object.
        assert!(
            api_tools[0].input_schema.is_object(),
            "stub schema must be an object even when the original was malformed"
        );
    }

    #[tokio::test]
    async fn fail_closed_tool_execute_returns_not_available() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let mut registry = ToolRegistry::new(ctx);

        registry.register(Arc::new(MalformedTool {
            name: "bad name".to_string(),
            schema: json!({"type": "object", "properties": {}}),
        }));

        let err = registry
            .execute_full("bad_name", json!({}))
            .await
            .expect_err("stub must refuse execution");
        assert!(
            matches!(err, ToolError::NotAvailable { .. }),
            "fail-closed stub should return NotAvailable, got: {err:?}"
        );
        assert!(
            err.to_string().contains("invalid tool name"),
            "error should carry the original failure reason"
        );
    }

    #[test]
    fn build_tool_validates_name_helper() {
        // Direct unit checks for the name contract.
        assert!(is_valid_tool_name("read_file"));
        assert!(is_valid_tool_name("read-file"));
        assert!(is_valid_tool_name("ReadFile123"));
        assert!(is_valid_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN)));
        assert!(!is_valid_tool_name(""));
        assert!(!is_valid_tool_name("bad name"));
        assert!(!is_valid_tool_name("bad/name"));
        assert!(!is_valid_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN + 1)));
    }

    #[test]
    fn sanitize_tool_name_collapses_invalid_chars() {
        assert_eq!(sanitize_tool_name("bad name"), "bad_name");
        assert_eq!(sanitize_tool_name("a/b@c"), "a_b_c");
        assert_eq!(sanitize_tool_name("  "), "__");
        assert_eq!(sanitize_tool_name(""), "fail_closed_tool");
        let long = sanitize_tool_name(&"x".repeat(MAX_TOOL_NAME_LEN + 100));
        assert_eq!(long.len(), MAX_TOOL_NAME_LEN);
    }
}
