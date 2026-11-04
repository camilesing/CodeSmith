//! `rhai` turn executor — a script-driven loop as a crate-compiled
//! registry entry (config `turn_executor = "rhai"`).
//!
//! The DSH "everything is a plugin" translation for this codebase: the
//! loop *shape* is script (a mod's `register_loop("main")` entry), the
//! loop *steps* stay the host's — every capability call the script makes
//! (`llm_request()`, `run_tools(calls)`, …) executes the same
//! [`HostAgentExecutor`] machinery the builtin loop runs
//! ([`stream_with_transparent_retry`] for the LLM round,
//! [`execute_tool_batches`] for tool dispatch), so streaming, transparent
//! retry, approval, sandbox, the event waterfalls, prefix-cache
//! discipline and "model-visible means logged" are inherited, not
//! reimplemented. The single-loop contract holds: this is one registry
//! entry resolved at the same chokepoint, one factory for the main turn
//! and every in-process child alike.
//!
//! ## Execution model
//!
//! The script runs on a **dedicated thread** (the blocking-call
//! convention: a Rhai engine must never park a tokio worker). Its
//! capability natives marshal each call over a channel to the async side
//! — the executor's `run` future serves them while it owns the
//! `&mut ChatHistory` borrow — and block on the reply channel (allowed on
//! that thread). Cancellation reaches the script two ways: the engine's
//! `on_progress` hook terminates a CPU-bound script at the cancelled
//! flag, and a cancelled turn makes every further capability call return
//! an error, which surfaces as a Rhai error and ends the entry cleanly.
//! A wall-clock budget (config `[rhai_loop] timeout_secs`, default 3600)
//! aborts a runaway loop the same way.
//!
//! ## Error posture
//!
//! **Fail loud**, the opposite of a behavior hook's fail-open: a loop
//! script error fails the turn (the user selected this loop; silence
//! would look like the model's failure). `note(text)` records script
//! decisions on the session log path (`Event::Status`) so live
//! presentation and the persisted record agree.
//!
//! ## Known limitations (deliberate, v1)
//!
//! - No compaction preflight, LSP flush, capacity gates, steer flush,
//!   sub-agent hold or `TurnStopping` steer seam inside a script loop —
//!   the builtin loop keeps those; a script loop that needs them calls
//!   `steers_pending()` itself and stops accordingly.
//! - `maybe_compact()` is not offered: the compaction trigger stays with
//!   the builtin loop (algorithm is core by declared contract).
//! - Plan-mode reminders and the thinking-only status tail are builtin
//!   behaviors a script loop does not inherit.
//! - The assembly glue this file carries (request assembly, event
//!   prologue/epilogue) mirrors `run_inner` rather than sharing it;
//!   extracting a shared `llm_step` is a future cleanup if the two loops
//!   drift.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use codesmith_agent::callback::StopReason;
use codesmith_agent::executor::AgentExecutor;
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message, SystemPrompt, Usage};
use codesmith_agent::tools::ToolSet;

use super::executor_composition::{
    BuiltTurn, TurnDeps, TurnExecutorFactory, TurnHarvest, build_host_executor,
};
use super::host_executor::HostAgentExecutor;
use super::loop_guard::LoopGuard;
use super::turn::stream::EarlyToolTask;
use crate::error_taxonomy::ErrorEnvelope;

/// The registry id of the script-driven loop factory.
pub const RHAI_TURN_EXECUTOR_ID: &str = "rhai";

/// Per-session settings for the script loop (config `[rhai_loop]`).
#[derive(Debug, Clone)]
pub struct RhaiLoopSettings {
    /// Pin the loop-hosting mod by id; `None` resolves the first mod that
    /// declared `register_loop` (warned when ambiguous).
    pub mod_id: Option<String>,
    /// Wall-clock budget for one turn's script execution.
    pub timeout_secs: u64,
}

impl Default for RhaiLoopSettings {
    fn default() -> Self {
        Self {
            mod_id: None,
            timeout_secs: 3600,
        }
    }
}

/// One capability call from the script thread, with its private reply
/// channel (std unbounded: `send` never blocks the async side).
enum LoopCall {
    Llm,
    RunTools {
        calls: Vec<(String, String, serde_json::Value)>,
    },
    Cancelled,
    Note {
        text: String,
    },
}

/// What the async side answers a [`LoopCall`] with.
enum LoopReply {
    Llm(LlmRound),
    Tools(ToolsRound),
    Cancelled(bool),
    Noted,
    Err(String),
}

/// The llm round as the script sees it (`llm_request()`'s return map).
#[derive(Clone)]
struct LlmRound {
    text: String,
    tool_uses: Vec<(String, String, serde_json::Value)>,
    stop_reason: Option<String>,
    usage: Usage,
    interrupted: bool,
}

/// The tool batch as the script sees it (`run_tools(calls)`'s return map).
#[derive(Clone)]
struct ToolsRound {
    /// The loop-guard halt message, when the batch tripped it (the script
    /// should stop the turn).
    halt: Option<String>,
    /// An approved `exit_plan_mode` ended the turn's plan phase.
    plan_approved: bool,
    error_count: usize,
}

/// Messages the script thread sends the serving side: a capability call
/// or the entry fn's terminal result.
enum LoopMsg {
    Call {
        call: LoopCall,
        reply: std::sync::mpsc::Sender<LoopReply>,
    },
    Done(Result<StopReason, String>),
}

/// The factory: resolves the loop-hosting mod at build time (loud on
/// misconfiguration) and wraps a [`HostAgentExecutor`] built exactly the
/// way the builtin factory builds one — the wrapped host provides every
/// step service the script drives.
pub(crate) struct RhaiTurnExecutorFactory;

impl TurnExecutorFactory for RhaiTurnExecutorFactory {
    fn id(&self) -> &'static str {
        RHAI_TURN_EXECUTOR_ID
    }

    fn build(&self, deps: TurnDeps) -> BuiltTurn {
        let settings = deps.rhai_loop.clone().unwrap_or_default();
        let runner = deps
            .extension_runner
            .clone()
            .expect("turn_executor = \"rhai\" requires the extension runtime");
        let rhai_mod = match &settings.mod_id {
            Some(id) => runner.mod_by_id(id).unwrap_or_else(|| {
                panic!(
                    "turn_executor = \"rhai\": mod '{id}' does not exist or is not loaded — \
                     check [rhai_loop] mod and /mods status"
                )
            }),
            None => runner.loop_mod().unwrap_or_else(|| {
                panic!(
                    "turn_executor = \"rhai\": no loaded mod declares register_loop — \
                     activate one, or set [rhai_loop] mod"
                )
            }),
        };
        let entry = rhai_mod
            .loop_entry()
            .expect("loop mod without an entry")
            .to_string();
        let host = build_host_executor(deps);
        let executor = Arc::new(RhaiTurnExecutor {
            host: Arc::clone(&host),
            rhai_mod,
            entry,
            settings,
        });
        // The wrapped host IS the harvest — every take_* reads the same
        // accumulators its step services wrote.
        let harvest: Arc<dyn TurnHarvest> = host;
        BuiltTurn { executor, harvest }
    }
}

/// The script-driven turn executor. One instance per turn (built by the
/// factory), wrapping the per-turn [`HostAgentExecutor`] whose step
/// methods serve the script's capability calls.
pub struct RhaiTurnExecutor {
    host: Arc<HostAgentExecutor>,
    rhai_mod: Arc<codesmith_extensions::script::RhaiMod>,
    entry: String,
    settings: RhaiLoopSettings,
}

/// Turn-level counters the epilogue reports (`turn-end` payload parity
/// with the builtin loop).
#[derive(Default)]
struct LoopStats {
    steps: u32,
    tool_calls: u32,
}

impl AgentExecutor for RhaiTurnExecutor {
    fn run<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<StopReason>> + Send + 'a>>
    {
        Box::pin(self.run_scripted(history, user_text))
    }
}

impl RhaiTurnExecutor {
    async fn run_scripted(
        &self,
        history: &mut dyn ChatHistory,
        user_text: String,
    ) -> anyhow::Result<StopReason> {
        let extension = self.host.extension_runner();
        let turn_id = uuid::Uuid::new_v4().to_string();
        let mut stats = LoopStats::default();

        // --- Prologue: the builtin loop's turn-opening event sequence
        // (mirrors run_inner's BeforeAgentStart/AgentStart/Input/TurnStart
        // ordering) so a scripted turn is observationally a turn.
        let mut base = self.host.base_system_prompt();
        if let Some(runner) = &extension
            && let Some(base_prompt) = &base
            && let Some(with_sections) = runner.append_prompt_sections(base_prompt.clone())
        {
            base = Some(with_sections);
        }
        if let Some(runner) = &extension {
            let out = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::BeforeAgentStart(
                        codesmith_agent::extension::AgentStartEvent {
                            system_prompt: None,
                            inject_message: None,
                        },
                    ),
                )
                .await;
            if let codesmith_agent::extension::ExtensionEvent::BeforeAgentStart(e) = out.event {
                if let Some(msg) = e.inject_message {
                    history.push(Message {
                        role: "user".to_string(),
                        content: vec![ContentBlock::Text {
                            text: msg,
                            cache_control: None,
                        }],
                    });
                }
                if let Some(sp) = e.system_prompt {
                    base = Some(SystemPrompt::Text(sp));
                }
            }
            let _ = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::AgentStart,
                )
                .await;
        }
        let user_text = if user_text.is_empty() {
            user_text
        } else {
            crate::engine::input_through_extensions(
                extension.as_ref(),
                self.host.dispatch_scope().clone(),
                user_text,
            )
            .await
        };
        if !user_text.is_empty() {
            history.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: user_text,
                    cache_control: None,
                }],
            });
        }
        if let Some(runner) = &extension {
            let _ = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::TurnStart {
                        turn_id: turn_id.clone(),
                    },
                )
                .await;
        }

        // --- The scripted loop.
        let stop = self
            .serve_loop(history, &base, extension.clone(), &mut stats)
            .await;

        // --- Epilogue: TurnEnd + AgentEnd with the same payload shape the
        // builtin loop emits.
        let reason = match &stop {
            Ok(StopReason::NoToolCalls) => codesmith_agent::extension::TurnEndReason::NoToolCalls,
            Ok(StopReason::Interrupted) => codesmith_agent::extension::TurnEndReason::Interrupted,
            Ok(_) => codesmith_agent::extension::TurnEndReason::NoToolCalls,
            Err(_) => codesmith_agent::extension::TurnEndReason::Error,
        };
        if let Some(runner) = &extension {
            let usage = self.host.take_usage();
            let _ = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::TurnEnd {
                        turn_id: turn_id.clone(),
                        reason,
                        steps: stats.steps,
                        tool_calls: stats.tool_calls,
                        usage,
                    },
                )
                .await;
            let _ = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::AgentEnd,
                )
                .await;
        }
        stop
    }

    /// Serve the script thread's capability calls until it finishes, the
    /// turn is cancelled, or the wall-clock budget expires. Holds the
    /// history borrow for the whole serve — the script never touches the
    /// transcript directly.
    async fn serve_loop(
        &self,
        history: &mut dyn ChatHistory,
        base: &Option<SystemPrompt>,
        extension: Option<Arc<codesmith_extensions::ExtensionRunner>>,
        stats: &mut LoopStats,
    ) -> anyhow::Result<StopReason> {
        let (tx, mut rx) = mpsc::unbounded_channel::<LoopMsg>();
        let aborted = Arc::new(AtomicBool::new(false));
        let cancel = self.host.host_cancel_token();

        // Cross-call step state (per-turn, like run_inner's run-level
        // locals): transparent stream retries and recovery budget persist
        // across the script's llm_request calls.
        let mut stream_retry_attempts: u32 = 0;
        let mut context_recovery_attempts: u8 = 0;

        let script = self.spawn_script_thread(tx, Arc::clone(&aborted));

        let budget = Duration::from_secs(self.settings.timeout_secs.max(1));
        let outcome: anyhow::Result<StopReason> = loop {
            tokio::select! {
                biased;
                _ = async {
                    match cancel.as_ref() {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    aborted.store(true, Ordering::Relaxed);
                    break Ok(StopReason::Interrupted);
                }
                _ = tokio::time::sleep(budget) => {
                    aborted.store(true, Ordering::Relaxed);
                    tracing::error!(
                        "rhai loop exceeded its {}s wall-clock budget — aborted",
                        self.settings.timeout_secs
                    );
                    break Err(anyhow::anyhow!(
                        "rhai loop exceeded its {}s wall-clock budget",
                        self.settings.timeout_secs
                    ));
                }
                msg = rx.recv() => match msg {
                    None => {
                        // The script thread ended without Done (panic) —
                        // its join below reports it.
                        break Err(anyhow::anyhow!("rhai loop thread ended unexpectedly"));
                    }
                    Some(LoopMsg::Done(result)) => {
                        break result.map_err(|e| anyhow::anyhow!("{e}"));
                    }
                    Some(LoopMsg::Call { call, reply }) => {
                        if aborted.load(Ordering::Relaxed) {
                            let _ = reply.send(LoopReply::Err("turn cancelled".into()));
                            continue;
                        }
                        let answered = self
                            .serve_one(
                                call,
                                reply,
                                history,
                                base,
                                &extension,
                                stats,
                                &mut stream_retry_attempts,
                                &mut context_recovery_attempts,
                            )
                            .await;
                        if let Some(stop) = answered {
                            aborted.store(true, Ordering::Relaxed);
                            break Ok(stop);
                        }
                    }
                },
            }
        };
        // Let a CPU-bound script observe the abort flag via on_progress;
        // bound the join so a wedged script cannot hang the turn forever.
        let _ = tokio::task::spawn_blocking(move || {
            let _ = script.join();
        })
        .await;
        outcome
    }

    /// Serve one capability call. `Some(stop)` means the call itself ended
    /// the turn (an interrupted LLM round).
    #[allow(clippy::too_many_arguments)]
    async fn serve_one(
        &self,
        call: LoopCall,
        reply: std::sync::mpsc::Sender<LoopReply>,
        history: &mut dyn ChatHistory,
        base: &Option<SystemPrompt>,
        extension: &Option<Arc<codesmith_extensions::ExtensionRunner>>,
        stats: &mut LoopStats,
        stream_retry_attempts: &mut u32,
        context_recovery_attempts: &mut u8,
    ) -> Option<StopReason> {
        match call {
            LoopCall::Llm => {
                stats.steps += 1;
                // Top-of-step steer drain (run_inner's loop-top behavior):
                // queued user steers land in the transcript before this
                // round's request snapshot.
                self.host.drain_steers(history).await;
                match self
                    .llm_round(
                        history,
                        base,
                        extension,
                        stream_retry_attempts,
                        context_recovery_attempts,
                    )
                    .await
                {
                    Ok(round) => {
                        if round.interrupted {
                            return Some(StopReason::Interrupted);
                        }
                        stats.tool_calls += u32::try_from(round.tool_uses.len()).unwrap_or(0);
                        let _ = reply.send(LoopReply::Llm(round));
                        None
                    }
                    Err(e) => {
                        let _ = reply.send(LoopReply::Err(e.to_string()));
                        None
                    }
                }
            }
            LoopCall::RunTools { calls } => {
                stats.tool_calls += u32::try_from(calls.len()).unwrap_or(0);
                match self.tools_round(history, extension, calls).await {
                    Ok(round) => {
                        let _ = reply.send(LoopReply::Tools(round));
                        None
                    }
                    Err(e) => {
                        let _ = reply.send(LoopReply::Err(e.to_string()));
                        None
                    }
                }
            }
            LoopCall::Cancelled => {
                let cancelled = self
                    .host
                    .host_cancel_token()
                    .as_ref()
                    .is_some_and(|t| t.is_cancelled());
                let _ = reply.send(LoopReply::Cancelled(cancelled));
                None
            }
            LoopCall::Note { text } => {
                self.host.emit_status(text).await;
                let _ = reply.send(LoopReply::Noted);
                None
            }
        }
    }

    /// One LLM round — the request-assembly + stream + persist sequence
    /// `run_inner` runs per step, ending at the tool-use handoff (the
    /// script decides what happens next). Mirrors `run_inner`'s ordering:
    /// catalog → three-zone freeze → prefix observation → headers
    /// waterfall → request → request waterfall → record → `on_llm_start`
    /// → stream (transparent retry inside) → `on_llm_end` → persist.
    async fn llm_round(
        &self,
        history: &mut dyn ChatHistory,
        base: &Option<SystemPrompt>,
        extension: &Option<Arc<codesmith_extensions::ExtensionRunner>>,
        stream_retry_attempts: &mut u32,
        context_recovery_attempts: &mut u8,
    ) -> anyhow::Result<LlmRound> {
        let client = self.host.client_handle();
        let _tools: Arc<ToolSet> = self.host.tool_set();
        let callback = self.host.callback_handle();
        let request_model = self.host.request_model_name();
        let system = self.host.refresh_system_prompt_for(base);

        let api_tools = self.host.api_tools_for(history);
        let frozen_prefix =
            codesmith_session::zones::PinnedPrefix::new(system.as_ref(), api_tools.clone())
                .freeze();
        self.host.observe_prefix_stability(&frozen_prefix).await;
        if let Some(runner) = extension {
            let headers: serde_json::Map<String, serde_json::Value> = client
                .request_headers()
                .into_iter()
                .map(|(name, value)| {
                    let masked = codesmith_agent::extension::mask_sensitive_header(&name, &value);
                    (name, serde_json::Value::String(masked))
                })
                .collect();
            let _ = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::BeforeProviderHeaders(
                        codesmith_agent::extension::BeforeProviderHeadersEvent {
                            headers: serde_json::Value::Object(headers),
                        },
                    ),
                )
                .await;
        }

        let mut request = codesmith_session::zones::ThreeZoneRequest {
            prefix: &frozen_prefix,
            log: &history.messages(),
            scratch: codesmith_session::zones::TurnScratch::new(),
            model: request_model,
            max_tokens: self.host.max_tokens(),
            system: system.clone(),
            tools: if api_tools.is_empty() {
                None
            } else {
                Some(api_tools)
            },
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: self.host.reasoning_effort(),
            stream: Some(true),
            temperature: self.host.temperature(),
            top_p: None,
        }
        .into_message_request();

        if let Some(runner) = extension {
            let out = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::BeforeProviderRequest(
                        codesmith_agent::extension::BeforeProviderRequestEvent {
                            messages: serde_json::to_value(&request.messages)
                                .unwrap_or(serde_json::Value::Null),
                        },
                    ),
                )
                .await;
            if let codesmith_agent::extension::ExtensionEvent::BeforeProviderRequest(e) = out.event
                && let Ok(rewritten) = serde_json::from_value::<Vec<Message>>(e.messages)
            {
                request.messages = rewritten;
            }
        }

        history.record_request(&request);
        callback.on_llm_start(&request).await;

        let mut early_tasks: HashMap<String, EarlyToolTask> = HashMap::new();
        let mut pending_steers: Vec<String> = Vec::new();
        // RecoveredContextOverflow restarts the round with the compacted
        // transcript, exactly as run_inner restarts the step.
        let (content, stop_reason, usage) = loop {
            match self
                .host
                .stream_with_transparent_retry(
                    &client,
                    request.clone(),
                    stream_retry_attempts,
                    context_recovery_attempts,
                    history,
                    system.as_ref(),
                    &mut early_tasks,
                    &mut pending_steers,
                    stats_like_step(stream_retry_attempts),
                )
                .await
            {
                Ok(super::turn::stream::StreamRoundOutcome::Content {
                    content,
                    stop_reason,
                    usage,
                }) => break (content, stop_reason, usage),
                Ok(super::turn::stream::StreamRoundOutcome::RecoveredContextOverflow) => {
                    continue;
                }
                Ok(super::turn::stream::StreamRoundOutcome::Interrupted { partial, usage }) => {
                    self.host.accumulate_usage_external(&usage);
                    let shown: String = partial
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if !shown.trim().is_empty() {
                        self.host.set_interrupted_text(shown);
                    }
                    return Ok(LlmRound {
                        text: String::new(),
                        tool_uses: Vec::new(),
                        stop_reason: None,
                        usage,
                        interrupted: true,
                    });
                }
                Err(e) => return Err(e),
            }
        };
        callback.on_llm_end(&content).await;
        self.host.accumulate_usage_external(&usage);

        let text: String = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let tool_uses: Vec<(String, String, serde_json::Value)> = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect();

        // Persist the sendable assistant turn (thinking-only content is
        // not persisted — same rule as run_inner).
        let has_sendable = content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. } | ContentBlock::ToolUse { .. }));
        if has_sendable {
            history.push(Message {
                role: "assistant".to_string(),
                content: content.clone(),
            });
        }
        // Speculatively-started early tasks the script does not dispatch
        // are aborted by `EarlyToolTask`'s own `Drop` — dropping the map is
        // the cleanup.

        Ok(LlmRound {
            text,
            tool_uses,
            stop_reason,
            usage,
            interrupted: false,
        })
    }

    /// One tool batch — `execute_tool_batches` with the same step locals
    /// `run_inner` passes, plus the loop guard the script loop owns.
    async fn tools_round(
        &self,
        history: &mut dyn ChatHistory,
        extension: &Option<Arc<codesmith_extensions::ExtensionRunner>>,
        calls: Vec<(String, String, serde_json::Value)>,
    ) -> anyhow::Result<ToolsRound> {
        let tools = self.host.tool_set();
        let callback = self.host.callback_handle();
        let mut early_tasks: HashMap<String, EarlyToolTask> = HashMap::new();
        let mut loop_guard = LoopGuard::default();
        let mut step_error_count: usize = 0;
        let mut step_error_envelopes: Vec<ErrorEnvelope> = Vec::new();
        let intent_summary: Option<String> = None;
        let outcome = self
            .host
            .execute_tool_batches(
                history,
                calls,
                None,
                &mut early_tasks,
                &mut loop_guard,
                &tools,
                &callback,
                extension,
                &intent_summary,
                &mut step_error_count,
                &mut step_error_envelopes,
            )
            .await;
        Ok(ToolsRound {
            halt: outcome.loop_guard_halt,
            plan_approved: outcome.plan_approved,
            error_count: step_error_count,
        })
    }

    /// Spawn the dedicated thread that runs the loop entry. The engine is
    /// fresh (per-turn capability natives cannot live on the shared
    /// runtime engine); the AST is the mod's compiled one.
    fn spawn_script_thread(
        &self,
        tx: mpsc::UnboundedSender<LoopMsg>,
        aborted: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        let rhai_mod = Arc::clone(&self.rhai_mod);
        let entry = self.entry.clone();
        let cancel = self.host.host_cancel_token();
        let timeout = self.settings.timeout_secs;
        std::thread::Builder::new()
            .name("rhai-loop".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let tx = std::sync::Arc::new(tx);
                let make_call = {
                    let tx = std::sync::Arc::clone(&tx);
                    move || {
                        let tx = std::sync::Arc::clone(&tx);
                        move |c: LoopCall| -> Result<LoopReply, Box<rhai::EvalAltResult>> {
                            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                            // Unbounded-channel send is synchronous and
                            // non-blocking — no blocking_send needed here.
                            tx.send(LoopMsg::Call {
                                call: c,
                                reply: reply_tx,
                            })
                            .map_err(|e| {
                                Box::new(rhai::EvalAltResult::ErrorRuntime(
                                    format!("loop host channel closed: {e}").into(),
                                    rhai::Position::NONE,
                                ))
                            })?;
                            reply_rx.recv().map_err(|_| {
                                Box::new(rhai::EvalAltResult::ErrorRuntime(
                                    "loop host dropped the reply".into(),
                                    rhai::Position::NONE,
                                ))
                            })
                        }
                    }
                };
                let llm_error =
                    |e: String| -> Box<rhai::EvalAltResult> {
                        Box::new(rhai::EvalAltResult::ErrorRuntime(
                            e.into(),
                            rhai::Position::NONE,
                        ))
                    };

                let engine = rhai_mod.engine_for_loop(|engine| {
                    engine.on_progress({
                        let aborted = Arc::clone(&aborted);
                        let cancel = cancel.clone();
                        move |_ops| {
                            let cancelled = cancel
                                .as_ref()
                                .is_some_and(|t| t.is_cancelled());
                            // Some(value) terminates the script; the value
                            // rides ErrorTerminated into the entry's Err.
                            (aborted.load(Ordering::Relaxed) || cancelled).then(|| {
                                rhai::Dynamic::from(
                                    "rhai loop aborted (cancelled or over budget)",
                                )
                            })
                        }
                    });
                    let map_round = |round: &LlmRound| -> rhai::Dynamic {
                        let mut m = rhai::Map::new();
                        m.insert("text".into(), rhai::Dynamic::from(round.text.clone()));
                        m.insert(
                            "interrupted".into(),
                            rhai::Dynamic::from(round.interrupted),
                        );
                        m.insert(
                            "stop_reason".into(),
                            round
                                .stop_reason
                                .clone()
                                .map(rhai::Dynamic::from)
                                .unwrap_or(rhai::Dynamic::UNIT),
                        );
                        m.insert(
                            "usage".into(),
                            {
                                let mut u = rhai::Map::new();
                                u.insert(
                                    "input_tokens".into(),
                                    rhai::Dynamic::from(i64::from(round.usage.input_tokens)),
                                );
                                u.insert(
                                    "output_tokens".into(),
                                    rhai::Dynamic::from(i64::from(round.usage.output_tokens)),
                                );
                                rhai::Dynamic::from(u)
                            },
                        );
                        m.insert(
                            "tool_uses".into(),
                            rhai::Dynamic::from(
                                round
                                    .tool_uses
                                    .iter()
                                    .map(|(id, name, input)| {
                                        let mut c = rhai::Map::new();
                                        c.insert("id".into(), rhai::Dynamic::from(id.clone()));
                                        c.insert(
                                            "name".into(),
                                            rhai::Dynamic::from(name.clone()),
                                        );
                                        c.insert(
                                            "input".into(),
                                            rhai::serde::to_dynamic(input)
                                                .unwrap_or(rhai::Dynamic::UNIT),
                                        );
                                        rhai::Dynamic::from(c)
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                        );
                        rhai::Dynamic::from(m)
                    };

                    let call = make_call();
                    engine.register_fn("llm_request", move || -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
                        match call(LoopCall::Llm)? {
                            LoopReply::Llm(round) => Ok(map_round(&round)),
                            LoopReply::Err(e) => Err(llm_error(e)),
                            _ => Err(llm_error("llm_request(): unexpected reply".into())),
                        }
                    });
                    let call = make_call();
                    engine.register_fn("run_tools", move |calls: rhai::Array| -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
                        let mut parsed = Vec::with_capacity(calls.len());
                        for item in &calls {
                            let map = item
                                .clone()
                                .try_cast::<rhai::Map>()
                                .ok_or_else(|| llm_error("run_tools(): entries must be maps".into()))?;
                            let id = map.get("id").and_then(|d| d.clone().try_cast::<String>())
                                .ok_or_else(|| llm_error("run_tools(): entry needs a string `id`".into()))?;
                            let name = map.get("name").and_then(|d| d.clone().try_cast::<String>())
                                .ok_or_else(|| llm_error("run_tools(): entry needs a string `name`".into()))?;
                            let input = map.get("input")
                                .and_then(|d| rhai::serde::from_dynamic::<serde_json::Value>(d).ok())
                                .unwrap_or(serde_json::Value::Null);
                            parsed.push((id, name, input));
                        }
                        match call(LoopCall::RunTools { calls: parsed })? {
                            LoopReply::Tools(round) => {
                                let mut m = rhai::Map::new();
                                m.insert(
                                    "halt".into(),
                                    round.halt.map(rhai::Dynamic::from).unwrap_or(rhai::Dynamic::UNIT),
                                );
                                m.insert("plan_approved".into(), rhai::Dynamic::from(round.plan_approved));
                                m.insert("errors".into(), rhai::Dynamic::from(round.error_count as i64));
                                Ok(rhai::Dynamic::from(m))
                            }
                            LoopReply::Err(e) => Err(llm_error(e)),
                            _ => Err(llm_error("run_tools(): unexpected reply".into())),
                        }
                    });
                    let call = make_call();
                    engine.register_fn("cancelled", move || -> bool {
                        matches!(call(LoopCall::Cancelled), Ok(LoopReply::Cancelled(true)))
                    });
                    let call = make_call();
                    engine.register_fn("note", move |text: &str| {
                        let _ = call(LoopCall::Note {
                            text: text.to_string(),
                        });
                    });
                });

                let ast = rhai_mod.loop_ast();
                let result: Result<rhai::Dynamic, Box<rhai::EvalAltResult>> =
                    engine.call_fn(&mut rhai::Scope::new(), &ast, &entry, ());
                // Enforce the wall-clock budget on this thread too: the
                // async side's sleep fires only while it is serving; a
                // script blocked on nothing still needs the on_progress
                // flag, which the async side sets on timeout.
                let _ = started.elapsed().as_secs() < timeout || tx
                    .send(LoopMsg::Done(Err("rhai loop budget exceeded".into())))
                    .is_err();
                let done = match result {
                    Ok(_) => Ok(StopReason::NoToolCalls),
                    Err(e) => Err(format!("rhai loop entry '{entry}' failed: {e}")),
                };
                let _ = tx.send(LoopMsg::Done(done));
            })
            .expect("spawn the rhai loop thread")
    }
}

/// `stream_with_transparent_retry` takes the caller's step number purely
/// for diagnostics; the script loop counts its own steps from one.
fn stats_like_step(_stream_retry_attempts: &u32) -> u32 {
    1
}

// === tests ==================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::host_executor::HostAgentExecutor;
    use codesmith_agent::executor::AgentExecutorConfig;
    use codesmith_agent::llm_client::{LlmClient, StreamEventBox};
    use codesmith_agent::memory::VecChatHistory;
    use codesmith_agent::models::{
        ContentBlock, ContentBlockStart, Delta, MessageDelta, MessageRequest, StreamEvent,
    };
    use codesmith_agent::tools::{Tool as FrameworkTool, ToolSet};
    use codesmith_tools::{ToolError, ToolResult};
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Minimal streaming mock: one `Vec<StreamEvent>` per LLM round.
    struct MockLlm {
        rounds: Mutex<VecDeque<Vec<StreamEvent>>>,
    }

    impl MockLlm {
        fn new(rounds: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                rounds: Mutex::new(rounds.into_iter().collect()),
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
            let next = self.rounds.lock().unwrap().pop_front();
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

    fn tool_use_block(idx: u32, _id: &str, name: &str, input_json: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::ToolUse {
                    id: "t-wire-1".to_string(),
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
                usage: Some(codesmith_agent::models::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..codesmith_agent::models::Usage::default()
                }),
            },
            StreamEvent::MessageStop,
        ]
    }

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
                Ok(ToolResult::success(format!("echo:{text}")))
            })
        }
    }

    /// The equivalence proof: `react-min` (the builtin shape as a script)
    /// drives a two-round tool turn to the same terminal state the builtin
    /// loop reaches — same stop reason, same transcript shape, usage
    /// harvested from the wrapped host.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn react_min_drives_a_tool_turn_like_the_builtin_loop() {
        let source = include_str!("../../../../examples/mods/react-min/mod.rhai").to_string();
        let rhai_mod = codesmith_extensions::script::RhaiMod::load_source(
            "react-min",
            "React Minimal",
            "0.1.0",
            source,
            codesmith_extensions::script::kv::ModKvStore::new(std::env::temp_dir().join(format!(
                "codesmith-rhai-loop-test-{}.json",
                std::process::id()
            ))),
            std::sync::Arc::new(codesmith_agent::extension::MessageProjectionHub::new()),
        )
        .expect("load react-min");
        let entry = rhai_mod.loop_entry().expect("declares a loop").to_string();

        let mut tools = ToolSet::new();
        tools.register(Arc::new(EchoTool));

        // Call 1: text + tool_use(echo). Call 2: text-only → NoToolCalls.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let host = Arc::new(HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            Arc::new(tools),
            Arc::new(codesmith_agent::callback::CallbackSet::new()),
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ));
        let executor = RhaiTurnExecutor {
            host,
            rhai_mod: Arc::new(rhai_mod),
            entry,
            settings: RhaiLoopSettings::default(),
        };

        let mut history = VecChatHistory::new();
        let reason = executor
            .run(&mut history, "echo world".to_string())
            .await
            .expect("the scripted turn runs");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Same transcript shape as the builtin loop: user, assistant
        // (text + tool_use), tool result, assistant (final).
        assert_eq!(history.len(), 4, "transcript shape");
        match &history.messages()[2].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, "echo:world");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        assert!(
            executor.host.take_usage().output_tokens > 0,
            "usage harvested from the wrapped host"
        );
    }
}
