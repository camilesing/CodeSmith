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

use codesmith_agent::models::Tool;
use codesmith_agent_runtime::background_task::{
    BackgroundTaskPollResult, BackgroundTaskPollSnapshot, BackgroundTaskStatus,
    BackgroundTaskSummary,
};
use codesmith_agent_runtime::host_services::{BgRegistryApi, HostServices, LspManagerApi};
use codesmith_agent_runtime::hooks::HookHost;
use codesmith_agent_runtime::lsp_config::LspConfig;
use codesmith_agent_runtime::lsp_diagnostics::DiagnosticBlock;
use codesmith_agent_runtime::runtime_ui::RuntimeUi;
use codesmith_agent_runtime::tool_dispatch::{ToolDispatcher, ToolMetadata};
use codesmith_tools::{ApprovalRequirement, ToolError, ToolResult};
use serde_json::Value;

use crate::background_task::SharedBackgroundTaskRegistry;
use crate::lsp::LspManager;
use crate::tools::ToolRegistry;

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
/// [`HostServices`]. Each accessor returns a trait-erased view of a service
/// whose concrete type lives in the host; `lsp` is the first, others follow
/// as the remaining `Engine` fields are decoupled.
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
