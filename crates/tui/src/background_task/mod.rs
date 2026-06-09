//! Unified background task registry bridging ShellManager, SubAgentManager,
//! and TaskManager into a single lifecycle surface.
//!
//! Mirrors Claude Code's background task layer (Task.ts, framework.ts,
//! LocalShellTask.tsx, DreamTask.ts, LocalAgentTask.tsx) adapted to Rust.

mod output;
mod shell_bridge;
mod agent_bridge;
mod dream_task;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::task_manager::SharedTaskManager;
use crate::tools::shell::{SharedShellManager, ShellStatus, ShellJobSnapshot};
use crate::tools::subagent::{SharedSubAgentManager, SubAgentType, SubAgentStatus, SubAgentResult};

pub use output::BackgroundTaskOutputManager;
pub use shell_bridge::default_stall_patterns;
pub use agent_bridge::{map_subagent_status, subagent_error};
pub use dream_task::{DreamTaskRunner, DreamResult, DreamStatus};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Background task type — mirrors Claude Code's `TaskType`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskType {
    /// Shell command running in background (bridged from ShellManager).
    Shell,
    /// Background sub-agent (bridged from SubAgentManager).
    Agent,
    /// Durable persistent task (bridged from TaskManager).
    Durable,
    /// Memory consolidation / dream task.
    Dream,
}

impl BackgroundTaskType {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Agent => "agent",
            Self::Durable => "durable",
            Self::Dream => "dream",
        }
    }
}

/// Unified background task status — normalizes the three subsystem status enums
/// plus adds `Stalled` for interactive-prompt detection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
    Cancelled,
    /// Shell command appears stalled (interactive prompt detected).
    Stalled,
}

impl BackgroundTaskStatus {
    /// True if the status is terminal (won't transition further).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Killed | Self::Cancelled
        )
    }
}

/// Map ShellStatus → BackgroundTaskStatus.
impl From<ShellStatus> for BackgroundTaskStatus {
    fn from(s: ShellStatus) -> Self {
        match s {
            ShellStatus::Running => Self::Running,
            ShellStatus::Completed => Self::Completed,
            ShellStatus::Failed => Self::Failed,
            ShellStatus::Killed => Self::Killed,
            ShellStatus::TimedOut => Self::Failed,
        }
    }
}

/// Map SubAgentStatus → BackgroundTaskStatus.
impl From<SubAgentStatus> for BackgroundTaskStatus {
    fn from(s: SubAgentStatus) -> Self {
        match s {
            SubAgentStatus::Running => Self::Running,
            SubAgentStatus::Completed => Self::Completed,
            SubAgentStatus::Interrupted(_) => Self::Failed,
            SubAgentStatus::Failed(_) => Self::Failed,
            SubAgentStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// Task-specific extension data — mirrors Claude Code's per-type state fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackgroundTaskExtension {
    Shell {
        command: String,
        cwd: PathBuf,
        exit_code: Option<i32>,
    },
    Agent {
        agent_type: SubAgentType,
        model: String,
        steps_taken: u32,
    },
    Durable {
        thread_id: Option<String>,
        turn_id: Option<String>,
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
    /// Path to output file on disk (for shell/durable tasks).
    pub output_file: Option<PathBuf>,
    /// Byte offset into output_file already consumed by UI.
    pub output_offset: usize,
    /// Whether user has been notified of completion.
    /// Terminal tasks are evicted once notified.
    pub notified: bool,
    /// Task-specific extension data.
    pub extension: BackgroundTaskExtension,
    /// Error message if status is Failed/Killed/Stalled.
    pub error: Option<String>,
    /// Reference to originating subsystem's internal id.
    pub source_id: String,
}

/// Stall detection pattern for shell output.
pub struct StallPattern {
    /// Regex pattern to match in shell output indicating interactive prompt.
    pub pattern: Regex,
    /// Human-readable description of what the stall means.
    pub description: String,
}

/// Result of a single task poll.
pub struct BackgroundTaskPollResult {
    pub task_id: String,
    pub old_status: BackgroundTaskStatus,
    pub new_status: BackgroundTaskStatus,
    pub output_delta: Option<String>,
    /// True if stall was detected on a shell task.
    pub stall_detected: bool,
}

/// Notification ready for injection into conversation.
/// Mirrors Claude Code's `<task_notification>` XML format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskNotification {
    pub task_id: String,
    pub task_type: BackgroundTaskType,
    pub status: BackgroundTaskStatus,
    pub description: String,
    pub result_summary: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Summary for task listing (TUI panel, /jobs command).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskSummary {
    pub id: String,
    pub source_id: String,
    pub task_type: BackgroundTaskType,
    pub status: BackgroundTaskStatus,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

impl From<&BackgroundTaskState> for BackgroundTaskSummary {
    fn from(s: &BackgroundTaskState) -> Self {
        let duration_ms = s.ended_at.map(|end| {
            (end - s.started_at).num_milliseconds().max(0) as u64
        });
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

/// Unified background task registry bridging ShellManager, SubAgentManager,
/// and TaskManager. Does NOT replace the existing managers — it wraps them
/// and translates lifecycle events into engine-level events.
pub struct BackgroundTaskRegistry {
    tasks: HashMap<String, BackgroundTaskState>,
    shell_manager: SharedShellManager,
    subagent_manager: SharedSubAgentManager,
    task_manager: Option<SharedTaskManager>,
    /// Completed task ids waiting for notification injection.
    pending_notifications: VecDeque<String>,
    /// Stall detection patterns for shell commands.
    stall_patterns: Vec<StallPattern>,
    /// Output manager for disk-based output with offset tracking.
    output_mgr: BackgroundTaskOutputManager,
}

impl BackgroundTaskRegistry {
    /// Create a new registry bridging the three subsystems.
    pub fn new(
        shell_manager: SharedShellManager,
        subagent_manager: SharedSubAgentManager,
        task_manager: Option<SharedTaskManager>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            tasks: HashMap::new(),
            shell_manager,
            subagent_manager,
            task_manager,
            pending_notifications: VecDeque::new(),
            stall_patterns: default_stall_patterns(),
            output_mgr: BackgroundTaskOutputManager::new(data_dir),
        }
    }

    /// Register a shell command as a background task.
    pub fn register_shell_task(
        &mut self,
        shell_id: String,
        command: String,
        cwd: PathBuf,
    ) -> BackgroundTaskState {
        let id = format!("bg_shell_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let output_file = self.output_mgr.output_path_for(&id);
        let description = summarize_command(&command, 80);
        let state = BackgroundTaskState {
            id: id.clone(),
            task_type: BackgroundTaskType::Shell,
            status: BackgroundTaskStatus::Running,
            description,
            started_at: Utc::now(),
            ended_at: None,
            output_file: Some(output_file),
            output_offset: 0,
            notified: false,
            extension: BackgroundTaskExtension::Shell {
                command,
                cwd,
                exit_code: None,
            },
            error: None,
            source_id: shell_id,
        };
        self.tasks.insert(id, state.clone());
        state
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
            output_file: None,
            output_offset: 0,
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
    pub fn register_dream_task(
        &mut self,
        memory_path: PathBuf,
    ) -> BackgroundTaskState {
        let id = format!("bg_dream_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let state = BackgroundTaskState {
            id: id.clone(),
            task_type: BackgroundTaskType::Dream,
            status: BackgroundTaskStatus::Running,
            description: "dreaming".to_string(),
            started_at: Utc::now(),
            ended_at: None,
            output_file: None,
            output_offset: 0,
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

    /// Update a single task's status.
    pub fn update_task_status(
        &mut self,
        id: &str,
        new_status: BackgroundTaskStatus,
        error: Option<String>,
    ) -> Option<BackgroundTaskPollResult> {
        let state = self.tasks.get_mut(id)?;
        let old_status = state.status;
        if old_status == new_status && error.is_none() {
            return None;
        }
        state.status = new_status;
        state.error = error;

        if new_status.is_terminal() {
            state.ended_at = Some(Utc::now());
            self.pending_notifications.push_back(id.to_string());
        }

        Some(BackgroundTaskPollResult {
            task_id: id.to_string(),
            old_status,
            new_status,
            output_delta: None,
            stall_detected: new_status == BackgroundTaskStatus::Stalled,
        })
    }

    /// Cancel a background task by delegating to the source subsystem.
    pub async fn cancel_task(&mut self, id: &str) -> Result<()> {
        let state = self.tasks.get(id)
            .ok_or_else(|| anyhow::anyhow!("Background task not found: {id}"))?
            .clone();

        match state.task_type {
            BackgroundTaskType::Shell => {
                // ShellManager uses std::sync::Mutex
                let mut mgr = self.shell_manager.lock().unwrap();
                mgr.kill(&state.source_id)?;
            }
            BackgroundTaskType::Agent => {
                // SubAgentManager uses tokio::sync::RwLock
                let mut mgr = self.subagent_manager.write().await;
                mgr.cancel(&state.source_id)?;
            }
            BackgroundTaskType::Durable => {
                // TaskManager has internal async mutex
                if let Some(tm) = &self.task_manager {
                    tm.cancel_task(&state.source_id).await?;
                }
            }
            BackgroundTaskType::Dream => {
                // Dream tasks don't delegate — just mark cancelled
            }
        }

        self.update_task_status(id, BackgroundTaskStatus::Cancelled, None);
        Ok(())
    }

    /// Background all foreground shell tasks.
    pub fn background_all(&mut self) -> Vec<BackgroundTaskState> {
        // Request ShellManager to background all foreground shells
        {
            let mut mgr = self.shell_manager.lock().unwrap();
            mgr.request_foreground_background();
        }

        // Collect currently running shell tasks
        self.tasks.values()
            .filter(|s| s.task_type == BackgroundTaskType::Shell
                && s.status == BackgroundTaskStatus::Running)
            .cloned()
            .collect()
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
            self.output_mgr.remove_output(&id).ok();
        }
    }

    /// Read incremental output from a task's output file starting at
    /// stored offset. Updates offset after reading.
    pub fn read_output_delta(&mut self, id: &str) -> Option<String> {
        let state = self.tasks.get_mut(id)?;
        let output_file = state.output_file.as_ref()?;
        let offset = state.output_offset;

        let (content, new_offset) = self.output_mgr.read_from_offset(
            output_file, offset,
        ).ok()?;

        if new_offset > offset {
            state.output_offset = new_offset;
        }
        Some(content)
    }

    /// Return pending notification messages ready for injection.
    /// Clears the queue after returning.
    pub fn drain_notifications(&mut self) -> Vec<BackgroundTaskNotification> {
        let mut result = Vec::new();
        while let Some(id) = self.pending_notifications.pop_front() {
            if let Some(state) = self.tasks.get_mut(&id) {
                state.notified = true;
                let duration_ms = state.ended_at.map(|end| {
                    (end - state.started_at).num_milliseconds().max(0) as u64
                });
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
        self.tasks.values().map(BackgroundTaskSummary::from).collect()
    }

    /// Get a specific task by id.
    pub fn get_task(&self, id: &str) -> Option<BackgroundTaskState> {
        self.tasks.get(id).cloned()
    }

    /// Poll all running tasks, update status from source subsystems,
    /// detect stalls, and generate completion notifications.
    pub async fn poll_tasks(&mut self) -> Vec<BackgroundTaskPollResult> {
        let mut results = Vec::new();

        // Poll shell tasks — ShellManager uses std::sync::Mutex
        let shell_ids: Vec<(String, String)> = self.tasks.iter()
            .filter(|(_, s)| s.task_type == BackgroundTaskType::Shell
                && s.status == BackgroundTaskStatus::Running)
            .map(|(id, s)| (id.clone(), s.source_id.clone()))
            .collect();

        for (bg_id, shell_id) in shell_ids {
            // Scope the lock so it drops before we mutate self
            let poll_info: Option<(BackgroundTaskStatus, Option<String>, String, Option<i32>)> = {
                let mut mgr = self.shell_manager.lock().unwrap();
                if let Ok(detail) = mgr.inspect_job(&shell_id) {
                    let tail = detail.snapshot.stdout_tail.clone();
                    let status: BackgroundTaskStatus = detail.snapshot.status.into();
                    let exit_code = detail.snapshot.exit_code;
                    // mgr drops here at scope end
                    Some((status, None, tail, exit_code))
                } else {
                    None
                }
            };

            if let Some((new_status, mut error, tail, exit_code_val)) = poll_info {
                // Check stall first (only if still Running)
                if new_status == BackgroundTaskStatus::Running && self.looks_like_prompt(&tail) {
                    if let Some(r) = self.update_task_status(
                        &bg_id, BackgroundTaskStatus::Stalled,
                        Some("Interactive prompt detected".to_string()),
                    ) {
                        results.push(r);
                    }
                    continue;
                }
                // Update extension with exit_code
                if let Some(state) = self.tasks.get_mut(&bg_id) {
                    if let BackgroundTaskExtension::Shell { ref mut exit_code, .. } = state.extension {
                        *exit_code = exit_code_val;
                    }
                }
                if new_status == BackgroundTaskStatus::Failed {
                    error = Some(format!("exit code {}", exit_code_val.unwrap_or(-1)));
                }
                if let Some(r) = self.update_task_status(&bg_id, new_status, error) {
                    results.push(r);
                }
            }
        }

        // Poll agent tasks — SubAgentManager uses tokio::sync::RwLock
        let agent_ids: Vec<(String, String)> = self.tasks.iter()
            .filter(|(_, s)| s.task_type == BackgroundTaskType::Agent
                && s.status == BackgroundTaskStatus::Running)
            .map(|(id, s)| (id.clone(), s.source_id.clone()))
            .collect();

        for (bg_id, agent_id) in agent_ids {
            // Scope the read lock
            let agent_info: Option<(BackgroundTaskStatus, Option<String>, u32)> = {
                let mgr = self.subagent_manager.read().await;
                let agents = mgr.list_filtered(false);
                let result = agents.iter().find(|a| a.agent_id == agent_id);
                result.map(|r| (r.status.clone().into(), agent_bridge::subagent_error(&r.status), r.steps_taken))
            };

            if let Some((new_status, error, steps_taken_val)) = agent_info {
                if let Some(state) = self.tasks.get_mut(&bg_id) {
                    if let BackgroundTaskExtension::Agent { ref mut steps_taken, .. } = state.extension {
                        *steps_taken = steps_taken_val;
                    }
                }
                if let Some(r) = self.update_task_status(&bg_id, new_status, error) {
                    results.push(r);
                }
            }
        }

        results
    }

    /// Check if the tail of shell output looks like an interactive prompt.
    fn looks_like_prompt(&self, tail: &str) -> bool {
        let last_line = tail.trim_end().lines().last().unwrap_or("");
        self.stall_patterns.iter().any(|p| p.pattern.is_match(last_line))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format a background task notification for injection into conversation.
/// Mirrors Claude Code's `<task_notification>` XML format.
pub fn format_notification_message(notification: &BackgroundTaskNotification) -> String {
    let status_label = match notification.status {
        BackgroundTaskStatus::Completed => "completed",
        BackgroundTaskStatus::Failed => "failed",
        BackgroundTaskStatus::Killed => "killed",
        BackgroundTaskStatus::Cancelled => "cancelled",
        BackgroundTaskStatus::Stalled => "stalled",
        _ => "unknown",
    };
    format!(
        "<background_task_notification>\n\
         <task_id>{}</task_id>\n\
         <task_type>{}</task_type>\n\
         <status>{}</status>\n\
         <description>{}</description>\n\
         <result_summary>{}</result_summary>\n\
         <duration_ms>{}</duration_ms>\n\
         </background_task_notification>",
        notification.task_id,
        notification.task_type.as_str(),
        status_label,
        notification.description,
        notification.result_summary.as_deref().unwrap_or("(no output)"),
        notification.duration_ms.unwrap_or(0),
    )
}

fn summarize_command(cmd: &str, max_len: usize) -> String {
    if cmd.len() <= max_len {
        cmd.to_string()
    } else {
        format!("{}…", &cmd[..max_len])
    }
}

fn summarize_prompt(prompt: &str, max_len: usize) -> String {
    let first_line = prompt.lines().next().unwrap_or("");
    if first_line.len() <= max_len {
        first_line.to_string()
    } else {
        format!("{}…", &first_line[..max_len])
    }
}