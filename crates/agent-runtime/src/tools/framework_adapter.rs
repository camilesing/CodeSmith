//! Bridge the production `ToolSpec` + `ToolContext` onto the framework-core
//! [`Tool`].
//!
//! The framework-core [`codesmith_agent::tools::Tool`] trait (LangChain
//! `BaseTool` analog, in `codesmith-agent`) is deliberately context-free:
//! `run` takes only a parsed `input`. The production [`ToolSpec`] trait (in
//! this crate) is context-passed — [`ToolSpec::execute`] receives a fat
//! `&ToolContext` per call. [`ToolSpecAdapter`] closes that gap: it captures a
//! `ToolContext` (shared via `Arc`) at construction and exposes the spec as a
//! framework `Tool`, forwarding metadata and delegating `run` to `execute`.
//!
//! This is the "land the bridge" step of ROADMAP §E — the production `Engine`
//! migration onto `AgentExecutor` is done (slice 20 §E cutover). The
//! adapter is purely the executor's `run` path; the richer approval /
//! parallelism / destructive / defer-loading metadata on `ToolSpec` has no
//! counterpart in the framework `Tool` (`capabilities` is advisory-only) and
//! stays on the host's `ToolDispatcher` / `ToolMetadata` surface. Leaf types
//! (`ToolResult` / `ToolError` / `ToolCapability`) are already shared via the
//! `codesmith-tools` crate, so `run` returns them with no translation.
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use codesmith_agent::tools::{Tool as FrameworkTool, ToolCapability, ToolError, ToolResult};
use serde_json::Value;

use super::spec::{ToolContext, ToolSpec};

/// Adapter exposing a production [`ToolSpec`] as a framework-core
/// [`FrameworkTool`].
///
/// Holds the [`ToolSpec`] and a shared [`ToolContext`] (an `Arc`, so cloning
/// for a `ToolSet` is a cheap refcount bump). [`FrameworkTool::run`] clones
/// both `Arc`s into owned, `'static` locals and delegates to
/// [`ToolSpec::execute`] — the captured context flows through unchanged. Build
/// one per spec, or use
/// [`ToolRegistry::to_framework_tool_set`](super::registry::ToolRegistry::to_framework_tool_set)
/// to wrap a whole registry in one call.
pub struct ToolSpecAdapter {
    spec: Arc<dyn ToolSpec>,
    context: Arc<ToolContext>,
}

impl ToolSpecAdapter {
    /// Wrap a `ToolSpec` with a shared `ToolContext`.
    #[must_use]
    pub fn new(spec: Arc<dyn ToolSpec>, context: Arc<ToolContext>) -> Self {
        Self { spec, context }
    }
}

impl FrameworkTool for ToolSpecAdapter {
    fn name(&self) -> &str {
        self.spec.name()
    }

    fn description(&self) -> &str {
        self.spec.description()
    }

    fn input_schema(&self) -> Value {
        self.spec.input_schema()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        self.spec.capabilities()
    }

    fn run(
        &self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + '_>> {
        // Clone the two `Arc`s into owned, `'static` locals so the future
        // neither borrows `&self` (lets it satisfy any `'_`) nor ties the
        // `ToolSpec`/`ToolContext` borrows to the adapter's lifetime. The leaf
        // result types are shared via `codesmith-tools` — no translation.
        let spec = self.spec.clone();
        let context = self.context.clone();
        Box::pin(async move {
            // Deref-coercion: `&context` (`&Arc<ToolContext>`) -> `&ToolContext`.
            spec.execute(input, &context).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent::callback::{NoopCallback, StopReason};
    use codesmith_agent::executor::{AgentExecutor, AgentExecutorConfig, DefaultAgentExecutor};
    use codesmith_agent::llm_client::{LlmClient, LlmClientHandle, StreamEventBox};
    use codesmith_agent::memory::{ChatHistory, VecChatHistory};
    use codesmith_agent::models::{
        ContentBlock, ContentBlockStart, Delta, MessageDelta, MessageRequest, StreamEvent,
    };
    use codesmith_agent::tools::ToolSet;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// A `ToolSpec` that echoes its `text` input, stamped with the captured
    /// workspace path so tests can prove the context flowed through.
    struct EchoSpec;

    #[async_trait::async_trait]
    impl ToolSpec for EchoSpec {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input, stamped with the workspace path."
        }
        fn input_schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: Value,
            context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult {
                content: format!("{}|{text}", context.workspace.display()),
                success: true,
                metadata: None,
            })
        }
    }

    fn temp_context() -> Arc<ToolContext> {
        let tmp = tempdir().expect("tempdir");
        Arc::new(ToolContext::new(tmp.path().to_path_buf()))
    }

    #[tokio::test]
    async fn adapter_forwards_metadata_and_delegates_run() {
        let ctx = temp_context();
        let workspace_stamp = ctx.workspace.display().to_string();
        let adapter = ToolSpecAdapter::new(Arc::new(EchoSpec), ctx);

        // Metadata is forwarded from the spec.
        assert_eq!(adapter.name(), "echo");
        assert_eq!(
            adapter.description(),
            "Echoes input, stamped with the workspace path."
        );
        assert_eq!(
            adapter.input_schema()["properties"]["text"]["type"],
            "string"
        );
        assert_eq!(adapter.capabilities(), vec![ToolCapability::ReadOnly]);

        // `run` delegates to `execute` with the *captured* context — the
        // workspace path (only knowable via ToolContext) is stamped in.
        let out = adapter
            .run(serde_json::json!({"text":"hi"}))
            .await
            .expect("echo succeeds");
        assert!(out.success);
        assert!(
            out.content.starts_with(&workspace_stamp),
            "context workspace stamped: {}",
            out.content
        );
        assert!(out.content.ends_with("|hi"));
    }

    #[tokio::test]
    async fn adapter_registers_in_toolset_as_dyn_tool() {
        let ctx = temp_context();
        let mut set = ToolSet::new();
        set.register(Arc::new(ToolSpecAdapter::new(Arc::new(EchoSpec), ctx)));

        // Retrieved as the erased `Arc<dyn Tool>` and run through the trait
        // object — proves the adapter coerces and the boxed future is `Send`.
        let tool = set.get("echo").expect("echo registered");
        let out = tool
            .run(serde_json::json!({"text":"world"}))
            .await
            .expect("run");
        assert!(out.success);
        assert!(out.content.ends_with("|world"));

        // Wire defs are rebuilt from the forwarded metadata.
        let wire = set.to_api_tools();
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].name, "echo");
        assert_eq!(wire[0].tool_type.as_deref(), Some("custom"));
    }

    // === Executor integration =================================================
    //
    // Drives a real `ToolSpec` (behind the adapter) through the framework-core
    // `DefaultAgentExecutor` with a mock LLM — the headline proof that the
    // bridge composes with the agent loop. Mirrors the executor's own
    // `tool_call_then_finish` test, but the tool is a context-passed
    // `ToolSpec` reached only via `ToolSpecAdapter`.

    /// A `LlmClient` that pops canned `StreamEvent` lists from a queue, one
    /// per `create_message_stream` call.
    struct MockLlm {
        calls: Mutex<VecDeque<Vec<StreamEvent>>>,
    }

    impl MockLlm {
        fn new(calls: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                calls: Mutex::new(calls.into_iter().collect()),
            }
        }
    }

    impl LlmClient for MockLlm {
        fn provider_name(&self) -> &'static str {
            "mock"
        }
        fn model(&self) -> &str {
            "mock-v0"
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = anyhow::Result<codesmith_agent::models::MessageResponse>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async { anyhow::bail!("mock does not implement create_message") })
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<StreamEventBox>> + Send + '_>> {
            let next = self.calls.lock().unwrap().pop_front();
            Box::pin(async move {
                let events = next.unwrap_or_default();
                Ok(
                    Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
                        as StreamEventBox,
                )
            })
        }
    }

    fn text_block(idx: u32, body: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::Text {
                    text: String::new(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::TextDelta {
                    text: body.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn tool_use_block(idx: u32, id: &str, name: &str, input_json: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: serde_json::Value::Null,
                    caller: None,
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::InputJsonDelta {
                    partial_json: input_json.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn finish(stop: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(stop.to_string()),
                    stop_sequence: None,
                },
                usage: None,
            },
            StreamEvent::MessageStop,
        ]
    }

    #[tokio::test]
    async fn executor_drives_toolspec_through_adapter() {
        let ctx = temp_context();
        let workspace_stamp = ctx.workspace.display().to_string();

        let mut tools = ToolSet::new();
        tools.register(Arc::new(ToolSpecAdapter::new(Arc::new(EchoSpec), ctx)));

        // Call 1: text + tool_use(echo). Call 2: text-only -> NoToolCalls.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let executor = DefaultAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            Arc::new(tools),
            Arc::new(NoopCallback),
            AgentExecutorConfig::default(),
        );

        let mut history = VecChatHistory::new();
        let reason = executor
            .run(&mut history, "echo world".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The ToolResult fed back must carry the captured context's workspace
        // path — proof the adapter routed `execute` through the real
        // `ToolContext`, not a stub.
        // [user, assistant(text+tooluse), user(toolresult), assistant(text)]
        assert_eq!(history.len(), 4);
        match &history.messages()[2].content[0] {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(
                    content.starts_with(&workspace_stamp),
                    "context stamped: {content}"
                );
                assert!(content.ends_with("|world"));
                assert_eq!(is_error, &Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }
}
