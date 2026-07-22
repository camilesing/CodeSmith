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
use codesmith_tools::{ToolCapability, ToolError, ToolResult};
use serde_json::Value;

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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::{ExtensionContext, ExtensionMode, ToolDefinition};
    use codesmith_extensions::HostExtensionContext;
    use codesmith_tools::ToolCapability;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
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
            CancellationToken::new(),
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
}
