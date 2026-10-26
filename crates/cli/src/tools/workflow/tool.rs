//! The `workflow` tool and the CLI sub-agent spawner.
//!
//! The tool composes [`PythonWorkflowEngine`] (one-shot kernel) with
//! [`CliSubAgentSpawner`], which wraps the SAME one-shot sub-agent run
//! path the `agent_open`/`agent_eval`/`agent_close` family runs on —
//! `SubagentRunTool` sync mode (open + eval + close composed into one
//! run). Children therefore inherit the existing caps, spawn-depth budget,
//! and approval ceiling; the workflow layer adds only its per-run agent
//! budget and wall-clock bound.
//!
//! The script text is tool input, so it lands in the transcript
//! automatically (repo rule "model-visible means logged" — satisfied by
//! construction).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use codesmith_agent_runtime::workflow::{
    MAX_WORKFLOW_AGENTS, SubAgentObjective, SubAgentRunResult, SubAgentSpawner, WorkflowEngine,
    WorkflowScript,
};

use crate::tools::subagent::{SharedSubAgentManager, SubAgentRuntime, SubagentRunTool};

use super::engine::PythonWorkflowEngine;

/// Inline output cap for the tool result (bytes). Larger receipts are
/// truncated at a char boundary with an explicit marker — follow the
/// existing tool output norms (`agent_run` truncates at 50k chars; the
/// workflow receipt is structured JSON, so the cap is tighter).
const WORKFLOW_OUTPUT_CAP_BYTES: usize = 16 * 1024;
/// `log(...)` lines included in the receipt (tail).
const RECEIPT_LOG_TAIL: usize = 20;

/// The model-writable orchestration tool.
pub struct WorkflowTool {
    manager: SharedSubAgentManager,
    runtime: SubAgentRuntime,
    /// `SessionBeforeFork` emit (fork veto) — threaded to every
    /// `SubagentRunTool` the spawner builds, so a workflow child is
    /// vetoable exactly like a model-facing `agent_run` child. See
    /// [`AgentOpenTool`](crate::tools::subagent::AgentOpenTool).
    extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    /// Test seam (recorded-replay, same role `HeadlessSelfProvider::
    /// with_program` plays for the headless provider): replaces the CLI
    /// sub-agent spawner so tool-layer tests run without real sub-agents,
    /// subprocesses of the binary, or network.
    spawner_override: Option<Arc<dyn SubAgentSpawner>>,
}

impl WorkflowTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager, runtime: SubAgentRuntime) -> Self {
        Self {
            manager,
            runtime,
            extension_runner: None,
            spawner_override: None,
        }
    }

    /// Attach the extension runner (`SessionBeforeFork` emit) so workflow
    /// children hit the same fork-veto seam as `agent_run` children.
    #[must_use]
    pub fn with_extension_runner(
        mut self,
        runner: Arc<codesmith_extensions::ExtensionRunner>,
    ) -> Self {
        self.extension_runner = Some(runner);
        self
    }

    /// Override the sub-agent spawner (tests).
    #[cfg(test)]
    #[must_use]
    pub fn with_spawner_override(mut self, spawner: Arc<dyn SubAgentSpawner>) -> Self {
        self.spawner_override = Some(spawner);
        self
    }

    fn engine(&self, context: &ToolContext) -> PythonWorkflowEngine {
        let spawner: Arc<dyn SubAgentSpawner> = match &self.spawner_override {
            Some(spawner) => Arc::clone(spawner),
            None => Arc::new(
                CliSubAgentSpawner::new(
                    Arc::clone(&self.manager),
                    self.runtime.clone(),
                    context.clone(),
                )
                .with_extension_runner(self.extension_runner.clone()),
            ),
        };
        PythonWorkflowEngine::new(spawner)
            .with_timeout(self.runtime.workflow_timeout)
            // Workflow lifecycle emissions (observe-only Emit family) —
            // same runner the fork-veto threading attached.
            .with_extension_runner(self.extension_runner.clone())
            .with_default_max_agents(
                self.runtime
                    .workflow_max_agents
                    .unwrap_or(crate::config::DEFAULT_MAX_SUBAGENTS as u32),
            )
    }
}

#[async_trait]
impl ToolSpec for WorkflowTool {
    fn name(&self) -> &'static str {
        "workflow"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Run a Python orchestration script on a one-shot sandboxed kernel that orchestrates sub-agents ",
            "through injected helpers. Script API: run_agent(objective, *, role=None, model=None, timeout_s=None) ",
            "-> {'result': str, 'steps': int, 'status': str} runs ONE sub-agent to completion; ",
            "agents([{objective, role?, model?, timeout_s?}, ...]) fans out over a list CONCURRENTLY ",
            "(bounded by max_agents, total across the run); log(message) appends to the run's logs; ",
            "finalize(value) sets the run's return value (returned as JSON). ",
            "A child that fails comes back with status='failed' and its error text in 'result' — that is data your ",
            "script handles, not an exception; exceeding the agent budget or a bug in your script fails the whole run ",
            "with the traceback. Sub-agents are one-shot runs on the same path as agent_run: they inherit caps, ",
            "spawn-depth budget, and the approval ceiling. The whole run has a wall-clock timeout ",
            "(default 600s) that kills the kernel; when it fires, in-flight children are cancelled mid-run ",
            "and their partial transcripts are not recorded. Sub-agent results are self-reports — re-verify claimed ",
            "side effects before reporting them as facts."
        )
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "Python source. Do not include markdown fences. Use run_agent/agents/log helpers; call finalize(value) to return the run's result."
                },
                "max_agents": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_WORKFLOW_AGENTS,
                    "description": "Hard cap on sub-agents this run may spawn (concurrent + total). Default: 10."
                }
            },
            "required": ["script"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecutesCode,
            // Orchestration is session management like the spawn family —
            // the children's own registries are what plan mode narrows.
            ToolCapability::AllowedInPlanMode,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // Same tier as `agent_open`/`agent_run`: the spawned children
        // inherit the parent's approval posture as their ceiling, and the
        // kernel is orchestration logic only (no more than a repl block).
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let source = input
            .get("script")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .ok_or_else(|| ToolError::missing_field("script"))?;
        let max_agents = match input.get("max_agents") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let requested = value
                    .as_u64()
                    .and_then(|raw| u32::try_from(raw).ok())
                    .ok_or_else(|| {
                        ToolError::invalid_input(
                            "max_agents must be an integer between 1 and \
                             {MAX_WORKFLOW_AGENTS}",
                        )
                    })?;
                if requested == 0 || requested > MAX_WORKFLOW_AGENTS {
                    return Err(ToolError::invalid_input(format!(
                        "max_agents {requested} outside 1..={MAX_WORKFLOW_AGENTS}"
                    )));
                }
                Some(requested)
            }
        };

        let engine = self.engine(context);
        let script = WorkflowScript {
            source: source.to_string(),
            max_agents,
            timeout: engine.timeout(),
        };
        match engine.run(script).await {
            Ok(outcome) => {
                let logs_total = outcome.logs.len();
                let logs_tail: Vec<&str> = outcome
                    .logs
                    .iter()
                    .rev()
                    .take(RECEIPT_LOG_TAIL)
                    .rev()
                    .map(String::as_str)
                    .collect();
                let receipt = json!({
                    "status": "ok",
                    "return_value": outcome.return_value,
                    "agent_calls": outcome.agent_calls,
                    "elapsed_ms": outcome.elapsed.as_millis() as u64,
                    "logs_tail": logs_tail,
                    "logs_total": logs_total,
                });
                let mut content = serde_json::to_string(&receipt)
                    .map_err(|e| ToolError::execution_failed(format!("workflow receipt: {e}")))?;
                truncate_inline(&mut content);
                Ok(ToolResult::success(content).with_metadata(json!({
                    "agent_calls": outcome.agent_calls,
                    "elapsed_ms": outcome.elapsed.as_millis() as u64,
                })))
            }
            Err(err) => {
                let agent_calls = engine.agent_calls_of_last_run();
                let logs = engine.logs_of_last_run();
                let logs_tail: Vec<&str> = logs
                    .iter()
                    .rev()
                    .take(10)
                    .rev()
                    .map(String::as_str)
                    .collect();
                let mut message = format!("{err}");
                if !logs_tail.is_empty() {
                    message.push_str(&format!("\nlogs tail:\n{}", logs_tail.join("\n")));
                }
                if agent_calls > 0 {
                    message.push_str(&format!(
                        "\n({agent_calls} sub-agent(s) had been spawned before the failure)"
                    ));
                }
                truncate_inline(&mut message);
                Err(ToolError::execution_failed(message))
            }
        }
    }
}

/// Cap an inline payload at [`WORKFLOW_OUTPUT_CAP_BYTES`] with an explicit
/// marker (char-boundary safe).
fn truncate_inline(text: &mut String) {
    if text.len() <= WORKFLOW_OUTPUT_CAP_BYTES {
        return;
    }
    let mut cut = WORKFLOW_OUTPUT_CAP_BYTES;
    while !text.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    let omitted = text.len() - cut;
    text.truncate(cut);
    text.push_str(&format!(
        "\n[... workflow output truncated: {omitted} bytes omitted ...]"
    ));
}

/// One-shot sub-agent spawner: wraps `SubagentRunTool` sync mode — the
/// same open+eval+close-composed run path the model's own `agent_run`
/// calls hit — so depth budget, caps, model resolution, the approval
/// ceiling, AND the `SessionBeforeFork` veto are the existing machinery's,
/// not a parallel path.
pub struct CliSubAgentSpawner {
    manager: SharedSubAgentManager,
    runtime: SubAgentRuntime,
    context: ToolContext,
    /// `SessionBeforeFork` emit (fork veto) — attached to every
    /// `SubagentRunTool` this spawner builds (same idiom as the registry
    /// wiring for the model-facing spawn tools).
    extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
}

impl CliSubAgentSpawner {
    #[must_use]
    pub fn new(
        manager: SharedSubAgentManager,
        runtime: SubAgentRuntime,
        context: ToolContext,
    ) -> Self {
        Self {
            manager,
            runtime,
            context,
            extension_runner: None,
        }
    }

    /// Attach the extension runner so each spawned child hits the fork
    /// veto seam before forking session state.
    #[must_use]
    pub fn with_extension_runner(
        mut self,
        runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    ) -> Self {
        self.extension_runner = runner;
        self
    }
}

impl SubAgentSpawner for CliSubAgentSpawner {
    fn spawn_and_run(
        &self,
        objective: SubAgentObjective,
    ) -> Pin<Box<dyn Future<Output = SubAgentRunResult> + Send + '_>> {
        Box::pin(async move {
            let mut tool = SubagentRunTool::new(Arc::clone(&self.manager), self.runtime.clone());
            if let Some(runner) = &self.extension_runner {
                tool = tool.with_extension_runner(Arc::clone(runner));
            }
            let mut input = json!({ "prompt": objective.objective });
            if let Some(role) = objective.role.as_deref() {
                input["role"] = json!(role);
            }
            if let Some(model) = objective.model.as_deref() {
                input["model"] = json!(model);
            }
            let run = tool.execute(input, &self.context);
            let outcome = match objective.timeout_s {
                Some(secs) => match tokio::time::timeout(Duration::from_secs(secs), run).await {
                    Ok(outcome) => outcome,
                    Err(_elapsed) => {
                        // Dropping the run future cancels the child — the
                        // same outcome the run path itself reports as
                        // `SubAgentStatus::Cancelled`, so the status
                        // vocabulary stays honest.
                        return SubAgentRunResult {
                            result: format!(
                                "run_agent: child exceeded the requested {secs}s and was cancelled"
                            ),
                            steps: 0,
                            status: "cancelled".to_string(),
                        };
                    }
                },
                None => run.await,
            };
            match outcome {
                Ok(result) => {
                    let metadata = result.metadata.as_ref();
                    let steps = metadata
                        .and_then(|metadata| metadata.get("steps_taken"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32;
                    let status = metadata
                        .and_then(|metadata| metadata.get("status"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            if result.success {
                                "completed"
                            } else {
                                "failed"
                            }
                            .to_string()
                        });
                    SubAgentRunResult {
                        result: result.content,
                        steps,
                        status,
                    }
                }
                // A spawn refusal/execution error is an honest failed
                // child: the script sees it as data.
                Err(err) => SubAgentRunResult {
                    result: err.to_string(),
                    steps: 0,
                    status: "failed".to_string(),
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    /// Recorded-replay spawner — the `with_program` precedent adapted to
    /// the seam: no real sub-agents, no subprocess of the binary, no
    /// network. The Python kernel itself does run (it is the tool's own
    /// one-shot kernel, like the REPL runtime tests).
    struct RecordedSpawner {
        calls: Mutex<Vec<SubAgentObjective>>,
    }

    impl SubAgentSpawner for RecordedSpawner {
        fn spawn_and_run(
            &self,
            objective: SubAgentObjective,
        ) -> Pin<Box<dyn Future<Output = SubAgentRunResult> + Send + '_>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(objective.clone());
                SubAgentRunResult {
                    result: format!("recorded: {}", objective.objective),
                    steps: 2,
                    status: "completed".to_string(),
                }
            })
        }
    }

    fn tool_with(spawner: Arc<dyn SubAgentSpawner>) -> (WorkflowTool, ToolContext) {
        let workspace = std::env::temp_dir().join("codesmith-workflow-tool-tests");
        let context = ToolContext::new(workspace.clone());
        let manager = crate::tools::subagent::new_shared_subagent_manager(workspace, 4);
        let mut runtime = crate::tools::subagent::SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            context.clone(),
            false,
            None,
            manager.clone(),
        );
        runtime.workflow_timeout = Duration::from_secs(60);
        let tool = WorkflowTool::new(manager, runtime).with_spawner_override(spawner);
        (tool, context)
    }

    fn stub_client() -> crate::llm_client::LlmClientHandle {
        let config = crate::config::Config {
            api_key: Some("test-key".to_string()),
            ..crate::config::Config::default()
        };
        crate::core::engine::resolve_llm_client(&config).expect("stub client should construct")
    }

    #[tokio::test]
    async fn runs_script_and_returns_the_receipt() {
        let spawner = Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        });
        let (tool, context) = tool_with(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        let result = tool
            .execute(
                json!({
                    "script": concat!(
                        "one = run_agent('map the repo')\n",
                        "log('first done')\n",
                        "finalize({'answer': one['result'], 'status': one['status']})"
                    )
                }),
                &context,
            )
            .await
            .expect("workflow tool run");

        assert!(result.success);
        let receipt: Value =
            serde_json::from_str(&result.content).expect("content is the receipt JSON");
        assert_eq!(receipt["status"], "ok");
        assert_eq!(receipt["agent_calls"], 1);
        assert_eq!(receipt["return_value"]["answer"], "recorded: map the repo");
        assert_eq!(receipt["return_value"]["status"], "completed");
        assert_eq!(receipt["logs_tail"][0], "first done");
        let metadata = result.metadata.expect("receipt metadata");
        assert_eq!(metadata["agent_calls"], 1);
        let calls = spawner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].objective, "map the repo");
    }

    #[tokio::test]
    async fn missing_or_empty_script_is_rejected() {
        let (tool, context) = tool_with(Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        }));

        for input in [json!({}), json!({"script": "   "})] {
            let err = tool
                .execute(input, &context)
                .await
                .expect_err("script is required");
            assert!(
                matches!(&err, ToolError::MissingField { field } if field == "script"),
                "expected MissingField(script), got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn out_of_range_max_agents_is_rejected_at_input() {
        let (tool, context) = tool_with(Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        }));

        for bad in [0u64, (MAX_WORKFLOW_AGENTS as u64) + 1] {
            let err = tool
                .execute(json!({"script": "pass", "max_agents": bad}), &context)
                .await
                .expect_err("out-of-range max_agents must be rejected");
            assert!(
                err.to_string().contains("max_agents"),
                "error must name max_agents, got {err}"
            );
        }
    }

    #[tokio::test]
    async fn script_failure_surfaces_traceback_and_logs_tail() {
        let (tool, context) = tool_with(Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        }));

        let err = tool
            .execute(
                json!({"script": "log('about to fall')\nraise ValueError('boom')"}),
                &context,
            )
            .await
            .expect_err("raising script must fail the tool call");

        let message = err.to_string();
        assert!(
            message.contains("ValueError"),
            "traceback missing: {message}"
        );
        assert!(
            message.contains("about to fall"),
            "logs tail must ride the error: {message}"
        );
    }

    #[tokio::test]
    async fn oversized_receipt_is_truncated_with_a_marker() {
        let spawner = Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        });
        let (tool, context) = tool_with(Arc::clone(&spawner) as Arc<dyn SubAgentSpawner>);

        // ~64KB return value against the 16KB inline cap.
        let script = "finalize('x' * 65536)";
        let started = Instant::now();
        let result = tool
            .execute(json!({"script": script}), &context)
            .await
            .expect("big receipts still succeed");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "truncation must not wedge the run"
        );
        assert!(result.success);
        assert!(
            result.content.contains("workflow output truncated"),
            "truncation marker missing (len {})",
            result.content.len()
        );
        assert!(
            result.content.len() < WORKFLOW_OUTPUT_CAP_BYTES + 512,
            "receipt must be near the cap, got {}",
            result.content.len()
        );
        let _ = spawner; // silence unused in release-shaped configs
    }

    /// A vetoing `SessionBeforeFork` runner must cancel a workflow child
    /// exactly like a model-facing `agent_run` child (mirror of
    /// `session_before_fork_cancel_vetoes_agent_spawn` in the sub-agent
    /// tests). The veto fires inside `SubagentRunTool::execute` BEFORE any
    /// LLM work, so this exercises the real `CliSubAgentSpawner` path with
    /// no network: the run tool short-circuits on the Cancel outcome.
    #[tokio::test]
    async fn session_before_fork_veto_cancels_workflow_children() {
        let (runner, seen) = crate::tools::subagent::tests::runner_with_fork_handler(true).await;

        let workspace = std::env::temp_dir().join("codesmith-workflow-veto-tests");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        let context = ToolContext::new(workspace.clone());
        let manager = crate::tools::subagent::new_shared_subagent_manager(workspace, 4);
        let runtime = crate::tools::subagent::SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            context.clone(),
            false,
            None,
            manager.clone(),
        );
        let spawner =
            CliSubAgentSpawner::new(manager, runtime, context).with_extension_runner(Some(runner));

        let result = spawner
            .spawn_and_run(SubAgentObjective {
                objective: "map the repo".to_string(),
                role: None,
                model: None,
                timeout_s: None,
            })
            .await;

        assert_eq!(result.status, "failed", "vetoed child is a failed child");
        assert!(
            result.result.contains("vetoed"),
            "veto error must surface in the child result: {}",
            result.result
        );
        assert_eq!(result.steps, 0);
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|k| k.contains("SessionBeforeFork")),
            "the fork seam fired for the workflow child: {:?}",
            seen.lock().unwrap()
        );
    }

    // === workflow lifecycle events (observe-only, Emit) =====================

    /// The lifecycle family observes the whole run in order — Start before
    /// the kernel spawns, AgentStart/AgentEnd around the child, Log per
    /// `log(...)` call, End on the success path. The recording runner's
    /// handler subscribes to every event, so this also proves nothing
    /// else leaks through the tool path.
    #[tokio::test]
    async fn lifecycle_events_follow_the_run_sequence() {
        let (runner, seen) = crate::tools::subagent::tests::runner_with_fork_handler(false).await;
        let (tool, context) = tool_with(Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        }));
        let tool = tool.with_extension_runner(runner);

        let result = tool
            .execute(
                json!({
                    "script": concat!(
                        "one = run_agent('map the repo')\n",
                        "log('first done')\n",
                        "finalize({'answer': one['result']})"
                    )
                }),
                &context,
            )
            .await
            .expect("workflow tool run");

        assert!(result.success, "the run itself must still succeed");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [
                "WorkflowStart",
                "WorkflowAgentStart",
                "WorkflowAgentEnd",
                "WorkflowLog",
                "WorkflowEnd",
            ],
            "the lifecycle family must observe the run in order"
        );
    }

    /// A run rejected before it began (out-of-range `max_agents`) emits
    /// nothing — no `WorkflowStart`, none of the family. Handler authors
    /// rely on the Start/End pairing wrapping only runs that began.
    #[tokio::test]
    async fn budget_rejected_run_emits_no_lifecycle_events() {
        let (runner, seen) = crate::tools::subagent::tests::runner_with_fork_handler(false).await;
        let (tool, context) = tool_with(Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        }));
        let tool = tool.with_extension_runner(runner);

        let err = tool
            .execute(json!({"script": "pass", "max_agents": 0}), &context)
            .await
            .expect_err("out-of-range max_agents must be rejected");
        assert!(
            err.to_string().contains("max_agents"),
            "sanity: the rejection names max_agents, got {err}"
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "a run that never began emits nothing: {:?}",
            seen.lock().unwrap()
        );
    }

    /// Minimal `ExtensionContext` for binding a recording runner (same
    /// shape as the sub-agent tests' `ForkCtx`, which is private to that
    /// module).
    struct WorkflowEventsCtx;
    #[async_trait]
    impl codesmith_agent::extension::ExtensionContext for WorkflowEventsCtx {
        fn cwd(&self) -> &std::path::Path {
            std::path::Path::new(".")
        }
        fn mode(&self) -> codesmith_agent::extension::ExtensionMode {
            codesmith_agent::extension::ExtensionMode::Interactive
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn signal(&self) -> tokio_util::sync::CancellationToken {
            tokio_util::sync::CancellationToken::new()
        }
        fn generation(&self) -> u64 {
            1
        }
    }
    impl codesmith_agent::extension::ExtensionCommandContext for WorkflowEventsCtx {}

    /// Records every `WorkflowEnd` payload as `"{status} {error}
    /// {agent_calls}"` — enough to pin the success shape, the failure
    /// error kind, and the spawn count without freezing the whole map.
    struct EndRecorder {
        ends: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl codesmith_agent::extension::Handler for EndRecorder {
        async fn handle(
            &self,
            event: &codesmith_agent::extension::ExtensionEvent,
            _ctx: &dyn codesmith_agent::extension::ExtensionContext,
        ) -> Result<
            codesmith_agent::extension::HandlerOutcome,
            codesmith_agent::extension::ExtensionError,
        > {
            if let codesmith_agent::extension::ExtensionEvent::WorkflowEnd(e) = event {
                let error = e.error.as_deref().unwrap_or("none");
                self.ends
                    .lock()
                    .unwrap()
                    .push(format!("{} {error} {}", e.status, e.agent_calls));
            }
            Ok(codesmith_agent::extension::HandlerOutcome::Continue)
        }
    }

    struct EndRecorderExt {
        handler: Arc<EndRecorder>,
    }
    #[async_trait]
    impl codesmith_agent::extension::Extension for EndRecorderExt {
        fn metadata(&self) -> &codesmith_agent::extension::ExtensionMetadata {
            static M: std::sync::OnceLock<codesmith_agent::extension::ExtensionMetadata> =
                std::sync::OnceLock::new();
            M.get_or_init(|| {
                codesmith_agent::extension::ExtensionMetadata::from_strings(
                    "workflow-end-recorder".to_string(),
                    "workflow-end-recorder".to_string(),
                    "0.1.0".to_string(),
                )
            })
        }
        async fn configure(
            &self,
            api: &dyn codesmith_agent::extension::ExtensionApi,
        ) -> Result<(), codesmith_agent::extension::ExtensionError> {
            api.on_variant(
                codesmith_agent::extension::ExtensionEventKind::WorkflowEnd,
                self.handler.clone(),
            )?;
            Ok(())
        }
    }

    async fn runner_with_end_recorder()
    -> (Arc<codesmith_extensions::ExtensionRunner>, Arc<EndRecorder>) {
        let recorder = Arc::new(EndRecorder {
            ends: Mutex::new(Vec::new()),
        });
        let runner = Arc::new(codesmith_extensions::ExtensionRunner::new());
        runner
            .load(
                &(EndRecorderExt {
                    handler: Arc::clone(&recorder),
                }),
            )
            .await
            .expect("configure EndRecorderExt");
        runner.bind_core(Arc::new(WorkflowEventsCtx));
        (runner, recorder)
    }

    /// `WorkflowEnd` carries the terminal outcome without the return
    /// value: success is `completed`/no error with the spawn count, a
    /// raising script fails with the `script_error` kind (and only
    /// children that actually ran are counted).
    #[tokio::test]
    async fn workflow_end_payload_carries_outcome_and_error_kind() {
        let (runner, recorder) = runner_with_end_recorder().await;
        let (tool, context) = tool_with(Arc::new(RecordedSpawner {
            calls: Mutex::new(Vec::new()),
        }));
        let tool = tool.with_extension_runner(runner);

        let ok = tool
            .execute(
                json!({"script": "one = run_agent('map it')\nfinalize(one['status'])"}),
                &context,
            )
            .await
            .expect("successful run");
        assert!(ok.success);
        let failed = tool
            .execute(
                json!({"script": "log('about to fall')\nraise ValueError('boom')"}),
                &context,
            )
            .await
            .expect_err("raising script fails the run");
        assert!(
            failed.to_string().contains("ValueError"),
            "sanity: the tool error carries the traceback"
        );

        let ends = recorder.ends.lock().unwrap();
        assert_eq!(
            ends.as_slice(),
            ["completed none 1", "failed script_error 0"],
            "one End per run: success shape then failure kind"
        );
    }
}
