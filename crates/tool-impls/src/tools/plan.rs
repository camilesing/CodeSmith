//! Plan tool implementation with step tracking and validation

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
pub use codesmith_agent_runtime::tool_state::plan::*;

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
        "update_plan"
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
        vec![ToolCapability::WritesFiles]
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

            let status_str = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");

            let status = StepStatus::from_str(status_str).unwrap_or(StepStatus::Pending);

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
