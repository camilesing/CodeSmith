//! Re-export of [`codesmith_agent_runtime::cycle_manager`] (extracted in
//! Phase 6 §6a) plus the TUI-local `StructuredState` capture/render helper.
//!
//! The canonical home for the cycle data types (`CycleConfig`,
//! `CycleBriefing`, `CycleArchiveHeader`), the briefing pipeline
//! (`produce_briefing` / `extract_carry_forward` / `build_seed_messages`), and
//! the archive IO (`archive_cycle` / `open_archive`) is now the
//! `codesmith-agent-runtime` crate. This shim keeps `crate::cycle_manager::*`
//! paths in the TUI working. `StructuredState` stays here because its snapshot
//! sources (`SharedTodoList`, `SharedPlanState`, `SharedSubAgentManager`,
//! `WorkingSet`) are still TUI-local.

use std::path::PathBuf;

use crate::tools::plan::{PlanSnapshot, SharedPlanState};
use crate::tools::subagent::{SharedSubAgentManager, SubAgentResult, SubAgentStatus};
use crate::tools::todo::{SharedTodoList, TodoListSnapshot};
use crate::working_set::WorkingSet;

pub use codesmith_agent_runtime::cycle_manager::*;

/// Roll-up of state that survives a cycle boundary deterministically.
///
/// Construction is cheap — borrow the live state, snapshot it once, render it
/// into a system block. The snapshot decouples rendering from any mutex held
/// by the engine.
#[derive(Debug, Clone, Default)]
pub struct StructuredState {
    pub mode_label: String,
    pub workspace: PathBuf,
    pub cwd: Option<PathBuf>,
    pub working_set_summary: Option<String>,
    pub todo_snapshot: Option<TodoListSnapshot>,
    pub plan_snapshot: Option<PlanSnapshot>,
    pub subagent_snapshots: Vec<SubAgentResult>,
}

impl StructuredState {
    /// Capture the current state. All locks are held only for the duration of
    /// the snapshot.
    pub async fn capture(
        mode_label: impl Into<String>,
        workspace: PathBuf,
        cwd: Option<PathBuf>,
        working_set: &WorkingSet,
        todos: &SharedTodoList,
        plan_state: &SharedPlanState,
        subagents: Option<&SharedSubAgentManager>,
    ) -> Self {
        let working_set_summary = working_set.summary_block(&workspace);

        let todo_snapshot = {
            let guard = todos.lock().await;
            let snap = guard.snapshot();
            if snap.items.is_empty() {
                None
            } else {
                Some(snap)
            }
        };

        let plan_snapshot = {
            let guard = plan_state.lock().await;
            if guard.is_empty() {
                None
            } else {
                Some(guard.snapshot())
            }
        };

        let subagent_snapshots = if let Some(handle) = subagents {
            let guard = handle.read().await;
            guard
                .list()
                .into_iter()
                .filter(|s| matches!(s.status, SubAgentStatus::Running))
                .collect()
        } else {
            Vec::new()
        };

        Self {
            mode_label: mode_label.into(),
            workspace,
            cwd,
            working_set_summary,
            todo_snapshot,
            plan_snapshot,
            subagent_snapshots,
        }
    }

    /// Render the structured state as a single system block. Returns `None`
    /// when there is nothing meaningful to carry forward (rare in practice —
    /// at least the workspace and mode are always present).
    #[must_use]
    pub fn to_system_block(&self) -> Option<String> {
        let mut out = String::new();
        out.push_str("## Cycle State (Auto-Preserved)\n\n");
        out.push_str(&format!("- Mode: `{}`\n", self.mode_label));
        out.push_str(&format!("- Workspace: `{}`\n", self.workspace.display()));
        if let Some(cwd) = self.cwd.as_ref() {
            out.push_str(&format!("- Cwd: `{}`\n", cwd.display()));
        }

        if self.todo_snapshot.is_some() || self.plan_snapshot.is_some() {
            out.push_str("\n### Work\n");
        }

        if let Some(todos) = self.todo_snapshot.as_ref() {
            out.push_str(&format!(
                "\nChecklist ({}% complete)\n",
                todos.completion_pct
            ));
            for item in &todos.items {
                let marker = match item.status {
                    crate::tools::todo::TodoStatus::Pending => "[ ]",
                    crate::tools::todo::TodoStatus::InProgress => "[~]",
                    crate::tools::todo::TodoStatus::Completed => "[✓]",
                };
                out.push_str(&format!("- {marker} {}\n", item.content));
            }
        }

        if let Some(plan) = self.plan_snapshot.as_ref() {
            out.push_str("\nStrategy metadata\n");
            if let Some(explanation) = plan.explanation.as_ref() {
                out.push_str(&format!("{explanation}\n\n"));
            }
            for item in &plan.items {
                let marker = match item.status {
                    crate::tools::plan::StepStatus::Pending => "[ ]",
                    crate::tools::plan::StepStatus::InProgress => "[~]",
                    crate::tools::plan::StepStatus::Completed => "[✓]",
                };
                out.push_str(&format!("- {marker} {}\n", item.step));
            }
        }

        if !self.subagent_snapshots.is_empty() {
            out.push_str("\n### Open Sub-Agents\n");
            for s in &self.subagent_snapshots {
                let role = s.assignment.role.as_deref().unwrap_or("—");
                let goal = if s.assignment.objective.is_empty() {
                    "(no objective set)"
                } else {
                    s.assignment.objective.as_str()
                };
                out.push_str(&format!("- `{}` (role: {}) — {}\n", s.agent_id, role, goal));
            }
        }

        if let Some(working_set) = self.working_set_summary.as_deref() {
            out.push('\n');
            out.push_str(working_set);
            out.push('\n');
        }

        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_state_to_system_block_renders_minimal() {
        let state = StructuredState {
            mode_label: "agent".to_string(),
            workspace: PathBuf::from("/tmp/ws"),
            cwd: None,
            working_set_summary: None,
            todo_snapshot: None,
            plan_snapshot: None,
            subagent_snapshots: Vec::new(),
        };
        let block = state.to_system_block().expect("renders");
        assert!(block.contains("Mode: `agent`"));
        assert!(block.contains("Workspace: `/tmp/ws`"));
    }

    #[test]
    fn structured_state_to_system_block_unifies_work_state() {
        let state = StructuredState {
            mode_label: "agent".to_string(),
            workspace: PathBuf::from("/tmp/ws"),
            cwd: None,
            working_set_summary: None,
            todo_snapshot: Some(TodoListSnapshot {
                items: vec![crate::tools::todo::TodoItem {
                    id: 1,
                    content: "Run focused tests".to_string(),
                    status: crate::tools::todo::TodoStatus::InProgress,
                }],
                completion_pct: 0,
                in_progress_id: Some(1),
            }),
            plan_snapshot: Some(PlanSnapshot {
                explanation: Some("Keep sidebar state unified".to_string()),
                items: vec![crate::tools::plan::PlanItemArg {
                    step: "Update prompts".to_string(),
                    status: crate::tools::plan::StepStatus::Pending,
                }],
            }),
            subagent_snapshots: Vec::new(),
        };

        let block = state.to_system_block().expect("renders");

        assert!(block.contains("### Work"));
        assert!(block.contains("Checklist (0% complete)"));
        assert!(block.contains("Strategy"));
        assert!(!block.contains("### Plan"));
        assert!(!block.contains("### Todos"));
    }
}
