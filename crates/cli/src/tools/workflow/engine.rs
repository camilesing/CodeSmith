//! One-shot Python workflow engine.
//!
//! Spawns a single [`PythonRuntime`] kernel (no context file, no per-round
//! timeout — the engine owns the wall-clock bound), prepends a prelude that
//! defines the model-facing script API (`run_agent`, `agents`, `log`), and
//! runs the whole thing as one REPL round. The helpers call back over the
//! existing stdin/stdout RPC protocol as new `RpcRequest` workflow
//! variants; [`WorkflowRpcBridge`] services them through the framework
//! [`SubAgentSpawner`] seam. The script's return value is whatever it
//! passes to the REPL's existing `finalize(...)` helper.
//!
//! Timeout semantics copy the RLM precedent
//! (`RLM_EVAL_HARD_TIMEOUT`/`rlm_eval_timeout` in the RLM tools): the
//! wall-clock bound kills the kernel and fails the run with a clear
//! message — never a silent partial result. When the bound fires,
//! in-flight children are cancelled mid-run (their futures are dropped)
//! and their partial transcripts are not recorded.
//!
//! The workflow lifecycle events (`WorkflowStart` / `WorkflowLog` /
//! `WorkflowAgentStart` / `WorkflowAgentEnd` / `WorkflowEnd`) are emitted
//! here — run begin/end in the engine, per-child and per-log at the RPC
//! bridge — through the same `runner.emit(...)` idiom the `ToolsChange`
//! seam uses (observe-only `Emit`: the outcome is discarded). No runner
//! attached ⇒ no emissions; the tool works without extensions.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use codesmith_agent::extension::ExtensionEvent;
use codesmith_agent::extension::{
    WorkflowAgentEndEvent, WorkflowAgentStartEvent, WorkflowEndEvent, WorkflowLogEvent,
};
use futures_util::future::join_all;
use serde_json::Value;

use codesmith_agent_runtime::repl::runtime::{
    BatchResp, PythonRuntime, RpcDispatcher, RpcRequest, RpcResponse, SingleResp,
};
use codesmith_agent_runtime::workflow::{
    MAX_WORKFLOW_AGENTS, SubAgentObjective, SubAgentRunResult, SubAgentSpawner, WorkflowEngine,
    WorkflowError, WorkflowOutcome, WorkflowScript,
};

/// Per-run default agent cap: the engine's existing default sub-agent
/// budget (`DEFAULT_MAX_SUBAGENTS` = 10), so a workflow run starts with
/// the same fan-out allowance a user gets without config. Override via
/// `[workflow] max_agents` or the tool call's `max_agents` input.
const DEFAULT_MAX_AGENTS: u32 = crate::config::DEFAULT_MAX_SUBAGENTS as u32;

/// Byte cap for one `log(...)` entry (char-boundary truncation below).
const LOG_ENTRY_CAP: usize = 2_048;
/// Total `log(...)` entries retained per run (oldest dropped beyond this).
const LOG_ENTRIES_KEPT: usize = 512;

/// Python prelude defining the script API. Runs in the kernel's globals
/// before the model's script, reusing the bootstrap's `_rpc`/`_json`.
///
/// Budget violations and malformed payloads RAISE — the round fails with a
/// traceback (a script error surfaced to the model), never a silent skip.
/// A child that itself fails is honest data: the dict comes back with
/// `status: "failed"` and `result` carrying the error text.
const WORKFLOW_PRELUDE: &str = r#"
def _wf_check_single(resp):
    if isinstance(resp, dict) and resp.get("error"):
        raise RuntimeError("workflow agent call failed: " + str(resp["error"]))
    if not isinstance(resp, dict):
        raise RuntimeError("workflow agent call returned malformed response: " + str(resp)[:200])
    return resp.get("text", "")

def _wf_decode(text):
    try:
        out = _json.loads(text)
    except Exception:
        raise RuntimeError("workflow agent call returned a malformed payload: " + str(text)[:200])
    if not isinstance(out, dict):
        raise RuntimeError("workflow agent call must return a dict, got: " + str(out)[:200])
    return out

def run_agent(objective, *, role=None, model=None, timeout_s=None):
    """Run one sub-agent to completion. Returns {"result": str, "steps": int, "status": str}."""
    resp = _rpc({"type": "workflow_agent", "objective": str(objective),
                 "role": role, "model": model, "timeout_s": timeout_s})
    return _wf_decode(_wf_check_single(resp))

def _wf_objective(o):
    if not isinstance(o, dict):
        raise TypeError("each agents() entry must be a dict with an 'objective' key")
    if "objective" not in o:
        raise TypeError("each agents() entry needs an 'objective' key")
    return {"objective": str(o["objective"]),
            "role": o.get("role"), "model": o.get("model"),
            "timeout_s": o.get("timeout_s")}

def agents(objectives):
    """Fan out over objective dicts concurrently (bounded by max_agents). One dict per entry, in order."""
    if not isinstance(objectives, (list, tuple)):
        raise TypeError("agents(...) expects a list of objective dicts")
    payload = [_wf_objective(o) for o in objectives]
    resp = _rpc({"type": "workflow_agents", "objectives": payload})
    if isinstance(resp, dict) and resp.get("error"):
        raise RuntimeError("agents(...) failed: " + str(resp["error"]))
    results = (resp or {}).get("results", []) if isinstance(resp, dict) else []
    if len(results) != len(payload):
        raise RuntimeError("agents(...): size mismatch (%d/%d)" % (len(results), len(payload)))
    out = []
    for r in results:
        if isinstance(r, dict) and r.get("error"):
            raise RuntimeError("agents(...) failed: " + str(r["error"]))
        text = (r or {}).get("text", "") if isinstance(r, dict) else ""
        out.append(_wf_decode(text))
    return out

def log(message):
    """Append one line to this run's logs (returned in the tool receipt)."""
    _rpc({"type": "workflow_log", "message": str(message)})
    return None
"#;

/// Emit one workflow lifecycle event (all `DispatchMode::Emit` — the
/// outcome is advisory and discarded, the `ToolsChange` idiom). No-op
/// without a runner: the tool must work with extensions absent.
async fn emit_workflow_event(
    runner: &Option<Arc<codesmith_extensions::ExtensionRunner>>,
    event: ExtensionEvent,
) {
    if let Some(runner) = runner {
        let _ = runner.emit(event).await;
    }
}

/// The error-kind tag a failed run's [`WorkflowEndEvent`] payload carries
/// (the tool error carries the full text — the event notes only the kind).
fn workflow_error_kind(error: &WorkflowError) -> &'static str {
    match error {
        WorkflowError::ScriptError(_) => "script_error",
        WorkflowError::Timeout(_) => "timeout",
        WorkflowError::KernelSpawn(_) => "kernel_spawn",
        // Unreachable at the emit sites (rejected before `WorkflowStart`
        // fires); the arm keeps the match exhaustive.
        WorkflowError::TooManyAgents { .. } => "too_many_agents",
    }
}

/// Services the workflow RPCs coming back from the kernel. One bridge per
/// run; owns the run's agent budget and log buffer.
pub(crate) struct WorkflowRpcBridge {
    spawner: Arc<dyn SubAgentSpawner>,
    max_agents: u32,
    /// Workflow lifecycle emissions (`WorkflowAgentStart`/`WorkflowAgentEnd`
    /// around each child, `WorkflowLog` per `log(...)` call) — `None` ⇒
    /// no emissions.
    extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    /// Monotonic reservation ledger: every slot asked for, consumed via
    /// `fetch_add` BEFORE the cap check (reserve-before-spawn ordering).
    /// A rejection consumes its slots too, so every later attempt in the
    /// same run also fails — total-cap semantics.
    reserved: AtomicU32,
    /// Children actually spawned (reserved AND issued). This is what
    /// `agent_calls` reports, so reserved-but-rejected attempts are not
    /// counted as spawned.
    issued: AtomicU32,
    logs: StdMutex<Vec<String>>,
}

impl WorkflowRpcBridge {
    pub(crate) fn new(
        spawner: Arc<dyn SubAgentSpawner>,
        max_agents: u32,
        extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    ) -> Self {
        Self {
            spawner,
            max_agents,
            extension_runner,
            reserved: AtomicU32::new(0),
            issued: AtomicU32::new(0),
            logs: StdMutex::new(Vec::new()),
        }
    }

    /// Reserve `n` spawn slots. `Err` carries the model-facing budget
    /// error. The reservation ledger is consumed even on rejection, so
    /// every later attempt in the same run fails too (total-cap
    /// semantics); only a success bumps `issued`, so `agent_calls` counts
    /// children that actually spawned.
    fn reserve(&self, n: u32) -> Result<(), String> {
        let now = self.reserved.fetch_add(n, Ordering::SeqCst);
        if now.saturating_add(n) > self.max_agents {
            return Err(format!(
                "workflow agent budget exceeded: this run may spawn at most {} agents in \
                 total (tried to spawn {n} more with {now} already reserved); pass a higher \
                 max_agents in the workflow tool call if the task genuinely needs more",
                self.max_agents
            ));
        }
        self.issued.fetch_add(n, Ordering::SeqCst);
        Ok(())
    }

    fn push_log(&self, message: String) -> String {
        let mut logs = self.logs.lock().unwrap_or_else(|p| p.into_inner());
        if logs.len() >= LOG_ENTRIES_KEPT {
            logs.remove(0);
        }
        let entry = if message.len() > LOG_ENTRY_CAP {
            let cut = previous_char_boundary(&message, LOG_ENTRY_CAP);
            format!("{}[... log entry truncated]", &message[..cut])
        } else {
            message
        };
        logs.push(entry.clone());
        entry
    }

    /// Spawn one child with the `WorkflowAgentStart` / `WorkflowAgentEnd`
    /// emissions around it. Budget-rejected attempts never reach here — a
    /// child that never began emits nothing.
    async fn spawn_with_events(&self, objective: SubAgentObjective) -> SubAgentRunResult {
        emit_workflow_event(
            &self.extension_runner,
            ExtensionEvent::WorkflowAgentStart(WorkflowAgentStartEvent {
                objective: objective.objective.clone(),
            }),
        )
        .await;
        let result = self.spawner.spawn_and_run(objective).await;
        emit_workflow_event(
            &self.extension_runner,
            ExtensionEvent::WorkflowAgentEnd(WorkflowAgentEndEvent {
                status: result.status.clone(),
                steps: result.steps,
            }),
        )
        .await;
        result
    }

    pub(crate) fn agent_calls(&self) -> u32 {
        self.issued.load(Ordering::SeqCst)
    }

    pub(crate) fn take_logs(&self) -> Vec<String> {
        std::mem::take(&mut *self.logs.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// Largest index `<= idx` on a UTF-8 char boundary (same helper the REPL
/// runtime uses; kept local for the same reason).
fn previous_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while !s.is_char_boundary(idx) && idx > 0 {
        idx -= 1;
    }
    idx
}

fn encode_agent_result(result: &SubAgentRunResult) -> String {
    serde_json::to_string(result).unwrap_or_else(|_| {
        // The struct is plain strings/ints — serialization cannot fail in
        // practice; if it ever does, degrade to an honest failed dict
        // rather than an empty payload.
        r#"{"result":"agent result serialization failed","steps":0,"status":"failed"}"#.to_string()
    })
}

impl RpcDispatcher for WorkflowRpcBridge {
    fn dispatch<'a>(
        &'a self,
        req: RpcRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RpcResponse> + Send + 'a>> {
        Box::pin(async move {
            match req {
                RpcRequest::WorkflowAgent {
                    objective,
                    role,
                    model,
                    timeout_s,
                } => match self.reserve(1) {
                    Err(err) => RpcResponse::Single(SingleResp {
                        text: String::new(),
                        error: Some(err),
                    }),
                    Ok(()) => {
                        let result = self
                            .spawn_with_events(SubAgentObjective {
                                objective,
                                role,
                                model,
                                timeout_s,
                            })
                            .await;
                        RpcResponse::Single(SingleResp {
                            text: encode_agent_result(&result),
                            error: None,
                        })
                    }
                },
                RpcRequest::WorkflowAgents { objectives } => {
                    if objectives.is_empty() {
                        return RpcResponse::Batch(BatchResp { results: vec![] });
                    }
                    let n = objectives.len() as u32;
                    if let Err(err) = self.reserve(n) {
                        // Per-entry errors, mirroring the RLM batch_guard
                        // shape: the Python helper raises on the first one.
                        return RpcResponse::Batch(BatchResp {
                            results: objectives
                                .into_iter()
                                .map(|_| SingleResp {
                                    text: String::new(),
                                    error: Some(err.clone()),
                                })
                                .collect(),
                        });
                    }
                    let futures = objectives
                        .into_iter()
                        .map(|objective| self.spawn_with_events(objective));
                    let results = join_all(futures)
                        .await
                        .into_iter()
                        .map(|result| SingleResp {
                            text: encode_agent_result(&result),
                            error: None,
                        })
                        .collect();
                    RpcResponse::Batch(BatchResp { results })
                }
                RpcRequest::WorkflowLog { message } => {
                    // The event carries the same bounded entry the run's
                    // own buffer keeps, so the live emission and the
                    // recorded log agree.
                    let entry = self.push_log(message);
                    emit_workflow_event(
                        &self.extension_runner,
                        ExtensionEvent::WorkflowLog(WorkflowLogEvent { message: entry }),
                    )
                    .await;
                    RpcResponse::Single(SingleResp {
                        text: String::new(),
                        error: None,
                    })
                }
                // A workflow script orchestrates sub-agents; raw LLM/RLM
                // helpers are the RLM surface, not this one. Fail loud
                // instead of half-servicing them.
                RpcRequest::Llm { .. }
                | RpcRequest::LlmBatch { .. }
                | RpcRequest::Rlm { .. }
                | RpcRequest::RlmBatch { .. } => RpcResponse::Single(SingleResp {
                    text: String::new(),
                    error: Some(
                        "llm/rlm helpers are not available in a workflow kernel; orchestrate \
                         sub-agents with run_agent(...) / agents([...])"
                            .to_string(),
                    ),
                }),
            }
        })
    }
}

/// The one-shot workflow engine (see module docs).
pub struct PythonWorkflowEngine {
    spawner: Arc<dyn SubAgentSpawner>,
    timeout: Duration,
    default_max_agents: u32,
    /// Workflow lifecycle emissions (`WorkflowStart`/`WorkflowEnd` here,
    /// the child/log events at the bridge) — `None` ⇒ no emissions.
    extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    /// Receipt of the most recent run — recorded even on failure so the
    /// tool can tell the model how far a failed run got. The tool builds
    /// one engine per call, so "last" is "the" run.
    last_agent_calls: AtomicU32,
    last_logs: StdMutex<Vec<String>>,
}

impl PythonWorkflowEngine {
    #[must_use]
    pub fn new(spawner: Arc<dyn SubAgentSpawner>) -> Self {
        Self {
            spawner,
            timeout: Duration::from_secs(
                codesmith_agent_runtime::workflow::DEFAULT_WORKFLOW_TIMEOUT_SECS,
            ),
            default_max_agents: DEFAULT_MAX_AGENTS,
            extension_runner: None,
            last_agent_calls: AtomicU32::new(0),
            last_logs: StdMutex::new(Vec::new()),
        }
    }

    /// Attach the extension runner for the workflow lifecycle events
    /// (observe-only `Emit` family; the outcome is discarded).
    #[must_use]
    pub fn with_extension_runner(
        mut self,
        runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    ) -> Self {
        self.extension_runner = runner;
        self
    }

    /// Wall-clock bound for the whole run (kernel spawn to shutdown).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The configured wall-clock bound.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Per-run agent cap used when the script/tool input does not set one.
    #[must_use]
    pub fn with_default_max_agents(mut self, max_agents: u32) -> Self {
        self.default_max_agents = max_agents;
        self
    }

    /// Sub-agents the most recent run actually spawned.
    #[must_use]
    pub fn agent_calls_of_last_run(&self) -> u32 {
        self.last_agent_calls.load(Ordering::SeqCst)
    }

    /// `log(...)` lines the most recent run collected (empty before the
    /// first run).
    #[must_use]
    pub fn logs_of_last_run(&self) -> Vec<String> {
        self.last_logs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Terminal `WorkflowEnd` emission — success shape.
    async fn emit_end_success(&self, agent_calls: u32) {
        emit_workflow_event(
            &self.extension_runner,
            ExtensionEvent::WorkflowEnd(WorkflowEndEvent {
                status: "completed".to_string(),
                error: None,
                agent_calls,
            }),
        )
        .await;
    }

    /// Terminal `WorkflowEnd` emission — failure shape (the payload notes
    /// the error kind, not the full text).
    async fn emit_end_failure(&self, agent_calls: u32, error: &WorkflowError) {
        emit_workflow_event(
            &self.extension_runner,
            ExtensionEvent::WorkflowEnd(WorkflowEndEvent {
                status: "failed".to_string(),
                error: Some(workflow_error_kind(error).to_string()),
                agent_calls,
            }),
        )
        .await;
    }

    async fn run_inner(&self, script: WorkflowScript) -> Result<WorkflowOutcome, WorkflowError> {
        let max_agents = script.max_agents.unwrap_or(self.default_max_agents);
        if max_agents == 0 || max_agents > MAX_WORKFLOW_AGENTS {
            // Rejected before the run began: no Start/End pair fires.
            return Err(WorkflowError::TooManyAgents {
                requested: max_agents,
            });
        }
        let timeout = script.timeout;
        // Run begin — before the kernel spawns.
        emit_workflow_event(&self.extension_runner, ExtensionEvent::WorkflowStart).await;
        let mut kernel = match PythonRuntime::spawn_unbounded().await {
            Ok(kernel) => kernel,
            Err(err) => {
                let error = WorkflowError::KernelSpawn(err);
                self.emit_end_failure(0, &error).await;
                return Err(error);
            }
        };
        let bridge = WorkflowRpcBridge::new(
            Arc::clone(&self.spawner),
            max_agents,
            self.extension_runner.clone(),
        );
        let started = Instant::now();
        let code = format!("{WORKFLOW_PRELUDE}\n{}", script.source);

        // The wall-clock bound covers the whole run (prelude + script +
        // every in-flight child RPC). On timeout the inner future is
        // dropped and `shutdown()` below kills the kernel.
        let executed = tokio::time::timeout(timeout, kernel.run(&code, Some(&bridge))).await;
        kernel.shutdown().await;
        let agent_calls = bridge.agent_calls();
        let logs = bridge.take_logs();
        self.last_agent_calls.store(agent_calls, Ordering::SeqCst);
        *self.last_logs.lock().unwrap_or_else(|p| p.into_inner()) = logs.clone();

        match executed {
            Err(_elapsed) => {
                let error = WorkflowError::Timeout(timeout);
                self.emit_end_failure(agent_calls, &error).await;
                Err(error)
            }
            Ok(Err(protocol_error)) => {
                let error = WorkflowError::ScriptError(format!(
                    "workflow kernel round failed: {protocol_error}"
                ));
                self.emit_end_failure(agent_calls, &error).await;
                Err(error)
            }
            Ok(Ok(round)) => {
                if round.has_error {
                    let stderr = round.stderr.trim();
                    let error = WorkflowError::ScriptError(format!(
                        "{}{}",
                        round.stdout.trim_end(),
                        if stderr.is_empty() {
                            String::new()
                        } else {
                            format!("\nstderr:\n{stderr}")
                        }
                    ));
                    self.emit_end_failure(agent_calls, &error).await;
                    return Err(error);
                }
                self.emit_end_success(agent_calls).await;
                Ok(WorkflowOutcome {
                    return_value: round.final_json.unwrap_or(Value::Null),
                    agent_calls,
                    logs,
                    elapsed: started.elapsed(),
                })
            }
        }
    }
}

impl WorkflowEngine for PythonWorkflowEngine {
    fn run(
        &self,
        script: WorkflowScript,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<WorkflowOutcome, WorkflowError>> + Send + '_>,
    > {
        Box::pin(async move { self.run_inner(script).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded-replay spawner: no real sub-agents, no network. Records
    /// every objective, optionally delays (to prove fan-out concurrency),
    /// and fails objectives containing a marker.
    struct MockSpawner {
        calls: StdMutex<Vec<SubAgentObjective>>,
        delay: Duration,
        fail_marker: Option<&'static str>,
    }

    impl MockSpawner {
        fn new() -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                delay: Duration::ZERO,
                fail_marker: None,
            }
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }

        fn failing(mut self, marker: &'static str) -> Self {
            self.fail_marker = Some(marker);
            self
        }

        fn objectives(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .map(|objective| objective.objective.clone())
                .collect()
        }
    }

    impl SubAgentSpawner for MockSpawner {
        fn spawn_and_run(
            &self,
            objective: SubAgentObjective,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SubAgentRunResult> + Send + '_>>
        {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(objective.clone());
                if let Some(marker) = self.fail_marker
                    && objective.objective.contains(marker)
                {
                    return SubAgentRunResult {
                        result: format!("child error: {marker} exploded"),
                        steps: 3,
                        status: "failed".to_string(),
                    };
                }
                tokio::time::sleep(self.delay).await;
                SubAgentRunResult {
                    result: format!("did: {}", objective.objective),
                    steps: 1,
                    status: "completed".to_string(),
                }
            })
        }
    }

    fn script(source: &str) -> WorkflowScript {
        WorkflowScript {
            source: source.to_string(),
            max_agents: None,
            timeout: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn agents_fan_out_concurrently_and_return_receipt_data() {
        let spawner = Arc::new(MockSpawner::new().with_delay(Duration::from_secs(1)));
        let engine = PythonWorkflowEngine::new(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        let outcome = engine
            .run(script(
                "out = agents([{'objective': 'alpha'}, {'objective': 'beta'}])\n\
                 log('children done')\n\
                 finalize({'n': len(out), 'first': out[0]['result'],\
                  'statuses': [r['status'] for r in out]})",
            ))
            .await
            .expect("workflow run succeeds");

        assert_eq!(spawner.objectives(), ["alpha", "beta"]);
        assert_eq!(outcome.agent_calls, 2);
        assert_eq!(outcome.return_value["n"], 2);
        assert_eq!(outcome.return_value["first"], "did: alpha");
        assert_eq!(
            outcome.return_value["statuses"],
            serde_json::json!(["completed", "completed"])
        );
        assert_eq!(outcome.logs, ["children done"]);
        // Concurrency proof: two 1s children finished well under 2x their
        // own latency — a serialized run would need ≥ 2s.
        assert!(
            outcome.elapsed < Duration::from_secs(2),
            "fan-out must run concurrently, took {:?}",
            outcome.elapsed
        );
    }

    #[tokio::test]
    async fn failing_child_propagates_honestly_into_the_script_result() {
        let spawner = Arc::new(MockSpawner::new().failing("broken"));
        let engine = PythonWorkflowEngine::new(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        let outcome = engine
            .run(script(
                "out = agents([{'objective': 'fine'}, {'objective': 'broken one'}])\n\
                 finalize({'statuses': [r['status'] for r in out],\
                  'results': [r['result'] for r in out]})",
            ))
            .await
            .expect("a failed child is data, not a run error");

        assert_eq!(outcome.agent_calls, 2);
        assert_eq!(
            outcome.return_value["statuses"],
            serde_json::json!(["completed", "failed"])
        );
        let failed_text = outcome.return_value["results"][1]
            .as_str()
            .expect("failed child result is text");
        assert!(
            failed_text.contains("child error: broken exploded"),
            "failed child must surface its error text, got {failed_text}"
        );
    }

    #[tokio::test]
    async fn timeout_kills_kernel_and_fails_the_run() {
        let spawner = Arc::new(MockSpawner::new());
        let engine = PythonWorkflowEngine::new(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>)
            .with_timeout(Duration::from_secs(1));

        let started = Instant::now();
        let err = engine
            .run(WorkflowScript {
                source: "import time\ntime.sleep(60)\nfinalize('never')".to_string(),
                max_agents: None,
                timeout: Duration::from_secs(1),
            })
            .await
            .expect_err("a hung script must be killed");

        assert!(
            matches!(err, WorkflowError::Timeout(bound) if bound == Duration::from_secs(1)),
            "expected a timeout error, got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the kernel must actually be killed, wall clock was {:?}",
            started.elapsed()
        );
        assert_eq!(spawner.objectives().len(), 0, "no child ran");
    }

    #[tokio::test]
    async fn budget_exceeded_is_a_script_error_not_a_silent_skip() {
        let spawner = Arc::new(MockSpawner::new());
        let engine = PythonWorkflowEngine::new(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        let err = engine
            .run(WorkflowScript {
                source: "agents([{'objective': 'a'}, {'objective': 'b'}, {'objective': 'c'}])"
                    .to_string(),
                max_agents: Some(2),
                timeout: Duration::from_secs(60),
            })
            .await
            .expect_err("3 objectives against a budget of 2 must fail");

        match err {
            WorkflowError::ScriptError(text) => {
                assert!(
                    text.contains("workflow agent budget exceeded"),
                    "budget violation must name itself, got: {text}"
                );
                assert!(
                    text.contains("at most 2"),
                    "budget violation must state the cap, got: {text}"
                );
            }
            other => panic!("expected ScriptError, got {other:?}"),
        }
        // The first 2 slots were reserved before the rejection — but the
        // RPC failed as a whole, so no child actually ran.
        assert_eq!(spawner.objectives().len(), 0);
        // Reserved-but-rejected attempts must not count as spawned.
        assert_eq!(
            engine.agent_calls_of_last_run(),
            0,
            "agent_calls counts issued spawns only, not reservations"
        );
    }

    /// A run that spawns successfully and THEN trips the budget reports
    /// only the children that actually ran — the reservation ledger and
    /// the issued count are tracked separately.
    #[tokio::test]
    async fn budget_rejection_reports_only_issued_spawns() {
        let spawner = Arc::new(MockSpawner::new());
        let engine = PythonWorkflowEngine::new(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        let err = engine
            .run(WorkflowScript {
                source: "solo = run_agent('solo task')\n\
                         agents([{'objective': 'a'}, {'objective': 'b'}])"
                    .to_string(),
                max_agents: Some(2),
                timeout: Duration::from_secs(60),
            })
            .await
            .expect_err("1 issued + 2 more against a budget of 2 must fail");

        assert!(
            err.to_string().contains("workflow agent budget exceeded"),
            "expected the budget error, got {err}"
        );
        // Exactly the solo child ran (1 issued); the rejected batch's 2
        // reservations must not inflate the spawned count.
        assert_eq!(spawner.objectives(), ["solo task"]);
        assert_eq!(
            engine.agent_calls_of_last_run(),
            1,
            "only the child that actually spawned counts"
        );
    }

    #[tokio::test]
    async fn out_of_range_max_agents_is_rejected_before_the_kernel_starts() {
        let spawner = Arc::new(MockSpawner::new());
        let engine = PythonWorkflowEngine::new(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        for bad in [0u32, MAX_WORKFLOW_AGENTS + 1] {
            let err = engine
                .run(WorkflowScript {
                    source: "pass".to_string(),
                    max_agents: Some(bad),
                    timeout: Duration::from_secs(5),
                })
                .await
                .expect_err("out-of-range max_agents must be rejected");
            assert!(
                matches!(err, WorkflowError::TooManyAgents { requested } if requested == bad),
                "expected TooManyAgents({bad}), got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn script_exception_becomes_script_error_with_traceback() {
        let spawner = Arc::new(MockSpawner::new());
        let engine = PythonWorkflowEngine::new(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        let err = engine
            .run(script("log('before the fall')\nraise ValueError('boom')"))
            .await
            .expect_err("a raising script must fail the run");

        match err {
            WorkflowError::ScriptError(text) => {
                assert!(text.contains("ValueError"), "traceback missing: {text}");
                assert!(text.contains("boom"), "exception text missing: {text}");
            }
            other => panic!("expected ScriptError, got {other:?}"),
        }
    }

    #[test]
    fn prelude_helpers_do_not_shadow_finalize_or_rpc_names() {
        // Cheap static guard: the prelude defines exactly the model-facing
        // surface and nothing that collides with the bootstrap's control
        // names (`_rpc`, `finalize`, sentinels start with `_RLM`-style
        // prefixes).
        for name in ["run_agent", "agents", "log"] {
            assert!(
                WORKFLOW_PRELUDE.contains(&format!("def {name}(")),
                "prelude must define {name}"
            );
        }
        assert!(!WORKFLOW_PRELUDE.contains("def finalize"));
        assert!(!WORKFLOW_PRELUDE.contains("def _rpc"));
    }
}
