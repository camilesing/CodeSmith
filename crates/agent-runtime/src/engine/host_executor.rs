//! Host-side [`AgentExecutor`] — the designated home for the production turn
//! loop migration (ROADMAP §E "接真引擎").
//!
//! The framework-core [`DefaultAgentExecutor`](codesmith_agent::executor::DefaultAgentExecutor)
//! is the minimal, host-agnostic reference loop. The production `Engine` in
//! this crate carries the real turn loop (`handle_deepseek_turn`, ~2.4k lines)
//! with ten guardrails (compaction / capacity / approval / steer /
//! transparent-retry / early-tool-start / subagent / LSP / loop-guard / cycle).
//! [`HostAgentExecutor`] is the host-side [`AgentExecutor`] impl that will
//! absorb those guardrails slice by slice, eventually replacing
//! `handle_deepseek_turn`. The three host→framework bridges are already in
//! place to compose it:
//!
//! - [`ToolSpecAdapter`](crate::tools::framework_adapter::ToolSpecAdapter) —
//!   production `ToolSpec`+`ToolContext` → framework `Tool` (the `run` path).
//! - [`CallbackBridge`](crate::callback_bridge::CallbackBridge) — `mpsc::Sender<Event>`
//!   + `HookHost` → framework `Callback` (tool-lifecycle hooks).
//! - [`SessionChatHistory`](crate::session_history::SessionChatHistory) —
//!   production `Session` → framework `ChatHistory` (the transcript).
//!
//! ## This slice: loop-guard absorbed
//!
//! [`HostAgentExecutor`] runs the LLM↔tool loop (reusing
//! [`accumulate_stream`](codesmith_agent::executor::accumulate_stream) for stream
//! reduction) and now absorbs the first production guardrail — **loop-guard**
//! ([`LoopGuard`]): the 3rd identical tool call in a turn is blocked (a `ToolResult`
//! error is fed back instead of executing), and 3 / 8 consecutive failures of
//! the same tool warn / halt the turn. The guard state is a local `LoopGuard`
//! that persists across steps within one `run` (matching `turn_loop`), and
//! guardrail status surfaces over the host's `Event` channel (`event_tx`) —
//! **not** via the framework `Callback`: guardrails are host-side concerns and
//! the `Callback` trait stays untouched per ROADMAP §E. `&self` still suffices
//! here — `LoopGuard` is local and `mpsc::Sender::send` takes `&self`; this slice
//! is the proof that local-state guardrails need no interior mutability. The
//! first guardrail needing `Engine` mutable state (e.g. LSP's `pending_lsp_blocks`,
//! steer's `rx_steer`) will introduce `Arc<Mutex<…>>` / channel handles. It is
//! **not yet wired into `handle_send_message`**; the production
//! `handle_deepseek_turn` remains the live path — the value of landing it now is
//! the composition proof (the three bridges light up end-to-end inside a real
//! `AgentExecutor::run` driving a real `ToolSpec` over a real `Session`; see the
//! headline test) plus the first guardrail absorbed at the per-tool / post-tool
//! seams below.
//!
//! ## Guardrail insertion points
//!
//! The loop has four seams where guardrails are absorbed incrementally:
//!
//! 1. **per-step pre-request** — compaction, capacity pre-request, steer drain,
//!    LSP flush, system-prompt refresh (top of the `loop`). *(not yet absorbed)*
//! 2. **per-step post-stream** — transparent stream-retry, subagent handoff,
//!    thinking-only handling (after `accumulate_stream`, before tool extraction).
//!    *(not yet absorbed)*
//! 3. **per-tool** — ✅ **loop-guard `record_attempt`** (block the 3rd identical
//!    call) + **`record_outcome`** (warn at 3 / halt at 8 consecutive failures);
//!    approval, early-tool-start, parallel dispatch still to come (inside the
//!    tool `for` loop).
//! 4. **per-step post-tool** — ✅ **loop-guard halt short-circuit** (returns
//!    `StopReason::Error`); capacity post-tool, LSP post-edit still to come
//!    (after the tool loop).
//!
//! Streaming deltas (`MessageDelta` / `ThinkingDelta`) will continue to flow
//! over the `Event` channel directly, emitted by an inline stream reducer (a
//! later slice replaces the `accumulate_stream` call) — they have no `Callback`
//! method and stay off the `Callback` path (see `callback_bridge` docs).
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use codesmith_agent::callback::{Callback, StopReason};
use codesmith_agent::executor::{accumulate_stream, AgentExecutor, AgentExecutorConfig};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message, MessageRequest};
use codesmith_agent::tools::{ToolError, ToolResult, ToolSet};

use super::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
use crate::events::Event;

/// The `ToolResult` fed back when the loop-guard blocks an identical repeat
/// call (mirrors `turn_loop::loop_guard_block_tool_result`). Duplicated here
/// rather than imported to keep this slice additive — zero production call-site
/// changes; a later cleanup can lift it into `loop_guard` proper as the single
/// source of truth.
fn block_tool_result(message: String) -> ToolResult {
    ToolResult::error(message).with_metadata(serde_json::json!({
        "loop_guard": "identical_tool_call"
    }))
}

/// Host-side [`AgentExecutor`] — the growing home for the production turn loop.
///
/// Construct from the four framework collaborators: an [`LlmClientHandle`], a
/// [`ToolSet`] (built via
/// [`ToolRegistry::to_framework_tool_set`](crate::tools::registry::ToolRegistry::to_framework_tool_set)
/// in production), a [`Callback`] (a [`CallbackBridge`](crate::callback_bridge::CallbackBridge)
/// in production), and an [`AgentExecutorConfig`]. The optional `event_tx`
/// surfaces guardrail status (e.g. loop-guard warn/halt) onto the host's UI
/// `Event` channel — distinct from the `Callback`, which carries the framework
/// loop's own tool-lifecycle hooks. Nothing is mutated on `self` per run; the
/// transcript is mutated in place through [`ChatHistory`].
pub struct HostAgentExecutor {
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
    event_tx: Option<mpsc::Sender<Event>>,
}

impl HostAgentExecutor {
    /// Construct from the four collaborators + config + an optional guardrail
    /// status channel (`None` for embeds that don't surface guardrail status).
    #[must_use]
    pub fn new(
        client: LlmClientHandle,
        tools: Arc<ToolSet>,
        callback: Arc<dyn Callback>,
        config: AgentExecutorConfig,
        event_tx: Option<mpsc::Sender<Event>>,
    ) -> Self {
        Self {
            client,
            tools,
            callback,
            config,
            event_tx,
        }
    }

    /// Surface a guardrail status message onto the host's UI `Event` channel,
    /// if one was supplied. Guardrails emit here directly rather than through
    /// the framework `Callback` (see the module docs).
    async fn emit_status(&self, message: String) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(Event::status(message)).await;
        }
    }
}

impl AgentExecutor for HostAgentExecutor {
    fn run<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason>> + Send + 'a>> {
        Box::pin(self.run_inner(history, user_text))
    }
}

impl HostAgentExecutor {
    /// The bare LLM↔tool loop. Mirrors `DefaultAgentExecutor::run_inner`; will
    /// grow guardrail seams at the four points noted in the module docs.
    async fn run_inner<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Result<StopReason> {
        // Cheap Arc clones so the loop body borrows locals, not `&self` fields.
        let client = self.client.clone();
        let tools = self.tools.clone();
        let callback = self.callback.clone();
        let max_steps = self.config.max_steps;
        let max_tokens = self.config.max_tokens;
        let system = self.config.system.clone();
        let temperature = self.config.temperature;

        // Seed the transcript with the user turn.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: user_text,
                cache_control: None,
            }],
        });

        // Loop-guard state persists across steps within this run (one
        // `LoopGuard` per turn, matching `turn_loop`).
        let mut loop_guard = LoopGuard::default();
        let mut step: u32 = 0;
        loop {
            // (1) per-step pre-request seam — compaction / capacity / steer /
            // LSP / cycle land here later.
            if step >= max_steps {
                callback.on_complete(&StopReason::MaxSteps).await;
                return Ok(StopReason::MaxSteps);
            }

            let api_tools = tools.to_api_tools();
            let request = MessageRequest {
                model: client.model().to_string(),
                messages: history.messages().to_vec(),
                max_tokens,
                system: system.clone(),
                tools: if api_tools.is_empty() {
                    None
                } else {
                    Some(api_tools)
                },
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: None,
                stream: Some(true),
                temperature,
                top_p: None,
            };

            callback.on_llm_start(&request).await;
            let stream = client.create_message_stream(request).await?;
            let (content, _stop_reason) = accumulate_stream(stream).await?;
            // (2) per-step post-stream seam — transparent-retry / subagent land here.
            callback.on_llm_end(&content).await;

            // Persist the assistant turn.
            history.push(Message {
                role: "assistant".to_string(),
                content: content.clone(),
            });

            // Collect tool calls (preserve order).
            let tool_uses: Vec<(String, String, serde_json::Value)> = content
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input, .. } => Some((id, name, input)),
                    _ => None,
                })
                .collect();

            if tool_uses.is_empty() {
                callback.on_complete(&StopReason::NoToolCalls).await;
                return Ok(StopReason::NoToolCalls);
            }

            // Execute each tool sequentially and feed the result back as a
            // `role:"user"` `ToolResult` block (Anthropic/OpenAI-compat shape).
            //
            // (3) per-tool seam — loop-guard (absorbed); approval /
            // early-tool-start / parallel land here later. `loop_guard_halt` is
            // per-step: a halt short-circuits the tool loop and the whole turn
            // at the (4) seam below.
            let mut loop_guard_halt: Option<String> = None;
            for (id, name, input) in tool_uses {
                callback.on_tool_start(&name, &input).await;
                // loop-guard: block the 3rd identical (name+args) call this turn.
                let (result, blocked) = match loop_guard.record_attempt(&name, &input) {
                    AttemptDecision::Block(message) => {
                        (Ok(block_tool_result(message)), true)
                    }
                    AttemptDecision::Proceed => (
                        match tools.get(&name) {
                            Some(tool) => tool.run(input.clone()).await,
                            None => Err(ToolError::NotAvailable {
                                message: format!("no tool named '{name}'"),
                            }),
                        },
                        false,
                    ),
                };
                callback.on_tool_end(&name, &result).await;

                // loop-guard: track consecutive failures of this tool (warn at
                // 3, halt at 8). A guard-blocked call records no outcome — it
                // is an intervention, not an execution, so it doesn't count
                // toward the failure halt.
                if !blocked {
                    let success = result.as_ref().map(|r| r.success).unwrap_or(false);
                    match loop_guard.record_outcome(&name, success) {
                        OutcomeDecision::Continue => {}
                        OutcomeDecision::Warn(message) => {
                            tracing::warn!("{}", message);
                            self.emit_status(message).await;
                        }
                        OutcomeDecision::Halt(message) => {
                            loop_guard_halt.get_or_insert(message);
                        }
                    }
                }

                let (content_str, is_error) = match &result {
                    Ok(r) => (r.content.clone(), !r.success),
                    Err(e) => (format!("Error: {e}"), true),
                };
                history.push(Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: content_str,
                        is_error: Some(is_error),
                        content_blocks: None,
                    }],
                });
            }

            // (4) per-step post-tool seam — loop-guard halt (absorbed);
            // capacity / LSP post-edit land here later.
            if let Some(message) = loop_guard_halt {
                tracing::warn!("{}", message);
                self.emit_status(message.clone()).await;
                callback
                    .on_complete(&StopReason::Error(message.clone()))
                    .await;
                return Ok(StopReason::Error(message));
            }

            callback.on_step(step).await;
            step += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callback_bridge::CallbackBridge;
    use crate::events::Event;
    use crate::hooks::{HookContext, HookEvent, HookHost, HookResult, MessageSubmitOutcome};
    use crate::session::Session;
    use crate::session_history::SessionChatHistory;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::spec::{ToolContext, ToolSpec};
    use codesmith_agent::llm_client::{LlmClient, StreamEventBox};
    use codesmith_agent::models::{ContentBlockStart, Delta, MessageDelta, StreamEvent};
    use codesmith_agent::tools::{ToolCapability, ToolError, ToolResult};
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    // === test doubles =======================================================

    /// A `ToolSpec` that echoes its `text` input, stamped with the captured
    /// workspace path so tests can prove the context flowed through the adapter.
    /// (Mirrors `framework_adapter` tests' `EchoSpec`.)
    struct EchoSpec;

    #[async_trait::async_trait]
    impl ToolSpec for EchoSpec {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input, stamped with the workspace path."
        }
        fn input_schema(&self) -> serde_json::Value {
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
            input: serde_json::Value,
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

    /// A `LlmClient` that pops canned `StreamEvent` lists from a queue, one
    /// per `create_message_stream` call. (Mirrors the bridge tests' `MockLlm`.)
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
        ) -> Pin<Box<dyn Future<Output = Result<codesmith_agent::models::MessageResponse>> + Send + '_>>
        {
            Box::pin(async { anyhow::bail!("mock does not implement create_message") })
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            let next = self.calls.lock().unwrap().pop_front();
            Box::pin(async move {
                let events = next.unwrap_or_default();
                Ok(Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
                    as StreamEventBox)
            })
        }
    }

    /// A `HookHost` test double that records every `execute` call.
    /// (Mirrors `callback_bridge` tests' `RecordingHookHost`.)
    #[derive(Default)]
    struct RecordingHookHost {
        calls: std::sync::Arc<Mutex<Vec<(HookEvent, HookContext)>>>,
    }

    impl RecordingHookHost {
        fn calls(&self) -> Vec<(HookEvent, HookContext)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl HookHost for RecordingHookHost {
        fn execute(&self, event: HookEvent, context: &HookContext) -> Vec<HookResult> {
            self.calls
                .lock()
                .unwrap()
                .push((event, context.clone()));
            Vec::new()
        }
        fn execute_pre_compact_hook(&self, _context: &HookContext) -> Option<String> {
            None
        }
        fn execute_message_submit_transform(
            &self,
            _context: &HookContext,
            _original_text: &str,
        ) -> MessageSubmitOutcome {
            MessageSubmitOutcome::unchanged()
        }
        fn has_hooks_for_event(&self, _event: HookEvent) -> bool {
            true
        }
        fn is_enabled(&self) -> bool {
            true
        }
        fn session_id(&self) -> &str {
            "test"
        }
        fn collect_shell_env(&self, _context: &HookContext) -> HashMap<String, String> {
            HashMap::new()
        }
    }

    fn test_template() -> HookContext {
        HookContext::new()
            .with_session_id("test")
            .with_workspace(PathBuf::from("/tmp/codesmith-test"))
            .with_model("mock-v0")
    }

    // === stream-event builders (mirroring the bridge tests) =================

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

    /// Drain all events currently buffered in `rx` into a `Vec`.
    fn drain(rx: &mut mpsc::Receiver<Event>) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    fn fresh_session() -> Session {
        Session::new(
            "mock-v0".to_string(),
            PathBuf::from("/tmp/codesmith-test"),
            false,
            false,
            PathBuf::from("/tmp/codesmith-test/notes.md"),
            PathBuf::from("/tmp/codesmith-test/mcp.json"),
        )
    }

    // === tests ==============================================================

    #[tokio::test]
    async fn host_executor_drives_full_bridge_trio() {
        // Registry with a real ToolSpec → framework ToolSet via the adapter.
        let tmp = tempdir().expect("tempdir");
        let workspace_stamp = tmp.path().display().to_string();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        // Real Session → framework ChatHistory.
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);

        // CallbackBridge: mock Event channel + RecordingHookHost.
        let (tx, mut rx) = mpsc::channel(256);
        let hooks = Arc::new(RecordingHookHost::default());
        let callback: Arc<dyn Callback> = Arc::new(CallbackBridge::new(
            Some(tx),
            Some(hooks.clone()),
            test_template(),
        ));

        // Call 1: text + tool_use(echo). Call 2: text-only -> NoToolCalls.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
        );

        let reason = executor
            .run(&mut history, "echo world".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, assistant(text+tooluse), user(toolresult), assistant(text)]
        assert_eq!(history.len(), 4);
        // The same bytes live on the underlying Session.
        assert_eq!(sess.messages.len(), 4);

        // The ToolResult carries the captured context's workspace path —
        // proof the ToolSpec flowed through ToolSpecAdapter into the loop.
        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert!(
                    content.starts_with(&workspace_stamp),
                    "context stamped: {content}"
                );
                assert!(content.ends_with("|world"));
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        // Event channel: ToolCallStarted + ToolCallComplete with matching ids.
        let events = drain(&mut rx);
        let started = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallStarted { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .expect("ToolCallStarted emitted");
        let complete = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallComplete { id, name, result } => {
                    Some((id.clone(), name.clone(), result.clone()))
                }
                _ => None,
            })
            .expect("ToolCallComplete emitted");
        assert_eq!(started.0, complete.0, "bridge ids correlate");
        assert_eq!(started.1, "echo");
        assert_eq!(started.2, serde_json::json!({"text":"world"}));
        assert_eq!(complete.1, "echo");
        match complete.2 {
            Ok(r) => {
                assert!(r.content.ends_with("|world"));
                assert!(r.success);
            }
            Err(e) => panic!("expected Ok ToolResult, got Err: {e}"),
        }

        // HookHost: ToolCallBefore + ToolCallAfter with full context.
        let calls = hooks.calls();
        assert_eq!(calls.len(), 2, "one Before + one After");
        assert_eq!(calls[0].0, HookEvent::ToolCallBefore);
        assert_eq!(calls[1].0, HookEvent::ToolCallAfter);
        assert_eq!(calls[0].1.tool_name.as_deref(), Some("echo"));
        assert_eq!(calls[0].1.session_id.as_deref(), Some("test"));
        assert_eq!(calls[1].1.tool_name.as_deref(), Some("echo"));
        assert_eq!(calls[1].1.tool_result.as_deref().unwrap().ends_with("|world"), true);
        assert_eq!(calls[1].1.tool_success, Some(true));
    }

    #[tokio::test]
    async fn host_executor_missing_tool_records_error_result() {
        // Empty ToolSet -> "ghost" lookup fails with NotAvailable.
        let tools = Arc::new(ToolSet::new());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call = text_block(0, "calling ghost");
        call.extend(tool_use_block(1, "t1", "ghost", r#"{}"#));
        call.extend(finish("tool_use"));
        let mut finish_call = text_block(0, "ok");
        finish_call.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call, finish_call])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert!(content.starts_with("Error:"));
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn host_executor_exhausts_steps() {
        // Mock always returns a tool call -> hits MaxSteps.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let make_call = || {
            let mut c = text_block(0, "looping");
            c.extend(tool_use_block(1, "t1", "echo", r#"{"text":"x"}"#));
            c.extend(finish("tool_use"));
            c
        };

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![make_call(), make_call(), make_call()])),
            tools,
            callback,
            AgentExecutorConfig {
                max_steps: 2,
                ..AgentExecutorConfig::default()
            },
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::MaxSteps);
        // user + (assistant + toolresult) x2 = 1 + 2*2 = 5
        assert_eq!(history.len(), 5);
    }

    // === loop-guard (seam 3 + 4) ===========================================

    #[tokio::test]
    async fn loop_guard_blocks_third_identical_call() {
        let tmp = tempdir().expect("tempdir");
        let workspace_stamp = tmp.path().display().to_string();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Three identical echo calls, then a text-only turn that ends the run.
        let call = || {
            let mut c = text_block(0, "again");
            c.extend(tool_use_block(1, "t1", "echo", r#"{"text":"x"}"#));
            c.extend(finish("tool_use"));
            c
        };
        let mut done = text_block(0, "done");
        done.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call(), call(), call(), done])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, asst, tr(echo), asst, tr(echo), asst, tr(block), asst] = 8.
        assert_eq!(history.len(), 8);

        // First two tool results are real echo output (workspace-stamped) —
        // proof the tool actually ran twice.
        for &idx in &[2usize, 4] {
            match &sess.messages[idx].content[0] {
                ContentBlock::ToolResult { content, is_error, .. } => {
                    assert!(
                        content.starts_with(&workspace_stamp),
                        "echo ran at msg[{idx}]: {content}"
                    );
                    assert_eq!(*is_error, Some(false));
                }
                other => panic!("msg[{idx}] not ToolResult: {other:?}"),
            }
        }
        // Third is the loop-guard block — echo did NOT run, error, block message.
        match &sess.messages[6].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert!(
                    !content.starts_with(&workspace_stamp),
                    "echo must not run on the blocked call: {content}"
                );
                assert!(
                    content.contains("already been made 3 times"),
                    "block message: {content}"
                );
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("msg[6] not ToolResult: {other:?}"),
        }
    }

    #[tokio::test]
    async fn loop_guard_warns_at_three_failures() {
        // No tools registered — every tool call hits "ghost" (NotAvailable),
        // which counts as a failure for the loop-guard.
        let tools = Arc::new(ToolSet::new());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Vary the args each call so `record_attempt` (keyed on name+args) never
        // blocks; `record_outcome` is keyed on name only, so failures still
        // accumulate toward the warn threshold (3).
        let failing = |n: u64| {
            let mut c = text_block(0, "trying");
            c.extend(tool_use_block(1, "t1", "ghost", &format!(r#"{{"n":{n}}}"#)));
            c.extend(finish("tool_use"));
            c
        };
        let mut done = text_block(0, "done");
        done.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![failing(1), failing(2), failing(3), done])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        let events = drain(&mut rx);
        let warned = events.iter().any(|e| {
            matches!(
                e,
                Event::Status { message } if message.contains("failed 3 consecutive times")
            )
        });
        assert!(warned, "expected a warn status event, got: {events:?}");
    }

    #[tokio::test]
    async fn loop_guard_halts_after_eight_failures() {
        let tools = Arc::new(ToolSet::new()); // ghost

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let failing = |n: u64| {
            let mut c = text_block(0, "trying");
            c.extend(tool_use_block(1, "t1", "ghost", &format!(r#"{{"n":{n}}}"#)));
            c.extend(finish("tool_use"));
            c
        };
        // 8 distinct-arg failures → the 8th triggers Halt.
        let calls: Vec<Vec<StreamEvent>> = (1..=8).map(failing).collect();

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(calls)),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        let msg = match reason {
            StopReason::Error(m) => m,
            other => panic!("expected Error, got {other:?}"),
        };
        assert!(
            msg.contains("failed 8 consecutive times"),
            "halt message: {msg}"
        );

        let events = drain(&mut rx);
        let halted = events.iter().any(|e| {
            matches!(
                e,
                Event::Status { message } if message.contains("failed 8 consecutive times")
            )
        });
        assert!(halted, "expected a halt status event, got: {events:?}");
    }
}
