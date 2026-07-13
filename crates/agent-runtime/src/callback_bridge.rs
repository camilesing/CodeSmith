//! Bridge the host's `mpsc::Sender<Event>` UI channel + `HookHost` shell-command
//! surface onto the framework-core [`Callback`].
//!
//! The framework-core [`Callback`] trait (LangChain `Callbacks` analog, in
//! `codesmith-agent`) is the executor's observation seam — a host-agnostic set
//! of async hooks the agent loop fires at each phase. The production `Engine`
//! in this crate has two richer, host-specific observation paths instead: a
//! [`mpsc::Sender<Event>`](tokio::sync::mpsc::Sender) push channel that streams
//! fine-grained UI events (`MessageDelta`, `ToolCallStarted`, …), and a
//! [`HookHost`] trait that spawns user-configured shell commands at lifecycle
//! points (`ToolCallBefore` / `ToolCallAfter` / …). [`CallbackBridge`] closes
//! that gap: it is a `Callback` impl that forwards the tool-lifecycle hooks
//! onto **both** existing paths, so a framework executor driving a real
//! `ToolSet` still lights up the UI and fires user hooks through one seam.
//!
//! ## Bridged vs. documented gaps
//!
//! The bridge is **intentionally partial** — it forwards only the Callback
//! methods with a clean 1:1 host mapping, and leaves the rest to the trait's
//! default no-ops. This matches the ROADMAP §E gap analysis:
//!
//! | Callback method       | `Event` channel            | `HookHost`            |
//! |-----------------------|----------------------------|-----------------------|
//! | `on_tool_start`       | `ToolCallStarted`          | `ToolCallBefore`      |
//! | `on_tool_end`         | `ToolCallComplete`         | `ToolCallAfter`       |
//! | `on_llm_start`        | — (no precise event¹)      | — (no LLM-start hook) |
//! | `on_llm_end`          | — (content not on wire²)   | — (no LLM-end hook)   |
//! | `on_step`             | — (no step event variant)  | —                     |
//! | `on_complete`         | — (`TurnComplete`³)         | —                     |
//! | `on_stream_delta`     | `MessageDelta` / `ThinkingDelta` / `MessageStarted` / `ThinkingStarted` / `ThinkingComplete` / `MessageComplete` / `ToolCallStarted` | — |
//!
//! ¹ `TurnStarted` only carries a turn id and is emitted by the engine caller,
//!   not the executor loop. ² `MessageComplete` only carries a block index; the
//!   assembled content lives in the engine, not on the wire, and the block
//!   index is owned by the stream-reduction code. ³ `TurnComplete` carries full
//!   `usage` / `tool_catalog` / `base_url` the `Callback` does not have; the
//!   engine caller emits it after the executor returns, so the bridge does not
//!   duplicate it.
//!
//! Streaming deltas (`MessageDelta` / `ThinkingDelta`) and block-lifecycle
//! events (`MessageStarted` / `ThinkingStarted` / `ThinkingComplete` /
//! `MessageComplete` / `ToolCallStarted`) flow through the `on_stream_delta`
//! hook (§E inline-stream-reduction + block-lifecycle + tool-call-start slices).
//! The bridge maps [`StreamDelta::Text`] → `Event::MessageDelta`,
//! [`StreamDelta::Thinking`] → `Event::ThinkingDelta`, the four lifecycle
//! `StreamDelta` variants → their same-named `Event` variants, and
//! [`StreamDelta::ToolCallStarted`] → `Event::ToolCallStarted` (with the real
//! wire tool-call id), forwarding each to the `Event` channel as it arrives
//! (lifecycle events fire at `ContentBlockStart` / `ContentBlockStop` in the
//! inline reducer).
//!
//! ## Tool-call id passthrough + dedup
//!
//! The framework `Callback::on_tool_start` carries the wire tool-call `id`
//! (from `ContentBlock::ToolUse { id, .. }`), so the bridge no longer
//! synthesizes `bridge-{n}` ids — the real wire id flows through to both
//! `Event::ToolCallStarted` and `Event::ToolCallComplete`. When the inline
//! reducer emits `StreamDelta::ToolCallStarted` at stream-time
//! (`ContentBlockStop` for tool blocks), the bridge marks the id as
//! "announced"; the execute-time `on_tool_start` then skips re-emitting
//! `Event::ToolCallStarted` (dedup). If no stream-time emission occurred
//! (e.g. the CORE `DefaultAgentExecutor` with `accumulate_stream`), the
//! execute-time `on_tool_start` sends `Event::ToolCallStarted` as a fallback.
//! The stashed start `input` is also replayed into the `ToolCallAfter` hook
//! context — the framework `on_tool_end(name, result)` signature is input-less,
//! but the production `execute_post_tool_hook` needs `tool_args`, so the pending
//! stack bridges that too.
//!
//! This is the "land the bridge" step of ROADMAP §E — the production
//! `Engine`/`turn_loop` migration onto `AgentExecutor` (which would construct
//! this bridge per turn and hand it to the executor) is a later slice. Leaf
//! types (`ToolResult` / `ToolError`) are already shared via `codesmith-tools`,
//! so `Event::ToolCallComplete { result }` type-checks with no translation.
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use codesmith_agent::callback::{Callback, StreamDelta};
use codesmith_tools::{ToolError, ToolResult};

use crate::events::Event;
use crate::hooks::{HookContext, HookEvent, HookHost};

/// Mutable per-bridge state behind a `Mutex` (the `Callback` methods take
/// `&self`, so interior mutability is required, matching the executor's own
/// `RecordingCallback` test double).
#[derive(Debug, Default)]
struct BridgeState {
    /// Tool-call ids already announced at stream-time (via
    /// `StreamDelta::ToolCallStarted`). When `on_tool_start` fires at
    /// execute-time, it checks this set and skips re-emitting
    /// `Event::ToolCallStarted` if the id is present — deduplicating the
    /// stream-time and execute-time announcements.
    announced: std::collections::HashSet<String>,
    /// LIFO of pending tool calls: `(wire_id, stashed_input)`. Pushed on
    /// `on_tool_start`, popped on `on_tool_end` so the end event pairs with the
    /// most recent start and `tool_args` can be replayed into the
    /// `ToolCallAfter` hook context.
    pending: Vec<(String, serde_json::Value)>,
}

/// A [`Callback`] that forwards tool-lifecycle hooks onto the host's
/// `mpsc::Sender<Event>` UI channel and `HookHost` shell-hook surface.
///
/// Construct one per turn (or per executor) with the turn-level
/// [`HookContext`] fields pre-filled (`session_id` / `workspace` / `model` /
/// `thread_id` / `tokens` / `mode`); the bridge clones and enriches it per
/// call with `tool_name` / `tool_args` / `tool_result`, mirroring the
/// production `build_tool_hook_context` + `execute_pre/post_tool_hook`
/// helpers. See the module docs for the bridged-vs-gap table and the
/// synthesized-id rationale.
pub struct CallbackBridge {
    /// UI event channel. `None` disables `Event` emission (hooks still fire).
    tx: Option<mpsc::Sender<Event>>,
    /// Shell-command hook surface. `None` disables hook emission.
    hooks: Option<Arc<dyn HookHost>>,
    /// Turn-level `HookContext` template; cloned + enriched per tool call.
    hook_template: HookContext,
    /// Id counter + pending start↔end correlation stack.
    state: Arc<Mutex<BridgeState>>,
}

impl CallbackBridge {
    /// Build a bridge from the two host paths + a turn-level `HookContext`
    /// template (pre-filled with `session_id` / `workspace` / `model` / …).
    #[must_use]
    pub fn new(
        tx: Option<mpsc::Sender<Event>>,
        hooks: Option<Arc<dyn HookHost>>,
        hook_template: HookContext,
    ) -> Self {
        Self {
            tx,
            hooks,
            hook_template,
            state: Arc::new(Mutex::new(BridgeState::default())),
        }
    }
}

impl Callback for CallbackBridge {
    fn on_tool_start<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        input: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        // Clone the `&self` fields into owned locals (cheap: `Sender` and
        // `HookContext` are `Clone`, `hooks`/`state` are `Arc`), then capture
        // `id`/`name`/`input` by borrow — mirrors `CallbackSet`'s pattern so
        // the boxed future neither borrows `&self` nor ties the args to a
        // narrower lifetime than `'a`.
        let tx = self.tx.clone();
        let hooks = self.hooks.clone();
        let template = self.hook_template.clone();
        let state = self.state.clone();
        Box::pin(async move {
            // Stash the wire id + input so `on_tool_end` can both pair the end
            // event (real wire id) and replay `tool_args` into the
            // `ToolCallAfter` hook context. If this id was already announced at
            // stream-time (via `StreamDelta::ToolCallStarted`), skip the
            // execute-time `Event::ToolCallStarted` — dedup.
            let already_announced = {
                let mut s = state.lock().expect("bridge state mutex poisoned");
                s.pending.push((id.to_string(), input.clone()));
                !s.announced.insert(id.to_string())
            };

            if !already_announced {
                if let Some(tx) = tx.as_ref() {
                    let _ = tx
                        .send(Event::ToolCallStarted {
                            id: id.to_string(),
                            name: name.to_string(),
                            input: input.clone(),
                        })
                        .await;
                }
            }

            if let Some(hooks) = hooks.as_ref() {
                if hooks.has_hooks_for_event(HookEvent::ToolCallBefore) {
                    let ctx = template
                        .clone()
                        .with_tool_name(name)
                        .with_tool_args(input);
                    let _ = hooks.execute(HookEvent::ToolCallBefore, &ctx);
                }
            }
        })
    }

    fn on_tool_end<'a>(
        &'a self,
        name: &'a str,
        result: &'a Result<ToolResult, ToolError>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let tx = self.tx.clone();
        let hooks = self.hooks.clone();
        let template = self.hook_template.clone();
        let state = self.state.clone();
        Box::pin(async move {
            // Pop the most recent start: its wire id pairs the end event,
            // and its stashed input fills `tool_args` (the `on_tool_end`
            // signature is input-less, but `ToolCallAfter` hooks want it).
            let (id, input) = state
                .lock()
                .expect("bridge state mutex poisoned")
                .pending
                .pop()
                .unwrap_or_else(|| ("bridge-?".to_string(), serde_json::Value::Null));

            if let Some(tx) = tx.as_ref() {
                let _ = tx
                    .send(Event::ToolCallComplete {
                        id: id.clone(),
                        name: name.to_string(),
                        result: result.clone(),
                    })
                    .await;
            }

            if let Some(hooks) = hooks.as_ref() {
                if hooks.has_hooks_for_event(HookEvent::ToolCallAfter) {
                    let (text, success) = match result {
                        Ok(r) => (r.content.clone(), r.success),
                        Err(e) => (e.to_string(), false),
                    };
                    let ctx = template
                        .clone()
                        .with_tool_name(name)
                        .with_tool_args(&input)
                        .with_tool_result(&text, success, None);
                    let _ = hooks.execute(HookEvent::ToolCallAfter, &ctx);
                }
            }
        })
    }

    fn on_stream_delta<'a>(
        &'a self,
        delta: &'a StreamDelta,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let tx = self.tx.clone();
        let state = self.state.clone();
        Box::pin(async move {
            let Some(tx) = tx.as_ref() else {
                return;
            };
            let event = match delta {
                StreamDelta::Text { index, content } => Event::MessageDelta {
                    index: *index,
                    content: content.clone(),
                },
                StreamDelta::Thinking { index, content } => Event::ThinkingDelta {
                    index: *index,
                    content: content.clone(),
                },
                StreamDelta::MessageStarted { index } => Event::MessageStarted { index: *index },
                StreamDelta::ThinkingStarted { index } => {
                    Event::ThinkingStarted { index: *index }
                }
                StreamDelta::ThinkingComplete { index } => {
                    Event::ThinkingComplete { index: *index }
                }
                StreamDelta::MessageComplete { index } => {
                    Event::MessageComplete { index: *index }
                }
                StreamDelta::ToolCallStarted { id, name, input } => {
                    // Mark this id as announced so the execute-time
                    // `on_tool_start` skips re-emitting `Event::ToolCallStarted`
                    // (dedup — the stream-time emission is the single source).
                    {
                        let mut s = state.lock().expect("bridge state mutex poisoned");
                        s.announced.insert(id.clone());
                    }
                    Event::ToolCallStarted {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }
                }
            };
            let _ = tx.send(event).await;
        })
    }

    // `on_llm_start`, `on_llm_end`, `on_step`, `on_complete`: intentionally
    // un-overridden — see the "Bridged vs. documented gaps" table in the module
    // docs. The trait's default no-ops apply; the host's `TurnStarted` /
    // `TurnComplete` events are emitted directly by the engine caller, not
    // through this bridge.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{HookResult, MessageSubmitOutcome};
    use codesmith_agent::executor::{AgentExecutor, AgentExecutorConfig, DefaultAgentExecutor};
    use codesmith_agent::llm_client::{LlmClient, LlmClientHandle, StreamEventBox};
    use codesmith_agent::memory::{ChatHistory, VecChatHistory};
    use codesmith_agent::models::{
        ContentBlock, ContentBlockStart, Delta, MessageDelta, MessageRequest, StreamEvent,
    };
    use codesmith_agent::tools::{Tool as FrameworkTool, ToolSet};
    use std::collections::{HashMap, VecDeque};

    // === RecordingHookHost ===================================================

    /// A `HookHost` test double that records every `execute` call (event +
    /// context) and returns empty results for the rest. `has_hooks_for_event`
    /// reports `true` so the bridge's gating lets `execute` through.
    #[derive(Default)]
    struct RecordingHookHost {
        calls: Arc<Mutex<Vec<(HookEvent, HookContext)>>>,
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

    /// A turn-level `HookContext` template for tests (session id + workspace).
    fn test_template() -> HookContext {
        HookContext::new()
            .with_session_id("test")
            .with_workspace(std::path::PathBuf::from("/tmp/codesmith-test"))
            .with_model("mock-v0")
    }

    // === mock LLM + stream builders (mirroring framework_adapter tests) ======

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
                Ok(Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
                    as StreamEventBox)
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

    fn tool_use_block(idx: u32, _id: &str, name: &str, input_json: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::ToolUse {
                    id: "unused-wire-id".to_string(),
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

    // === tests ===============================================================

    #[tokio::test]
    async fn bridge_forwards_tool_start_and_end_to_event_channel_and_hooks() {
        let (tx, mut rx) = mpsc::channel(256);
        let hooks = Arc::new(RecordingHookHost::default());
        let bridge = CallbackBridge::new(Some(tx), Some(hooks.clone()), test_template());

        let input = serde_json::json!({"text":"world"});
        bridge.on_tool_start("wire-1", "echo", &input).await;
        let result = Ok(ToolResult {
            content: "echo:world".to_string(),
            success: true,
            metadata: None,
        });
        bridge.on_tool_end("echo", &result).await;

        // Event channel: start + complete with matching wire ids.
        let events = drain(&mut rx);
        let (started, complete) = match (events.first(), events.get(1)) {
            (Some(Event::ToolCallStarted { .. }), Some(Event::ToolCallComplete { .. })) => {
                (events[0].clone(), events[1].clone())
            }
            _ => panic!("expected ToolCallStarted then ToolCallComplete, got {events:?}"),
        };
        let (s_id, s_name, s_input) = match started {
            Event::ToolCallStarted { id, name, input } => (id, name, input),
            _ => unreachable!(),
        };
        let (c_id, c_name, c_result) = match complete {
            Event::ToolCallComplete { id, name, result } => (id, name, result),
            _ => unreachable!(),
        };
        assert_eq!(s_id, "wire-1", "wire id passthrough: {s_id}");
        assert_eq!(s_id, c_id, "start/end ids must correlate");
        assert_eq!(s_name, "echo");
        assert_eq!(s_input, input);
        assert_eq!(c_name, "echo");
        // `ToolResult`/`ToolError` (codesmith-tools) deliberately don't impl
        // `PartialEq`, so assert on the fields rather than `assert_eq!` the
        // whole `Result`.
        match c_result {
            Ok(r) => {
                assert_eq!(r.content, "echo:world");
                assert!(r.success);
                assert!(r.metadata.is_none());
            }
            Err(e) => panic!("expected Ok ToolResult, got Err: {e}"),
        }

        // HookHost: ToolCallBefore then ToolCallAfter, with template + call
        // fields populated.
        let calls = hooks.calls();
        assert_eq!(calls.len(), 2);
        let (before_evt, before_ctx) = &calls[0];
        let (after_evt, after_ctx) = &calls[1];
        assert_eq!(*before_evt, HookEvent::ToolCallBefore);
        assert_eq!(*after_evt, HookEvent::ToolCallAfter);

        // Before: tool_name + tool_args + turn-level template (session_id).
        assert_eq!(before_ctx.tool_name.as_deref(), Some("echo"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                before_ctx.tool_args.as_deref().unwrap_or("")
            )
            .unwrap(),
            input
        );
        assert_eq!(before_ctx.session_id.as_deref(), Some("test"));
        assert!(before_ctx.tool_result.is_none(), "no result on Before");

        // After: tool_name + tool_args (replayed from stash) + tool_result.
        assert_eq!(after_ctx.tool_name.as_deref(), Some("echo"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                after_ctx.tool_args.as_deref().unwrap_or("")
            )
            .unwrap(),
            input
        );
        assert_eq!(after_ctx.tool_result.as_deref(), Some("echo:world"));
        assert_eq!(after_ctx.tool_success, Some(true));
    }

    #[tokio::test]
    async fn bridge_emits_events_even_without_hooks() {
        let (tx, mut rx) = mpsc::channel(256);
        // No HookHost — hooks path is disabled.
        let bridge = CallbackBridge::new(Some(tx), None, test_template());

        let input = serde_json::json!({"text":"hi"});
        bridge.on_tool_start("wire-1", "echo", &input).await;
        bridge
            .on_tool_end(
                "echo",
                &Ok(ToolResult {
                    content: "echo:hi".to_string(),
                    success: true,
                    metadata: None,
                }),
            )
            .await;

        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ToolCallStarted { name, .. } if name == "echo"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ToolCallComplete { name, .. } if name == "echo"))
        );
    }

    #[tokio::test]
    async fn bridge_forwards_stream_deltas_to_event_channel() {
        let (tx, mut rx) = mpsc::channel(256);
        let bridge = CallbackBridge::new(Some(tx), None, test_template());

        // Emit a text delta then a thinking delta — the bridge should map them
        // to Event::MessageDelta and Event::ThinkingDelta respectively.
        bridge
            .on_stream_delta(&StreamDelta::Text {
                index: 0,
                content: "hello ".to_string(),
            })
            .await;
        bridge
            .on_stream_delta(&StreamDelta::Text {
                index: 0,
                content: "world".to_string(),
            })
            .await;
        bridge
            .on_stream_delta(&StreamDelta::Thinking {
                index: 1,
                content: "pondering".to_string(),
            })
            .await;

        let events = drain(&mut rx);
        assert_eq!(events.len(), 3, "three deltas → three events");
        match &events[0] {
            Event::MessageDelta { index, content } => {
                assert_eq!(*index, 0);
                assert_eq!(content, "hello ");
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
        match &events[1] {
            Event::MessageDelta { index, content } => {
                assert_eq!(*index, 0);
                assert_eq!(content, "world");
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
        match &events[2] {
            Event::ThinkingDelta { index, content } => {
                assert_eq!(*index, 1);
                assert_eq!(content, "pondering");
            }
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_stream_delta_noop_without_tx() {
        // No tx → on_stream_delta is a silent no-op (no panic).
        let bridge = CallbackBridge::new(None, None, test_template());
        bridge
            .on_stream_delta(&StreamDelta::Text {
                index: 0,
                content: "ghost".to_string(),
            })
            .await;
    }

    // === Executor integration ================================================
    //
    // Drives a real tool-call roundtrip through the framework-core
    // `DefaultAgentExecutor` with the bridge as its `Callback`, proving a
    // single seam lights up both the UI event channel and the shell hooks.
    // Mirrors `framework_adapter::executor_drives_toolspec_through_adapter`.

    struct EchoTool;
    impl FrameworkTool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes the input."
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
                    content: format!("echo:{text}"),
                    success: true,
                    metadata: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn executor_drives_callback_bridge() {
        let (tx, mut rx) = mpsc::channel(256);
        let hooks = Arc::new(RecordingHookHost::default());
        let bridge = Arc::new(CallbackBridge::new(
            Some(tx),
            Some(hooks.clone()),
            test_template(),
        ));

        let mut tools = ToolSet::new();
        tools.register(Arc::new(EchoTool));

        // Call 1: text + tool_use(echo). Call 2: text-only -> NoToolCalls.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let executor = DefaultAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            Arc::new(tools),
            bridge,
            AgentExecutorConfig::default(),
        );

        let mut history = VecChatHistory::new();
        let reason = executor
            .run(&mut history, "echo world".to_string())
            .await
            .expect("run");
        assert_eq!(reason, codesmith_agent::callback::StopReason::NoToolCalls);

        // The ToolResult fed back carries the tool's output.
        assert_eq!(history.len(), 4);
        match &history.messages()[2].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert_eq!(content, "echo:world");
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
        assert_eq!(started.0, complete.0, "ids correlate");
        assert_eq!(started.1, "echo");
        assert_eq!(started.2, serde_json::json!({"text":"world"}));
        assert_eq!(complete.1, "echo");
        match complete.2 {
            Ok(r) => {
                assert_eq!(r.content, "echo:world");
                assert!(r.success);
                assert!(r.metadata.is_none());
            }
            Err(e) => panic!("expected Ok ToolResult, got Err: {e}"),
        }

        // HookHost: ToolCallBefore + ToolCallAfter with full context.
        let calls = hooks.calls();
        assert_eq!(calls.len(), 2, "one Before + one After");
        assert_eq!(calls[0].0, HookEvent::ToolCallBefore);
        assert_eq!(calls[1].0, HookEvent::ToolCallAfter);
        assert_eq!(calls[0].1.tool_name.as_deref(), Some("echo"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                calls[1].1.tool_args.as_deref().unwrap_or("")
            )
            .unwrap(),
            serde_json::json!({"text":"world"})
        );
        assert_eq!(calls[1].1.tool_result.as_deref(), Some("echo:world"));
        assert_eq!(calls[1].1.tool_success, Some(true));
        // Turn-level template field flowed through to both hook contexts.
        assert_eq!(calls[0].1.session_id.as_deref(), Some("test"));
    }
}
