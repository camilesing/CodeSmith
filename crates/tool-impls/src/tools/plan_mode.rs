//! Plan mode tools — EnterPlanMode, ExitPlanMode, WritePlanFile.
//!
//! Two independent axes decide whether these tools work:
//!
//! - **`AppMode::Plan` on the turn** — the user's choice (`Shift+Tab`,
//!   `/mode plan`, a preset, a headless run). The plan file is the one write
//!   plan mode permits, so the three tools serve this axis directly; the slug
//!   is minted on the first write, leaving a plan-mode turn that only
//!   investigates with no file behind.
//! - **`PlanModeState::active`** — a plan session the *model* opened with
//!   `enter_plan_mode` while the turn was not already in `AppMode::Plan`,
//!   mirroring Claude Code's EnterPlanMode/ExitPlanMode pattern.
//!
//! Known limitations, by design:
//!
//! - **Exiting does not change the session mode by itself.** A tool cannot move
//!   `AppMode` — the user owns it (`/mode`, `Shift+Tab`) — so `exit_plan_mode`
//!   reports the handoff. The *host* may restore it when it approves the call:
//!   the REPL returns the session to the mode it came from and the engine ends
//!   that turn (`should_stop_after_plan_tool`), so the next message is
//!   dispatched with the write tools. A host that does neither (headless, the
//!   thread runtime) leaves the turn read-only and relies on the user's switch.
//! - **A model-initiated `enter_plan_mode` is enforced per call, not by the
//!   registry.** The registry and mode prompt are fixed at turn start from
//!   `AppMode`, so the entry narrows the turn through the engine's execution
//!   gate instead (`plan_mode_refusal`, reading `active` live): from the next
//!   tool call of the *same* turn, a `WritesFiles`/`ExecutesCode` tool is
//!   refused. Its children are narrowed at spawn for the same reason.
//! - **`write_plan_file` serves both axes** (the turn's `AppMode::Plan`, or an
//!   active model-initiated session), minting the slug on the first write.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::plan::SharedPlanState;
use crate::tools::plan_file;
use codesmith_agent_runtime::mode::AppMode;
pub use codesmith_agent_runtime::tool_state::plan_mode::*;
use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

/// Appended to every `exit_plan_mode` result: the plan is the user's to act on,
/// and implementation is the next turn's business.
///
/// A host that restores the session mode when it approves this call (the REPL
/// does, and ends the turn right there) makes the next message writable; a host
/// that does not leaves the read-only set in place, which is why the fallback
/// names the switch. The tool itself never touches the mode.
const PLAN_HANDOFF: &str = "The plan is with the user — do not start implementing in this \
turn. The writable toolset arrives with their next message; if it does not, ask them to \
switch (`/mode agent`, or Shift+Tab) before editing.";

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
plan is finalized. The user must approve before implementation begins.

If the approval is refused, the refusal arrives with the user's feedback as a \
message. Revise the plan file accordingly and call `exit_plan_mode` again — \
plan mode stays active until a plan is approved, so never start implementing a \
refused plan, and never answer a refusal with a question the feedback already \
answers.";

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
        _input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let mut state = self.plan_mode_state.lock().await;

        // Already in plan mode is one refusal whichever axis put us there:
        // entering again would mint a second slug and orphan the plan file
        // already written under the first.
        if state.active || context.mode == AppMode::Plan {
            return Err(ToolError::ExecutionFailed {
                message: "Already in plan mode. Write the plan with write_plan_file, then \
                          call exit_plan_mode to present it."
                    .to_string(),
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

        state.active = true;
        state.current_slug = Some(slug);

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
        vec![
            ToolCapability::RequiresApproval,
            ToolCapability::AllowedInPlanMode,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let mut state = self.plan_mode_state.lock().await;

        if !state.active && context.mode != AppMode::Plan {
            return Err(ToolError::ExecutionFailed {
                message: "Not in plan mode. Call enter_plan_mode first.".to_string(),
            });
        }

        // A plan-mode turn that never wrote a plan has nothing to report.
        let Some(slug) = state.current_slug.clone() else {
            state.active = false;
            return Ok(ToolResult::success(format!(
                "Exited plan mode (no plan file was written). {PLAN_HANDOFF}"
            )));
        };

        // Read the plan file content
        let plan_content = plan_file::read_plan_file(&slug)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to read plan file: {e}"),
            })?
            .unwrap_or_default();

        state.active = false;
        state.current_slug = None;

        if plan_content.trim().is_empty() {
            Ok(ToolResult::success(format!(
                "Exited plan mode (no plan content written). {PLAN_HANDOFF}"
            )))
        } else {
            let mut result = ToolResult::success(format!(
                "Exited plan mode. {PLAN_HANDOFF}\n\nPlan content:\n{plan_content}"
            ));
            result.metadata = Some(json!({
                "plan_content": plan_content,
                "plan_slug": slug,
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
/// PlanState for REPL rendering.
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
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // Hold the guard across the disk write: a concurrent exit/enter
        // used to interleave between the snapshot and the write, writing
        // the plan file after plan mode ended (or under a stale slug).
        // The plan_state lock is only taken after, so no deadlock.
        let mut state = self.plan_mode_state.lock().await;

        // Allowed in a plan-mode turn — the plan file is the one write plan
        // mode permits — or in a model-initiated plan session.
        if !state.active && context.mode != AppMode::Plan {
            return Err(ToolError::PermissionDenied {
                message:
                    "write_plan_file is only available in plan mode. Call enter_plan_mode first."
                        .to_string(),
            });
        }
        // Mint the slug on the first write: a plan-mode turn that only
        // investigates leaves no plan file behind, and the file that does
        // appear is the one the user will be asked to approve.
        let slug = match state.current_slug.clone() {
            Some(slug) => slug,
            None => {
                let slug =
                    plan_file::generate_plan_slug().map_err(|e| ToolError::ExecutionFailed {
                        message: format!("Failed to generate plan slug: {e}"),
                    })?;
                state.current_slug = Some(slug.clone());
                slug
            }
        };

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
        // Also update in-memory PlanState for REPL rendering. Derive plan
        // steps from markdown headings (## / ### lines) OUTSIDE fenced code
        // blocks, preserving existing per-step statuses by text so a
        // rewrite does not reset in-progress/completed steps to pending.
        // The sync always runs (even to an empty plan) so the REPL cannot
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::plan::new_shared_plan_state;
    use codesmith_agent_runtime::test_support::{ScopedCodeSmithHome, lock_test_env};

    fn context_in(mode: AppMode) -> ToolContext {
        ToolContext::new(std::env::temp_dir()).with_mode(mode)
    }

    // lock_test_env serializes the whole async test (env mutation): the
    // guard intentionally outlives every await in the body.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn write_plan_file_works_in_plan_mode_without_entering() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let mode_state = new_shared_plan_mode_state();

        WritePlanFileTool::new(mode_state.clone(), new_shared_plan_state())
            .execute(
                json!({"content": "# Plan\n\n## Context\n\nwhy"}),
                &context_in(AppMode::Plan),
            )
            .await
            .expect("plan mode's one permitted write must not require enter_plan_mode first");

        let slug = mode_state
            .lock()
            .await
            .current_slug
            .clone()
            .expect("the first write mints the slug");
        assert_eq!(
            plan_file::read_plan_file(&slug).await.expect("read"),
            Some("# Plan\n\n## Context\n\nwhy".to_string()),
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn plan_file_tools_stay_refused_outside_plan_mode() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let mode_state = new_shared_plan_mode_state();
        let agent_turn = context_in(AppMode::Agent);

        let error = WritePlanFileTool::new(mode_state.clone(), new_shared_plan_state())
            .execute(json!({"content": "# Plan"}), &agent_turn)
            .await
            .expect_err("an Agent-mode turn must not write the plan file");
        assert!(
            matches!(error, ToolError::PermissionDenied { .. }),
            "the mode gate, not an execution failure: {error:?}"
        );

        assert!(
            ExitPlanModeTool::new(mode_state.clone(), new_shared_plan_state())
                .execute(json!({}), &agent_turn)
                .await
                .is_err(),
            "leaving a plan session this turn is not in must fail loud"
        );
        assert!(mode_state.lock().await.current_slug.is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn exit_plan_mode_hands_over_the_plan_in_plan_mode() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let mode_state = new_shared_plan_mode_state();
        let plan_state = new_shared_plan_state();
        let plan_turn = context_in(AppMode::Plan);

        WritePlanFileTool::new(mode_state.clone(), plan_state.clone())
            .execute(
                json!({"content": "# Plan\n\n## Context\n\nwhy"}),
                &plan_turn,
            )
            .await
            .expect("write");

        let result = ExitPlanModeTool::new(mode_state.clone(), plan_state)
            .execute(json!({}), &plan_turn)
            .await
            .expect("a plan-mode turn must be able to present its plan");
        assert!(
            result.content.contains("## Context"),
            "the plan body is what the user is asked to approve: {}",
            result.content
        );

        let state = mode_state.lock().await;
        assert!(!state.active);
        assert!(state.current_slug.is_none(), "the session is handed over");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn enter_plan_mode_refuses_when_the_turn_is_already_in_plan_mode() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let mode_state = new_shared_plan_mode_state();

        assert!(
            EnterPlanModeTool::new(mode_state.clone(), new_shared_plan_state())
                .execute(json!({}), &context_in(AppMode::Plan))
                .await
                .is_err(),
            "a second entry would mint a second slug and orphan the first plan file"
        );
        assert!(mode_state.lock().await.current_slug.is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn model_initiated_entry_still_serves_an_agent_turn() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let mode_state = new_shared_plan_mode_state();
        let plan_state = new_shared_plan_state();
        let agent_turn = context_in(AppMode::Agent);

        EnterPlanModeTool::new(mode_state.clone(), plan_state.clone())
            .execute(json!({}), &agent_turn)
            .await
            .expect("entering plan mode from an Agent turn");
        assert!(mode_state.lock().await.active);

        WritePlanFileTool::new(mode_state.clone(), plan_state.clone())
            .execute(
                json!({"content": "# Plan\n\n## Context\n\nwhy"}),
                &agent_turn,
            )
            .await
            .expect("the model-initiated session writes its own plan file");

        ExitPlanModeTool::new(mode_state.clone(), plan_state)
            .execute(json!({}), &agent_turn)
            .await
            .expect("exiting the model-initiated session");

        let state = mode_state.lock().await;
        assert!(!state.active);
        assert!(state.current_slug.is_none());
    }
}
