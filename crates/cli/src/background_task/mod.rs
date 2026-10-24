//! Unified background task registry for sub-agent and dream lifecycle.
//!
//! The registry tracks background sub-agents and dream (memory-consolidation)
//! tasks, polls their source subsystem, and queues completion notifications
//! for the engine to inject.
//!
//! Shell jobs are deliberately not registry tasks: since the
//! `register_shell_task` bridge was removed (with the never-sent
//! `Op::StartBackgroundShell`), background shells live entirely in the shell
//! manager — `/jobs` lists them, `exec_shell background=true` and
//! `task_shell_start` start them, and Ctrl+B detaches a foreground shell
//! through the flag this registry forwards (`background_all`). The registry
//! keeps the shell-manager handle only for that flag. Durable tasks were
//! never registered either; both regain registry integration only if a
//! registration path returns.

mod agent_bridge;
mod dream_task;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::tools::shell::SharedShellManager;
use crate::tools::subagent::{SharedSubAgentManager, SubAgentType};

pub use codesmith_agent_runtime::background_task::{
    BackgroundTaskNotification, BackgroundTaskStatus, BackgroundTaskSummary, BackgroundTaskType,
};
pub use codesmith_agent_runtime::host_services::BgRegistryApi;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Task-specific extension data — mirrors Claude Code's per-type state fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackgroundTaskExtension {
    Agent {
        agent_type: SubAgentType,
        model: String,
        steps_taken: u32,
    },
    Dream {
        consolidation_round: u32,
        memory_path: PathBuf,
    },
}

/// Unified background task state — mirrors Claude Code's `TaskStateBase`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskState {
    pub id: String,
    pub task_type: BackgroundTaskType,
    pub status: BackgroundTaskStatus,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Whether user has been notified of completion.
    /// Terminal tasks are evicted once notified.
    pub notified: bool,
    /// Task-specific extension data.
    pub extension: BackgroundTaskExtension,
    /// Error message if status is Failed/Killed.
    pub error: Option<String>,
    /// Reference to originating subsystem's internal id.
    pub source_id: String,
}

impl From<&BackgroundTaskState> for BackgroundTaskSummary {
    fn from(s: &BackgroundTaskState) -> Self {
        let duration_ms = s
            .ended_at
            .map(|end| (end - s.started_at).num_milliseconds().max(0) as u64);
        Self {
            id: s.id.clone(),
            source_id: s.source_id.clone(),
            task_type: s.task_type,
            status: s.status,
            description: s.description.clone(),
            started_at: s.started_at,
            ended_at: s.ended_at,
            duration_ms,
            error: s.error.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

pub type SharedBackgroundTaskRegistry = Arc<Mutex<BackgroundTaskRegistry>>;

/// Unified background task registry bridging SubAgentManager (and the dream
/// skeleton). Does NOT replace the manager — it wraps it and translates
/// lifecycle events into engine-level events.
pub struct BackgroundTaskRegistry {
    tasks: HashMap<String, BackgroundTaskState>,
    /// Held only to forward the foreground-shell detach flag (Ctrl+B).
    shell_manager: SharedShellManager,
    subagent_manager: SharedSubAgentManager,
    /// Completed task ids waiting for notification injection.
    pending_notifications: VecDeque<String>,
}

impl BackgroundTaskRegistry {
    /// Create a new registry bridging the tracked subsystems.
    pub fn new(shell_manager: SharedShellManager, subagent_manager: SharedSubAgentManager) -> Self {
        Self {
            tasks: HashMap::new(),
            shell_manager,
            subagent_manager,
            pending_notifications: VecDeque::new(),
        }
    }

    /// Register a sub-agent as a background task.
    pub fn register_agent_task(
        &mut self,
        agent_id: String,
        agent_type: SubAgentType,
        model: String,
        prompt: String,
    ) -> BackgroundTaskState {
        let id = format!("bg_agent_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let description = summarize_prompt(&prompt, 80);
        let state = BackgroundTaskState {
            id: id.clone(),
            task_type: BackgroundTaskType::Agent,
            status: BackgroundTaskStatus::Running,
            description,
            started_at: Utc::now(),
            ended_at: None,
            notified: false,
            extension: BackgroundTaskExtension::Agent {
                agent_type,
                model,
                steps_taken: 0,
            },
            error: None,
            source_id: agent_id,
        };
        self.tasks.insert(id, state.clone());
        state
    }

    /// Register a dream (memory consolidation) task.
    pub fn register_dream_task(&mut self, memory_path: PathBuf) -> BackgroundTaskState {
        let id = format!("bg_dream_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let state = BackgroundTaskState {
            id: id.clone(),
            task_type: BackgroundTaskType::Dream,
            status: BackgroundTaskStatus::Running,
            description: "dreaming".to_string(),
            started_at: Utc::now(),
            ended_at: None,
            notified: false,
            extension: BackgroundTaskExtension::Dream {
                consolidation_round: 0,
                memory_path,
            },
            error: None,
            source_id: id.clone(),
        };
        self.tasks.insert(id, state.clone());
        state
    }

    /// Update a single task's status. Returns the new status when the state
    /// moved (and queued a completion notification for terminal states).
    pub fn update_task_status(
        &mut self,
        id: &str,
        new_status: BackgroundTaskStatus,
        error: Option<String>,
    ) -> Option<BackgroundTaskStatus> {
        let state = self.tasks.get_mut(id)?;
        let old_status = state.status;
        if old_status == new_status && error.is_none() {
            return None;
        }
        if old_status.is_terminal() {
            return None;
        }
        state.status = new_status;
        // Only overwrite the stored diagnostic when a new message is
        // actually supplied: a `None`-carrying transition (poll-driven
        // Cancelled/Completed, `cancel_task`) must not erase an earlier
        // update's failure reason from a terminal task.
        if error.is_some() {
            state.error = error;
        }

        if new_status.is_terminal() {
            state.ended_at = Some(Utc::now());
            self.pending_notifications.push_back(id.to_string());
        }

        Some(new_status)
    }

    /// Cancel a background task by delegating to the source subsystem.
    pub async fn cancel_task(&mut self, id: &str) -> Result<()> {
        let state = self
            .tasks
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Background task not found: {id}"))?
            .clone();

        match state.task_type {
            BackgroundTaskType::Agent => {
                // SubAgentManager uses tokio::sync::RwLock
                let mut mgr = self.subagent_manager.write().await;
                mgr.cancel(&state.source_id)?;
            }
            BackgroundTaskType::Dream => {
                // Dream tasks don't delegate — just mark cancelled
            }
        }

        self.update_task_status(id, BackgroundTaskStatus::Cancelled, None);
        Ok(())
    }

    /// Ask the shell manager to detach its foreground shell(s) on their next
    /// poll (Ctrl+B). The detached jobs are shell-manager jobs, not registry
    /// tasks — the registry never sees them.
    pub fn background_all(&mut self) {
        let mut mgr = self
            .shell_manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        mgr.request_foreground_background();
    }

    /// Evict terminal tasks that have been notified.
    pub fn evict_notified(&mut self) {
        let mut to_remove = Vec::new();
        for (id, state) in &self.tasks {
            if state.status.is_terminal() && state.notified {
                to_remove.push(id.clone());
            }
        }
        for id in to_remove {
            self.tasks.remove(&id);
        }
    }

    /// Return pending notification messages ready for injection.
    /// Clears the queue after returning.
    pub fn drain_notifications(&mut self) -> Vec<BackgroundTaskNotification> {
        let mut result = Vec::new();
        while let Some(id) = self.pending_notifications.pop_front() {
            if let Some(state) = self.tasks.get_mut(&id) {
                state.notified = true;
                let duration_ms = state
                    .ended_at
                    .map(|end| (end - state.started_at).num_milliseconds().max(0) as u64);
                result.push(BackgroundTaskNotification {
                    task_id: state.id.clone(),
                    task_type: state.task_type,
                    status: state.status,
                    description: state.description.clone(),
                    result_summary: None,
                    duration_ms,
                });
            }
        }
        result
    }

    /// List all background tasks.
    pub fn list_tasks(&self) -> Vec<BackgroundTaskSummary> {
        self.tasks
            .values()
            .map(BackgroundTaskSummary::from)
            .collect()
    }

    /// Get a specific task by id.
    pub fn get_task(&self, id: &str) -> Option<BackgroundTaskState> {
        self.tasks.get(id).cloned()
    }

    /// Poll all running tasks, update status from the source subsystem, and
    /// queue completion notifications for terminal transitions.
    pub async fn poll_tasks(&mut self) {
        // Poll agent tasks — SubAgentManager uses tokio::sync::RwLock
        let agent_ids: Vec<(String, String)> = self
            .tasks
            .iter()
            .filter(|(_, s)| {
                s.task_type == BackgroundTaskType::Agent
                    && s.status == BackgroundTaskStatus::Running
            })
            .map(|(id, s)| (id.clone(), s.source_id.clone()))
            .collect();

        for (bg_id, agent_id) in agent_ids {
            // Scope the read lock
            let agent_info: Option<(BackgroundTaskStatus, Option<String>, u32)> = {
                let mgr = self.subagent_manager.read().await;
                // The unfiltered list, not `list_filtered(false)`: the
                // filter drops prior-session terminal agents and `cleanup`
                // evicts old terminal agents, so a background agent that
                // finished and was cleaned up before this poll vanished
                // from the filtered view — leaving the registry task
                // `Running` forever, with no notification and no eviction.
                let agents = mgr.list();
                let result = agents.iter().find(|a| a.agent_id == agent_id);
                result.map(|r| {
                    (
                        r.status.clone().into(),
                        agent_bridge::subagent_error(&r.status),
                        r.steps_taken,
                    )
                })
            };

            if let Some((new_status, error, steps_taken_val)) = agent_info {
                if let Some(state) = self.tasks.get_mut(&bg_id)
                    && let BackgroundTaskExtension::Agent {
                        ref mut steps_taken,
                        ..
                    } = state.extension
                {
                    *steps_taken = steps_taken_val;
                }
                self.update_task_status(&bg_id, new_status, error);
            } else {
                // The agent record is gone entirely (evicted by
                // `SubAgentManager::cleanup`, or a prior-session restart):
                // the task can never complete, so fail it now instead of
                // leaving it `Running` forever with no notification and no
                // eviction path. The registry entry is created after the
                // source agent registers, so an absent record is not a
                // registration race.
                self.update_task_status(
                    &bg_id,
                    BackgroundTaskStatus::Failed,
                    Some("source agent record disappeared".to_string()),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn summarize_prompt(prompt: &str, max_len: usize) -> String {
    let first_line = prompt.lines().next().unwrap_or("");
    if first_line.chars().count() <= max_len {
        first_line.to_string()
    } else {
        format!("{}…", truncate_chars(first_line, max_len))
    }
}

use codesmith_agent_runtime::tools::handle::truncate_chars;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> BackgroundTaskRegistry {
        let shell = crate::tools::shell::new_shared_shell_manager(PathBuf::from("."));
        let agents = Arc::new(tokio::sync::RwLock::new(
            crate::tools::subagent::SubAgentManager::new(PathBuf::from("."), 1),
        ));
        BackgroundTaskRegistry::new(shell, agents)
    }

    #[test]
    fn terminal_tasks_do_not_retransition_or_renotify() {
        let mut reg = test_registry();
        let state = reg.register_dream_task(PathBuf::from("/tmp/mem.bin"));
        assert!(
            reg.update_task_status(
                &state.id,
                BackgroundTaskStatus::Failed,
                Some("exit code 1".to_string())
            )
            .is_some()
        );
        assert_eq!(reg.drain_notifications().len(), 1);
        let ended_at = reg.get_task(&state.id).expect("task kept").ended_at;
        assert!(
            reg.update_task_status(&state.id, BackgroundTaskStatus::Cancelled, None)
                .is_none(),
            "a terminal status must not transition again"
        );
        assert!(
            reg.drain_notifications().is_empty(),
            "no duplicate completion notification"
        );
        let after = reg.get_task(&state.id).expect("task kept");
        assert_eq!(after.status, BackgroundTaskStatus::Failed);
        assert_eq!(after.error.as_deref(), Some("exit code 1"));
        assert_eq!(after.ended_at, ended_at, "ended_at must not be overwritten");
    }

    #[test]
    fn summarize_prompt_uses_first_line() {
        let multi = "first line\nsecond line";
        assert_eq!(summarize_prompt(multi, 80), "first line");
    }

    #[test]
    fn summarize_prompt_truncates_long_first_line() {
        let long = "b".repeat(100);
        let result = summarize_prompt(&long, 50);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn summarize_prompt_multibyte_boundary_no_panic() {
        let emoji = "😀".repeat(50);
        let result = summarize_prompt(&emoji, 3);
        assert_eq!(result.chars().count(), 4);
        assert!(result.ends_with('…'));
    }
}
