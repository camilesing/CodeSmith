//! Background task data types shared across run-forms.
//!
//! The unified lifecycle/registry layer stays in the TUI (it bridges
//! `ShellManager` / `SubAgentManager` / `TaskManager`, which are still
//! TUI-local). These plain data types — `BackgroundTaskType`,
//! `BackgroundTaskStatus`, `BackgroundTaskNotification`,
//! `BackgroundTaskSummary` — plus the notification formatter move here so the
//! `Event` protocol (and other run-forms) can reference them without a TUI
//! dependency.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::subagent::SubAgentStatus;
use crate::tools::shell_types::ShellStatus;

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

/// Result of a single background-task poll: the status transition + any output
/// delta produced since the last poll. Plain data so the host poller (and the
/// engine core once moved) can consume it without depending on the concrete
/// `BackgroundTaskRegistry`, which stays in the TUI.
#[derive(Debug, Clone)]
pub struct BackgroundTaskPollResult {
    pub task_id: String,
    pub old_status: BackgroundTaskStatus,
    pub new_status: BackgroundTaskStatus,
    pub output_delta: Option<String>,
    /// True if a stall (interactive prompt) was detected on a shell task.
    pub stall_detected: bool,
}

/// Atomic snapshot returned by [`crate::host_services::BgRegistryApi::poll_once`]:
/// the poll results plus the notifications drained in the same locked pass.
/// Returning them together lets the host poller emit events without holding
/// the registry lock across `Event`-channel awaits.
#[derive(Debug, Clone)]
pub struct BackgroundTaskPollSnapshot {
    pub results: Vec<BackgroundTaskPollResult>,
    pub notifications: Vec<BackgroundTaskNotification>,
}

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
        notification
            .result_summary
            .as_deref()
            .unwrap_or("(no output)"),
        notification.duration_ms.unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_task_status_is_terminal() {
        assert!(BackgroundTaskStatus::Completed.is_terminal());
        assert!(BackgroundTaskStatus::Failed.is_terminal());
        assert!(BackgroundTaskStatus::Killed.is_terminal());
        assert!(BackgroundTaskStatus::Cancelled.is_terminal());
        assert!(!BackgroundTaskStatus::Running.is_terminal());
        assert!(!BackgroundTaskStatus::Pending.is_terminal());
        assert!(!BackgroundTaskStatus::Stalled.is_terminal());
    }

    #[test]
    fn format_notification_message_produces_xml_structure() {
        let notification = BackgroundTaskNotification {
            task_id: "bg-001".to_string(),
            task_type: BackgroundTaskType::Shell,
            status: BackgroundTaskStatus::Completed,
            description: "cargo build".to_string(),
            result_summary: Some("Build succeeded".to_string()),
            duration_ms: Some(5000),
        };
        let xml = format_notification_message(&notification);
        assert!(xml.contains("<background_task_notification>"));
        assert!(xml.contains("<task_id>bg-001</task_id>"));
        assert!(xml.contains("<task_type>shell</task_type>"));
        assert!(xml.contains("<status>completed</status>"));
        assert!(xml.contains("<description>cargo build</description>"));
        assert!(xml.contains("<result_summary>Build succeeded</result_summary>"));
        assert!(xml.contains("<duration_ms>5000</duration_ms>"));
        assert!(xml.contains("</background_task_notification>"));
    }
}
