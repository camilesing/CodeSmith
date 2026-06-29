//! TUI-side implementations of the agent-runtime trait contracts.
//!
//! `ToolDispatcher` is implemented for [`ToolRegistry`], delegating to its
//! inherent methods and passing the registry's internal `ToolContext` to
//! tools. [`TuiRuntimeUi`] implements [`RuntimeUi`] by calling the TUI's
//! notification and clipboard free functions.
//!
//! These impls are the "bridge" that lets the engine core (once moved to
//! `codesmith-agent-runtime`) invoke tools and UI side-effects through trait
//! objects without depending on the concrete TUI types.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use codesmith_agent::models::Tool;
use codesmith_agent_runtime::background_task::{
    BackgroundTaskPollResult, BackgroundTaskPollSnapshot, BackgroundTaskStatus,
    BackgroundTaskSummary,
};
use codesmith_agent_runtime::hooks::HookHost;
use codesmith_agent_runtime::host_services::{
    BgRegistryApi, HostServices, LspManagerApi, SeamManagerApi, ShellApi, ShellExecResult,
    ShellExecStatus, SpawnSubAgentRequest, StructuredStateRequest, SubAgentApi,
    SubAgentSpawnResult, TurnDispatchPlan, TurnDispatchRequest,
};
use codesmith_agent_runtime::lsp_config::LspConfig;
use codesmith_agent_runtime::lsp_diagnostics::DiagnosticBlock;
use codesmith_agent_runtime::models::Message;
use codesmith_agent_runtime::runtime_ui::RuntimeUi;
use codesmith_agent_runtime::tool_dispatch::{ToolDispatcher, ToolMetadata};
use codesmith_tools::{ApprovalRequirement, ToolError, ToolResult};
use serde_json::Value;

use super::{
    Event, build_model_tool_catalog, build_tool_context_for, build_turn_tool_registry_builder_for,
    configure_plugin_tools,
};
use crate::background_task::SharedBackgroundTaskRegistry;
use crate::cycle_manager::StructuredState;
use crate::features::Feature;
use crate::lsp::LspManager;
use crate::seam_manager::SeamManager;
use crate::tools::ToolRegistry;
use crate::tools::shell::{SharedShellManager, ShellManager, ShellResult, ShellStatus};
use crate::tools::subagent::{
    Mailbox, SharedSubAgentManager, SubAgentForkContext, SubAgentManager, SubAgentResult,
    SubAgentRuntime, SubAgentType, resolve_subagent_assignment_route,
};
use crate::tui::app::AppMode;
use crate::utils::spawn_supervised;

#[async_trait::async_trait]
impl ToolDispatcher for ToolRegistry {
    fn has_tool(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn resolve(&self, requested: &str) -> Option<String> {
        ToolRegistry::resolve(self, requested).map(String::from)
    }

    fn metadata(&self, name: &str) -> Option<ToolMetadata> {
        let spec = self.get(name)?;
        Some(ToolMetadata {
            name: spec.name().to_string(),
            description: spec.description().to_string(),
            capabilities: spec.capabilities(),
            approval_requirement: spec.approval_requirement(),
            is_read_only: spec.is_read_only(),
            is_sandboxable: spec.is_sandboxable(),
            supports_parallel: spec.supports_parallel(),
            defer_loading: spec.defer_loading(),
        })
    }

    fn is_destructive(&self, name: &str, input: &Value) -> bool {
        self.get(name)
            .is_some_and(|spec| spec.is_destructive(input))
    }

    fn is_interactive(&self, name: &str, input: &Value) -> bool {
        self.get(name)
            .is_some_and(|spec| spec.is_interactive(input))
    }

    fn approval_requirement_for(&self, name: &str, input: &Value) -> Option<ApprovalRequirement> {
        let spec = self.get(name)?;
        Some(spec.approval_requirement_for_input(input, self.context()))
    }

    fn validate_input(&self, name: &str, input: &Value) -> Result<(), ToolError> {
        let spec = self
            .get(name)
            .ok_or_else(|| ToolError::not_available(format!("tool '{name}' is not registered")))?;
        spec.validate_input(input, self.context())
    }

    fn to_api_tools(&self) -> Vec<Tool> {
        ToolRegistry::to_api_tools(self)
    }

    fn to_api_tools_with_cache(&self, enable_cache: bool) -> Vec<Tool> {
        ToolRegistry::to_api_tools_with_cache(self, enable_cache)
    }

    async fn execute(
        &self,
        name: &str,
        input: Value,
        sandbox_override: Option<Value>,
    ) -> Result<ToolResult, ToolError> {
        let context_override = sandbox_override.and_then(|v| {
            match serde_json::from_value::<crate::sandbox::SandboxPolicy>(v) {
                Ok(policy) => Some(self.context().clone().with_elevated_sandbox_policy(policy)),
                Err(e) => {
                    tracing::warn!(error = %e, "invalid sandbox override policy JSON");
                    None
                }
            }
        });
        self.execute_full_with_context(name, input, context_override.as_ref())
            .await
    }

    fn hook_host(&self) -> Option<Arc<dyn HookHost>> {
        self.context()
            .runtime
            .hook_executor
            .clone()
            .map(|h| -> Arc<dyn HookHost> { h })
    }
}

/// Zero-sized TUI runtime-UI bridge: delegates to the TUI's notification and
/// clipboard free functions. Constructed where the engine needs
/// `Arc<dyn RuntimeUi>`.
pub(crate) struct TuiRuntimeUi;

impl RuntimeUi for TuiRuntimeUi {
    fn notify_busy(&self) {
        crate::tui::notifications::set_taskbar_progress_busy();
    }

    fn start_title_animation(&self, label: &str) {
        crate::tui::notifications::start_title_animation(label);
    }

    fn clipboard_images_dir(&self, workspace: &std::path::Path) -> PathBuf {
        crate::tui::clipboard::clipboard_images_dir(workspace)
    }
}

/// Bridge the TUI's concrete [`LspManager`] onto the engine-core trait
/// [`LspManagerApi`] by delegating to its inherent `config` /
/// `diagnostics_for`. Uses fully-qualified call syntax so the trait method
/// and the inherent method (same names) stay unambiguous.
#[async_trait::async_trait]
impl LspManagerApi for LspManager {
    fn config(&self) -> &LspConfig {
        LspManager::config(self)
    }

    async fn diagnostics_for(&self, file: &Path, edit_seq: u64) -> Option<DiagnosticBlock> {
        LspManager::diagnostics_for(self, file, edit_seq).await
    }
}

/// Bridge the TUI's concrete [`EngineHost`] onto the engine-core trait
/// [`HostServices`]. The sync accessors (`lsp` / `bg_registry` / `seam`)
/// return trait-erased views of services whose concrete types live in the
/// host; the async [`HostServices::build_turn_dispatcher`] factory assembles
/// the per-turn `ToolContext` / `ToolRegistryBuilder` / `SubAgentRuntime`
/// from a portable [`TurnDispatchRequest`] plus the host's own
/// terminal-coupled managers, returning the trait-erased registry and
/// catalog the streaming turn loop consumes. Keeping all of this here (the
/// bridge file) means the `Engine` body — which moves to
/// `codesmith-agent-runtime` in a later phase — only calls the trait method
/// and stays free of these concrete TUI types.
#[async_trait::async_trait]
impl HostServices for super::EngineHost {
    fn lsp(&self) -> &dyn LspManagerApi {
        &*self.lsp_manager
    }

    fn bg_registry(&self) -> Arc<dyn BgRegistryApi> {
        // `new_impl` always seeds `runtime_services.background_task_registry`
        // before the engine runs, so this is `Some` for any engine that reaches
        // `run()`. The clone is a cheap `Arc` bump.
        let registry = self
            .runtime_services
            .background_task_registry
            .as_ref()
            .expect("background_task_registry is set by new_impl before run()")
            .clone();
        Arc::new(BgRegistryHost(registry))
    }

    fn seam(&self) -> Option<&dyn SeamManagerApi> {
        match &self.seam_manager {
            Some(s) => Some(s),
            None => None,
        }
    }

    fn subagents(&self) -> Arc<dyn SubAgentApi> {
        Arc::new(SubAgentManagerHost(Arc::clone(&self.subagent_manager)))
    }

    fn shell(&self) -> Arc<dyn ShellApi> {
        Arc::new(ShellManagerHost(Arc::clone(&self.shell_manager)))
    }

    async fn spawn_subagent(
        &self,
        req: SpawnSubAgentRequest<'_>,
    ) -> anyhow::Result<SubAgentSpawnResult> {
        // Sub-agents don't inherit YOLO mode — use Agent-mode defaults, same
        // as the pre-factory `Op::SpawnSubAgent` body did.
        let tool_context = build_tool_context_for(
            self,
            req.session,
            req.config,
            AppMode::Agent,
            req.session.auto_approve,
            req.cancel_token.clone(),
            req.runtime_ui,
        );
        let mut runtime = SubAgentRuntime::new(
            req.llm_client,
            req.session.model.clone(),
            tool_context,
            req.session.allow_shell,
            Some(req.tx_event.clone()),
            Arc::clone(&self.subagent_manager),
        )
        .with_role_models(req.config.subagent_model_overrides.clone())
        .with_auto_model(req.session.auto_model)
        .with_reasoning_effort(
            req.session.reasoning_effort.clone(),
            req.session.reasoning_effort_auto,
        )
        .with_max_spawn_depth(req.config.max_spawn_depth)
        .with_step_api_timeout(req.config.subagent_api_timeout)
        .with_mcp_pool(req.mcp_pool)
        .background_runtime();
        let route =
            resolve_subagent_assignment_route(&runtime, None, req.prompt, &SubAgentType::General)
                .await;
        runtime.model = route.model;
        runtime.reasoning_effort = route.reasoning_effort;
        runtime.reasoning_effort_auto = false;

        let result = {
            let mut manager = self.subagent_manager.write().await;
            manager.spawn_background(
                Arc::clone(&self.subagent_manager),
                runtime,
                SubAgentType::General,
                req.prompt.to_string(),
                None,
            )
        };
        result.map(|snapshot| SubAgentSpawnResult {
            agent_id: snapshot.agent_id,
        })
    }

    async fn capture_structured_state(&self, req: StructuredStateRequest<'_>) -> Option<String> {
        let state = StructuredState::capture(
            req.mode_label,
            req.workspace,
            req.cwd,
            req.working_set,
            req.todos,
            req.plan_state,
            Some(&self.subagent_manager),
        )
        .await;
        state.to_system_block()
    }

    async fn build_turn_dispatcher(&self, req: TurnDispatchRequest<'_>) -> TurnDispatchPlan {
        let mode = req.mode;
        let auto_approve = req.auto_approve;
        let session = req.session;
        let config = req.config;

        let todo_list = config.todos.clone();
        let plan_state = config.plan_state.clone();

        let tool_context = build_tool_context_for(
            self,
            session,
            config,
            mode,
            auto_approve,
            req.cancel_token.clone(),
            req.runtime_ui,
        );
        let mut builder = build_turn_tool_registry_builder_for(
            session,
            config,
            &req.llm_client,
            mode,
            todo_list,
            plan_state,
        );

        let fork_context_for_runtime = if config.features.enabled(Feature::Subagents) {
            let state = StructuredState::capture(
                mode.label(),
                config.workspace.clone(),
                std::env::current_dir().ok(),
                &session.working_set,
                &config.todos,
                &config.plan_state,
                Some(&self.subagent_manager),
            )
            .await;
            Some(SubAgentForkContext {
                system: session.system_prompt.clone(),
                messages: session.messages.clone(),
                structured_state_block: state.to_system_block(),
                current_assistant_text: None,
                current_turn_tool_calls: None,
            })
        } else {
            None
        };

        // Mailbox for structured sub-agent envelopes (#128/#130). One per
        // turn: the receiver is drained by a short-lived task that converts
        // envelopes into `Event::SubAgentMailbox` so the UI can route them
        // to the matching in-transcript card. The drainer exits naturally
        // when every cloned sender is dropped at turn-end.
        let mailbox_for_runtime = if config.features.enabled(Feature::Subagents) {
            let mailbox_cancel = req.cancel_token.child_token();
            let (mailbox, mut receiver) = Mailbox::new(mailbox_cancel.clone());
            let tx_event_clone = req.tx_event.clone();
            spawn_supervised(
                "subagent-mailbox-drainer",
                std::panic::Location::caller(),
                async move {
                    while let Some(envelope) = receiver.recv().await {
                        if tx_event_clone
                            .send(Event::SubAgentMailbox {
                                seq: envelope.seq,
                                message: envelope.message,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                },
            );
            Some((mailbox, mailbox_cancel))
        } else {
            None
        };

        let mcp_pool = req.mcp_pool;

        let mut tool_registry = match mode {
            AppMode::Agent | AppMode::Yolo => {
                if config.features.enabled(Feature::Subagents) {
                    let runtime = if let Some(client) = req.llm_client.clone() {
                        let mut rt = SubAgentRuntime::new(
                            client,
                            session.model.clone(),
                            tool_context.clone(),
                            session.allow_shell,
                            Some(req.tx_event.clone()),
                            Arc::clone(&self.subagent_manager),
                        )
                        .with_role_models(config.subagent_model_overrides.clone())
                        .with_auto_model(session.auto_model)
                        .with_reasoning_effort(
                            session.reasoning_effort.clone(),
                            session.reasoning_effort_auto,
                        )
                        .with_max_spawn_depth(config.max_spawn_depth)
                        .with_step_api_timeout(config.subagent_api_timeout)
                        .with_mcp_pool(mcp_pool.clone())
                        .with_parent_completion_tx(req.tx_subagent_completion.clone());
                        if let Some(context) = fork_context_for_runtime.clone() {
                            rt = rt.with_fork_context(context);
                        }
                        if let Some((mailbox, cancel_token)) = mailbox_for_runtime.as_ref() {
                            rt = rt
                                .with_mailbox(mailbox.clone())
                                .with_cancel_token(cancel_token.clone());
                        }
                        Some(rt)
                    } else {
                        None
                    };
                    Some(
                        builder
                            .with_subagent_tools(
                                self.subagent_manager.clone(),
                                runtime.expect("sub-agent runtime should exist with active client"),
                            )
                            .build(tool_context),
                    )
                } else {
                    Some(builder.build(tool_context))
                }
            }
            AppMode::Coordinator => {
                // Coordinator mode requires subagents — it must be able to
                // spawn worker agents. Add subagent + send_message tools.
                if config.features.enabled(Feature::Subagents)
                    && let Some(client) = req.llm_client.clone()
                {
                    let mut rt = SubAgentRuntime::new(
                        client,
                        session.model.clone(),
                        tool_context.clone(),
                        true, // Coordinator workers need shell access
                        Some(req.tx_event.clone()),
                        Arc::clone(&self.subagent_manager),
                    )
                    .with_role_models(config.subagent_model_overrides.clone())
                    .with_auto_model(session.auto_model)
                    .with_reasoning_effort(
                        session.reasoning_effort.clone(),
                        session.reasoning_effort_auto,
                    )
                    .with_max_spawn_depth(config.max_spawn_depth)
                    .with_step_api_timeout(config.subagent_api_timeout)
                    .with_mcp_pool(mcp_pool.clone())
                    .with_parent_completion_tx(req.tx_subagent_completion.clone());
                    if let Some(context) = fork_context_for_runtime.clone() {
                        rt = rt.with_fork_context(context);
                    }
                    if let Some((mailbox, cancel_token)) = mailbox_for_runtime.as_ref() {
                        rt = rt
                            .with_mailbox(mailbox.clone())
                            .with_cancel_token(cancel_token.clone());
                    }
                    builder = builder.with_subagent_tools(self.subagent_manager.clone(), rt);
                }
                // send_message — coordinator needs messaging but NOT
                // team_create/team_delete.
                if config.features.enabled(Feature::AgentTeams)
                    && let Some(tc) = config.team_context.clone()
                {
                    builder = builder.with_send_message_tool(tc);
                }
                builder = builder.with_task_stop_tool();
                Some(builder.build(tool_context))
            }
            _ => Some(builder.build(tool_context)),
        };

        // Load plugin tools from the user's tools directory and apply any
        // config.toml overrides. Explicit overrides win over auto-discovered
        // scripts with the same tool name.
        let mut plugin_tool_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if let Some(ref mut tool_registry) = tool_registry {
            plugin_tool_names = configure_plugin_tools(tool_registry, config.tools.as_ref());
        }

        let mcp_tools = req.mcp_tools;
        let tools = tool_registry.as_ref().map(|registry| {
            let mut catalog = build_model_tool_catalog(
                registry.to_api_tools_with_cache(true),
                mcp_tools,
                mode,
                &config.tools_always_load,
            );
            for tool in &mut catalog {
                if plugin_tool_names.contains(&tool.name) {
                    tool.defer_loading = Some(false);
                }
            }
            catalog
        });

        TurnDispatchPlan {
            tool_registry: tool_registry.map(|r| Arc::new(r) as Arc<dyn ToolDispatcher>),
            tools,
        }
    }
}

/// Bridge the TUI's concrete [`SeamManager`] onto the engine-core trait
/// [`SeamManagerApi`] by delegating to its inherent API. `enabled` substitutes
/// for the old `config().enabled` read so `SeamConfig` stays TUI-local.
#[async_trait::async_trait]
impl SeamManagerApi for SeamManager {
    fn enabled(&self) -> bool {
        SeamManager::config(self).enabled
    }

    fn seam_level_for(
        &self,
        active_input_tokens: usize,
        highest_existing_level: Option<u8>,
    ) -> Option<u8> {
        SeamManager::seam_level_for(self, active_input_tokens, highest_existing_level)
    }

    fn verbatim_window_start(&self, message_count: usize) -> usize {
        SeamManager::verbatim_window_start(self, message_count)
    }

    async fn seam_count(&self) -> usize {
        SeamManager::seam_count(self).await
    }

    async fn highest_level(&self) -> Option<u8> {
        SeamManager::highest_level(self).await
    }

    async fn collect_seam_texts(&self, messages: &[Message]) -> Vec<String> {
        SeamManager::collect_seam_texts(self, messages).await
    }

    async fn produce_soft_seam(
        &self,
        messages: &[Message],
        level: u8,
        start_idx: usize,
        end_idx: usize,
        workspace: Option<&Path>,
        pinned_indices: &[usize],
    ) -> anyhow::Result<String> {
        SeamManager::produce_soft_seam(
            self,
            messages,
            level,
            start_idx,
            end_idx,
            workspace,
            pinned_indices,
        )
        .await
    }

    async fn recompact(
        &self,
        existing_seams: &[String],
        new_messages: &[&Message],
        level: u8,
        start_idx: usize,
        end_idx: usize,
    ) -> anyhow::Result<String> {
        SeamManager::recompact(
            self,
            existing_seams,
            new_messages,
            level,
            start_idx,
            end_idx,
        )
        .await
    }

    async fn produce_flash_briefing(
        &self,
        existing_seams: &[String],
        structured_state: Option<&str>,
    ) -> anyhow::Result<String> {
        SeamManager::produce_flash_briefing(self, existing_seams, structured_state).await
    }

    async fn reset(&self) {
        SeamManager::reset(self).await;
    }
}

/// Bridge the TUI's [`SharedBackgroundTaskRegistry`] onto the engine-core
/// trait [`BgRegistryApi`]. A newtype is required: the orphan rule forbids
/// `impl`-ing a foreign trait for `Arc<Mutex<..>>`. Each method locks the
/// inner registry and converts `BackgroundTaskState` results into the
/// portable `BackgroundTaskSummary`, so callers never hold a guard across an
/// `Event`-channel await.
#[derive(Clone)]
pub(crate) struct BgRegistryHost(pub SharedBackgroundTaskRegistry);

#[async_trait::async_trait]
impl BgRegistryApi for BgRegistryHost {
    async fn register_shell_task(
        &self,
        shell_id: String,
        command: String,
        cwd: PathBuf,
    ) -> BackgroundTaskSummary {
        let mut g = self.0.lock().await;
        BackgroundTaskSummary::from(&g.register_shell_task(shell_id, command, cwd))
    }

    async fn cancel_task(&self, id: &str) -> anyhow::Result<()> {
        let mut g = self.0.lock().await;
        g.cancel_task(id).await
    }

    async fn list_tasks(&self) -> Vec<BackgroundTaskSummary> {
        let g = self.0.lock().await;
        g.list_tasks()
    }

    async fn read_output_delta(&self, id: &str) -> Option<String> {
        let mut g = self.0.lock().await;
        g.read_output_delta(id)
    }

    async fn background_all(&self) -> Vec<BackgroundTaskSummary> {
        let mut g = self.0.lock().await;
        g.background_all()
            .iter()
            .map(BackgroundTaskSummary::from)
            .collect()
    }

    async fn register_dream_task(&self, memory_path: PathBuf) -> BackgroundTaskSummary {
        let mut g = self.0.lock().await;
        BackgroundTaskSummary::from(&g.register_dream_task(memory_path))
    }

    async fn update_task_status(
        &self,
        id: &str,
        new_status: BackgroundTaskStatus,
        error: Option<String>,
    ) -> Option<BackgroundTaskPollResult> {
        let mut g = self.0.lock().await;
        g.update_task_status(id, new_status, error)
    }

    async fn poll_once(&self) -> BackgroundTaskPollSnapshot {
        // One locked pass: poll, drain notifications, evict notified tasks.
        // Mirrors the previous poller loop's lock granularity exactly.
        let mut g = self.0.lock().await;
        let results = g.poll_tasks().await;
        let notifications = g.drain_notifications();
        g.evict_notified();
        BackgroundTaskPollSnapshot {
            results,
            notifications,
        }
    }
}

/// Bridge the TUI's [`SharedSubAgentManager`] onto the engine-core trait
/// [`SubAgentApi`]. A newtype is required (orphan rule forbids impl-ing a
/// foreign trait for `Arc<RwLock<..>>`); each method locks the inner `RwLock`
/// itself and fully-qualifies the inherent call so the trait method and the
/// inherent method (same names) stay unambiguous — mirroring the
/// [`LspManagerApi`] / [`SeamManagerApi`] bridges above.
#[derive(Clone)]
pub(crate) struct SubAgentManagerHost(pub SharedSubAgentManager);

#[async_trait::async_trait]
impl SubAgentApi for SubAgentManagerHost {
    async fn running_count(&self) -> usize {
        let g = self.0.read().await;
        SubAgentManager::running_count(&g)
    }

    async fn list(&self) -> Vec<SubAgentResult> {
        let g = self.0.read().await;
        SubAgentManager::list(&g)
    }

    async fn cleanup(&self, max_age: Duration) {
        let mut g = self.0.write().await;
        SubAgentManager::cleanup(&mut g, max_age);
    }
}

/// Bridge the TUI's [`SharedShellManager`] onto the engine-core trait
/// [`ShellApi`]. A newtype is required (orphan rule); `execute` locks the
/// inner `std::sync::Mutex` synchronously and converts the TUI-local
/// [`ShellResult`] into the portable [`ShellExecResult`].
pub(crate) struct ShellManagerHost(pub SharedShellManager);

impl ShellApi for ShellManagerHost {
    fn execute(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
    ) -> anyhow::Result<ShellExecResult> {
        let mut shell = self.0.lock().unwrap_or_else(|p| p.into_inner());
        ShellManager::execute(&mut shell, command, working_dir, timeout_ms, background)
            .map(shell_exec_result_from)
    }
}

/// Convert the TUI-local [`ShellResult`] into the portable
/// [`ShellExecResult`]. A free function (not `impl From`) because the orphan
/// rule forbids `impl From<ShellResult> for ShellExecResult` from the TUI
/// crate (both the trait and the target type are foreign).
fn shell_exec_result_from(r: ShellResult) -> ShellExecResult {
    ShellExecResult {
        task_id: r.task_id,
        status: match r.status {
            ShellStatus::Running => ShellExecStatus::Running,
            ShellStatus::Completed => ShellExecStatus::Completed,
            ShellStatus::Failed => ShellExecStatus::Failed,
            ShellStatus::Killed => ShellExecStatus::Killed,
            ShellStatus::TimedOut => ShellExecStatus::TimedOut,
        },
        exit_code: r.exit_code,
        stdout: r.stdout,
        stderr: r.stderr,
        duration_ms: r.duration_ms,
    }
}
