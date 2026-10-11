//! Goal tools for the model-visible LLM-as-judge loop.
//!
//! The TUI already has a `/goal` command and passes its objective into the
//! engine prompt. This module keeps the runtime slice separate: a small
//! session-scoped state object plus tools the model can use to inspect and
//! close out that state.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};
pub use codesmith_agent_runtime::tool_state::goal::*;

fn lock_goal_state(
    state: &SharedGoalState,
) -> Result<std::sync::MutexGuard<'_, GoalState>, ToolError> {
    state
        .lock()
        .map_err(|_| ToolError::execution_failed("goal state lock poisoned"))
}

fn parse_token_budget(input: &Value) -> Result<Option<u32>, ToolError> {
    let Some(raw) = input.get("token_budget") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(value) = raw.as_u64() else {
        return Err(ToolError::invalid_input(
            "token_budget must be a non-negative integer",
        ));
    };
    u32::try_from(value)
        .map(Some)
        .map_err(|_| ToolError::invalid_input("token_budget is too large"))
}

fn json_result(snapshot: &GoalSnapshot) -> Result<ToolResult, ToolError> {
    ToolResult::json(snapshot).map_err(|err| ToolError::execution_failed(err.to_string()))
}

pub struct CreateGoalTool {
    goal_state: SharedGoalState,
}

impl CreateGoalTool {
    #[must_use]
    pub fn new(goal_state: SharedGoalState) -> Self {
        Self { goal_state }
    }
}

#[async_trait]
impl ToolSpec for CreateGoalTool {
    fn name(&self) -> &'static str {
        "create_goal"
    }

    fn description(&self) -> &'static str {
        "Create or replace the current runtime goal. Use this when the user asks for a persistent goal that should be audited before the turn is allowed to finish."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "The full objective to pursue. Keep the complete user goal, not a shortened one-turn version."
                },
                "token_budget": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional soft token budget for the goal."
                }
            },
            "required": ["objective"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        let objective = required_str(&input, "objective")?.trim().to_string();
        if objective.is_empty() {
            return Err(ToolError::invalid_input("objective cannot be empty"));
        }
        let token_budget = parse_token_budget(&input)?;
        let snapshot = {
            let mut state = lock_goal_state(&self.goal_state)?;
            state.create(objective, token_budget);
            state.snapshot()
        };
        json_result(&snapshot)
    }
}

pub struct GetGoalTool {
    goal_state: SharedGoalState,
}

impl GetGoalTool {
    #[must_use]
    pub fn new(goal_state: SharedGoalState) -> Self {
        Self { goal_state }
    }
}

#[async_trait]
impl ToolSpec for GetGoalTool {
    fn name(&self) -> &'static str {
        "get_goal"
    }

    fn description(&self) -> &'static str {
        "Inspect the current runtime goal state, including objective, status, token budget, elapsed time, evidence, and blocker."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let snapshot = {
            let state = lock_goal_state(&self.goal_state)?;
            state.snapshot()
        };
        json_result(&snapshot)
    }
}

pub struct UpdateGoalTool {
    goal_state: SharedGoalState,
}

impl UpdateGoalTool {
    #[must_use]
    pub fn new(goal_state: SharedGoalState) -> Self {
        Self { goal_state }
    }
}

#[async_trait]
impl ToolSpec for UpdateGoalTool {
    fn name(&self) -> &'static str {
        "update_goal"
    }

    fn description(&self) -> &'static str {
        "Update the runtime goal. This is the LLM-as-judge completion gate: only mark complete when the objective has been verified against concrete current-state evidence."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["active", "complete", "blocked"],
                    "description": "Use complete only when the goal is fully satisfied; blocked when meaningful progress cannot continue; active to resume or revise the objective."
                },
                "evidence": {
                    "type": "string",
                    "description": "Required when status is complete. Briefly cite the proof that the goal is done."
                },
                "blocker": {
                    "type": "string",
                    "description": "Required when status is blocked. Explain the condition preventing progress."
                },
                "objective": {
                    "type": "string",
                    "description": "Optional replacement objective when status is active."
                }
            },
            "required": ["status"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        let status = required_str(&input, "status")?.trim().to_ascii_lowercase();
        let snapshot = {
            let mut state = lock_goal_state(&self.goal_state)?;
            match status.as_str() {
                "complete" => {
                    let evidence = input
                        .get("evidence")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string();
                    if evidence.is_empty() {
                        return Err(ToolError::invalid_input(
                            "evidence is required when status is complete",
                        ));
                    }
                    state
                        .mark_complete(evidence)
                        .map_err(ToolError::invalid_input)?;
                }
                "blocked" => {
                    let blocker = input
                        .get("blocker")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string();
                    if blocker.is_empty() {
                        return Err(ToolError::invalid_input(
                            "blocker is required when status is blocked",
                        ));
                    }
                    state
                        .mark_blocked(blocker)
                        .map_err(ToolError::invalid_input)?;
                }
                "active" => {
                    let objective = input
                        .get("objective")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string);
                    state.resume(objective).map_err(ToolError::invalid_input)?;
                }
                other => {
                    return Err(ToolError::invalid_input(format!(
                        "unsupported goal status '{other}'"
                    )));
                }
            }
            state.snapshot()
        };
        json_result(&snapshot)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn create_get_and_complete_goal() {
        let state = new_shared_goal_state();
        let ctx = ToolContext::new(".");

        let create = CreateGoalTool::new(state.clone());
        let created = create
            .execute(
                json!({
                    "objective": "ship the runtime slice",
                    "token_budget": 1200
                }),
                &ctx,
            )
            .await
            .expect("create goal");
        assert!(created.success);
        assert!(created.content.contains("\"status\": \"active\""));

        let get = GetGoalTool::new(state.clone());
        let current = get.execute(json!({}), &ctx).await.expect("get goal");
        assert!(current.content.contains("ship the runtime slice"));
        assert!(current.content.contains("\"token_budget\": 1200"));

        let update = UpdateGoalTool::new(state.clone());
        let completed = update
            .execute(
                json!({
                    "status": "complete",
                    "evidence": "focused tests passed"
                }),
                &ctx,
            )
            .await
            .expect("complete goal");
        assert!(completed.content.contains("\"status\": \"complete\""));
        assert!(completed.content.contains("focused tests passed"));
        assert!(!state.lock().expect("goal lock").is_active());
    }

    #[tokio::test]
    async fn update_goal_requires_completion_evidence() {
        let state =
            new_shared_goal_state_from_host(Some("prove completion".to_string()), None, false);
        let update = UpdateGoalTool::new(state);
        let err = update
            .execute(json!({"status": "complete"}), &ToolContext::new("."))
            .await
            .expect_err("missing evidence should fail");

        assert!(err.to_string().contains("evidence is required"));
    }

    #[test]
    fn continuation_prompt_includes_bound_and_goal_state() {
        let snapshot = GoalSnapshot {
            objective: Some("finish issue 2199".to_string()),
            status: "active".to_string(),
            token_budget: None,
            elapsed_seconds: Some(5),
            evidence: None,
            blocker: None,
        };

        let prompt = render_continuation_prompt(&snapshot, 2, 3);
        assert!(prompt.contains("Goal Continuation"));
        assert!(prompt.contains("finish issue 2199"));
        assert!(prompt.contains("Continuation pass: 2/3"));
    }
}
