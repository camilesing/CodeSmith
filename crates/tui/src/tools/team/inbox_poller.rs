//! Inbox poller — tokio background task that polls the leader's mailbox every
//! 1 second, classifies incoming messages, and dispatches actions through a
//! channel to the engine.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::core::ops::Op;
use crate::tools::team::protocol_handlers::handle_plan_approval_auto_approve;
use crate::tools::team::{
    IdleReason, StructuredProtocolMessage, TeammateMessage, process_inbox_messages, team_lead_name,
};

// ---------------------------------------------------------------------------
// Dispatch Types
// ---------------------------------------------------------------------------

/// A single dispatch item from the inbox poller to the engine.
///
/// Re-exported from `codesmith_agent_runtime::team::InboxDispatch` so the
/// engine and inbox poller share the same dispatch type.
pub use codesmith_agent_runtime::team::InboxDispatch;

/// Channel type for sending ops to the engine.
pub type TeamInboxTx = mpsc::Sender<Op>;
pub type TeamInboxRx = mpsc::Receiver<Op>;

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Classification buckets for inbox messages.
#[derive(Debug, Default)]
pub struct InboxClassification {
    pub shutdown_requests: Vec<ShutdownRequestEntry>,
    pub shutdown_approvals: Vec<ShutdownApprovedEntry>,
    pub shutdown_rejections: Vec<ShutdownRejectedEntry>,
    pub permission_requests: Vec<PermissionRequestEntry>,
    pub permission_responses: Vec<PermissionResponseEntry>,
    pub sandbox_permission_requests: Vec<SandboxPermissionRequestEntry>,
    pub sandbox_permission_responses: Vec<SandboxPermissionResponseEntry>,
    pub plan_approval_requests: Vec<PlanApprovalRequestEntry>,
    pub idle_notifications: Vec<IdleNotificationEntry>,
    pub task_assignments: Vec<TaskAssignmentEntry>,
    pub team_permission_updates: Vec<TeamPermissionUpdateEntry>,
    pub mode_set_requests: Vec<ModeSetRequestEntry>,
    pub regular_messages: Vec<TeammateMessage>,
}

/// Entry types for each classification bucket.
#[derive(Debug, Clone)]
pub struct ShutdownRequestEntry {
    pub request_id: String,
    pub from: String,
    pub reason: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ShutdownApprovedEntry {
    pub request_id: String,
    pub from: String,
    pub backend_type: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ShutdownRejectedEntry {
    pub request_id: String,
    pub from: String,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct PermissionRequestEntry {
    pub request_id: String,
    pub agent_id: String,
    pub tool_name: String,
    pub tool_use_id: String,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct PermissionResponseEntry {
    pub request_id: String,
    pub subtype: String,
    pub error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct SandboxPermissionRequestEntry {
    pub request_id: String,
    pub agent_id: String,
    pub domain: String,
}
#[derive(Debug, Clone)]
pub struct SandboxPermissionResponseEntry {
    pub request_id: String,
    pub subtype: String,
    pub error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct PlanApprovalRequestEntry {
    pub from: String,
    pub request_id: String,
    pub plan_file_path: String,
}
#[derive(Debug, Clone)]
pub struct IdleNotificationEntry {
    pub from: String,
    pub idle_reason: Option<IdleReason>,
    pub summary: Option<String>,
    pub completed_task_id: Option<String>,
    pub completed_status: Option<String>,
}
#[derive(Debug, Clone)]
pub struct TaskAssignmentEntry {
    pub task_id: String,
    pub subject: String,
    pub description: String,
    pub assigned_by: String,
}
#[derive(Debug, Clone)]
pub struct TeamPermissionUpdateEntry {
    pub from: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct ModeSetRequestEntry {
    pub from: String,
    pub permission_mode: String,
}

/// Classify protocol messages and plain text messages into buckets.
pub fn classify_inbox_messages(
    protocol_msgs: Vec<StructuredProtocolMessage>,
    text_msgs: Vec<TeammateMessage>,
) -> InboxClassification {
    let mut cls = InboxClassification::default();

    for proto in &protocol_msgs {
        match proto {
            StructuredProtocolMessage::ShutdownRequest {
                request_id,
                from,
                reason,
                ..
            } => {
                cls.shutdown_requests.push(ShutdownRequestEntry {
                    request_id: request_id.clone(),
                    from: from.clone(),
                    reason: reason.clone(),
                });
            }
            StructuredProtocolMessage::ShutdownApproved {
                request_id,
                from,
                backend_type,
                ..
            } => {
                cls.shutdown_approvals.push(ShutdownApprovedEntry {
                    request_id: request_id.clone(),
                    from: from.clone(),
                    backend_type: backend_type.clone(),
                });
            }
            StructuredProtocolMessage::ShutdownRejected {
                request_id,
                from,
                reason,
                ..
            } => {
                cls.shutdown_rejections.push(ShutdownRejectedEntry {
                    request_id: request_id.clone(),
                    from: from.clone(),
                    reason: reason.clone(),
                });
            }
            StructuredProtocolMessage::PermissionRequest {
                request_id,
                agent_id,
                tool_name,
                tool_use_id,
                description,
            } => {
                cls.permission_requests.push(PermissionRequestEntry {
                    request_id: request_id.clone(),
                    agent_id: agent_id.clone(),
                    tool_name: tool_name.clone(),
                    tool_use_id: tool_use_id.clone(),
                    description: description.clone(),
                });
            }
            StructuredProtocolMessage::PermissionResponse {
                request_id,
                subtype,
                error,
            } => {
                cls.permission_responses.push(PermissionResponseEntry {
                    request_id: request_id.clone(),
                    subtype: subtype.clone(),
                    error: error.clone(),
                });
            }
            StructuredProtocolMessage::SandboxPermissionRequest {
                request_id,
                agent_id,
                domain,
                ..
            } => {
                cls.sandbox_permission_requests
                    .push(SandboxPermissionRequestEntry {
                        request_id: request_id.clone(),
                        agent_id: agent_id.clone(),
                        domain: domain.clone(),
                    });
            }
            StructuredProtocolMessage::SandboxPermissionResponse {
                request_id,
                subtype,
                error,
            } => {
                cls.sandbox_permission_responses
                    .push(SandboxPermissionResponseEntry {
                        request_id: request_id.clone(),
                        subtype: subtype.clone(),
                        error: error.clone(),
                    });
            }
            StructuredProtocolMessage::PlanApprovalRequest {
                from,
                request_id,
                plan_file_path,
                ..
            } => {
                cls.plan_approval_requests.push(PlanApprovalRequestEntry {
                    from: from.clone(),
                    request_id: request_id.clone(),
                    plan_file_path: plan_file_path.clone(),
                });
            }
            StructuredProtocolMessage::IdleNotification {
                from,
                idle_reason,
                summary,
                completed_task_id,
                completed_status,
                ..
            } => {
                cls.idle_notifications.push(IdleNotificationEntry {
                    from: from.clone(),
                    idle_reason: idle_reason.clone(),
                    summary: summary.clone(),
                    completed_task_id: completed_task_id.clone(),
                    completed_status: completed_status.clone(),
                });
            }
            StructuredProtocolMessage::TaskAssignment {
                task_id,
                subject,
                description,
                assigned_by,
                ..
            } => {
                cls.task_assignments.push(TaskAssignmentEntry {
                    task_id: task_id.clone(),
                    subject: subject.clone(),
                    description: description.clone(),
                    assigned_by: assigned_by.clone(),
                });
            }
            StructuredProtocolMessage::TeamPermissionUpdate {
                from,
                allowed_tools,
                denied_tools,
                ..
            } => {
                cls.team_permission_updates.push(TeamPermissionUpdateEntry {
                    from: from.clone(),
                    allowed_tools: allowed_tools.clone(),
                    denied_tools: denied_tools.clone(),
                });
            }
            StructuredProtocolMessage::ModeSetRequest {
                from,
                permission_mode,
                ..
            } => {
                cls.mode_set_requests.push(ModeSetRequestEntry {
                    from: from.clone(),
                    permission_mode: permission_mode.clone(),
                });
            }
            StructuredProtocolMessage::PlanApprovalResponse { .. } => {
                // Responses are handled by teammate side, not leader.
                // No classification needed on leader inbox.
            }
            StructuredProtocolMessage::SandboxPermissionRequest {
                request_id,
                agent_id,
                domain,
                ..
            } => {
                cls.sandbox_permission_requests
                    .push(SandboxPermissionRequestEntry {
                        request_id: request_id.clone(),
                        agent_id: agent_id.clone(),
                        domain: domain.clone(),
                    });
            }
            StructuredProtocolMessage::SandboxPermissionResponse {
                request_id,
                subtype,
                error,
            } => {
                cls.sandbox_permission_responses
                    .push(SandboxPermissionResponseEntry {
                        request_id: request_id.clone(),
                        subtype: subtype.clone(),
                        error: error.clone(),
                    });
            }
        }
    }

    cls.regular_messages = text_msgs;
    cls
}

// ---------------------------------------------------------------------------
// Leader Inbox Poller
// ---------------------------------------------------------------------------

/// Run the leader inbox poller as a background tokio task.
/// Polls every 1 second, classifies messages, and dispatches actions
/// through the TeamInboxTx channel.
///
/// Auto-approve logic:
/// - PlanApprovalRequest → auto-approve, write response to teammate mailbox
/// - ShutdownApproval → informational (teammate already handled approval)
pub async fn run_leader_inbox_poller(
    team_name: String,
    tx_op: TeamInboxTx,
    cancel_token: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;
        if cancel_token.is_cancelled() {
            break;
        }

        let (protocol_msgs, text_msgs) = match process_inbox_messages(team_lead_name(), &team_name)
        {
            Ok((p, t)) => (p, t),
            Err(_) => continue,
        };

        if protocol_msgs.is_empty() && text_msgs.is_empty() {
            continue;
        }

        let cls = classify_inbox_messages(protocol_msgs, text_msgs);

        // Auto-approve plan approval requests (leader side).
        for entry in &cls.plan_approval_requests {
            let _ = handle_plan_approval_auto_approve(
                &entry.request_id,
                &entry.from,
                &team_name,
                "auto",
            );
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::PlanApprovalAutoApprove {
                        from: entry.from.clone(),
                        request_id: entry.request_id.clone(),
                    },
                })
                .await;
        }

        // Dispatch shutdown approvals.
        for entry in &cls.shutdown_approvals {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::ShutdownApprovalAction {
                        from: entry.from.clone(),
                        request_id: entry.request_id.clone(),
                        backend_type: entry.backend_type.clone(),
                    },
                })
                .await;
        }

        // Dispatch shutdown rejections (informational).
        for entry in &cls.shutdown_rejections {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::ShutdownRejectionInfo {
                        from: entry.from.clone(),
                        request_id: entry.request_id.clone(),
                        reason: entry.reason.clone(),
                    },
                })
                .await;
        }

        // Dispatch permission requests.
        for entry in &cls.permission_requests {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::PermissionRequestPending {
                        request_id: entry.request_id.clone(),
                        agent_id: entry.agent_id.clone(),
                        tool_name: entry.tool_name.clone(),
                        tool_use_id: entry.tool_use_id.clone(),
                        description: entry.description.clone(),
                    },
                })
                .await;
        }

        // Dispatch idle notifications.
        for entry in &cls.idle_notifications {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::IdleNotificationInfo {
                        from: entry.from.clone(),
                        idle_reason: entry.idle_reason.clone(),
                        summary: entry.summary.clone(),
                        completed_task_id: entry.completed_task_id.clone(),
                        completed_status: entry.completed_status.clone(),
                    },
                })
                .await;
        }

        // Dispatch mode set requests.
        for entry in &cls.mode_set_requests {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::ModeSetRequestAction {
                        from: entry.from.clone(),
                        permission_mode: entry.permission_mode.clone(),
                    },
                })
                .await;
        }

        // Dispatch team permission updates.
        for entry in &cls.team_permission_updates {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::TeamPermissionUpdateInfo {
                        from: entry.from.clone(),
                        allowed_tools: entry.allowed_tools.clone(),
                        denied_tools: entry.denied_tools.clone(),
                    },
                })
                .await;
        }

        // Dispatch task assignments (informational).
        for entry in &cls.task_assignments {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::TeammateMessage {
                        from: entry.assigned_by.clone(),
                        text: format!("Task assigned: {} - {}", entry.subject, entry.description),
                        summary: Some(format!("task: {}", entry.subject)),
                    },
                })
                .await;
        }

        // Dispatch permission responses.
        for entry in &cls.permission_responses {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::PermissionResponseReceived {
                        request_id: entry.request_id.clone(),
                        subtype: entry.subtype.clone(),
                        error: entry.error.clone(),
                    },
                })
                .await;
        }

        // Dispatch regular text messages.
        for msg in &cls.regular_messages {
            let _ = tx_op
                .send(Op::TeamInboxDispatch {
                    dispatch: InboxDispatch::TeammateMessage {
                        from: msg.from.clone(),
                        text: msg.text.clone(),
                        summary: msg.summary.clone(),
                    },
                })
                .await;
        }
    }
}
