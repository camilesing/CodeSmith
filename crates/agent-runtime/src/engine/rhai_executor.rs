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
//! - **Sub-agent fan-out is one level in v1** (`spawn_child`): children
//!   run the session's loop like any child turn, but a child's own
//!   `spawn_child` calls are rejected loudly (its bundle carries
//!   `max_spawn_depth: 0`) — grandchildren need a real delegation-policy
//!   assembly (the CLI spawn path's per-type narrowing), which this seam
//!   does not rebuild. Script-spawned children inherit the parent's tool
//!   surface (optionally narrowed by `allowed_tools`), a fixed evaluator
//!   system prompt, and the parent's `SessionBeforeFork` veto.
//! - The assembly glue this file carries (request assembly, event
//!   prologue/epilogue) mirrors `run_inner` rather than sharing it;
//!   extracting a shared `llm_step` is a future cleanup if the two loops
//!   drift.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use codesmith_agent::callback::StopReason;
use codesmith_agent::executor::AgentExecutor;
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message, SystemPrompt, Tool as ModelTool, Usage};
use codesmith_agent::tools::ToolSet;

use super::SteerInput;
use super::child_turn::{ChildToolSurface, ChildTurnOutcome, ChildTurnSpec, run_child_turn};
use super::executor_composition::{
    BuiltTurn, TurnDeps, TurnExecutorFactory, TurnHarvest, build_host_executor,
};
use super::host_executor::HostAgentExecutor;
use super::loop_guard::LoopGuard;
use super::turn::stream::EarlyToolTask;
use crate::error_taxonomy::ErrorEnvelope;
use crate::host_services::SubAgentApi;
use crate::subagent::SubAgentType;
use crate::tool_dispatch::ToolDispatcher;

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
    Llm {
        /// A script-authored steer for this round: pushed as a user
        /// message ahead of the request (model-visible ⇒ persisted).
        prompt: Option<String>,
    },
    RunTools {
        calls: Vec<(String, String, serde_json::Value)>,
    },
    Cancelled,
    Note {
        text: String,
    },
    Spawn {
        prompt: String,
        agent_type: String,
        allowed_tools: Option<Vec<String>>,
    },
    Eval {
        handle: u64,
        timeout_ms: u64,
    },
    Policy {
        field: String,
        default: i64,
    },
}

/// What the async side answers a [`LoopCall`] with.
enum LoopReply {
    Llm(LlmRound),
    Tools(ToolsRound),
    Cancelled(bool),
    Noted,
    Spawn(u64),
    Eval(ChildEval),
    PolicyInt(i64),
    Err(String),
}

/// A finished (or timed-out) child as the script sees it
/// (`eval_child`'s return map).
#[derive(Clone)]
struct ChildEval {
    status: String,
    final_text: Option<String>,
    steps: u32,
}

/// The system prompt script-spawned children run with. v1 is a fixed
/// evaluator prompt — the per-type template assembly lives in the CLI
/// spawn path and is deliberately not rebuilt on this seam.
const SCRIPT_CHILD_SYSTEM: &str = "You are a sub-agent spawned by a loop script. \
     Complete the assignment directly and reply with your final answer.";

/// Cap one script-authored round prompt (fail loud — a runaway build of a
/// scoring prompt must not silently flood the transcript).
const MAX_SCRIPT_PROMPT_CHARS: usize = 64 * 1024;

/// Cap per-field reads on the loop-tier policy (`loop_policy_int`).
const LOOP_POLICY_INT_MAX: i64 = 255;

/// The spawn-side pieces of [`TurnDeps`] the executor keeps to build
/// [`ChildTurnSpec`]s for `spawn_child` (extracted before the factory
/// hands the rest to [`build_host_executor`]).
struct LoopSpawnDeps {
    /// This turn's spawn depth (0 = the main turn).
    depth: u32,
    executor_id: String,
    client: codesmith_agent::llm_client::LlmClientHandle,
    model: String,
    reasoning_effort: Option<String>,
    max_tokens: u32,
    max_steps: u32,
    tools: Arc<ToolSet>,
    dispatcher: Option<Arc<dyn ToolDispatcher>>,
    catalog: Option<Arc<Vec<ModelTool>>>,
    cancel: CancellationToken,
    tx_event: Option<mpsc::Sender<crate::events::Event>>,
    subagents: Arc<dyn SubAgentApi>,
    extension: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    stream_idle_timeout: Option<Duration>,
    stream_idle_retry_increment: Duration,
    /// Live-children ceiling (`[runtime]` `max_subagents`): enforced
    /// against this turn's own outstanding handles.
    max_children: usize,
    /// Recursion ceiling (`[runtime] max_spawn_depth`).
    max_spawn_depth: u32,
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
        // Extract the spawn-side pieces before `deps` is consumed — the
        // child-turn seam (run_child_turn) needs them for `spawn_child`.
        let spawn = LoopSpawnDeps {
            depth: match &deps.shape {
                super::executor_composition::TurnShape::Main => 0,
                super::executor_composition::TurnShape::Child { depth, .. } => *depth,
            },
            executor_id: RHAI_TURN_EXECUTOR_ID.to_string(),
            client: deps.client.clone(),
            model: deps.request_model.clone().unwrap_or_default(),
            reasoning_effort: deps.reasoning_effort.clone(),
            max_tokens: deps.executor_config.max_tokens,
            max_steps: deps.executor_config.max_steps,
            tools: Arc::clone(&deps.tools),
            dispatcher: deps.tool_dispatcher.clone(),
            catalog: deps.request_catalog.clone(),
            cancel: deps.cancel_token.clone(),
            tx_event: deps.tx_event.clone(),
            subagents: Arc::clone(&deps.subagents),
            extension: deps.extension_runner.clone(),
            stream_idle_timeout: deps.stream_idle_timeout,
            stream_idle_retry_increment: deps.stream_idle_retry_increment,
            max_children: deps.max_subagents,
            max_spawn_depth: deps.max_spawn_depth,
        };
        let host = build_host_executor(deps);
        let executor = Arc::new(RhaiTurnExecutor {
            host: Arc::clone(&host),
            rhai_mod,
            entry,
            settings,
            spawn,
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
    spawn: LoopSpawnDeps,
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

        // Live children this turn's script spawned (spawn_child handles):
        // joined by eval_child, aborted on eval timeout, cancelled with
        // the turn (each child token is a child of the turn's), and
        // aborted at serve exit when the script never evaluated them.
        let mut children: HashMap<u64, tokio::task::JoinHandle<ChildTurnOutcome>> = HashMap::new();
        let mut next_handle: u64 = 0;

        let script = self.spawn_script_thread(tx, Arc::clone(&aborted));

        // One absolute deadline for the whole serve: a per-iteration
        // `sleep(budget)` would restart on every capability call, so a
        // busy runaway loop (each round under the budget) could never
        // trip it — only an idle gap could.
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.settings.timeout_secs.max(1));
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
                _ = tokio::time::sleep_until(deadline) => {
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
                                &mut children,
                                &mut next_handle,
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
        // Children the script spawned but never evaluated must not
        // outlive the turn as detached tasks: their tokens stop on the
        // cancel path, and this reaps every other exit (a normal `Done`,
        // the budget) the same way.
        for task in children.into_values() {
            task.abort();
        }
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
        children: &mut HashMap<u64, tokio::task::JoinHandle<ChildTurnOutcome>>,
        next_handle: &mut u64,
    ) -> Option<StopReason> {
        match call {
            LoopCall::Llm { prompt } => {
                stats.steps += 1;
                // A script-authored round prompt is a steer: persisted as
                // a user message ahead of the request (model-visible ⇒
                // logged), so purpose-distinguished rounds (expand /
                // score / answer) stay reconstructable from the log.
                if let Some(prompt) = prompt {
                    if prompt.chars().count() > MAX_SCRIPT_PROMPT_CHARS {
                        let _ = reply.send(LoopReply::Err(format!(
                            "llm_request(): prompt exceeds {} chars",
                            MAX_SCRIPT_PROMPT_CHARS
                        )));
                        return None;
                    }
                    if !prompt.trim().is_empty() {
                        history.push(Message {
                            role: "user".to_string(),
                            content: vec![ContentBlock::Text {
                                text: prompt,
                                cache_control: None,
                            }],
                        });
                    }
                }
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
            LoopCall::Spawn {
                prompt,
                agent_type,
                allowed_tools,
            } => match self
                .spawn_child(
                    prompt,
                    &agent_type,
                    allowed_tools,
                    extension,
                    children,
                    next_handle,
                )
                .await
            {
                Ok(handle) => {
                    let _ = reply.send(LoopReply::Spawn(handle));
                    None
                }
                Err(e) => {
                    let _ = reply.send(LoopReply::Err(e.to_string()));
                    None
                }
            },
            LoopCall::Eval { handle, timeout_ms } => {
                match self.eval_child(handle, timeout_ms, children).await {
                    Ok(eval) => {
                        let _ = reply.send(LoopReply::Eval(eval));
                        None
                    }
                    Err(e) => {
                        let _ = reply.send(LoopReply::Err(e.to_string()));
                        None
                    }
                }
            }
            LoopCall::Policy { field, default } => {
                let value = codesmith_agent::policy::policy_usize(
                    codesmith_agent::policy::PolicyKind::LatsPolicy,
                    &field,
                    default.unsigned_abs() as usize,
                    LOOP_POLICY_INT_MAX.unsigned_abs() as usize,
                ) as i64;
                let _ = reply.send(LoopReply::PolicyInt(value));
                None
            }
        }
    }

    /// Spawn one child turn (`spawn_child`): depth/count guards, the
    /// `SessionBeforeFork` veto, then a [`ChildTurnSpec`] through the same
    /// child-turn seam the CLI spawn path uses. Errors are loud — they
    /// surface as Rhai errors in the script.
    async fn spawn_child(
        &self,
        prompt: String,
        agent_type: &str,
        allowed_tools: Option<Vec<String>>,
        extension: &Option<Arc<codesmith_extensions::ExtensionRunner>>,
        children: &mut HashMap<u64, tokio::task::JoinHandle<ChildTurnOutcome>>,
        next_handle: &mut u64,
    ) -> anyhow::Result<u64> {
        let s = &self.spawn;
        let depth = s.depth + 1;
        if depth > s.max_spawn_depth {
            anyhow::bail!(
                "spawn_child rejected: depth {depth} would exceed [runtime] max_spawn_depth {}",
                s.max_spawn_depth
            );
        }
        if children.len() >= s.max_children {
            anyhow::bail!(
                "spawn_child rejected: {} live children already at the max_subagents ceiling",
                children.len()
            );
        }
        let Some(agent_type) = SubAgentType::from_str(agent_type) else {
            anyhow::bail!("spawn_child rejected: unknown agent type '{agent_type}'");
        };
        // The fork veto fires exactly as it does on the CLI spawn path —
        // a serial emit; `Cancel` refuses the fork.
        if let Some(runner) = extension {
            let out = runner
                .emit_scoped(
                    self.host.dispatch_scope().clone(),
                    codesmith_agent::extension::ExtensionEvent::SessionBeforeFork,
                )
                .await;
            if let codesmith_agent::extension::HandlerOutcome::Cancel { reason } = out.outcome {
                anyhow::bail!("spawn_child vetoed (SessionBeforeFork): {reason}");
            }
        }
        // Tool surface: the parent's, narrowed by `allowed_tools` when the
        // script names a subset (unknown names fail loud). Execution flows
        // through the ToolSet, so narrowing it narrows what the child can
        // actually run; the dispatcher stays the parent's (metadata,
        // approval classification — the delegation surface).
        let dispatcher = s.dispatcher.clone().ok_or_else(|| {
            anyhow::anyhow!("spawn_child unavailable: this turn has no tool dispatcher")
        })?;
        let (tool_set, catalog) = match &allowed_tools {
            None => (
                Arc::clone(&s.tools),
                s.catalog
                    .as_ref()
                    .map(|c| c.as_ref().clone())
                    .unwrap_or_default(),
            ),
            Some(names) => {
                let mut narrowed = ToolSet::new();
                for name in names {
                    let Some(tool) = s.tools.get(name) else {
                        anyhow::bail!(
                            "spawn_child rejected: allowed_tools names unknown tool '{name}'"
                        );
                    };
                    narrowed.register(Arc::clone(tool));
                }
                let catalog = s
                    .catalog
                    .as_ref()
                    .map(|c| {
                        c.iter()
                            .filter(|t| names.iter().any(|n| n == &t.name))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                (Arc::new(narrowed), catalog)
            }
        };
        // Follow-up input is not offered in v1: a closed channel means
        // "no agent_send_input" to the child's loop.
        let (_input_tx, input_rx) = mpsc::channel::<SteerInput>(1);
        let spec = ChildTurnSpec {
            agent_id: format!("script-child-{}", uuid::Uuid::new_v4()),
            agent_type,
            depth,
            parent: None,
            executor_id: s.executor_id.clone(),
            client: s.client.clone(),
            model: s.model.clone(),
            reasoning_effort: s.reasoning_effort.clone(),
            max_tokens: s.max_tokens,
            max_steps: s.max_steps,
            system: SystemPrompt::Text(SCRIPT_CHILD_SYSTEM.to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: prompt,
                    cache_control: None,
                }],
            }],
            tools: ChildToolSurface {
                tool_set,
                dispatcher,
                catalog,
            },
            input: input_rx,
            cancel: s.cancel.child_token(),
            events: super::child_turn::ChildEventSink {
                event_tx: s.tx_event.clone(),
                mailbox: None,
            },
            stream_idle_timeout: s.stream_idle_timeout,
            stream_idle_retry_increment: s.stream_idle_retry_increment,
            extension_runner: s.extension.clone(),
            subagents: Arc::clone(&s.subagents),
        };
        let handle = *next_handle;
        *next_handle += 1;
        children.insert(handle, tokio::spawn(run_child_turn(spec)));
        Ok(handle)
    }

    /// Await one spawned child (`eval_child`). A timeout aborts the child
    /// task (dropping its lease) and errors loudly — an abandoned branch
    /// must not linger as a live child.
    async fn eval_child(
        &self,
        handle: u64,
        timeout_ms: u64,
        children: &mut HashMap<u64, tokio::task::JoinHandle<ChildTurnOutcome>>,
    ) -> anyhow::Result<ChildEval> {
        let Some(mut task) = children.remove(&handle) else {
            anyhow::bail!("eval_child rejected: unknown handle {handle}");
        };
        let timeout = Duration::from_millis(timeout_ms.max(1));
        match tokio::time::timeout(timeout, &mut task).await {
            Ok(Ok(outcome)) => Ok(ChildEval {
                status: match outcome.status {
                    super::child_turn::ChildTurnStatus::Completed => "completed",
                    super::child_turn::ChildTurnStatus::Cancelled => "cancelled",
                    super::child_turn::ChildTurnStatus::Failed(_) => "failed",
                }
                .to_string(),
                final_text: outcome.final_text,
                steps: outcome.steps,
            }),
            Ok(Err(join_err)) => anyhow::bail!("eval_child: child task failed: {join_err}"),
            Err(_) => {
                task.abort();
                anyhow::bail!("eval_child: timed out after {timeout_ms}ms; child aborted");
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
        let mut pending_steers: Vec<SteerInput> = Vec::new();
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
        let self_spawn_depth = self.spawn.depth;
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
                        match call(LoopCall::Llm { prompt: None })? {
                            LoopReply::Llm(round) => Ok(map_round(&round)),
                            LoopReply::Err(e) => Err(llm_error(e)),
                            _ => Err(llm_error("llm_request(): unexpected reply".into())),
                        }
                    });
                    let call = make_call();
                    engine.register_fn("llm_request", move |spec: rhai::Map| -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
                        let prompt = match spec.get("prompt") {
                            Some(d) => d.clone().try_cast::<String>().ok_or_else(|| {
                                llm_error("llm_request(): `prompt` must be a string".into())
                            })?,
                            None => return Err(llm_error(
                                "llm_request(map): only `prompt` is recognized — pass #{prompt: \"…\"}".into(),
                            )),
                        };
                        match call(LoopCall::Llm { prompt: Some(prompt) })? {
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

                    // --- capability v2: sub-agent fan-out + loop policy ----
                    let call = make_call();
                    engine.register_fn("spawn_child", move |spec: rhai::Map| -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
                        let prompt = spec.get("prompt").and_then(|d| d.clone().try_cast::<String>())
                            .ok_or_else(|| llm_error("spawn_child(): needs a string `prompt`".into()))?;
                        let agent_type = spec.get("type").and_then(|d| d.clone().try_cast::<String>())
                            .unwrap_or_else(|| "general".to_string());
                        let allowed_tools = match spec.get("allowed_tools") {
                            None => None,
                            Some(d) => {
                                let items = d.clone().try_cast::<rhai::Array>()
                                    .ok_or_else(|| llm_error("spawn_child(): `allowed_tools` must be an array of strings".into()))?;
                                let mut names = Vec::with_capacity(items.len());
                                for item in items {
                                    names.push(item.try_cast::<String>().ok_or_else(|| {
                                        llm_error("spawn_child(): `allowed_tools` entries must be strings".into())
                                    })?);
                                }
                                Some(names)
                            }
                        };
                        match call(LoopCall::Spawn { prompt, agent_type, allowed_tools })? {
                            LoopReply::Spawn(handle) => Ok(rhai::Dynamic::from(handle as i64)),
                            LoopReply::Err(e) => Err(llm_error(e)),
                            _ => Err(llm_error("spawn_child(): unexpected reply".into())),
                        }
                    });
                    let call = make_call();
                    engine.register_fn("eval_child", move |handle: rhai::INT, timeout_ms: rhai::INT| -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
                        if handle < 0 {
                            return Err(llm_error("eval_child(): handle must be non-negative".into()));
                        }
                        if timeout_ms <= 0 {
                            return Err(llm_error("eval_child(): timeout_ms must be positive".into()));
                        }
                        match call(LoopCall::Eval {
                            handle: handle as u64,
                            timeout_ms: timeout_ms as u64,
                        })? {
                            LoopReply::Eval(eval) => {
                                let mut m = rhai::Map::new();
                                m.insert("status".into(), rhai::Dynamic::from(eval.status.clone()));
                                m.insert(
                                    "final_text".into(),
                                    eval.final_text.clone().map(rhai::Dynamic::from).unwrap_or(rhai::Dynamic::UNIT),
                                );
                                m.insert("steps".into(), rhai::Dynamic::from(i64::from(eval.steps)));
                                Ok(rhai::Dynamic::from(m))
                            }
                            LoopReply::Err(e) => Err(llm_error(e)),
                            _ => Err(llm_error("eval_child(): unexpected reply".into())),
                        }
                    });
                    // Static per-turn fact: no channel round-trip needed.
                    let depth = self_spawn_depth;
                    engine.register_fn("spawn_depth", move || -> rhai::INT {
                        i64::from(depth)
                    });
                    let call = make_call();
                    engine.register_fn("loop_policy_int", move |field: &str, default: rhai::INT| -> Result<rhai::INT, Box<rhai::EvalAltResult>> {
                        match call(LoopCall::Policy {
                            field: field.to_string(),
                            default,
                        })? {
                            LoopReply::PolicyInt(v) => Ok(v),
                            LoopReply::Err(e) => Err(llm_error(e)),
                            _ => Err(llm_error("loop_policy_int(): unexpected reply".into())),
                        }
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
            spawn: test_spawn_deps(Arc::new(RoutedMockLlm::new(Vec::new())), None, 3),
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

    // === capability v2: sub-agent fan-out + loop policy ======================

    struct NoSubAgents;

    #[async_trait::async_trait]
    impl crate::host_services::SubAgentApi for NoSubAgents {
        async fn running_count(&self) -> usize {
            0
        }
        async fn list(&self) -> Vec<crate::subagent::SubAgentResult> {
            Vec::new()
        }
        async fn cleanup(&self, _max_age: Duration) {}
        async fn live_running_snapshots(&self) -> Vec<crate::subagent::SubAgentResult> {
            Vec::new()
        }
    }

    fn test_dispatcher() -> Arc<dyn ToolDispatcher> {
        let registry =
            crate::tools::registry::ToolRegistry::new(crate::tools::spec::ToolContext::new(
                std::path::PathBuf::from("/tmp/codesmith-rhai-loop-test"),
            ));
        Arc::new(registry)
    }

    fn test_spawn_deps(
        client: codesmith_agent::llm_client::LlmClientHandle,
        extension: Option<Arc<codesmith_extensions::ExtensionRunner>>,
        max_spawn_depth: u32,
    ) -> LoopSpawnDeps {
        LoopSpawnDeps {
            depth: 0,
            executor_id: RHAI_TURN_EXECUTOR_ID.to_string(),
            client,
            model: String::new(),
            reasoning_effort: None,
            max_tokens: 4096,
            max_steps: 8,
            tools: Arc::new(ToolSet::new()),
            dispatcher: Some(test_dispatcher()),
            catalog: None,
            cancel: CancellationToken::new(),
            tx_event: None,
            subagents: Arc::new(NoSubAgents),
            extension,
            stream_idle_timeout: None,
            stream_idle_retry_increment: Duration::ZERO,
            max_children: 4,
            max_spawn_depth,
        }
    }

    /// Routes mock rounds by needle-in-last-user-message — the parent and
    /// its concurrently-running children share one client handle, so
    /// pop-order mocks would be nondeterministic; content routing is not.
    struct RoutedMockLlm {
        routes: Vec<(&'static str, &'static str)>,
        delay: Option<Duration>,
    }

    impl RoutedMockLlm {
        fn new(routes: Vec<(&'static str, &'static str)>) -> Self {
            Self {
                routes,
                delay: None,
            }
        }

        /// `new` with a per-round delay — paces a spinning script so the
        /// wall-clock budget (not the Rhai operation cap) trips first.
        fn delayed(routes: Vec<(&'static str, &'static str)>, delay: Duration) -> Self {
            Self {
                routes,
                delay: Some(delay),
            }
        }
    }

    impl LlmClient for RoutedMockLlm {
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
            request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<StreamEventBox>> + Send + '_>> {
            let needle = request
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .and_then(|m| {
                    m.content.iter().find_map(|b| match b {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                })
                .unwrap_or("");
            let reply = self
                .routes
                .iter()
                .find(|(n, _)| needle.contains(n))
                .map(|(_, r)| (*r).to_string())
                .unwrap_or_else(|| "no route".to_string());
            let mut events = text_block(0, &reply);
            events.extend(finish("end_turn"));
            let delay = self.delay;
            Box::pin(async move {
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                Ok(
                    Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
                        as StreamEventBox,
                )
            })
        }
    }

    /// A bound runner hosting one loop mod — children resolve the same
    /// entry through it (the rhai factory requires a runner).
    async fn runner_with(source: &str) -> Arc<codesmith_extensions::ExtensionRunner> {
        let rhai_mod = codesmith_extensions::script::RhaiMod::load_source(
            "test-loop-mod",
            "Test Loop",
            "0.1.0",
            format!("register_loop(\"main\");\n{source}"),
            codesmith_extensions::script::kv::ModKvStore::ephemeral(),
            std::sync::Arc::new(codesmith_agent::extension::MessageProjectionHub::new()),
        )
        .expect("loop mod compiles");
        let runner = Arc::new(codesmith_extensions::ExtensionRunner::new());
        runner.bind_core(Arc::new(codesmith_extensions::HostExtensionContext::new(
            std::path::PathBuf::from("/tmp/codesmith-rhai-loop-test"),
            codesmith_agent::extension::ExtensionMode::Interactive,
            Arc::new(std::sync::Mutex::new(true)),
            Arc::new(std::sync::Mutex::new(CancellationToken::new())),
            runner.generation_arc(),
        )));
        // bind_mod flushes resolved policies into the ambient process
        // store — serialize the whole transition with the ambient policy
        // tests (the shared crate lock) so a concurrent publish→assert
        // window can't be wiped by this flush. The bind runs on the
        // blocking pool so the std guard is never held across an await.
        let bind_runner = Arc::clone(&runner);
        let bind = tokio::task::spawn_blocking(move || {
            let handle = tokio::runtime::Handle::current();
            let _guard = crate::policy_test_lock();
            handle.block_on(bind_runner.bind_mod(std::sync::Arc::new(rhai_mod)))
        });
        bind.await.expect("bind task runs").expect("bind loop mod");
        runner
    }

    fn scripted_executor(
        source: &str,
        routes: Vec<(&'static str, &'static str)>,
        extension: Option<Arc<codesmith_extensions::ExtensionRunner>>,
        max_spawn_depth: u32,
    ) -> RhaiTurnExecutor {
        let rhai_mod = codesmith_extensions::script::RhaiMod::load_source(
            "test-loop-mod",
            "Test Loop",
            "0.1.0",
            format!("register_loop(\"main\");\n{source}"),
            codesmith_extensions::script::kv::ModKvStore::ephemeral(),
            std::sync::Arc::new(codesmith_agent::extension::MessageProjectionHub::new()),
        )
        .expect("loop mod compiles");
        let entry = rhai_mod.loop_entry().expect("declares a loop").to_string();
        let client: codesmith_agent::llm_client::LlmClientHandle =
            Arc::new(RoutedMockLlm::new(routes));
        let host = Arc::new(HostAgentExecutor::new(
            client.clone(),
            Arc::new(ToolSet::new()),
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
        RhaiTurnExecutor {
            host,
            rhai_mod: Arc::new(rhai_mod),
            entry,
            settings: RhaiLoopSettings::default(),
            spawn: test_spawn_deps(client, extension, max_spawn_depth),
        }
    }

    /// Best-first over a two-leaf fan-out: both children run the same
    /// script (evaluator branch at depth > 0), the parent picks the higher
    /// embedded score and spends its final round on the winner — the
    /// deterministic shallow tree the LATS example generalizes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn script_fan_out_evals_children_and_picks_best_branch() {
        let source = r#"
            fn text_of(t) { "" + t }
            fn main() {
                if spawn_depth() > 0 {
                    // Evaluator child: one round against the assignment.
                    llm_request();
                    return;
                }
                let a = spawn_child(#{prompt: "BRANCH alpha: assess", type: "general"});
                let b = spawn_child(#{prompt: "BRANCH beta: assess", type: "general"});
                let ra = eval_child(a, 60000);
                let rb = eval_child(b, 60000);
                // Best-first pick: the higher embedded score wins the round.
                let pick = if text_of(rb.final_text) >= text_of(ra.final_text) { "beta" } else { "alpha" };
                llm_request(#{prompt: "final answer for " + pick});
            }
        "#;
        let runner = runner_with(source).await;
        let executor = scripted_executor(
            source,
            vec![
                ("BRANCH alpha", "score 40"),
                ("BRANCH beta", "score 90"),
                ("final answer", "chosen"),
            ],
            Some(runner),
            3,
        );

        let mut history = VecChatHistory::new();
        let reason = executor
            .run(&mut history, "pick the best branch".to_string())
            .await
            .expect("the scripted turn runs");
        assert_eq!(reason, StopReason::NoToolCalls);

        let messages = history.messages();
        let final_prompt = messages
            .iter()
            .rev()
            .find_map(|m| {
                m.content.iter().find_map(|b| match b {
                    ContentBlock::Text { text, .. } if m.role == "user" => Some(text.clone()),
                    _ => None,
                })
            })
            .expect("a final round prompt");
        assert_eq!(
            final_prompt, "final answer for beta",
            "the higher-scoring child wins the final round"
        );
        let last_text = messages
            .iter()
            .rev()
            .find_map(|m| {
                m.content.iter().find_map(|b| match b {
                    ContentBlock::Text { text, .. } if m.role == "assistant" => Some(text.clone()),
                    _ => None,
                })
            })
            .expect("a final assistant text");
        assert_eq!(last_text, "chosen");
    }

    /// The depth ceiling is loud: `spawn_child` past
    /// `[runtime] max_spawn_depth` errors the script, which fails the turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn script_spawn_past_max_depth_fails_loud() {
        let source = r#"
            fn main() {
                spawn_child(#{prompt: "too deep", type: "general"});
            }
        "#;
        let executor = scripted_executor(source, vec![], None, 0);

        let mut history = VecChatHistory::new();
        let err = executor
            .run(&mut history, "try to fan out".to_string())
            .await
            .expect_err("the depth ceiling must fail the turn");
        assert!(
            err.to_string().contains("max_spawn_depth"),
            "error names the ceiling: {err}"
        );
    }

    /// `loop_policy_int` reads the `lats-policy` kind with the script's
    /// compiled fallback, and a published override wins (last-wins store).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_policy_int_reads_lats_policy_with_fallback() {
        let source = r#"
            fn main() {
                let w = loop_policy_int("width", 7);
                llm_request(#{prompt: "width is " + w});
            }
        "#;
        // No registration in this private store → the compiled fallback.
        let empty = codesmith_agent::policy::PolicyStore::new();
        let executor = scripted_executor(source, vec![("width is", "ok")], None, 3);
        let mut history = VecChatHistory::new();
        empty
            .scope(executor.run(&mut history, "tune".to_string()))
            .await
            .expect("runs on the fallback");
        assert!(history.messages().iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text, .. } if text == "width is 7"))
        }));

        // A published override wins.
        let store = codesmith_agent::policy::PolicyStore::new();
        let mut set = std::collections::HashMap::new();
        set.insert(
            codesmith_agent::policy::PolicyKind::LatsPolicy,
            serde_json::json!({"width": 9}),
        );
        store.publish(set);
        let executor = scripted_executor(source, vec![("width is", "ok")], None, 3);
        let mut history = VecChatHistory::new();
        store
            .scope(executor.run(&mut history, "tune".to_string()))
            .await
            .expect("runs on the override");
        assert!(history.messages().iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text, .. } if text == "width is 9"))
        }));
    }

    /// The wall-clock budget is absolute: a script that keeps the calls
    /// coming (each round well under the budget) still trips it. A
    /// per-select-iteration `sleep(budget)` would restart on every call,
    /// so only an idle gap could ever time out — a busy runaway loop
    /// never would.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wall_clock_budget_trips_despite_constant_activity() {
        let source = r#"
            fn main() {
                loop {
                    let r = llm_request();
                    if r.interrupted { return (); }
                }
            }
        "#;
        let mut executor = scripted_executor(source, vec![("", "ok")], None, 3);
        executor.settings = RhaiLoopSettings {
            timeout_secs: 1,
            ..RhaiLoopSettings::default()
        };
        // 10ms rounds: ~100 rounds inside the 1s budget, far under the
        // Rhai operation cap — only the deadline can end this turn.
        executor.host = Arc::new(HostAgentExecutor::new(
            Arc::new(RoutedMockLlm::delayed(
                vec![("", "ok")],
                Duration::from_millis(10),
            )),
            Arc::new(ToolSet::new()),
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

        let mut history = VecChatHistory::new();
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            executor.run(&mut history, "spin".to_string()),
        )
        .await
        .expect("the budget must end the turn, not spin until the harness timeout");
        let err = outcome.expect_err("a budget overrun errors the turn");
        assert!(
            err.to_string().contains("wall-clock budget"),
            "error names the budget: {err}"
        );
    }

    /// Cancellation liveness: a cancel landing mid-serve (here inside
    /// `eval_child`, whose child runs a long round) must end the turn
    /// promptly with `Interrupted` — never hang the join on the script
    /// thread. Two properties keep that true and this test guards them
    /// together: the engine's `on_progress` hook terminates a script at
    /// the aborted flag at the next operation boundary (so a cancelled
    /// script cannot park on a further capability call), and an
    /// interrupted LLM round drops the reply (so the blocked script
    /// errors out through the dropped sender instead of waiting).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_mid_call_loop_ends_promptly() {
        let source = r#"
            fn main() {
                if spawn_depth() > 0 {
                    llm_request();
                    return ();
                }
                let h = spawn_child(#{prompt: "slow work", type: "general"});
                let r = eval_child(h, 60000);
                llm_request(#{prompt: "after eval " + r.status});
            }
        "#;
        let rhai_mod = codesmith_extensions::script::RhaiMod::load_source(
            "test-loop-mod",
            "Test Loop",
            "0.1.0",
            format!("register_loop(\"main\");\n{source}"),
            codesmith_extensions::script::kv::ModKvStore::ephemeral(),
            std::sync::Arc::new(codesmith_agent::extension::MessageProjectionHub::new()),
        )
        .expect("loop mod compiles");
        let entry = rhai_mod.loop_entry().expect("declares a loop").to_string();
        // A real runner: the child (which runs the rhai executor too)
        // requires one at its factory build — without it the child fails
        // before ever spanning the cancel.
        let runner = runner_with(source).await;
        let token = CancellationToken::new();
        let client: codesmith_agent::llm_client::LlmClientHandle = Arc::new(
            RoutedMockLlm::delayed(vec![("", "ok")], Duration::from_millis(300)),
        );
        let host = Arc::new(HostAgentExecutor::new(
            client.clone(),
            Arc::new(ToolSet::new()),
            Arc::new(codesmith_agent::callback::CallbackSet::new()),
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(token.clone()),
            None,
        ));
        let executor = RhaiTurnExecutor {
            host,
            rhai_mod: Arc::new(rhai_mod),
            entry,
            settings: RhaiLoopSettings::default(),
            spawn: test_spawn_deps(client, Some(runner), 3),
        };

        let mut history = VecChatHistory::new();
        let outcome = {
            let run = executor.run(&mut history, "race".to_string());
            tokio::pin!(run);
            // Let the turn run into the child's 300ms round (polling run
            // here — an unpollled future does no work), then cancel
            // mid-serve: the child interrupts BECAUSE of the cancel, so
            // the eval reply and the script's next call land strictly
            // after the token fired.
            tokio::select! {
                _ = &mut run => panic!("the turn ended before the cancel"),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            token.cancel();
            tokio::time::timeout(Duration::from_secs(10), &mut run)
                .await
                .expect("the turn must end, not hang on the join")
        };
        assert_eq!(
            outcome.expect("the interrupted turn completes"),
            StopReason::Interrupted
        );
    }
}
