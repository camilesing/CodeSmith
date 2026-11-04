//! Sub-agent turns on the one loop — the child-turn seam (DSH alignment,
//! iteration B: one factory makes every agent).
//!
//! The host (the CLI's spawn path) knows what a child is *for*: its
//! prompt, its narrowed tool surface, its model route, its channels. It
//! hands that over as a [`ChildTurnSpec`] — plain data. [`run_child_turn`]
//! owns what makes it a *turn*: it resolves the configured `turn_executor`
//! factory from the same registry the main turn uses (one selection for
//! every agent), acquires the child's [`TurnLease`](crate::turn_lease::TurnLease),
//! builds a child-shaped `TurnDeps` (`TurnShape::Child`) and runs the
//! built executor over the child's transcript. The loop is the main turn's
//! loop — streaming with the idle watchdog, transparent retry, the loop
//! guard, the truncation gate, parallel batches — and its extension
//! dispatch runs at the child's agent-type scope, where the runner skips
//! dylib-origin handlers and around wrappers (5b: dylib code never enters a
//! sub-agent, so the lease declares `dylib_exposed: false`).
//!
//! What a child turn does **not** get (known limitations, by design):
//! - the main turn's session services: compaction and the capacity gates,
//!   LSP diagnostics, `<turn_meta>`, the plan-session gate, the result
//!   claim verifier, prefix-stability observation and the session journal
//!   (sub-agents keep no journal; the host stores the final transcript);
//! - an approval prompt or `request_user_input`: a child never asks through
//!   the loop (DSH: delegated approval is "never"). Its tool surface
//!   carries the delegation policy and asks out-of-workspace path questions
//!   through the parent's approval router itself;
//! - sub-agent completion delivery or the blocking hold: only the main
//!   turn's direct children report back on the completion channel;
//! - live per-step token usage: the executor reports none per step, so the
//!   mailbox gets one `TokenUsage` with the run's total at the end.
//!
//! Composition parity with the main turn (iteration C): the child's turn is
//! wrapped by the around-turn chain at its scope (dylib-origin wrappers
//! never wrap a child), and its assistant text deltas dispatch
//! `assistant-stream` through the budgeted observer at the same scope.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use codesmith_agent::callback::{Callback, StopReason};
use codesmith_agent::executor::AgentExecutorConfig;
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::memory::{ChatHistory, VecChatHistory};
use codesmith_agent::models::{ContentBlock, Message, MessageRequest, SystemPrompt, Tool, Usage};
use codesmith_agent::tools::{ToolError, ToolResult, ToolSet};
use codesmith_extensions::ExtensionRunner;

use super::executor_composition::{TurnDeps, TurnShape, resolve_turn_executor};
use crate::events::Event;
use crate::host_services::SubAgentApi;
use crate::mailbox::{Mailbox, MailboxMessage};
use crate::mode::AppMode;
use crate::subagent::SubAgentType;
use crate::tool_dispatch::ToolDispatcher;
use crate::turn_lease::{TurnLeaseKey, TurnLeaseSpec, shared_turn_leases};

/// Everything a sub-agent turn needs that only its host knows. Built by
/// the host's spawn path, consumed by [`run_child_turn`].
pub struct ChildTurnSpec {
    /// The sub-agent's id (lease key; progress and mailbox addressing).
    pub agent_id: String,
    /// The sub-agent type — the `Scope::AgentType` the turn dispatches at.
    pub agent_type: SubAgentType,
    /// Spawn depth (1 = a direct child of the main turn).
    pub depth: u32,
    /// The spawning sub-agent's id; `None` when the main turn spawned it.
    pub parent: Option<String>,
    /// The `turn_executor` id to build with — the session's selection,
    /// shared with the main turn.
    pub executor_id: String,
    pub client: LlmClientHandle,
    /// The routed model every request names (empty = the client's own).
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub max_tokens: u32,
    pub max_steps: u32,
    pub system: SystemPrompt,
    /// The opening transcript: the assignment for a fresh child; the
    /// parent's request prefix, fork state and the assignment for a forked
    /// one.
    pub messages: Vec<Message>,
    pub tools: ChildToolSurface,
    /// Follow-up input (`agent_send_input`), delivered as user messages at
    /// the next step boundary — the loop's steer seam.
    pub input: mpsc::Receiver<String>,
    pub cancel: CancellationToken,
    pub events: ChildEventSink,
    /// Idle watchdog window for the child's streams (`None` = disabled).
    pub stream_idle_timeout: Option<Duration>,
    pub stream_idle_retry_increment: Duration,
    pub extension_runner: Option<Arc<ExtensionRunner>>,
    /// The session's sub-agent view — the manager the child's own spawn
    /// tools register grandchildren in.
    pub subagents: Arc<dyn SubAgentApi>,
}

/// A child's tool surface, already narrowed by the host's delegation
/// policy: what runs ([`ToolSet`]), what the loop asks about it
/// ([`ToolDispatcher`]), and what the model is shown (`catalog`).
pub struct ChildToolSurface {
    pub tool_set: Arc<ToolSet>,
    pub dispatcher: Arc<dyn ToolDispatcher>,
    pub catalog: Vec<Tool>,
}

/// Where a child's progress goes: `Event::AgentProgress` on the parent's
/// event channel and envelopes on the spawn tree's mailbox. Never the
/// parent's turn events — a child's stream deltas and tool calls are not
/// the parent's.
#[derive(Clone, Default)]
pub struct ChildEventSink {
    pub event_tx: Option<mpsc::Sender<Event>>,
    pub mailbox: Option<Mailbox>,
}

impl ChildEventSink {
    fn progress(&self, agent_id: &str, status: String) {
        self.mailbox(MailboxMessage::progress(agent_id, status.clone()));
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(Event::AgentProgress {
                id: agent_id.to_string(),
                status,
            });
        }
    }

    fn mailbox(&self, message: MailboxMessage) {
        if let Some(mailbox) = &self.mailbox {
            let _ = mailbox.send(message);
        }
    }
}

/// How a child turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildTurnStatus {
    /// The model finished (no tool calls), or the step budget ran out.
    Completed,
    /// The cancel token fired (agent cancel, a reload drain, shutdown).
    Cancelled,
    /// The turn could not run or aborted; the message is display-only.
    Failed(String),
}

/// What a finished child turn hands back to its host.
#[derive(Debug)]
pub struct ChildTurnOutcome {
    pub status: ChildTurnStatus,
    /// The last non-empty assistant text of the child's own turn (never
    /// the forked prefix's); `None` unless `Completed`.
    pub final_text: Option<String>,
    /// Model requests the turn issued.
    pub steps: u32,
    /// The whole transcript, opening messages included.
    pub messages: Vec<Message>,
    /// Index in `messages` where the child's own turn begins. The messages
    /// before it are the forked parent prefix, so a transcript reader must
    /// not recover their text as the child's output.
    pub child_messages_start: usize,
    pub usage: Usage,
}

/// Run one sub-agent turn through the configured turn executor.
pub async fn run_child_turn(spec: ChildTurnSpec) -> ChildTurnOutcome {
    let ChildTurnSpec {
        agent_id,
        agent_type,
        depth,
        parent,
        executor_id,
        client,
        model,
        reasoning_effort,
        max_tokens,
        max_steps,
        system,
        messages,
        tools,
        input,
        cancel,
        events,
        stream_idle_timeout,
        stream_idle_retry_increment,
        extension_runner,
        subagents,
    } = spec;
    // Misconfiguration fails loud: the same resolution, and the same
    // remedy text, as the main turn's assembly.
    let factory = match resolve_turn_executor(&executor_id) {
        Ok(factory) => factory,
        Err(message) => {
            return ChildTurnOutcome {
                status: ChildTurnStatus::Failed(message),
                final_text: None,
                steps: 0,
                // Nothing ran: there is no child-owned message.
                child_messages_start: messages.len(),
                messages,
                usage: Usage::default(),
            };
        }
    };
    // The registration scope every dispatch of this turn runs at (the
    // executor's dispatch scope and the around-turn view below read it).
    let child_scope = codesmith_agent::extension::Scope::AgentType(agent_type.as_str().to_string());
    // Structural ownership: the lease covers the executor's lifetime (it
    // is dropped after `built`), so a reload of the runtime this child was
    // assembled under drains it before the two-phase dylib unload.
    let lease = shared_turn_leases().acquire(TurnLeaseSpec {
        key: TurnLeaseKey {
            agent: agent_id.clone(),
            generation: extension_runner.as_ref().map_or(0, |r| r.generation()),
            executor: executor_id,
        },
        depth,
        cancel: cancel.clone(),
        dylib_exposed: false,
        scope: extension_runner.as_ref().map(Arc::downgrade),
    });
    events.mailbox(MailboxMessage::Started {
        agent_id: agent_id.clone(),
        agent_type: agent_type.as_str().to_string(),
    });
    events.progress(&agent_id, format!("started ({})", agent_type.as_str()));

    let callback = Arc::new(ChildTurnCallback {
        agent_id: agent_id.clone(),
        max_steps,
        steps: AtomicU32::new(0),
        events: events.clone(),
        stream_observer: extension_runner.clone().map(|runner| StreamObserver {
            runner,
            scope: child_scope.clone(),
            warned: std::sync::atomic::AtomicBool::new(false),
            spent_ms: std::sync::atomic::AtomicU64::new(0),
        }),
    });
    // Composition parity (DSH): a sub-agent's run is wrapped by the
    // around-turn chain at its scope — the same `next()` composition the
    // main turn gets, minus dylib-origin wrappers (5b). Snapshotted at this
    // turn's assembly, like the engine's.
    let around_turn_handlers = extension_runner
        .as_ref()
        .map(|runner| runner.around_turn_handlers_for(&child_scope))
        .unwrap_or_default();
    let deps = TurnDeps {
        shape: TurnShape::Child {
            agent_type: agent_type.as_str().to_string(),
            depth,
            parent,
        },
        client,
        tools: tools.tool_set,
        callback: callback.clone(),
        executor_config: AgentExecutorConfig {
            max_steps,
            max_tokens,
            system: Some(system),
            temperature: None,
        },
        request_model: Some(model.clone()).filter(|m| !m.trim().is_empty()),
        reasoning_effort,
        tx_event: None,
        rx_steer: Arc::new(AsyncMutex::new(input)),
        rx_approval: None,
        rx_user_input: None,
        rx_subagent_completion: None,
        cancel_token: cancel,
        lsp: None,
        turn_meta: None,
        reinject: None,
        deliverables: None,
        context: None,
        mode: AppMode::Agent,
        max_subagents: 0,
        plan_mode_state: None,
        progressive_disclosure: false,
        stream_idle_timeout,
        stream_idle_retry_increment,
        request_catalog: Some(Arc::new(tools.catalog)),
        tool_dispatcher: Some(tools.dispatcher),
        extension_runner,
        prefix_stability: None,
        pending_result_verifications: None,
        tool_durations: None,
        subagents,
        session_id: None,
        // A child turn runs the default `[rhai_loop]` settings (mod pin and
        // budget): the child spec carries the executor id, not the settings
        // — see the rhai_executor module's known limitations.
        rhai_loop: None,
    };
    let built = factory.build(deps);

    let opening = messages.len();
    let mut history = VecChatHistory::new();
    for message in messages {
        history.push(message);
    }
    let run = built.executor.run(&mut history, String::new());
    let stop = if around_turn_handlers.is_empty() {
        // No-extension parity is structural: an empty chain is the bare run.
        run.await
    } else {
        let turn_event = codesmith_agent::extension::TurnAroundEvent {
            turn_id: agent_id.clone(),
        };
        codesmith_agent::extension::wrap_turn_dispatch(&around_turn_handlers, turn_event, run).await
    };
    let usage = built.harvest.take_usage();
    events.mailbox(MailboxMessage::token_usage(
        agent_id.as_str(),
        model,
        usage.clone(),
    ));
    // A drain cancels the token and latches its reason first; surface it
    // so a reload-stopped child explains itself in the parent's feed.
    if let Some(reason) = lease.cancel_reason() {
        events.progress(&agent_id, format!("cancelled: {reason}"));
    }
    drop(built);
    drop(lease);

    let status = match stop {
        Ok(StopReason::NoToolCalls | StopReason::MaxSteps | StopReason::PlanApproved) => {
            ChildTurnStatus::Completed
        }
        Ok(StopReason::Interrupted) => ChildTurnStatus::Cancelled,
        Ok(StopReason::Error(message)) => ChildTurnStatus::Failed(message),
        Err(err) => ChildTurnStatus::Failed(err.to_string()),
    };
    let messages = history.messages();
    let final_text = (status == ChildTurnStatus::Completed)
        .then(|| last_assistant_text(&messages[opening.min(messages.len())..]))
        .flatten();
    ChildTurnOutcome {
        status,
        final_text,
        steps: callback.steps.load(Ordering::SeqCst),
        child_messages_start: opening.min(messages.len()),
        messages,
        usage,
    }
}

/// The last non-empty text block the assistant wrote in `messages`.
fn last_assistant_text(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter(|message| message.role == "assistant")
        .flat_map(|message| message.content.iter().rev())
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
}

/// The child's observation callback: the executor's lifecycle hooks,
/// rendered as the parent-facing progress feed (`step N/M: …`) and the
/// mailbox's per-tool envelopes.
struct ChildTurnCallback {
    agent_id: String,
    max_steps: u32,
    steps: AtomicU32,
    events: ChildEventSink,
    /// `assistant-stream` observation at the child's scope; `None` without
    /// an extension runtime.
    stream_observer: Option<StreamObserver>,
}

/// The hot-path `assistant-stream` dispatch for a child's text deltas — the
/// main turn's discipline (`budgeted_observe_emit`: bounded per delta, a
/// per-turn breaker once handlers miss the budget), at the child's scope.
struct StreamObserver {
    runner: Arc<ExtensionRunner>,
    scope: codesmith_agent::extension::Scope,
    warned: std::sync::atomic::AtomicBool,
    spent_ms: std::sync::atomic::AtomicU64,
}

impl ChildTurnCallback {
    fn step_progress(&self, what: String) {
        let step = self.steps.load(Ordering::SeqCst);
        self.events.progress(
            &self.agent_id,
            format!("step {step}/{}: {what}", self.max_steps),
        );
    }
}

impl Callback for ChildTurnCallback {
    fn on_llm_start<'a>(
        &'a self,
        _request: &'a MessageRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.steps.fetch_add(1, Ordering::SeqCst);
        self.step_progress("requesting model response".to_string());
        Box::pin(async {})
    }

    fn on_llm_end<'a>(
        &'a self,
        content: &'a [ContentBlock],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let calls = content
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .count();
        if calls > 0 {
            self.step_progress(format!("executing {calls} tool call(s)"));
        }
        Box::pin(async {})
    }

    fn on_tool_start<'a>(
        &'a self,
        _id: &'a str,
        name: &'a str,
        _input: &'a serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.step_progress(format!("running tool '{name}'"));
        self.events.mailbox(MailboxMessage::ToolCallStarted {
            agent_id: self.agent_id.clone(),
            tool_name: name.to_string(),
            step: self.steps.load(Ordering::SeqCst),
        });
        Box::pin(async {})
    }

    fn on_tool_end<'a>(
        &'a self,
        name: &'a str,
        result: &'a Result<ToolResult, ToolError>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.step_progress(format!("finished tool '{name}'"));
        self.events.mailbox(MailboxMessage::ToolCallCompleted {
            agent_id: self.agent_id.clone(),
            tool_name: name.to_string(),
            step: self.steps.load(Ordering::SeqCst),
            ok: matches!(result, Ok(r) if r.success),
        });
        Box::pin(async {})
    }

    fn on_stream_delta<'a>(
        &'a self,
        delta: &'a codesmith_agent::callback::StreamDelta,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let (Some(observer), codesmith_agent::callback::StreamDelta::Text { content, .. }) =
                (&self.stream_observer, delta)
            else {
                return;
            };
            if observer.warned.load(Ordering::Relaxed)
                || !observer.runner.has_handlers_scoped(
                    codesmith_agent::extension::ExtensionEventKind::AssistantStream,
                    &observer.scope,
                )
            {
                return;
            }
            let event = codesmith_agent::extension::ExtensionEvent::AssistantStream(
                codesmith_agent::extension::AssistantStreamEvent {
                    text: content.clone(),
                },
            );
            crate::callback_bridge::budgeted_observe_emit(
                Arc::clone(&observer.runner),
                observer.scope.clone(),
                event,
                &observer.warned,
                &observer.spent_ms,
                "AssistantStream",
            )
            .await;
        })
    }

    fn on_complete<'a>(
        &'a self,
        reason: &'a StopReason,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        match reason {
            StopReason::NoToolCalls => self.step_progress("complete".to_string()),
            StopReason::Interrupted => {
                self.step_progress("cancelled".to_string());
                self.events.mailbox(MailboxMessage::Cancelled {
                    agent_id: self.agent_id.clone(),
                });
            }
            _ => {}
        }
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executor_composition::{
        BuiltTurn, HostAgentExecutorFactory, TurnExecutorFactory, shared_turn_executors,
    };
    use crate::subagent::SubAgentResult;
    use crate::test_support::mock_llm::{MockLlmClient, canned};
    use crate::tools::registry::ToolRegistry;
    use crate::tools::spec::ToolContext;
    use std::sync::Mutex as StdMutex;

    /// A factory that records the shapes it was asked to build, then builds
    /// the builtin loop — proof that a child turn goes through the
    /// configured factory, not around it.
    struct ShapeProbe {
        id: &'static str,
        shapes: Arc<StdMutex<Vec<TurnShape>>>,
    }

    impl TurnExecutorFactory for ShapeProbe {
        fn id(&self) -> &'static str {
            self.id
        }
        fn build(&self, deps: TurnDeps) -> BuiltTurn {
            self.shapes.lock().unwrap().push(deps.shape.clone());
            HostAgentExecutorFactory.build(deps)
        }
    }

    struct NoSubAgents;

    #[async_trait::async_trait]
    impl SubAgentApi for NoSubAgents {
        async fn running_count(&self) -> usize {
            0
        }
        async fn list(&self) -> Vec<SubAgentResult> {
            Vec::new()
        }
        async fn cleanup(&self, _max_age: Duration) {}
        async fn live_running_snapshots(&self) -> Vec<SubAgentResult> {
            Vec::new()
        }
    }

    fn user(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn spec(
        client: LlmClientHandle,
        executor_id: &str,
        workspace: &std::path::Path,
    ) -> ChildTurnSpec {
        let registry = ToolRegistry::new(ToolContext::new(workspace.to_path_buf()));
        ChildTurnSpec {
            agent_id: "agent_probe".to_string(),
            agent_type: SubAgentType::Explore,
            depth: 1,
            parent: None,
            executor_id: executor_id.to_string(),
            client,
            model: "child-model".to_string(),
            reasoning_effort: Some("low".to_string()),
            max_tokens: 4096,
            max_steps: 8,
            system: SystemPrompt::Text("child system".to_string()),
            messages: vec![user("look around")],
            tools: ChildToolSurface {
                tool_set: Arc::new(registry.to_framework_tool_set()),
                dispatcher: Arc::new(registry),
                catalog: Vec::new(),
            },
            input: mpsc::channel(4).1,
            cancel: CancellationToken::new(),
            events: ChildEventSink::default(),
            stream_idle_timeout: None,
            stream_idle_retry_increment: Duration::ZERO,
            extension_runner: None,
            subagents: Arc::new(NoSubAgents),
        }
    }

    /// The child turn is assembled by the factory the configured id names
    /// (DSH: one factory makes every agent), in the child shape, and runs
    /// the shared streaming loop with the child's own model route.
    #[tokio::test]
    async fn child_turn_is_built_by_the_configured_factory() {
        let shapes = Arc::new(StdMutex::new(Vec::new()));
        let _probe = shared_turn_executors()
            .register(Arc::new(ShapeProbe {
                id: "child-turn-shape-probe",
                shapes: shapes.clone(),
            }))
            .expect("register the probe factory");
        let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "child says hi",
        )]));
        let workspace = tempfile::tempdir().expect("workspace");

        let outcome = run_child_turn(spec(
            mock.clone(),
            "child-turn-shape-probe",
            workspace.path(),
        ))
        .await;

        assert_eq!(outcome.status, ChildTurnStatus::Completed, "{outcome:?}");
        assert_eq!(outcome.final_text.as_deref(), Some("child says hi"));
        assert_eq!(outcome.steps, 1);
        assert_eq!(
            *shapes.lock().unwrap(),
            vec![TurnShape::Child {
                agent_type: "explore".to_string(),
                depth: 1,
                parent: None,
            }],
            "the configured factory built exactly this child turn"
        );
        let requests = mock.captured_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].stream, Some(true), "children stream");
        assert_eq!(
            requests[0].model, "child-model",
            "the routed model rides the request"
        );
        assert_eq!(requests[0].reasoning_effort.as_deref(), Some("low"));
    }

    /// An unregistered executor id fails the child loudly, naming the id
    /// and the remedy — never a silent fallback to the builtin loop.
    #[tokio::test]
    async fn unregistered_child_executor_fails_loud() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        let workspace = tempfile::tempdir().expect("workspace");

        let outcome =
            run_child_turn(spec(mock.clone(), "no-such-executor", workspace.path())).await;

        let ChildTurnStatus::Failed(message) = &outcome.status else {
            panic!("expected a failed child, got {outcome:?}");
        };
        assert!(message.contains("no-such-executor"), "{message}");
        assert!(message.contains("turn_executor"), "{message}");
        assert_eq!(mock.call_count(), 0, "nothing was sent");
    }

    /// A child's follow-up input (the steer seam) meets the `Input`
    /// waterfall at the child's agent-type scope: a compiled-in handler
    /// transforms it, a dylib-origin handler never sees it (5b — dylib code
    /// never runs from a sub-agent dispatch).
    #[tokio::test]
    async fn child_follow_up_input_meets_the_seam_at_the_child_scope() {
        use codesmith_agent::extension::{
            Extension, ExtensionApi, ExtensionContext, ExtensionError, ExtensionEvent,
            ExtensionEventKind, ExtensionMetadata, ExtensionMode, Handler, HandlerOutcome,
            InputEvent,
        };
        use codesmith_extensions::{HostExtensionContext, RegistrationOrigin};

        struct Suffix(&'static str);
        #[async_trait::async_trait]
        impl Handler for Suffix {
            async fn handle(
                &self,
                event: &ExtensionEvent,
                _ctx: &dyn ExtensionContext,
            ) -> Result<HandlerOutcome, ExtensionError> {
                let ExtensionEvent::Input(input) = event else {
                    return Ok(HandlerOutcome::Continue);
                };
                Ok(HandlerOutcome::Transform(ExtensionEvent::Input(
                    InputEvent {
                        text: format!("{} +{}", input.text, self.0),
                    },
                )))
            }
        }
        struct SuffixExt(&'static str);
        #[async_trait::async_trait]
        impl Extension for SuffixExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("input-suffix");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.on_variant(ExtensionEventKind::Input, Arc::new(Suffix(self.0)))
            }
        }

        let runner = Arc::new(ExtensionRunner::new());
        runner
            .load(&SuffixExt("compiled"))
            .await
            .expect("load compiled-in");
        runner
            .load_as(&SuffixExt("dylib"), RegistrationOrigin::Dylib)
            .await
            .expect("load dylib-stamped");
        let workspace = tempfile::tempdir().expect("workspace");
        runner.bind_core(Arc::new(HostExtensionContext::new(
            workspace.path().to_path_buf(),
            ExtensionMode::Interactive,
            Arc::new(StdMutex::new(true)),
            Arc::new(StdMutex::new(CancellationToken::new())),
            runner.generation_arc(),
        )));

        let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")]));
        let mut spec = spec(
            mock.clone(),
            crate::engine::BUILTIN_TURN_EXECUTOR_ID,
            workspace.path(),
        );
        let (input_tx, input_rx) = mpsc::channel(4);
        input_tx
            .send("follow-up note".to_string())
            .await
            .expect("queue the follow-up");
        spec.input = input_rx;
        spec.extension_runner = Some(runner);

        let outcome = run_child_turn(spec).await;

        assert_eq!(outcome.status, ChildTurnStatus::Completed, "{outcome:?}");
        let sent = serde_json::to_string(&mock.captured_requests()[0].messages).unwrap();
        assert!(
            sent.contains("follow-up note +compiled"),
            "the compiled-in handler transformed the child's input: {sent}"
        );
        assert!(
            !sent.contains("+dylib"),
            "a dylib-origin handler never sees a child's input: {sent}"
        );
    }

    // === Iteration C — composition parity at the child scope =============

    use codesmith_agent::extension::{
        AroundTurnHandler, Extension, ExtensionApi, ExtensionContext, ExtensionError,
        ExtensionEvent, ExtensionEventKind, ExtensionMetadata, ExtensionMode, Handler,
        HandlerOutcome, Scope, TurnAroundEvent, TurnNext,
    };
    use codesmith_extensions::{HostExtensionContext, RegistrationOrigin};

    fn bound_runner(workspace: &std::path::Path) -> Arc<ExtensionRunner> {
        let runner = Arc::new(ExtensionRunner::new());
        let _ = workspace;
        runner
    }

    fn bind(runner: &Arc<ExtensionRunner>, workspace: &std::path::Path) {
        runner.bind_core(Arc::new(HostExtensionContext::new(
            workspace.to_path_buf(),
            ExtensionMode::Interactive,
            Arc::new(StdMutex::new(true)),
            Arc::new(StdMutex::new(CancellationToken::new())),
            runner.generation_arc(),
        )));
    }

    struct StampAround {
        label: &'static str,
        seen: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl AroundTurnHandler for StampAround {
        async fn call(
            &self,
            turn: TurnAroundEvent,
            next: TurnNext<'_>,
        ) -> anyhow::Result<StopReason> {
            self.seen
                .lock()
                .unwrap()
                .push(format!("{}:pre:{}", self.label, turn.turn_id));
            let reason = next.await;
            self.seen
                .lock()
                .unwrap()
                .push(format!("{}:post", self.label));
            reason
        }
    }

    struct AroundExt {
        label: &'static str,
        seen: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Extension for AroundExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("child-around-turn");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.register_around_turn(Arc::new(StampAround {
                label: self.label,
                seen: self.seen.clone(),
            }))
        }
    }

    /// A child's run is wrapped by the around-turn chain at its scope (the
    /// `turn_id` a child's wrapper sees is its agent id), and a
    /// dylib-origin wrapper never wraps a child (5b).
    #[tokio::test]
    async fn around_turn_wraps_the_child_run_at_its_scope() {
        let workspace = tempfile::tempdir().expect("workspace");
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let runner = bound_runner(workspace.path());
        runner
            .load(&AroundExt {
                label: "compiled",
                seen: seen.clone(),
            })
            .await
            .expect("load compiled-in");
        runner
            .load_as(
                &AroundExt {
                    label: "dylib",
                    seen: seen.clone(),
                },
                RegistrationOrigin::Dylib,
            )
            .await
            .expect("load dylib-stamped");
        bind(&runner, workspace.path());

        let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")]));
        let mut child = spec(
            mock.clone(),
            crate::engine::BUILTIN_TURN_EXECUTOR_ID,
            workspace.path(),
        );
        child.extension_runner = Some(runner);

        let outcome = run_child_turn(child).await;

        assert_eq!(outcome.status, ChildTurnStatus::Completed, "{outcome:?}");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "compiled:pre:agent_probe".to_string(),
                "compiled:post".to_string()
            ],
            "the compiled-in wrapper brackets the child's run; the dylib one never wraps it"
        );
    }

    struct KindRecorder {
        seen: Arc<StdMutex<Vec<ExtensionEventKind>>>,
    }

    #[async_trait::async_trait]
    impl Handler for KindRecorder {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            self.seen.lock().unwrap().push(event.kind());
            Ok(HandlerOutcome::Continue)
        }
    }

    /// Records every kind it receives: subscribe-to-all at `Global`, or one
    /// kind at a foreign agent-type scope.
    struct RecorderExt {
        seen: Arc<StdMutex<Vec<ExtensionEventKind>>>,
        foreign_scope: bool,
    }

    #[async_trait::async_trait]
    impl Extension for RecorderExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("child-scope-recorder");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            let handler = Arc::new(KindRecorder {
                seen: self.seen.clone(),
            });
            if self.foreign_scope {
                api.on_variant_scoped(
                    ExtensionEventKind::TurnStart,
                    Scope::AgentType("plan".to_string()),
                    handler,
                )
            } else {
                api.on(handler)
            }
        }
    }

    struct EchoSpec;

    #[async_trait::async_trait]
    impl crate::tools::spec::ToolSpec for EchoSpec {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo the text back"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        fn capabilities(&self) -> Vec<codesmith_agent::tools::ToolCapability> {
            vec![codesmith_agent::tools::ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success(
                input["text"].as_str().unwrap_or_default().to_string(),
            ))
        }
    }

    /// The child-scope coverage table (DSH plan iteration C): which event
    /// kinds a sub-agent turn dispatches at its agent-type scope, pinned
    /// over a representative run (a follow-up input, a tool call, a final
    /// text answer). Every kind is listed — adding a kind without placing
    /// it here fails the length check. `false` rows split two ways:
    /// session-level kinds that never reach a child, and conditional kinds
    /// this run does not trigger but that dispatch at the child's scope
    /// when they occur (`ProviderRequestError` on a failed stream,
    /// `SessionBeforeFork` when the child's own spawn tool forks at depth
    /// ≥ 1). A dylib-origin handler and a handler scoped to another agent
    /// type see nothing of the child.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn child_scope_coverage_table() {
        use ExtensionEventKind as K;
        let table: &[(K, bool)] = &[
            // Fire in every child turn of this shape.
            (K::BeforeAgentStart, true),
            (K::AgentStart, true),
            (K::TurnStart, true),
            (K::Input, true),
            (K::BeforeProviderHeaders, true),
            (K::BeforeProviderRequest, true),
            (K::AssistantStream, true),
            (K::AfterProviderResponse, true),
            (K::ToolCall, true),
            (K::ToolExecutionStart, true),
            (K::ToolExecutionEnd, true),
            (K::ToolResult, true),
            (K::TurnStopping, true),
            (K::TurnEnd, true),
            (K::AgentEnd, true),
            // Conditional — dispatched at the child's scope when triggered.
            (K::ProviderRequestError, false),
            (K::SessionBeforeFork, false),
            // Session-level / main-turn-only — never at a child's scope.
            (K::ToolExecutionUpdate, false),
            (K::ProjectTrust, false),
            (K::SessionStart, false),
            (K::SessionShutdown, false),
            (K::SessionBeforeSwitch, false),
            (K::SessionBeforeCompact, false),
            (K::SessionCompact, false),
            (K::ResultVerified, false),
            (K::AgentSettled, false),
            (K::ToolsChange, false),
            (K::WorkflowStart, false),
            (K::WorkflowLog, false),
            (K::WorkflowAgentStart, false),
            (K::WorkflowAgentEnd, false),
            (K::WorkflowEnd, false),
        ];
        assert_eq!(table.len(), 32, "every ExtensionEventKind has a row");

        let workspace = tempfile::tempdir().expect("workspace");
        let global = Arc::new(StdMutex::new(Vec::new()));
        let dylib = Arc::new(StdMutex::new(Vec::new()));
        let foreign = Arc::new(StdMutex::new(Vec::new()));
        let runner = bound_runner(workspace.path());
        runner
            .load(&RecorderExt {
                seen: global.clone(),
                foreign_scope: false,
            })
            .await
            .expect("load the global recorder");
        runner
            .load_as(
                &RecorderExt {
                    seen: dylib.clone(),
                    foreign_scope: false,
                },
                RegistrationOrigin::Dylib,
            )
            .await
            .expect("load the dylib recorder");
        runner
            .load(&RecorderExt {
                seen: foreign.clone(),
                foreign_scope: true,
            })
            .await
            .expect("load the foreign-scope recorder");
        bind(&runner, workspace.path());

        let mut registry = ToolRegistry::new(ToolContext::new(workspace.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let mock = Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn("call_echo", "echo", r#"{"text":"hi"}"#),
            canned::simple_text_turn("all done"),
        ]));
        let mut child = spec(
            mock.clone(),
            crate::engine::BUILTIN_TURN_EXECUTOR_ID,
            workspace.path(),
        );
        child.tools = ChildToolSurface {
            tool_set: Arc::new(registry.to_framework_tool_set()),
            catalog: registry.to_api_tools(),
            dispatcher: Arc::new(registry),
        };
        let (input_tx, input_rx) = mpsc::channel(4);
        input_tx
            .send("also check the docs".to_string())
            .await
            .expect("queue the follow-up");
        child.input = input_rx;
        child.extension_runner = Some(runner);

        let outcome = run_child_turn(child).await;

        assert_eq!(outcome.status, ChildTurnStatus::Completed, "{outcome:?}");
        assert_eq!(outcome.final_text.as_deref(), Some("all done"));
        let fired = global.lock().unwrap().clone();
        for (kind, expected) in table {
            assert_eq!(
                fired.contains(kind),
                *expected,
                "child-scope coverage drift at {kind:?} (fired: {fired:?})"
            );
        }
        assert!(
            dylib.lock().unwrap().is_empty(),
            "a dylib-origin handler sees nothing of a child: {:?}",
            dylib.lock().unwrap()
        );
        assert!(
            foreign.lock().unwrap().is_empty(),
            "a handler scoped to another agent type sees nothing of this child: {:?}",
            foreign.lock().unwrap()
        );
    }
}
