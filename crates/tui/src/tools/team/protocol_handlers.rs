//! Protocol handlers — dedicated handler functions for structured team messages.
//!
//! Extracted from SendMessageTool for reuse by both the tool and the inbox
//! poller. Each handler writes protocol responses to the appropriate mailbox
//! and performs side effects (cancel tokens, remove members, etc.).

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::tools::team::{
    TeammateMessage, StructuredProtocolMessage, IdleReason,
    write_to_mailbox, team_lead_name, read_team_file,
    remove_member_by_name, write_team_file,
};

// ---------------------------------------------------------------------------
// Permission Request Registry
// ---------------------------------------------------------------------------

/// Decision from leader on a permission request.
#[derive(Debug, Clone)]
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
pub type SharedPermissionRequestRegistry = Arc<StdMutex<PermissionRequestRegistry>>;

/// Create a new empty SharedPermissionRequestRegistry.
pub fn new_shared_permission_registry() -> SharedPermissionRequestRegistry {
    Arc::new(StdMutex::new(PermissionRequestRegistry::new()))
}

// ---------------------------------------------------------------------------
// Shutdown Protocol Handlers
// ---------------------------------------------------------------------------

/// Generate a shutdown request and write it to the target teammate's mailbox.
/// Returns the generated request_id.
pub fn handle_shutdown_request(
    leader_name: &str,
    target_name: &str,
    team_name: &str,
    reason: Option<String>,
) -> anyhow::Result<String> {
    let request_id = format!("req-shutdown-{}", chrono::Utc::now().timestamp_millis());
    let now = chrono::Utc::now().to_rfc3339();

    let protocol = StructuredProtocolMessage::ShutdownRequest {
        request_id: request_id.clone(),
        from: leader_name.to_string(),
        reason,
        timestamp: now.clone(),
    };
    let text = serde_json::to_string(&protocol)?;

    let msg = TeammateMessage {
        from: leader_name.to_string(),
        text,
        timestamp: now,
        read: false,
        color: None,
        summary: Some("shutdown request".to_string()),
    };

    write_to_mailbox(target_name, team_name, msg)?;
    Ok(request_id)
}

/// Handle shutdown approval from a teammate. Cancel the teammate's token,
/// remove from team file, and unassign their tasks.
pub fn handle_shutdown_approval(
    request_id: &str,
    teammate_name: &str,
    team_name: &str,
    cancel_tokens: &HashMap<String, CancellationToken>,
) -> anyhow::Result<()> {
    // Cancel the teammate's runtime token.
    if let Some(token) = cancel_tokens.get(teammate_name) {
        token.cancel();
    }

    // Remove the teammate from the team file.
    let mut team_file = read_team_file(team_name)?;
    remove_member_by_name(&mut team_file, teammate_name);
    write_team_file(&team_file)?;

    Ok(())
}

/// Handle shutdown rejection from a teammate. The teammate continues working.
pub fn handle_shutdown_rejection(
    request_id: &str,
    leader_name: &str,
    teammate_name: &str,
    team_name: &str,
    reason: String,
) -> anyhow::Result<()> {
    // Rejection is informational — no side effects needed.
    // The teammate's mailbox already contains their rejection response
    // written by handle_shutdown_request in teammate_lifecycle.
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan Approval Protocol Handlers
// ---------------------------------------------------------------------------

/// Auto-approve a plan: write plan_approval_response to teammate's mailbox
/// with the approved permission mode.
pub fn handle_plan_approval_auto_approve(
    request_id: &str,
    teammate_name: &str,
    team_name: &str,
    permission_mode: &str,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    let protocol = StructuredProtocolMessage::PlanApprovalResponse {
        request_id: request_id.to_string(),
        approved: true,
        feedback: None,
        permission_mode: Some(permission_mode.to_string()),
        timestamp: now.clone(),
    };
    let text = serde_json::to_string(&protocol)?;

    let msg = TeammateMessage {
        from: team_lead_name().to_string(),
        text,
        timestamp: now,
        read: false,
        color: None,
        summary: Some("plan approved".to_string()),
    };

    write_to_mailbox(teammate_name, team_name, msg)?;
    Ok(())
}

/// Reject a plan: write plan_approval_response with feedback to teammate's mailbox.
pub fn handle_plan_approval_rejection(
    request_id: &str,
    teammate_name: &str,
    team_name: &str,
    feedback: String,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    let protocol = StructuredProtocolMessage::PlanApprovalResponse {
        request_id: request_id.to_string(),
        approved: false,
        feedback: Some(feedback),
        permission_mode: None,
        timestamp: now.clone(),
    };
    let text = serde_json::to_string(&protocol)?;

    let msg = TeammateMessage {
        from: team_lead_name().to_string(),
        text,
        timestamp: now,
        read: false,
        color: None,
        summary: Some("plan rejected".to_string()),
    };

    write_to_mailbox(teammate_name, team_name, msg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Permission Protocol Handlers
// ---------------------------------------------------------------------------

/// Handle a permission request from a worker. Writes the request to the
/// leader's mailbox for routing to the approval dialog.
pub fn handle_permission_request(
    request_id: &str,
    agent_id: &str,
    tool_name: &str,
    tool_use_id: &str,
    description: &str,
    team_name: &str,
) -> anyhow::Result<String> {
    let now = chrono::Utc::now().to_rfc3339();

    let protocol = StructuredProtocolMessage::PermissionRequest {
        request_id: request_id.to_string(),
        agent_id: agent_id.to_string(),
        tool_name: tool_name.to_string(),
        tool_use_id: tool_use_id.to_string(),
        description: description.to_string(),
    };
    let text = serde_json::to_string(&protocol)?;

    let msg = TeammateMessage {
        from: agent_id.to_string(),
        text,
        timestamp: now,
        read: false,
        color: None,
        summary: Some(format!("permission request: {}", tool_name)),
    };

    write_to_mailbox(team_lead_name(), team_name, msg)?;
    Ok(request_id.to_string())
}

/// Handle a permission response from the leader. Write to the worker's mailbox.
pub fn handle_permission_response(
    request_id: &str,
    subtype: &str,
    error: Option<String>,
    teammate_name: &str,
    team_name: &str,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    let protocol = StructuredProtocolMessage::PermissionResponse {
        request_id: request_id.to_string(),
        subtype: subtype.to_string(),
        error,
    };
    let text = serde_json::to_string(&protocol)?;

    let msg = TeammateMessage {
        from: team_lead_name().to_string(),
        text,
        timestamp: now,
        read: false,
        color: None,
        summary: Some(format!("permission response: {}", subtype)),
    };

    write_to_mailbox(teammate_name, team_name, msg)?;
    Ok(())
}