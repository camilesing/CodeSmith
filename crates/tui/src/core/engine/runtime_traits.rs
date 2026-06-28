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
use codesmith_agent_runtime::host_services::{HostServices, LspManagerApi};
use codesmith_agent_runtime::hooks::HookHost;
use codesmith_agent_runtime::lsp_config::LspConfig;
use codesmith_agent_runtime::lsp_diagnostics::DiagnosticBlock;
use codesmith_agent_runtime::runtime_ui::RuntimeUi;
use codesmith_agent_runtime::tool_dispatch::{ToolDispatcher, ToolMetadata};
use codesmith_tools::{ApprovalRequirement, ToolError, ToolResult};
use serde_json::Value;

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
}
