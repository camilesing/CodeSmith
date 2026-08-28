//! # Framework-core tool contract
//!
//! The host-agnostic executable-tool seam — the LangChain `BaseTool` analog.
//! A [`Tool`] holds its own dependencies (workspace handle, shell, http client,
//! …) injected at construction; [`Tool::run`] takes only a parsed `input` and
//! returns a [`ToolResult`]. This keeps the framework core free of the fat
//! per-call `ToolContext` that lives in `codesmith-agent-runtime`: a host can
//! bridge its existing context-passed tools to this trait via an adapter that
//! captures the context, without the core knowing about it.
//!
//! ## What lives here vs. elsewhere
//!
//! - **Here** — the executable [`Tool`] *trait* + the [`ToolSet`] registry.
//! - [`crate::models::Tool`] — the *wire* tool definition sent to the model
//!   (name / description / `input_schema`). [`ToolSet::to_api_tools`] converts
//!   executable tools into wire definitions. The two `Tool` types live in
//!   different modules (`tools::Tool` vs `models::Tool`); the module path
//!   disambiguates.
//! - `codesmith_tools` — the leaf value-types ([`ToolResult`], [`ToolError`],
//!   [`ToolCapability`], [`ApprovalRequirement`]); re-exported below so
//!   consumers depend only on `codesmith-agent`.
//! - `codesmith-agent-runtime::tools::spec` — the production `ToolSpec` trait +
//!   fat `ToolContext`, used by the live `Engine`. Migrating it onto this trait
//!   is a later ROADMAP §E slice.
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// Re-export the leaf value-types so downstream crates can depend solely on
// `codesmith-agent` for the tool contract.
pub use codesmith_tools::{ApprovalRequirement, ToolCapability, ToolError, ToolResult};

use crate::models;

/// A permissive default JSON schema (`{"type":"object"}`) returned by
/// [`Tool::input_schema`] when an impl doesn't override it.
fn default_input_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object" })
}

/// Host-agnostic executable tool contract (LangChain `BaseTool` analog).
///
/// Each implementation owns its dependencies; [`run`](Tool::run) receives only
/// the parsed tool input. Dyn-safe so a [`ToolSet`] can hold `Arc<dyn Tool>`.
pub trait Tool: Send + Sync {
    /// Canonical tool name the model emits in a `tool_use` block.
    fn name(&self) -> &str;

    /// Human-readable description shown to the model.
    fn description(&self) -> &str;

    /// JSON schema for the tool input. Default: a permissive object.
    fn input_schema(&self) -> serde_json::Value {
        default_input_schema()
    }

    /// Declared capabilities (read-only / writes-files / …). Default: empty.
    /// The framework executor does not gate on these; they are advisory metadata
    /// for hosts that need approval/parallelism decisions.
    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }

    /// Execute the tool against a parsed `input`, returning a [`ToolResult`]
    /// (or a [`ToolError`]). Boxed future for dyn-safety, matching
    /// [`crate::llm_client::LlmClient`]. The future borrows `&self`.
    fn run(
        &self,
        input: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + '_>>;
}

/// Framework-level tool registry: a name → `Arc<dyn Tool>` map.
///
/// Simpler than the production `ToolDispatcher` (which carries approval /
/// parallelism / metadata concerns): this is the minimal registry an
/// [`crate::executor::AgentExecutor`] drives. A host with richer dispatch
/// needs keeps its own registry and bridges it.
#[derive(Default)]
pub struct ToolSet {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolSet {
    /// Create an empty tool set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert/replace a tool, keyed by its [`Tool::name`].
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Number of registered tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Convert every registered tool into the wire [`models::Tool`] definition
    /// the model receives in a `MessageRequest.tools`. Tools are emitted in the
    /// `HashMap`'s arbitrary iteration order — hosts needing a stable order
    /// should sort the result.
    #[must_use]
    pub fn to_api_tools(&self) -> Vec<models::Tool> {
        self.tools
            .values()
            .map(|t| models::Tool {
                // Anthropic custom-tool convention; harmless for OpenAI-compat
                // (ignored by the shaper). Providers needing a different tag can
                // post-process.
                tool_type: Some("custom".to_string()),
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
                output_schema: None,
                allowed_callers: None,
                defer_loading: None,
                input_examples: None,
                strict: None,
                cache_control: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes back the input text."
        }
        fn run(
            &self,
            input: serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + '_>> {
            Box::pin(async move {
                let text = input
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(ToolResult {
                    content: text,
                    success: true,
                    metadata: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn register_and_run_tool() {
        let mut set = ToolSet::new();
        set.register(Arc::new(EchoTool));
        let tool = set.get("echo").expect("echo registered");
        let out = tool
            .run(serde_json::json!({"text": "hi"}))
            .await
            .expect("echo succeeds");
        assert!(out.success);
        assert_eq!(out.content, "hi");
    }

    #[tokio::test]
    async fn to_api_tools_emits_wire_definitions() {
        let mut set = ToolSet::new();
        set.register(Arc::new(EchoTool));
        let wire = set.to_api_tools();
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].name, "echo");
        assert_eq!(wire[0].tool_type.as_deref(), Some("custom"));
        assert_eq!(wire[0].input_schema, default_input_schema());
    }

    #[test]
    fn empty_set_is_empty() {
        let set = ToolSet::new();
        assert!(set.is_empty());
        assert!(set.to_api_tools().is_empty());
    }
}
