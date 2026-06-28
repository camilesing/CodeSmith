//! Shared types for agent-team inbox dispatch.
//!
//! [`IdleReason`] and [`InboxDispatch`] are consumed by both the engine
//! (which acts on dispatch items) and the TUI's inbox poller (which produces
//! them), so they live here in the terminal-agnostic runtime rather than in
//! the TUI. The TUI re-exports them at their historical
//! `crate::tools::team` paths for backwards compatibility.

use serde::{Deserialize, Serialize};

/// Idle reason variants — why a teammate went idle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IdleReason {
    /// Turn completed normally, available for new work.
    Available,
    /// Interrupted by external signal (cancel, etc).
    Interrupted,
    /// Failed during turn execution.
    Failed,
}

/// A single dispatch item from the inbox poller to the engine.
#[derive(Debug, Clone)]
pub enum InboxDispatch {
    /// Regular teammate message to inject into leader conversation.
    TeammateMessage {
        from: String,
        text: String,
        summary: Option<String>,
    },
    /// Permission request that needs leader's approval dialog.
    PermissionRequestPending {
        request_id: String,
        agent_id: String,
        tool_name: String,
        tool_use_id: String,
        description: String,
    },
    /// Permission response received from leader for a worker's request.
    PermissionResponseReceived {
        request_id: String,
        subtype: String,
        error: Option<String>,
    },
    /// Shutdown request — passed through as message for model decision.
    ShutdownRequestMessage {
        from: String,
        request_id: String,
        reason: Option<String>,
    },
    /// Shutdown approval — leader must kill teammate, remove from team.
    ShutdownApprovalAction {
        from: String,
        request_id: String,
        backend_type: Option<String>,
    },
    /// Shutdown rejection — informational.
    ShutdownRejectionInfo {
        from: String,
        request_id: String,
        reason: String,
    },
    /// Plan approval request — auto-approved by poller; dispatch info.
    PlanApprovalAutoApprove { from: String, request_id: String },
    /// Idle notification — informational for leader.
    IdleNotificationInfo {
        from: String,
        idle_reason: Option<IdleReason>,
        summary: Option<String>,
        completed_task_id: Option<String>,
        completed_status: Option<String>,
    },
    /// Mode set request — worker should change permission mode.
    ModeSetRequestAction {
        from: String,
        permission_mode: String,
    },
    /// Team permission update — informational ack.
    TeamPermissionUpdateInfo {
        from: String,
        allowed_tools: Vec<String>,
        denied_tools: Vec<String>,
    },
}
