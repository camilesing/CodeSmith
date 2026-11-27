//! Plan tool implementation with step tracking and validation

use async_trait::async_trait;
use serde_json::json;

pub use codesmith_agent_runtime::tool_state::plan::*;
use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use codesmith_agent_runtime::tools::tool_catalog::UPDATE_PLAN_NAME;

// === Types ===

// === Plan State ===

// === UpdatePlanTool - ToolSpec implementation ===

/// Tool for updating the implementation plan
pub struct UpdatePlanTool {
    plan_state: SharedPlanState,
}

impl UpdatePlanTool {
    pub fn new(plan_state: SharedPlanState) -> Self {
        Self { plan_state }
    }
}

#[async_trait]
impl ToolSpec for UpdatePlanTool {
    fn name(&self) -> &'static str {
        UPDATE_PLAN_NAME
    }

    fn description(&self) -> &'static str {
        "Update optional high-level strategy metadata for complex initiatives. Use checklist_write for primary Work progress; update_plan should capture phase-level approach changes, not duplicate checklist items. Each strategy step has a description and status (pending, in_progress, completed). Optionally include an explanation of the overall approach."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "explanation": {
                    "type": "string",
                    "description": "Optional high-level explanation of the plan or approach"
                },
                "plan": {
                    "type": "array",
                    "description": "List of plan steps",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": {
                                "type": "string",
                                "description": "Description of the step"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Step status"
                            }
                        },
                        "required": ["step", "status"]
                    }
                }
            },
            "required": ["plan"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::AllowedInPlanMode,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let explanation = input
            .get("explanation")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        let plan_items = input
            .get("plan")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::invalid_input("Missing or invalid 'plan' array"))?;

        let mut plan_args = Vec::new();
        for item in plan_items {
            let step = item
                .get("step")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_input("Plan item missing 'step'"))?;

            // Absent keeps the default; an unrecognized value is refused,
            // as the checklist tools do — it used to read as `pending`
            // behind a success message.
            let status = match item.get("status") {
                None | Some(serde_json::Value::Null) => StepStatus::Pending,
                Some(v) => v.as_str().and_then(StepStatus::from_str).ok_or_else(|| {
                    ToolError::invalid_input(format!(
                        "invalid 'status' value: {v} (expected pending, in_progress, or completed)"
                    ))
                })?,
            };

            plan_args.push(PlanItemArg {
                step: step.to_string(),
                status,
            });
        }

        let args = UpdatePlanArgs {
            explanation,
            plan: plan_args,
        };

        let mut state = self.plan_state.lock().await;

        state.update(args);

        let snapshot = state.snapshot();
        let (pending, in_progress, completed) = state.counts();
        let progress = state.progress_percent();

        let result = serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".to_string());

        Ok(ToolResult::success(format!(
            "Plan updated: {pending} pending, {in_progress} in progress, {completed} completed ({progress}% done)\n{result}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn update_plan_refuses_an_unrecognized_step_status() {
        let tool = UpdatePlanTool::new(new_shared_plan_state());
        let err = tool
            .execute(
                json!({"plan": [{"step": "a", "status": "complete"}]}),
                &ToolContext::new("."),
            )
            .await
            .expect_err("an unrecognized status is refused, not stored as pending");
        assert!(matches!(err, ToolError::InvalidInput { .. }), "{err:?}");

        // An absent status keeps its default.
        let ok = tool
            .execute(json!({"plan": [{"step": "a"}]}), &ToolContext::new("."))
            .await
            .expect("absent status defaults");
        assert!(
            ok.content.contains("1 pending, 0 in progress, 0 completed"),
            "{}",
            ok.content
        );
    }
}
