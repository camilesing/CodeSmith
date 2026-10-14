//! Plan mode tools — EnterPlanMode, ExitPlanMode, WritePlanFile.
//!
//! These tools enable model-initiated plan mode transitions, mirroring
//! Claude Code's EnterPlanMode/ExitPlanMode pattern. In plan mode, only
//! read-only tools and `write_plan_file` are available; all other
//! state-mutating tools are blocked at the dispatch layer.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::plan::SharedPlanState;
use crate::tools::plan_file;
pub use codesmith_agent_runtime::tool_state::plan_mode::*;
use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

/// 5-phase workflow instructions injected on EnterPlanMode.
const PLAN_MODE_INSTRUCTIONS: &str = "\
Plan mode is active. The user indicated that they do not want you to execute yet — \
you MUST NOT make any edits (with the exception of the plan file mentioned below), \
run any non-readonly tools, or otherwise make any changes to the system.

## Plan File Info
Your plan file is at the path shown in the tool result. This is the ONLY file \
you may write in plan mode. All other file writes, shell commands, and code \
execution are blocked.

## Plan Workflow

### Phase 1: Initial Understanding
Goal: Gain a comprehensive understanding of the user's request by reading \
through code and asking them questions. Only use read-only tools and \
search agents in this phase.

### Phase 2: Deep Exploration
Goal: Explore the codebase in parallel. Identify all files that will need \
changes. Map dependencies and constraints.

### Phase 3: Strategic Planning
Goal: Design an implementation approach. Consider trade-offs, sequencing, \
and risks. Use `write_plan_file` to persist your evolving plan to disk.

### Phase 4: Final Plan
Goal: Write your finalized plan using `write_plan_file`. Begin with a \
**Context** section explaining why this change is being made. Include only \
your recommended approach. Ensure the plan is concise but detailed enough \
to execute effectively.

### Phase 5: Exit Plan Mode
Goal: Present your plan for user approval. Call `exit_plan_mode` when your \
plan is finalized. The user must approve before implementation begins.";

// === PlanModeState ===

// === EnterPlanModeTool ===

/// Tool for entering plan mode.
///
/// Sets `PlanModeState.active = true`, generates a plan slug, and
/// returns 5-phase workflow instructions for the model to follow.
pub struct EnterPlanModeTool {
    plan_mode_state: SharedPlanModeState,
    #[allow(dead_code)]
    plan_state: SharedPlanState,
}

impl EnterPlanModeTool {
    pub fn new(plan_mode_state: SharedPlanModeState, plan_state: SharedPlanState) -> Self {
        Self {
            plan_mode_state,
            plan_state,
        }
    }
}

#[async_trait]
impl ToolSpec for EnterPlanModeTool {
    fn name(&self) -> &'static str {
        "enter_plan_mode"
    }

    fn description(&self) -> &'static str {
        "Enter plan mode to investigate and design before implementing. \
         In plan mode, you can only read files, search, and write the plan file. \
         No file edits, shell commands, or code execution are permitted. \
         Call exit_plan_mode when you have a finalized plan."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
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
        _input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let mut state = self.plan_mode_state.lock().await;

        if state.active {
            return Err(ToolError::ExecutionFailed {
                message: "Already in plan mode. Call exit_plan_mode to leave.".to_string(),
            });
        }

        // Generate slug for the plan file
        let slug = plan_file::generate_plan_slug().map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to generate plan slug: {e}"),
        })?;

        // Create empty plan file
        plan_file::write_plan_file(&slug, "")
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to create plan file: {e}"),
            })?;

        // Resolve the plan file path (pure computation) BEFORE mutating
        // state so a failure cannot leave plan mode half-activated with
        // the caller believing entry failed.
        let plan_path =
            plan_file::plan_file_path(&slug).map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to resolve plan file path: {e}"),
            })?;

        // Save the current mode and activate plan mode
        // The caller (the turn loop, in host_executor) should set pre_plan_mode to the current AppMode name
        // before calling this tool, or we capture it here as "Agent" (default).
        if state.pre_plan_mode.is_none() {
            state.pre_plan_mode = Some("Agent".to_string());
        }
        state.active = true;
        state.current_slug = Some(slug);
        state.model_initiated = true;

        let message = format!(
            "Entered plan mode. Plan file: {}\n\n{}",
            plan_path.display(),
            PLAN_MODE_INSTRUCTIONS
        );

        Ok(ToolResult::success(message))
    }
}

// === ExitPlanModeTool ===

/// Tool for exiting plan mode.
///
/// Requires user approval. Reads the plan file content and returns it
/// so it can be injected into the next turn's conversation context.
pub struct ExitPlanModeTool {
    plan_mode_state: SharedPlanModeState,
    #[allow(dead_code)]
    plan_state: SharedPlanState,
}

impl ExitPlanModeTool {
    pub fn new(plan_mode_state: SharedPlanModeState, plan_state: SharedPlanState) -> Self {
        Self {
            plan_mode_state,
            plan_state,
        }
    }
}

#[async_trait]
impl ToolSpec for ExitPlanModeTool {
    fn name(&self) -> &'static str {
        "exit_plan_mode"
    }

    fn description(&self) -> &'static str {
        "Exit plan mode after the plan is finalized. Requires user approval. \
         The plan file content will be injected into the conversation for the \
         next turn to act on."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let mut state = self.plan_mode_state.lock().await;

        if !state.active {
            return Err(ToolError::ExecutionFailed {
                message: "Not in plan mode. Call enter_plan_mode first.".to_string(),
            });
        }

        let slug = state
            .current_slug
            .clone()
            .ok_or_else(|| ToolError::ExecutionFailed {
                message: "plan mode is active but no plan file slug is set; state is inconsistent"
                    .to_string(),
            })?;
        let pre_mode = state
            .pre_plan_mode
            .clone()
            .unwrap_or_else(|| "Agent".to_string());

        // Read the plan file content
        let plan_content = plan_file::read_plan_file(&slug)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to read plan file: {e}"),
            })?
            .unwrap_or_default();

        // Clear plan mode state
        state.active = false;
        state.pre_plan_mode = None;
        state.current_slug = None;
        state.model_initiated = false;

        if plan_content.trim().is_empty() {
            Ok(ToolResult::success(format!(
                "Exited plan mode (no plan content written). Restoring {pre_mode} mode."
            )))
        } else {
            let mut result = ToolResult::success(format!(
                "Exited plan mode. Restoring {pre_mode} mode.\n\nPlan content:\n{plan_content}"
            ));
            result.metadata = Some(json!({
                "plan_content": plan_content,
                "plan_slug": slug,
                "restored_mode": pre_mode,
            }));
            Ok(result)
        }
    }
}

// === WritePlanFileTool ===

/// Tool for writing the plan file in plan mode.
///
/// This is the ONLY write-capable tool available during plan mode.
/// It writes the plan content to disk AND updates the in-memory
/// PlanState for TUI rendering.
pub struct WritePlanFileTool {
    plan_mode_state: SharedPlanModeState,
    plan_state: SharedPlanState,
}

impl WritePlanFileTool {
    pub fn new(plan_mode_state: SharedPlanModeState, plan_state: SharedPlanState) -> Self {
        Self {
            plan_mode_state,
            plan_state,
        }
    }
}

#[async_trait]
impl ToolSpec for WritePlanFileTool {
    fn name(&self) -> &'static str {
        "write_plan_file"
    }

    fn description(&self) -> &'static str {
        "Write or update the plan file. This is the only file you may write \
         in plan mode. Use this to persist your evolving plan to disk. The \
         plan should be written in markdown format."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The plan content to write, in markdown format"
                }
            },
            "required": ["content"],
            "additionalProperties": false
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
        // Hold the guard across the disk write: a concurrent exit/enter
        // used to interleave between the snapshot and the write, writing
        // the plan file after plan mode ended (or under a stale slug).
        // The plan_state lock is only taken after, so no deadlock.
        let state = self.plan_mode_state.lock().await;

        if !state.active {
            return Err(ToolError::PermissionDenied {
                message:
                    "write_plan_file is only available in plan mode. Call enter_plan_mode first."
                        .to_string(),
            });
        }
        let slug = state
            .current_slug
            .clone()
            .ok_or_else(|| ToolError::ExecutionFailed {
                message: "plan mode is active but no plan file slug is set; state is inconsistent"
                    .to_string(),
            })?;

        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("content"))?;

        // Write to disk
        plan_file::write_plan_file(&slug, content)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to write plan file: {e}"),
            })?;

        // Release plan_mode_state before taking plan_state (lock order:
        // plan_mode_state → plan_state, never nested).
        drop(state);
        // Also update in-memory PlanState for TUI rendering. Derive plan
        // steps from markdown headings (## / ### lines) OUTSIDE fenced code
        // blocks, preserving existing per-step statuses by text so a
        // rewrite does not reset in-progress/completed steps to pending.
        // The sync always runs (even to an empty plan) so the TUI cannot
        // keep showing a stale plan after the file on disk changed.
        let mut plan_state_guard = self.plan_state.lock().await;
        let existing: std::collections::HashMap<&str, crate::tools::plan::StepStatus> =
            plan_state_guard
                .steps()
                .iter()
                .map(|s| (s.text.as_str(), s.status.clone()))
                .collect();
        let mut in_fence = false;
        let steps: Vec<crate::tools::plan::PlanItemArg> = content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                    in_fence = !in_fence;
                    return None;
                }
                if in_fence || !(trimmed.starts_with("## ") || trimmed.starts_with("### ")) {
                    return None;
                }
                let text = trimmed.trim_start_matches('#').trim().to_string();
                Some(crate::tools::plan::PlanItemArg {
                    status: existing
                        .get(text.as_str())
                        .cloned()
                        .unwrap_or(crate::tools::plan::StepStatus::Pending),
                    step: text,
                })
            })
            .collect();

        plan_state_guard.update(crate::tools::plan::UpdatePlanArgs {
            explanation: None,
            plan: steps,
        });

        let path = plan_file::plan_file_path(&slug).map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to resolve plan file path: {e}"),
        })?;

        Ok(ToolResult::success(format!(
            "Plan written to {}",
            path.display()
        )))
    }
}
