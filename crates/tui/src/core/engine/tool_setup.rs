//! Per-turn tool registry setup.
//!
//! This keeps mode/feature-specific registry construction out of the send path.

use std::path::Path;

use super::*;
use crate::sandbox::SandboxPolicy;

/// Pick the sandbox policy that gates shell commands for a given UI mode.
///
/// - **Plan** (#1077): `ReadOnly` — no writes, no network. The previous
///   `WorkspaceWrite` policy let `python -c "open('f','w').write('x')"` mutate
///   files inside the workspace because it whitelisted the workspace as
///   writable. Plan mode is investigation only; if the user wants to change
///   files they should switch to Agent.
/// - **Coordinator**: `ReadOnly` — same rationale as Plan; the coordinator
///   cannot directly execute or write, so no sandbox writes needed.
/// - **Agent**: `WorkspaceWrite` with workspace as writable root and network
///   on. Approval flow gates risky individual commands; the sandbox handles
///   the rest. Network is allowed because cargo / npm / curl-style commands
///   are normal during agent work and DNS-deny breaks them silently.
/// - **YOLO**: `DangerFullAccess` — explicit no-guardrails contract.
pub(crate) fn sandbox_policy_for_mode(mode: AppMode, workspace: &Path) -> SandboxPolicy {
    match mode {
        AppMode::Plan | AppMode::Coordinator => SandboxPolicy::ReadOnly,
        AppMode::Agent => SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![workspace.to_path_buf()],
            network_access: true,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        },
        AppMode::Yolo => SandboxPolicy::DangerFullAccess,
    }
}

impl Engine {
    pub(super) fn build_turn_tool_registry_builder(
        &self,
        mode: AppMode,
        todo_list: SharedTodoList,
        plan_state: SharedPlanState,
    ) -> ToolRegistryBuilder {
        let mut builder = if mode == AppMode::Plan {
            ToolRegistryBuilder::new()
                .with_read_only_file_tools()
                .with_search_tools()
                .with_git_tools()
                .with_git_history_tools()
                .with_diagnostics_tool()
                .with_skill_tools()
                .with_validation_tools()
                .with_handle_tools()
                .with_runtime_read_only_task_tools()
                .with_todo_tool(todo_list)
                .with_plan_tool(plan_state)
                .with_plan_mode_tools_read_only(
                    self.config.plan_mode_state.clone(),
                    self.config.plan_state.clone(),
                )
                // write_plan_file is the sole writable tool in plan mode
                .with_tool(std::sync::Arc::new(
                    crate::tools::plan_mode::WritePlanFileTool::new(
                        self.config.plan_mode_state.clone(),
                        self.config.plan_state.clone(),
                    ),
                ))
                .with_task_v2_read_only_tools_if_available(
                    self.config.task_v2_manager.clone(),
                )
                .with_goal_tools(self.config.goal_state.clone())
        } else if mode == AppMode::Coordinator {
            // Coordinator mode: empty builder. Subagent + send_message tools
            // are added in engine.rs. User_input + recall_archive are added
            // in the common section below. Review, parallel, rlm, fim, patch,
            // web, slop_ledger are all excluded — coordinator only orchestrates.
            ToolRegistryBuilder::new()
        } else {
            ToolRegistryBuilder::new()
                .with_agent_tools(self.session.allow_shell)
                .with_todo_tool(todo_list)
                .with_plan_tool(plan_state)
                .with_plan_mode_tools(
                    self.config.plan_mode_state.clone(),
                    self.config.plan_state.clone(),
                )
                .with_task_v2_tools_if_available(
                    self.config.task_v2_manager.clone(),
                )
                .with_goal_tools(self.config.goal_state.clone())
                .with_worktree_tools(self.config.worktree_state.clone())
        };

        // Review + parallel are NOT added for coordinator — it delegates
        // all work to workers and doesn't need review/parallel capabilities.
        if mode != AppMode::Coordinator {
            builder = builder
                .with_review_tool(self.llm_client.clone(), self.session.model.clone())
                .with_parallel_tool();
        }

        // User input and recall archive are needed for all modes.
        builder = builder
            .with_user_input_tool()
            .with_recall_archive_tool();

        // SlopLedger: plan gets read-only, agent/yolo get the full set.
        // Coordinator skips slop_ledger entirely — it has no direct tools.
        if mode == AppMode::Plan {
            builder = builder.with_slop_ledger_read_only_tools();
        } else if mode != AppMode::Coordinator {
            builder = builder.with_slop_ledger_tools();
        }

        if mode != AppMode::Plan && mode != AppMode::Coordinator {
            builder = builder
                .with_rlm_tool(self.llm_client.clone(), self.session.model.clone())
                .with_fim_tool(self.llm_client.clone(), self.session.model.clone());
        }

        if self.config.features.enabled(Feature::ApplyPatch)
            && mode != AppMode::Plan && mode != AppMode::Coordinator
        {
            builder = builder.with_patch_tools();
        }
        if self.config.features.enabled(Feature::WebSearch)
            && mode != AppMode::Coordinator
        {
            builder = builder.with_web_tools();
        }
        // Shell tools (exec_shell, task_shell_start, etc.) are already gated
        // behind `allow_shell` inside `with_agent_tools`. No separate
        // feature-flag gate here to avoid double-registration.

        // Register the `remember` tool only when the user has opted in to
        // user-memory (#489). Without that opt-in the tool would always
        // fail; surfacing it would just waste catalog slots.
        if self.config.memory_enabled {
            builder = builder.with_remember_tool();
        }

        // Register KoD knowledge tools when Knowledge On Demand is enabled.
        if self.config.kod_enabled {
            builder = builder.with_knowledge_tools();
        }

        // Register Agent Teams tools when the feature is enabled.
        // Coordinator mode gets send_message only (added in engine.rs),
        // so skip the full team_tools here for coordinator.
        if self.config.features.enabled(Feature::AgentTeams)
            && mode != AppMode::Coordinator
        {
            builder = builder.with_team_tools_if_available(
                self.config.team_context.clone(),
            );
        }

        // Register image_analyze tool when vision_model is configured and feature enabled.
        if self.config.features.enabled(Feature::VisionModel)
            && let Some(ref vision_config) = self.config.vision_config
        {
            builder = builder.with_vision_tools(vision_config.clone());
        }

        // Register the `notify` tool unconditionally (#1322). It has no
        // side effects beyond a single terminal escape write and respects
        // the user's `[notifications].method` config (including `off`),
        // so there's no failure mode worth gating on.
        builder = builder.with_notify_tool();

        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sandbox_policy_for_mode_plan_is_readonly() {
        let policy = sandbox_policy_for_mode(AppMode::Plan, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::ReadOnly));
    }

    #[test]
    fn sandbox_policy_for_mode_coordinator_is_readonly() {
        let policy = sandbox_policy_for_mode(AppMode::Coordinator, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::ReadOnly));
    }

    #[test]
    fn sandbox_policy_for_mode_agent_is_workspace_write() {
        let policy = sandbox_policy_for_mode(AppMode::Agent, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::WorkspaceWrite { .. }));
        if let SandboxPolicy::WorkspaceWrite { writable_roots, network_access, .. } = policy {
            assert_eq!(writable_roots, vec![PathBuf::from("/repo")]);
            assert!(network_access);
        }
    }

    #[test]
    fn sandbox_policy_for_mode_yolo_is_danger_full_access() {
        let policy = sandbox_policy_for_mode(AppMode::Yolo, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::DangerFullAccess));
    }
}