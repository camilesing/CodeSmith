//! Plan shared state.
//!
//! State types extracted from `crates/tui/src/tools/plan.rs`;
//! the tool implementations stay in tui.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Status of a plan step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

impl StepStatus {
    #[allow(dead_code)]
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "pending" => Some(StepStatus::Pending),
            "in_progress" | "inprogress" => Some(StepStatus::InProgress),
            "completed" | "done" => Some(StepStatus::Completed),
            _ => None,
        }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn symbol(&self) -> &'static str {
        match self {
            StepStatus::Pending => "○",
            StepStatus::InProgress => "◎",
            StepStatus::Completed => "●",
        }
    }
}

/// Input representation for a plan item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItemArg {
    pub step: String,
    pub status: StepStatus,
}

/// Update payload used by the plan tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePlanArgs {
    #[serde(default)]
    pub explanation: Option<String>,
    pub plan: Vec<PlanItemArg>,
}

/// A plan step with timing information
#[derive(Debug, Clone)]
pub struct PlanStep {
    pub text: String,
    pub status: StepStatus,
    /// When the step was started (transitioned to `InProgress`)
    pub started_at: Option<Instant>,
    /// When the step was completed
    pub completed_at: Option<Instant>,
}

impl PlanStep {
    /// Create a new plan step.
    pub fn new(text: String, status: StepStatus) -> Self {
        Self {
            text,
            status,
            started_at: None,
            completed_at: None,
        }
    }

    /// Get the elapsed time if the step has timing info
    #[must_use]
    pub fn elapsed(&self) -> Option<Duration> {
        match (self.started_at, self.completed_at) {
            (Some(start), Some(end)) => Some(end.duration_since(start)),
            (Some(start), None) if self.status == StepStatus::InProgress => Some(start.elapsed()),
            _ => None,
        }
    }

    /// Format elapsed time for display
    #[must_use]
    pub fn elapsed_str(&self) -> String {
        match self.elapsed() {
            Some(d) => {
                let secs = d.as_secs();
                if secs < 60 {
                    format!("{secs}s")
                } else if secs < 3600 {
                    format!("{}m {}s", secs / 60, secs % 60)
                } else {
                    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
                }
            }
            None => String::new(),
        }
    }
}

/// Serializable snapshot for display
#[derive(Debug, Clone, Serialize)]
pub struct PlanSnapshot {
    pub explanation: Option<String>,
    pub items: Vec<PlanItemArg>,
}

/// State tracking for the current plan
#[derive(Debug, Clone, Default)]
pub struct PlanState {
    explanation: Option<String>,
    steps: Vec<PlanStep>,
}

impl PlanState {
    /// Event-sourcing slice 3 — the plan projection: rebuild the plan
    /// from a transcript by folding every `update_plan` call that actually
    /// executed (see [`super::executed_tool_call_ids`]); each input
    /// replaces the plan — last write wins. Parsing mirrors the live
    /// `UpdatePlanTool::execute` (tool-impls `plan.rs`): a non-string
    /// `explanation` is ignored, a missing or unrecognized `status`
    /// defaults to pending — a serde round-trip would reject inputs the
    /// live tool accepted and restore a stale plan. An item with a
    /// non-string `step` skips the whole write. Step timing is stamped at
    /// rebuild time (instants are not persisted); it is display-only
    /// metadata.
    pub fn rebuild_from_messages(messages: &[codesmith_agent::models::Message]) -> Self {
        use codesmith_agent::models::ContentBlock;

        let executed = super::executed_tool_call_ids(messages);

        let mut state = Self::default();
        for message in messages {
            if message.role != "assistant" {
                continue;
            }
            for block in &message.content {
                let ContentBlock::ToolUse {
                    id, name, input, ..
                } = block
                else {
                    continue;
                };
                if name != "update_plan" || !executed.contains(id.as_str()) {
                    continue;
                }
                let explanation = input
                    .get("explanation")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
                let Some(plan) = input.get("plan").and_then(|v| v.as_array()) else {
                    continue;
                };
                let mut items = Vec::new();
                let mut coherent = true;
                for item in plan {
                    let Some(step) = item.get("step").and_then(|v| v.as_str()) else {
                        coherent = false;
                        break;
                    };
                    let status = item
                        .get("status")
                        .and_then(|v| v.as_str())
                        .and_then(StepStatus::from_str)
                        .unwrap_or(StepStatus::Pending);
                    items.push(PlanItemArg {
                        step: step.to_string(),
                        status,
                    });
                }
                if coherent {
                    state.update(UpdatePlanArgs {
                        explanation,
                        plan: items,
                    });
                }
            }
        }
        state
    }

    /// Check whether the plan is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.explanation.as_deref().unwrap_or("").is_empty()
    }

    pub fn update(&mut self, args: UpdatePlanArgs) {
        self.explanation = args.explanation.filter(|s| !s.trim().is_empty());

        let now = Instant::now();
        let mut new_steps = Vec::new();
        let mut in_progress_seen = false;

        for item in args.plan {
            // Try to find existing step to preserve timing
            let existing = self.steps.iter().find(|s| s.text == item.step);

            let mut status = item.status;
            // Enforce single in_progress
            if status == StepStatus::InProgress {
                if in_progress_seen {
                    status = StepStatus::Pending;
                } else {
                    in_progress_seen = true;
                }
            }

            let step = if let Some(old) = existing {
                let mut s = old.clone();
                let old_status = s.status.clone();
                s.status = status.clone();

                // Track timing transitions
                if old_status == StepStatus::Pending && status == StepStatus::InProgress {
                    s.started_at = Some(now);
                }
                if old_status == StepStatus::InProgress && status == StepStatus::Completed {
                    s.completed_at = Some(now);
                }

                s
            } else {
                let mut s = PlanStep::new(item.step, status.clone());
                if status == StepStatus::InProgress {
                    s.started_at = Some(now);
                }
                s
            };

            new_steps.push(step);
        }

        self.steps = new_steps;
    }

    pub fn snapshot(&self) -> PlanSnapshot {
        PlanSnapshot {
            explanation: self.explanation.clone(),
            items: self
                .steps
                .iter()
                .map(|s| PlanItemArg {
                    step: s.text.clone(),
                    status: s.status.clone(),
                })
                .collect(),
        }
    }

    pub fn explanation(&self) -> Option<&str> {
        self.explanation.as_deref()
    }

    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    /// Get counts of steps by status
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut pending = 0;
        let mut in_progress = 0;
        let mut completed = 0;
        for s in &self.steps {
            match s.status {
                StepStatus::Pending => pending += 1,
                StepStatus::InProgress => in_progress += 1,
                StepStatus::Completed => completed += 1,
            }
        }
        (pending, in_progress, completed)
    }

    /// Get progress as a percentage
    pub fn progress_percent(&self) -> u8 {
        if self.steps.is_empty() {
            return 0;
        }
        let completed = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Completed)
            .count();
        let percent = completed.saturating_mul(100) / self.steps.len();
        u8::try_from(percent).unwrap_or(u8::MAX)
    }
}

/// Validation result for plan transitions
#[derive(Debug)]
#[allow(dead_code)]
pub enum PlanValidation {
    Ok,
    Warning(String),
    Error(String),
}

/// Validate a plan update
#[allow(dead_code)]
pub fn validate_plan_update(current: &PlanState, update: &UpdatePlanArgs) -> PlanValidation {
    let current_steps: std::collections::HashMap<_, _> = current
        .steps()
        .iter()
        .map(|s| (s.text.clone(), &s.status))
        .collect();

    for item in &update.plan {
        if let Some(old_status) = current_steps.get(&item.step) {
            // Check for invalid transitions
            match (old_status, &item.status) {
                (StepStatus::Completed, StepStatus::Pending) => {
                    return PlanValidation::Warning(format!(
                        "Step '{}' was completed but is now pending",
                        item.step
                    ));
                }
                (StepStatus::Completed, StepStatus::InProgress) => {
                    return PlanValidation::Warning(format!(
                        "Step '{}' was completed but is now in progress",
                        item.step
                    ));
                }
                _ => {}
            }
        }
    }

    PlanValidation::Ok
}

/// Shared reference to `PlanState` for use across tools
pub type SharedPlanState = Arc<Mutex<PlanState>>;

/// Create a new shared `PlanState`
pub fn new_shared_plan_state() -> SharedPlanState {
    Arc::new(Mutex::new(PlanState::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{assistant_tool_use, tool_result};

    #[test]
    fn plan_projection_last_write_wins() {
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "update_plan",
                serde_json::json!({"explanation": "first", "plan": [
                    {"step": "s1", "status": "pending"},
                    {"step": "s2", "status": "pending"},
                ]}),
            ),
            tool_result("call_2", false),
            assistant_tool_use(
                "call_2",
                "update_plan",
                serde_json::json!({"plan": [
                    {"step": "s1", "status": "completed"},
                    {"step": "s3", "status": "in_progress"},
                ]}),
            ),
            // Malformed (missing `plan`) is skipped, not fatal.
            tool_result("call_3", false),
            assistant_tool_use(
                "call_3",
                "update_plan",
                serde_json::json!({"explanation": "broken"}),
            ),
            // Unrelated calls ignored.
            tool_result("call_4", false),
            assistant_tool_use("call_4", "read_file", serde_json::json!({"path": "x"})),
        ];
        let state = PlanState::rebuild_from_messages(&messages);
        assert!(
            state.explanation.is_none(),
            "last write carried no explanation"
        );
        let snap = state.snapshot();
        assert_eq!(snap.items.len(), 2);
        assert_eq!(snap.items[0].step, "s1");
        assert_eq!(snap.items[0].status, StepStatus::Completed);
        assert_eq!(snap.items[1].step, "s3");
        assert_eq!(snap.items[1].status, StepStatus::InProgress);
    }

    #[test]
    fn plan_projection_parses_leniently_like_live_tool() {
        // The live UpdatePlanTool accepts "done"/"inprogress" statuses and
        // defaults a missing status to pending; a serde round-trip would
        // reject both and restore a stale plan on reload.
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "update_plan",
                serde_json::json!({"plan": [
                    {"step": "s1", "status": "done"},
                    {"step": "s2"},
                ]}),
            ),
        ];
        let state = PlanState::rebuild_from_messages(&messages);
        let snap = state.snapshot();
        assert_eq!(snap.items.len(), 2);
        assert_eq!(snap.items[0].status, StepStatus::Completed);
        assert_eq!(snap.items[1].status, StepStatus::Pending);
    }

    #[test]
    fn plan_projection_skips_errored_and_unpaired_calls() {
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "update_plan",
                serde_json::json!({"plan": [{"step": "ok", "status": "pending"}]}),
            ),
            // Errored write: not folded.
            tool_result("call_2", true),
            assistant_tool_use(
                "call_2",
                "update_plan",
                serde_json::json!({"plan": [{"step": "err", "status": "pending"}]}),
            ),
            // Unpaired write (interrupted turn): not folded.
            assistant_tool_use(
                "call_3",
                "update_plan",
                serde_json::json!({"plan": [{"step": "dangling", "status": "pending"}]}),
            ),
        ];
        let state = PlanState::rebuild_from_messages(&messages);
        let snap = state.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].step, "ok");
    }

    #[test]
    fn plan_projection_empty_when_no_calls() {
        let messages = vec![codesmith_agent::models::Message {
            role: "user".to_string(),
            content: vec![codesmith_agent::models::ContentBlock::Text {
                text: "hi".to_string(),
                cache_control: None,
            }],
        }];
        assert!(PlanState::rebuild_from_messages(&messages).is_empty());
    }
}
