//! `ExtensionToolSpecAdapter` — bridges an extension's `ToolDefinition`
//! (framework contract, `codesmith_agent::extension`) onto the host's
//! production `ToolSpec` trait. Mirrors `ToolSpecAdapter`
//! (`framework_adapter.rs:42-87`) which bridges the core `Tool` onto
//! `ToolSpec`.
//!
//! The agent loop sees only `ToolSpec` — it never names `ToolDefinition`
//! or `ExtensionContext`. An `Arc<ExtensionToolSpecAdapter>` is inserted
//! into the host `ToolRegistry` via `ToolRegistry::register` (which funnels
//! through the `build_tool` fail-closed chokepoint — so the adapter's
//! `input_schema()` MUST be object-rooted + `name()` MUST match
//! `^[a-zA-Z0-9_-]{1,64}$`, or the tool is swapped for `FailClosedTool`).

use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::extension::{ExtensionContext, ToolDefinition};
use codesmith_extensions::ExtensionRunner;
use codesmith_tools::{ToolCapability, ToolError, ToolResult};
use serde_json::Value;

use super::registry::ToolRegistry;
use super::spec::{ToolContext, ToolSpec};

/// Wrap an extension `ToolDefinition` into a host `ToolSpec`. The bound
/// `ctx` is handed to `ToolDefinition::execute` on each call; the host's
/// `ToolContext` is ignored (extensions stay decoupled from `ToolContext`'s
/// ~30 host-coupled fields — spec §5.1.1).
pub struct ExtensionToolSpecAdapter {
    tool: Arc<dyn ToolDefinition>,
    ctx: Arc<dyn ExtensionContext>,
}

impl ExtensionToolSpecAdapter {
    #[must_use]
    pub fn new(tool: Arc<dyn ToolDefinition>, ctx: Arc<dyn ExtensionContext>) -> Self {
        Self { tool, ctx }
    }
}

#[async_trait]
impl ToolSpec for ExtensionToolSpecAdapter {
    fn name(&self) -> &str {
        self.tool.name()
    }
    fn description(&self) -> &str {
        self.tool.description()
    }
    fn input_schema(&self) -> Value {
        // MUST be object-rooted — `build_tool` rejects non-object roots.
        let schema = self.tool.input_schema();
        if schema.get("type").and_then(|v| v.as_str()) == Some("object") {
            schema
        } else {
            serde_json::json!({ "type": "object" })
        }
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        self.tool.capabilities()
    }
    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        match self.tool.execute(input, &*self.ctx).await {
            Ok(result) => Ok(result),
            Err(err) => {
                // Map the extension error back to a `ToolError` execution
                // failure so the agent loop surfaces it as a normal tool
                // failure (not a crash). The extension error's message is
                // preserved; `StaleContext` gets a fixed string to mirror
                // the guard's Display rendering.
                let message = match err {
                    codesmith_agent::extension::ExtensionError::StaleContext => {
                        "extension context is stale".to_string()
                    }
                    other => other.to_string(),
                };
                Err(ToolError::execution_failed(message))
            }
        }
    }
}

/// §F5d — register every bound extension tool into the host `ToolRegistry`,
/// each wrapped in an [`ExtensionToolSpecAdapter`]. Called from
/// `EngineHost::build_turn_dispatcher` after plugin-tools are configured.
///
/// Per-turn rebuild → no persistent host holder; clearing `runner.tools[id]`
/// before the next turn's call suffices. Ext tools are **main-turn-only**:
/// they are NOT added to any subagent's tool-subset basis (§4b — subagents
/// build their own fresh built-in-only `ToolRegistry` and never hold dylib
/// `Arc`s; the exclusion is structural, not a runtime guard).
pub fn register_extension_tools(registry: &mut ToolRegistry, runner: &ExtensionRunner) {
    let Some(ctx) = runner.bound_context() else {
        // No bound context yet (pre-bind_core) → nothing to adapt.
        return;
    };
    for (_name, tool) in runner.bound_tools() {
        let adapter = ExtensionToolSpecAdapter::new(tool, ctx.clone());
        registry.register(Arc::new(adapter));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolRegistry;
    use async_trait::async_trait;
    use codesmith_agent::extension::{
        Extension, ExtensionApi, ExtensionCommandContext, ExtensionContext, ExtensionError,
        ExtensionMetadata, ExtensionMode, ToolDefinition,
    };
    use codesmith_extensions::{ExtensionRunner, HostExtensionContext};
    use codesmith_tools::ToolCapability;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio_util::sync::CancellationToken;

    struct EchoTool;
    #[async_trait]
    impl ToolDefinition for EchoTool {
        fn name(&self) -> &str {
            "echo_ext"
        }
        fn description(&self) -> &str {
            "Echoes the input text."
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ToolResult, codesmith_agent::extension::ExtensionError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult::success(format!("echo:{text}")))
        }
    }

    fn ctx() -> Arc<dyn ExtensionContext> {
        Arc::new(HostExtensionContext::new(
            PathBuf::from("."),
            ExtensionMode::Tui,
            Arc::new(std::sync::Mutex::new(true)),
            Arc::new(std::sync::Mutex::new(CancellationToken::new())),
            Arc::new(AtomicU64::new(0)),
        ))
    }

    #[tokio::test]
    async fn adapter_executes_extension_tool() {
        let adapter = ExtensionToolSpecAdapter::new(Arc::new(EchoTool), ctx());
        let tc = ToolContext::new(".");
        let out = adapter.execute(json!({"text":"hi"}), &tc).await.unwrap();
        assert!(out.success);
        assert_eq!(out.content, "echo:hi");
    }

    #[test]
    fn adapter_name_and_schema_pass_fail_closed_chokepoint() {
        let adapter = ExtensionToolSpecAdapter::new(Arc::new(EchoTool), ctx());
        assert_eq!(adapter.name(), "echo_ext");
        assert_eq!(
            adapter.input_schema().get("type").and_then(|v| v.as_str()),
            Some("object")
        );
    }

    // === §F5d T1 — register_extension_tools helper =========================

    /// A static (in-process) extension that registers `EchoTool` (defined above
    /// in this test module) so T1's helper test does not require a built dylib.
    struct ToolExt;
    #[async_trait]
    impl Extension for ToolExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("toolext");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.register_tool(Box::new(EchoTool))?;
            Ok(())
        }
    }

    /// `ExtensionCommandContext`-typed ctx for `bind_core` (the runner stores
    /// `Arc<dyn ExtensionCommandContext>`; `bound_context()` upcasts it back to
    /// `Arc<dyn ExtensionContext>` for the adapter). A self-contained test mock
    /// mirroring `Ctx` (runner.rs:370-384) — avoids depending on
    /// `HostExtensionContext::new`'s ctor signature (a cross-crate guess).
    fn ctx_cmd() -> Arc<dyn ExtensionCommandContext> {
        struct CmdCtx {
            generation: u64,
        }
        #[async_trait]
        impl ExtensionContext for CmdCtx {
            fn cwd(&self) -> &std::path::Path {
                std::path::Path::new(".")
            }
            fn mode(&self) -> ExtensionMode {
                ExtensionMode::Tui
            }
            fn is_idle(&self) -> bool {
                true
            }
            fn signal(&self) -> tokio_util::sync::CancellationToken {
                tokio_util::sync::CancellationToken::new()
            }
            fn generation(&self) -> u64 {
                self.generation
            }
        }
        impl ExtensionCommandContext for CmdCtx {}
        Arc::new(CmdCtx { generation: 1 })
    }

    #[tokio::test]
    async fn register_extension_tools_adapts_bound_tools_into_registry() {
        let runner = ExtensionRunner::new();
        runner.load(&ToolExt).await.expect("load ToolExt");
        runner.bind_core(ctx_cmd());

        let mut registry = ToolRegistry::new(ToolContext::new("."));
        register_extension_tools(&mut registry, &runner);

        assert!(registry.contains("echo_ext"), "adapter registered echo_ext");
        let tool = registry.get("echo_ext").expect("echo_ext present");
        let out = tool
            .execute(serde_json::json!({"text":"hi"}), &ToolContext::new("."))
            .await
            .expect("execute via ToolSpec path");
        // EchoTool (extension.rs:119) returns ToolResult::success("echo:hi");
        // the adapter forwards it as the ToolSpec execute result — mirrors
        // `adapter_executes_extension_tool` at extension.rs:155-162.
        assert!(out.success, "ToolSpec execute succeeds");
        assert_eq!(out.content, "echo:hi");
    }

    #[tokio::test]
    async fn register_extension_tools_before_bind_core_is_noop() {
        let runner = ExtensionRunner::new();
        runner.load(&ToolExt).await.unwrap();
        let mut registry = ToolRegistry::new(ToolContext::new("."));
        register_extension_tools(&mut registry, &runner);
        assert!(!registry.contains("echo_ext"));
    }
}
