//! Protocol handlers — dedicated handler functions for structured team messages.
//!
//! Extracted from SendMessageTool for reuse by both the tool and the inbox
//! poller. Each handler writes protocol responses to the appropriate mailbox
//! and performs side effects (cancel tokens, remove members, etc.).

use crate::tools::team::{
    IdleReason, StructuredProtocolMessage, TeammateMessage, team_lead_name, write_to_mailbox,
};

// ---------------------------------------------------------------------------
// Permission Request Registry (moved to `codesmith_agent_runtime::team`)
// ---------------------------------------------------------------------------

pub use codesmith_agent_runtime::team::{
    PermissionDecision, PermissionRequestRegistry, SharedPermissionRequestRegistry,
    new_shared_permission_registry,
};

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

// `handle_shutdown_approval` now lives in `codesmith_agent_runtime::team`;
// re-exported here so historical `crate::tools::team::protocol_handlers`
// call sites and the `proto_shutdown_approval` alias keep resolving.
pub use codesmith_agent_runtime::team::handle_shutdown_approval;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ScopedCodeSmithHome, lock_test_env};
    use crate::tools::team::team_file::{TeamFile, create_team_file, format_lead_agent_id};
    use crate::tools::team::teammate_mailbox::{parse_structured_protocol, read_mailbox};

    fn make_team_file(name: &str) -> TeamFile {
        TeamFile {
            name: name.to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: format_lead_agent_id(name),
            lead_session_id: None,
            team_allowed_paths: None,
            members: vec![],
        }
    }

    #[test]
    fn permission_request_registry_register_and_resolve_allow() {
        let mut reg = PermissionRequestRegistry::new();
        let rx = reg.register("req-1".to_string());
        assert!(reg.resolve("req-1", PermissionDecision::Allow));
        assert_eq!(rx.blocking_recv(), Ok(PermissionDecision::Allow));
    }

    #[test]
    fn permission_request_registry_resolve_unknown_returns_false() {
        let mut reg = PermissionRequestRegistry::new();
        assert!(!reg.resolve("nonexistent", PermissionDecision::Allow));
    }

    #[test]
    fn permission_request_registry_resolve_deny() {
        let mut reg = PermissionRequestRegistry::new();
        let rx = reg.register("req-2".to_string());
        assert!(reg.resolve(
            "req-2",
            PermissionDecision::Deny {
                reason: Some("unsafe".to_string())
            }
        ));
        let decision = rx.blocking_recv().expect("recv");
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn handle_shutdown_request_writes_to_target_mailbox() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team_file("proto-test")).expect("team");

        handle_shutdown_request(
            "leader",
            "worker1",
            "proto-test",
            Some("cleanup".to_string()),
        )
        .expect("request");

        let msgs = read_mailbox("worker1", "proto-test").expect("read");
        assert_eq!(msgs.len(), 1);
        let parsed = parse_structured_protocol(&msgs[0].text).expect("parse");
        assert!(matches!(
            parsed,
            StructuredProtocolMessage::ShutdownRequest { .. }
        ));
    }
}
