//! Per-turn tool registry setup.
//!
//! This keeps mode/feature-specific registry construction out of the send path.

use std::sync::Arc;

use codesmith_agent_runtime::runtime_ui::RuntimeUi;

use super::*;
use crate::tools::ToolRegistryBuilder;
use crate::tools::plan::SharedPlanState;
use crate::tools::shell::wrap_shell_manager;
use crate::tools::todo::SharedTodoList;

// sandbox_policy_for_mode now lives in codesmith_agent_runtime::sandbox
// (moved with its tests in Phase C6-2). Re-exported here so the TUI
// construction fns below and historical call sites (engine.rs, ui.rs, and
// the engine test module) keep resolving verbatim.
pub(crate) use codesmith_agent_runtime::sandbox::sandbox_policy_for_mode;

/// Build a [`ToolContext`] from explicit inputs rather than an `&Engine`
/// borrow.
///
/// This is the host-side helper shared by the engine-body
/// `Engine::build_tool_context` (still used by `Op::SpawnSubAgent` and tests)
/// and the [`HostServices::build_turn_dispatcher`] factory. Extracting it as a
/// free function lets both call sites share one implementation while the
/// `Engine` struct + `impl Engine` blocks are still being migrated to
/// `codesmith-agent-runtime` — the factory lives on `EngineHost` and cannot
/// call `impl Engine` methods.
pub(super) fn build_tool_context_for(
    host: &EngineHost,
    session: &Session,
    config: &EngineConfig,
    mode: AppMode,
    auto_approve: bool,
    cancel_token: CancellationToken,
    runtime_ui: &Arc<dyn RuntimeUi>,
) -> ToolContext {
    // Load the per-workspace trusted-paths list (#29) on every tool-context
    // build. Cheap (a small JSON file) and always reflects the latest
    // `/trust add` / `/trust remove` mutations without an explicit cache
    // refresh hook.
    let trusted = crate::workspace_trust::WorkspaceTrust::load_for(&session.workspace);
    let mut trusted_external_paths = trusted.paths().to_vec();
    let clipboard_images_dir = runtime_ui.clipboard_images_dir(&session.workspace);
    if !trusted_external_paths
        .iter()
        .any(|path| path == &clipboard_images_dir)
    {
        trusted_external_paths.push(clipboard_images_dir);
    }
    let mut ctx = ToolContext::with_auto_approve(
        session.workspace.clone(),
        session.trust_mode,
        session.notes_path.clone(),
        session.mcp_config_path.clone(),
        mode == AppMode::Yolo || mode == AppMode::Coordinator || auto_approve,
    )
    .with_state_namespace(session.id.clone())
    .with_features(config.features.clone())
    .with_shell_manager(wrap_shell_manager(
        host.shell_manager
            .as_ref()
            .expect("shell_manager is set by new_impl before turn dispatch")
            .clone(),
    ))
    .with_runtime_services(host.runtime_services.clone())
    .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
        session.id.clone(),
        session.model.clone(),
        session.workspace.clone(),
        session.system_prompt.clone(),
        session.messages.clone(),
    ))
    .with_cancel_token(cancel_token.clone())
    .with_trusted_external_paths(trusted_external_paths);

    // Set effective cwd: if a worktree session is active, shift cwd
    // to the worktree path so relative paths resolve inside it.
    {
        let wt_state = config.worktree_state.lock().unwrap();
        if wt_state.active && wt_state.worktree_path.is_some() {
            ctx = ctx.with_cwd(wt_state.worktree_path.clone().unwrap());
        } else {
            ctx = ctx.with_cwd(session.cwd.clone());
        }
    }

    // Hand the user-memory path to tools so the model-callable
    // `remember` tool can append entries (#489). `None` when the
    // feature is disabled — tools short-circuit on that.
    if config.memory_enabled {
        ctx.memory_path = Some(config.memory_path.clone());
    }
    if config.kod_enabled {
        ctx.memory_dir = Some(config.memory_dir.clone());
    }

    if let Some(decider) = config.network_policy.as_ref() {
        ctx = ctx.with_network_policy(decider.clone());
    }

    // Wire the large-output router (#548). Only attaches when the
    // [workshop] config table is present; sub-agents don't inherit the
    // router (their ToolContext is built separately) to prevent recursive
    // routing of the synthesis call itself.
    if let Some(workshop_cfg) = config.workshop.as_ref()
        && let Some(vars_arc) = host.workshop_vars.as_ref()
    {
        let router =
            crate::tools::large_output_router::LargeOutputRouter::new(workshop_cfg.clone());
        ctx = ctx.with_large_output_router(router, vars_arc.clone());
    }

    // Wire the external sandbox backend (#516). exec_shell checks this
    // field and routes commands through the backend instead of spawning
    // a local process when it's set.
    if let Some(backend) = host.sandbox_backend.as_ref() {
        ctx = ctx.with_sandbox_backend(std::sync::Arc::clone(backend));
    }

    // Wire search provider config.
    ctx.search_provider = config.search_provider;
    ctx.search_api_key = config.search_api_key.clone();

    let policy = sandbox_policy_for_mode(mode, &session.workspace);
    let mut ctx = ctx
        .with_elevated_sandbox_policy(policy)
        .with_sandbox_runtime(config.sandbox_runtime.clone());
    if matches!(mode, AppMode::Plan) {
        ctx = ctx.with_shell_network_denied_hint(
            "Shell command blocked: Plan mode runs shell commands in a read-only sandbox — no writes, no network. Use Agent mode (`/mode agent`) for any command that creates or modifies files, or that needs network access.",
        );
    }
    ctx
}

pub(super) fn build_turn_tool_registry_builder_for(
    session: &Session,
    config: &EngineConfig,
    llm_client: &Option<LlmClientHandle>,
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
                config.plan_mode_state.clone(),
                config.plan_state.clone(),
            )
            // write_plan_file is the sole writable tool in plan mode
            .with_tool(std::sync::Arc::new(
                crate::tools::plan_mode::WritePlanFileTool::new(
                    config.plan_mode_state.clone(),
                    config.plan_state.clone(),
                ),
            ))
            .with_task_v2_read_only_tools_if_available(config.task_v2_manager.clone())
            .with_goal_tools(config.goal_state.clone())
    } else if mode == AppMode::Coordinator {
        // Coordinator mode: empty builder. Subagent + send_message tools
        // are added in engine.rs. User_input + recall_archive are added
        // in the common section below. Review, parallel, rlm, fim, patch,
        // web, slop_ledger are all excluded — coordinator only orchestrates.
        ToolRegistryBuilder::new()
    } else {
        ToolRegistryBuilder::new()
            .with_agent_tools(session.allow_shell)
            .with_todo_tool(todo_list)
            .with_plan_tool(plan_state)
            .with_plan_mode_tools(config.plan_mode_state.clone(), config.plan_state.clone())
            .with_task_v2_tools_if_available(config.task_v2_manager.clone())
            .with_goal_tools(config.goal_state.clone())
            .with_worktree_tools(config.worktree_state.clone())
    };

    // Review + parallel are NOT added for coordinator — it delegates
    // all work to workers and doesn't need review/parallel capabilities.
    if mode != AppMode::Coordinator {
        builder = builder
            .with_review_tool(llm_client.clone(), session.model.clone())
            .with_parallel_tool();
    }

    // User input and recall archive are needed for all modes.
    builder = builder.with_user_input_tool().with_recall_archive_tool();

    // SlopLedger: plan gets read-only, agent/yolo get the full set.
    // Coordinator skips slop_ledger entirely — it has no direct tools.
    if mode == AppMode::Plan {
        builder = builder.with_slop_ledger_read_only_tools();
    } else if mode != AppMode::Coordinator {
        builder = builder.with_slop_ledger_tools();
    }

    if mode != AppMode::Plan && mode != AppMode::Coordinator {
        builder = builder
            .with_rlm_tool(llm_client.clone(), session.model.clone())
            .with_fim_tool(llm_client.clone(), session.model.clone());
    }

    if config.features.enabled(Feature::ApplyPatch)
        && mode != AppMode::Plan
        && mode != AppMode::Coordinator
    {
        builder = builder.with_patch_tools();
    }
    if config.features.enabled(Feature::WebSearch) && mode != AppMode::Coordinator {
        builder = builder.with_web_tools();
    }
    // Shell tools (exec_shell, task_shell_start, etc.) are already gated
    // behind `allow_shell` inside `with_agent_tools`. No separate
    // feature-flag gate here to avoid double-registration.

    // Register the `remember` tool only when the user has opted in to
    // user-memory (#489). Without that opt-in the tool would always
    // fail; surfacing it would just waste catalog slots.
    if config.memory_enabled {
        builder = builder.with_remember_tool();
    }

    // Register KoD knowledge tools when Knowledge On Demand is enabled.
    if config.kod_enabled {
        builder = builder.with_knowledge_tools();
    }

    // Register Agent Teams tools when the feature is enabled.
    // Coordinator mode gets send_message only (added in engine.rs),
    // so skip the full team_tools here for coordinator.
    if config.features.enabled(Feature::AgentTeams) && mode != AppMode::Coordinator {
        builder = builder.with_team_tools_if_available(config.team_context.clone());
    }

    // Register image_analyze tool when vision_model is configured and feature enabled.
    if config.features.enabled(Feature::VisionModel)
        && let Some(ref vision_config) = config.vision_config
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
