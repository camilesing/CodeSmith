//! Framework seam for the model-writable `workflow` tool: a Python
//! orchestration script runs on a one-shot kernel and drives sub-agents
//! through injected helpers (`run_agent`, `agents`, `log`).
//!
//! This module owns only the seam vocabulary — value types, the
//! [`SubAgentSpawner`] spawn surface the kernel helpers call, and the
//! [`WorkflowEngine`] contract. The one-shot kernel itself reuses
//! [`crate::repl::runtime::PythonRuntime`] (the RLM REPL protocol carries
//! the workflow RPCs as new `RpcRequest` variants), and the concrete
//! engine + CLI sub-agent spawner live in the implementation group
//! (`codesmith-cli::tools::workflow`).
//!
//! The script text is tool *input*, so the model-visible script is
//! reconstructable from the session transcript by construction (repo rule
//! "model-visible means logged").
//!
//! Known limitations — what this design does **not** do:
//! - The script is a *synchronous* orchestrator: `run_agent`/`agents`
//!   await their children inside one tool call. There is no async
//!   settlement or wake delivery back into a running turn — a script that
//!   wants a child's answer must wait for it.
//! - Sub-agent fan-out inherits the existing caps, spawn-depth budget, and
//!   approval ceiling of the sub-agent path it rides; the workflow layer
//!   adds only the per-run `max_agents` budget on top. Extension seams for
//!   workflow children are the sub-agent run path's own: `SessionBeforeFork`
//!   fires per child (the provider threads the extension runner into every
//!   one-shot run it composes, so a fork-vetoing extension cancels workflow
//!   children exactly like `agent_run` children), and the child's in-run
//!   seams fire from inside its own loop. The workflow layer's own events
//!   are the observe-only lifecycle family `WorkflowStart` / `WorkflowLog`
//!   / `WorkflowAgentStart` / `WorkflowAgentEnd` / `WorkflowEnd` — all
//!   `DispatchMode::Emit` (advisory; handler outcomes are ignored at these
//!   seams), emitted from the CLI engine/RPC bridge, never here.
//! - There is no `runtime_threads` persistence item: the tool result in
//!   the transcript (return value + `agent_calls` + `elapsed_ms` + logs
//!   tail) is the record of a run.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Default wall-clock bound for one workflow run (seconds). Overridable via
/// `[workflow] timeout_secs`; values above [`MAX_WORKFLOW_TIMEOUT_SECS`]
/// are rejected at config load (fail loud, never silently clamped there).
pub const DEFAULT_WORKFLOW_TIMEOUT_SECS: u64 = 600;
/// Hard upper bound for `[workflow] timeout_secs` (and therefore for any
/// one `workflow` tool call). Matches the RLM `subagents` api-timeout
/// ceiling family (1800s).
pub const MAX_WORKFLOW_TIMEOUT_SECS: u64 = 1800;
/// Hard upper bound on `max_agents` for one run, mirroring the CLI
/// sub-agent ceiling (`MAX_SUBAGENTS` = 20). The provider rejects larger
/// requests with [`WorkflowError::TooManyAgents`] before spawning a kernel.
pub const MAX_WORKFLOW_AGENTS: u32 = 20;

/// One workflow run request. Built by the provider (the `workflow` tool)
/// from model input + config; `timeout` is the effective wall-clock bound
/// the engine enforces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowScript {
    /// Python source of the orchestration script.
    pub source: String,
    /// Per-run cap on total spawned agents. `None` → the engine's
    /// configured default.
    #[serde(default)]
    pub max_agents: Option<u32>,
    /// Effective wall-clock bound for the whole run.
    #[serde(default = "default_timeout")]
    pub timeout: Duration,
}

fn default_timeout() -> Duration {
    Duration::from_secs(DEFAULT_WORKFLOW_TIMEOUT_SECS)
}

/// Result of one completed workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowOutcome {
    /// The value the script passed to `finalize(...)`, or `null` when the
    /// script never finalized.
    pub return_value: serde_json::Value,
    /// Sub-agents this run actually spawned (children of failed status
    /// included; budget-rejected attempts excluded).
    pub agent_calls: u32,
    /// Lines collected through the script's `log(...)` helper.
    pub logs: Vec<String>,
    /// Wall-clock duration of the run.
    pub elapsed: Duration,
}

/// One child spawn requested by a workflow script helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentObjective {
    /// The child's objective text (the `prompt` of the one-shot run).
    pub objective: String,
    /// Optional role alias (`worker`, `explorer`, `awaiter`, `default`).
    #[serde(default)]
    pub role: Option<String>,
    /// Optional model override; validated at spawn time by the sub-agent
    /// path, not here.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional per-child wall-clock bound in seconds. `None` → bounded
    /// only by the run's own timeout.
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Result of one child spawn, as surfaced back into the script's
/// `run_agent`/`agents` helpers. `status` uses the sub-agent run path's
/// own vocabulary (`"completed" | "failed" | "interrupted" | "cancelled"`);
/// on failure `result` carries the error text — a failed child is data the
/// script sees honestly, not a raised exception. Only the per-child
/// `timeout_s` path synthesizes `"cancelled"`; a child dropped by the
/// run's wall-clock bound produces no `SubAgentRunResult` at all — the
/// run fails `WorkflowError::Timeout` and the in-flight result is
/// discarded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentRunResult {
    pub result: String,
    pub steps: u32,
    pub status: String,
}

/// Framework-side spawn surface the engine's kernel helpers call. The
/// implementation (CLI) wraps the same one-shot sub-agent run path the
/// `agent_open`/`agent_eval`/`agent_close` family uses, so children
/// inherit caps, spawn-depth budget, and the parent's approval ceiling.
/// Dropping the returned future is the cancellation path — an
/// implementation must tear down whatever it started. No cancellation
/// token fires on drop: dropping the future simply stops the wrapped
/// one-shot sub-agent run from being polled.
pub trait SubAgentSpawner: Send + Sync {
    /// Spawn one child and run it to completion (dyn-safe boxed future,
    /// matching the [`crate::repl::runtime::RpcDispatcher`] idiom).
    fn spawn_and_run(
        &self,
        objective: SubAgentObjective,
    ) -> Pin<Box<dyn Future<Output = SubAgentRunResult> + Send + '_>>;
}

/// Engine contract: run one script on a one-shot kernel and return its
/// outcome. Failures are loud — no silent skips.
pub trait WorkflowEngine: Send + Sync {
    fn run(
        &self,
        script: WorkflowScript,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowOutcome, WorkflowError>> + Send + '_>>;
}

/// Everything a workflow run can fail with. Carried out of the engine and
/// mapped to a tool error by the provider.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// The script raised, exceeded the agent budget from Python, or the
    /// kernel round failed at the protocol level.
    #[error("workflow script failed: {0}")]
    ScriptError(String),
    /// The run exceeded its wall-clock bound; the kernel was killed.
    #[error("workflow run exceeded the hard deadline of {0:?} and the kernel was killed")]
    Timeout(Duration),
    /// The one-shot Python kernel could not be started (missing
    /// interpreter, bootstrap failure).
    #[error("failed to start workflow kernel: {0}")]
    KernelSpawn(String),
    /// `max_agents` outside `1..=MAX_WORKFLOW_AGENTS` — rejected before
    /// the kernel starts.
    #[error("workflow max_agents {requested} outside 1..={MAX_WORKFLOW_AGENTS}")]
    TooManyAgents { requested: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_script_round_trips() {
        let script = WorkflowScript {
            source: "finalize(run_agent('x'))".to_string(),
            max_agents: Some(4),
            timeout: Duration::from_secs(91),
        };
        let json = serde_json::to_string(&script).expect("serialize");
        let back: WorkflowScript = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.source, script.source);
        assert_eq!(back.max_agents, Some(4));
        assert_eq!(back.timeout, Duration::from_secs(91));
    }

    #[test]
    fn workflow_script_defaults_max_agents_and_timeout() {
        let json = r#"{"source":"pass"}"#;
        let script: WorkflowScript = serde_json::from_str(json).expect("deserialize");
        assert_eq!(script.max_agents, None);
        assert_eq!(
            script.timeout,
            Duration::from_secs(DEFAULT_WORKFLOW_TIMEOUT_SECS)
        );
    }

    #[test]
    fn sub_agent_objective_accepts_sparse_dicts() {
        // The Python helper always emits all four keys, but the wire format
        // must tolerate a sparse dict (serde defaults) — protocol evolution
        // headroom, same posture as the other RpcRequest payloads.
        let objective: SubAgentObjective =
            serde_json::from_str(r#"{"objective":"do it"}"#).expect("deserialize");
        assert_eq!(objective.objective, "do it");
        assert_eq!(objective.role, None);
        assert_eq!(objective.model, None);
        assert_eq!(objective.timeout_s, None);
    }

    #[test]
    fn sub_agent_run_result_round_trips() {
        let result = SubAgentRunResult {
            result: "found 3 crates".to_string(),
            steps: 7,
            status: "completed".to_string(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: SubAgentRunResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.result, "found 3 crates");
        assert_eq!(back.steps, 7);
        assert_eq!(back.status, "completed");
    }

    #[test]
    fn workflow_outcome_round_trips() {
        let outcome = WorkflowOutcome {
            return_value: serde_json::json!({"n": 2}),
            agent_calls: 2,
            logs: vec!["children done".to_string()],
            elapsed: Duration::from_millis(1234),
        };
        let json = serde_json::to_string(&outcome).expect("serialize");
        let back: WorkflowOutcome = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.return_value, serde_json::json!({"n": 2}));
        assert_eq!(back.agent_calls, 2);
        assert_eq!(back.logs, vec!["children done".to_string()]);
        assert_eq!(back.elapsed, Duration::from_millis(1234));
    }
}
