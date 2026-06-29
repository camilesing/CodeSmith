//! Shared types for agent-team inbox dispatch.
//!
//! [`IdleReason`] and [`InboxDispatch`] are consumed by both the engine
//! (which acts on dispatch items) and the TUI's inbox poller (which produces
//! them), so they live here in the terminal-agnostic runtime rather than in
//! the TUI. The TUI re-exports them at their historical
//! `crate::tools::team` paths for backwards compatibility.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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

// ---------------------------------------------------------------------------
// Permission Request Registry
// ---------------------------------------------------------------------------

/// Decision from leader on a permission request.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    Allow,
    Deny { reason: Option<String> },
}

/// Registry for pending permission requests awaiting leader approval.
/// Each request maps to a oneshot channel that resolves when the leader
/// approves or denies.
pub struct PermissionRequestRegistry {
    pending: HashMap<String, oneshot::Sender<PermissionDecision>>,
}

impl PermissionRequestRegistry {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Register a pending permission request. Returns the Receiver that
    /// will resolve when the leader makes a decision.
    pub fn register(&mut self, request_id: String) -> oneshot::Receiver<PermissionDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(request_id, tx);
        rx
    }

    /// Resolve a pending request. Returns false if no matching request found
    /// (e.g., already resolved or timed out).
    pub fn resolve(&mut self, request_id: &str, decision: PermissionDecision) -> bool {
        if let Some(tx) = self.pending.remove(request_id) {
            tx.send(decision).is_ok()
        } else {
            false
        }
    }
}

/// Thread-safe shared reference to the permission request registry.
pub type SharedPermissionRequestRegistry = Arc<Mutex<PermissionRequestRegistry>>;

/// Create a new empty SharedPermissionRequestRegistry.
pub fn new_shared_permission_registry() -> SharedPermissionRequestRegistry {
    Arc::new(Mutex::new(PermissionRequestRegistry::new()))
}
