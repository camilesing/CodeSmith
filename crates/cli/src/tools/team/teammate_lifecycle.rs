//! Teammate lifecycle — in-process teammate loop with priority inbox,
//! task claiming, permission resolution, and plan mode transition.
//!
//! State machine: Initializing → Active → Idle → (new message → Active)
//! or (shutdown_request → ShutdownPending → Terminated/Idle).
//!
//! Priority inbox scanning: shutdown > lead messages > peer messages >
//! permission responses > plan approval responses > task assignments.
#![allow(dead_code)]
#![allow(unused_assignments)]

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::tools::spec::{ToolContext, ToolSpec};
use crate::tools::subagent::{
    SharedSubAgentManager, SubAgentRuntime, SubAgentType, SubagentRunTool,
};
use crate::tools::task_v2::{SharedTaskV2Manager, TaskV2Record, TaskV2Status};
use crate::tools::team::protocol_handlers::{PermissionDecision, SharedPermissionRequestRegistry};
use crate::tools::team::{
    IdleReason, StructuredProtocolMessage, TeammateMessage, is_structured_protocol_message,
    mark_messages_as_read, parse_structured_protocol, read_unread_messages, team_lead_name,
    write_to_mailbox,
};

// ---------------------------------------------------------------------------
// State & Result Types
// ---------------------------------------------------------------------------

/// Teammate lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeammateState {
    /// Just spawned, about to enter first prompt loop.
    Initializing,
    /// Actively processing a prompt (LLM turn + tool execution).
    Active,
    /// Turn completed, waiting for new messages or task assignment.
    Idle,
    /// Leader sent shutdown_request, model deciding.
    ShutdownPending,
    /// Model approved shutdown, agent exiting.
    ShutdownApproved,
    /// Model rejected shutdown, back to Idle.
    ShutdownRejected,
    /// Agent failed or was killed.
    Terminated,
}

/// Result of a teammate lifecycle run.
#[derive(Debug)]
pub struct TeammateResult {
    pub agent_id: String,
    pub agent_name: String,
    pub final_state: TeammateState,
    pub summary: Option<String>,
}

/// Runtime context for an in-process teammate.
/// Carries identity, team info, and all necessary services.
pub struct TeammateRuntime {
    pub agent_id: String,
    pub agent_name: String,
    pub team_name: String,
    pub color: Option<String>,
    pub cancel_token: CancellationToken,
    pub task_v2_manager: SharedTaskV2Manager,
    pub initial_prompt: String,
    /// Current permission mode, updated by ModeSetRequest and PlanApprovalResponse.
    pub permission_mode: String,
    /// Registry for pending permission requests awaiting leader approval.
    pub permission_registry: SharedPermissionRequestRegistry,
    /// Shared sub-agent manager used to run each teammate prompt through the
    /// normal sub-agent runtime path.
    pub subagent_manager: SharedSubAgentManager,
    pub subagent_runtime: SubAgentRuntime,
    pub tool_context: ToolContext,
    pub agent_type: SubAgentType,
    pub model: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Priority Inbox
// ---------------------------------------------------------------------------

/// Result of priority-based inbox scanning.
#[derive(Debug, Default)]
pub struct PriorityInboxResult {
    /// Shutdown request found — highest priority, return immediately.
    pub shutdown_request: Option<ShutdownRequestEntry>,
    /// Messages from team-lead.
    pub lead_messages: Vec<TeammateMessage>,
    /// Messages from peers.
    pub peer_messages: Vec<TeammateMessage>,
    /// Permission responses to resolve via oneshot channels.
    pub permission_responses: Vec<PermissionResponseEntry>,
    /// Plan approval responses — transition out of plan mode if approved.
    pub plan_approval_responses: Vec<PlanApprovalResponseEntry>,
    /// Task assignments from leader.
    pub task_assignments: Vec<TaskAssignmentEntry>,
    /// Idle notifications from other teammates (informational).
    pub idle_notifications: Vec<IdleNotificationEntry>,
    /// Mode set requests from leader.
    pub mode_set_requests: Vec<ModeSetRequestEntry>,
    /// Team permission updates from leader.
    pub team_permission_updates: Vec<TeamPermissionUpdateEntry>,
    /// Other unhandled protocol messages.
    pub other_protocol: Vec<StructuredProtocolMessage>,
}

/// Entry types for priority inbox.
#[derive(Debug, Clone)]
pub struct ShutdownRequestEntry {
    pub request_id: String,
    pub from: String,
    pub reason: Option<String>,
}
#[derive(Debug, Clone)]
pub struct PermissionResponseEntry {
    pub request_id: String,
    pub subtype: String,
    pub error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct PlanApprovalResponseEntry {
    pub request_id: String,
    pub approved: bool,
    pub feedback: Option<String>,
    pub permission_mode: Option<String>,
}
#[derive(Debug, Clone)]
pub struct TaskAssignmentEntry {
    pub task_id: String,
    pub subject: String,
    pub description: String,
    pub assigned_by: String,
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
pub struct ModeSetRequestEntry {
    pub from: String,
    pub permission_mode: String,
}
#[derive(Debug, Clone)]
pub struct TeamPermissionUpdateEntry {
    pub from: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
}

/// Scan inbox with priority ordering.
/// 1. Shutdown requests (highest priority)
/// 2. Team-lead messages (prioritized over peers)
/// 3. Peer messages (FIFO)
/// 4. Permission responses (resolve pending channels)
/// 5. Plan approval responses (transition out of plan mode)
pub fn scan_inbox_with_priority(
    agent_name: &str,
    team_name: &str,
) -> anyhow::Result<PriorityInboxResult> {
    let unread = read_unread_messages(agent_name, team_name)?;
    mark_messages_as_read(agent_name, team_name)?;

    let mut result = PriorityInboxResult::default();

    for msg in &unread {
        if is_structured_protocol_message(&msg.text) {
            if let Some(proto) = parse_structured_protocol(&msg.text) {
                match &proto {
                    StructuredProtocolMessage::ShutdownRequest {
                        request_id,
                        from,
                        reason,
                        ..
                    } => {
                        // Only the first shutdown request is returned (highest priority).
                        if result.shutdown_request.is_none() {
                            result.shutdown_request = Some(ShutdownRequestEntry {
                                request_id: request_id.clone(),
                                from: from.clone(),
                                reason: reason.clone(),
                            });
                        }
                    }
                    StructuredProtocolMessage::PermissionResponse {
                        request_id,
                        subtype,
                        error,
                    } => {
                        result.permission_responses.push(PermissionResponseEntry {
                            request_id: request_id.clone(),
                            subtype: subtype.clone(),
                            error: error.clone(),
                        });
                    }
                    StructuredProtocolMessage::PlanApprovalResponse {
                        request_id,
                        approved,
                        feedback,
                        permission_mode,
                        ..
                    } => {
                        result
                            .plan_approval_responses
                            .push(PlanApprovalResponseEntry {
                                request_id: request_id.clone(),
                                approved: *approved,
                                feedback: feedback.clone(),
                                permission_mode: permission_mode.clone(),
                            });
                    }
                    StructuredProtocolMessage::TaskAssignment {
                        task_id,
                        subject,
                        description,
                        assigned_by,
                        ..
                    } => {
                        result.task_assignments.push(TaskAssignmentEntry {
                            task_id: task_id.clone(),
                            subject: subject.clone(),
                            description: description.clone(),
                            assigned_by: assigned_by.clone(),
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
                        result.idle_notifications.push(IdleNotificationEntry {
                            from: from.clone(),
                            idle_reason: idle_reason.clone(),
                            summary: summary.clone(),
                            completed_task_id: completed_task_id.clone(),
                            completed_status: completed_status.clone(),
                        });
                    }
                    StructuredProtocolMessage::ModeSetRequest {
                        from,
                        permission_mode,
                        ..
                    } => {
                        result.mode_set_requests.push(ModeSetRequestEntry {
                            from: from.clone(),
                            permission_mode: permission_mode.clone(),
                        });
                    }
                    StructuredProtocolMessage::TeamPermissionUpdate {
                        from,
                        allowed_tools,
                        denied_tools,
                        ..
                    } => {
                        result
                            .team_permission_updates
                            .push(TeamPermissionUpdateEntry {
                                from: from.clone(),
                                allowed_tools: allowed_tools.clone(),
                                denied_tools: denied_tools.clone(),
                            });
                    }
                    _ => {
                        result.other_protocol.push(proto.clone());
                    }
                }
            }
        } else {
            // Classify text messages by sender.
            if msg.from == team_lead_name() {
                result.lead_messages.push(msg.clone());
            } else {
                result.peer_messages.push(msg.clone());
            }
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Task Claiming
// ---------------------------------------------------------------------------

/// Find and claim the next unassigned, unblocked task from the team's
/// task list. Returns the claimed task if found.
pub async fn try_claim_next_task(
    agent_name: &str,
    task_v2_manager: &SharedTaskV2Manager,
) -> Option<TaskV2Record> {
    let mut manager = task_v2_manager.lock().await;

    let tasks = match manager.list_tasks() {
        Ok(t) => t,
        Err(_) => return None,
    };

    // Build set of IDs that are not yet completed (still blocking).
    let unresolved_ids: std::collections::HashSet<String> = tasks
        .iter()
        .filter(|t| t.status != TaskV2Status::Completed)
        .map(|t| t.id.clone())
        .collect();

    // Find first pending, unassigned, unblocked task.
    for task in &tasks {
        if task.status != TaskV2Status::Pending {
            continue;
        }
        if task.owner.is_some() {
            continue;
        }
        if !task
            .blocked_by
            .iter()
            .all(|bid| !unresolved_ids.contains(bid))
        {
            continue;
        }

        // Claim it.
        let claimed = match manager.claim_task(&task.id, agent_name) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Set status to in_progress.
        let _ = manager.update_task(
            &task.id,
            Some(TaskV2Status::InProgress),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );

        return Some(claimed);
    }

    None
}

// ---------------------------------------------------------------------------
// Wait Loop
// ---------------------------------------------------------------------------

/// Result of waiting for next prompt, shutdown, or available task.
pub enum WaitResult {
    /// Shutdown request received — must be handled before anything else.
    ShutdownRequest(ShutdownRequestEntry),
    /// New messages (lead/peer) or task assignment available.
    NewMessage(PriorityInboxResult),
    /// A task was claimed from the team task list.
    TaskClaimed(TaskV2Record),
    /// CancellationToken was triggered.
    Aborted,
}

/// Wait for next prompt, shutdown, or available task.
/// Polls every 500ms with priority checks.
pub async fn wait_for_next_prompt_or_shutdown(rt: &mut TeammateRuntime) -> WaitResult {
    let mut interval = tokio::time::interval(Duration::from_millis(500));

    loop {
        interval.tick().await;

        if rt.cancel_token.is_cancelled() {
            return WaitResult::Aborted;
        }

        let inbox = match scan_inbox_with_priority(&rt.agent_name, &rt.team_name) {
            Ok(i) => i,
            Err(_) => continue,
        };

        // Shutdown request has highest priority.
        if let Some(entry) = inbox.shutdown_request {
            return WaitResult::ShutdownRequest(entry);
        }

        // Resolve pending permission responses.
        resolve_permission_responses(&inbox.permission_responses, &rt.permission_registry);

        // Apply plan approval responses.
        for resp in &inbox.plan_approval_responses {
            if resp.approved {
                rt.permission_mode = resp
                    .permission_mode
                    .clone()
                    .unwrap_or_else(|| "auto".to_string());
            }
        }

        // Apply mode set requests (only from team-lead).
        for req in &inbox.mode_set_requests {
            if req.from == team_lead_name() {
                rt.permission_mode = req.permission_mode.clone();
            }
        }

        // Build prompt from lead + peer messages + task assignments.
        if !inbox.lead_messages.is_empty()
            || !inbox.peer_messages.is_empty()
            || !inbox.task_assignments.is_empty()
        {
            return WaitResult::NewMessage(inbox);
        }

        // Try to claim a task if idle.
        if let Some(task) = try_claim_next_task(&rt.agent_name, &rt.task_v2_manager).await {
            return WaitResult::TaskClaimed(task);
        }
    }
}

/// Resolve permission responses from inbox by matching to oneshot channels.
fn resolve_permission_responses(
    responses: &[PermissionResponseEntry],
    registry: &SharedPermissionRequestRegistry,
) {
    let mut reg = registry.lock().unwrap();
    for resp in responses {
        let decision = if resp.subtype == "success" || resp.subtype == "allow" {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny {
                reason: resp.error.clone(),
            }
        };
        reg.resolve(&resp.request_id, decision);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Process unread inbox messages, separating protocol messages from plain text.
/// Legacy function — prefer scan_inbox_with_priority() for new code.
pub fn process_inbox_messages(
    agent_name: &str,
    team_name: &str,
) -> anyhow::Result<(Vec<StructuredProtocolMessage>, Vec<TeammateMessage>)> {
    let unread = read_unread_messages(agent_name, team_name)?;
    mark_messages_as_read(agent_name, team_name)?;

    let mut protocol_msgs = Vec::new();
    let mut text_msgs = Vec::new();

    for msg in &unread {
        if is_structured_protocol_message(&msg.text) {
            if let Some(protocol) = parse_structured_protocol(&msg.text) {
                protocol_msgs.push(protocol);
            }
        } else {
            text_msgs.push(msg.clone());
        }
    }

    Ok((protocol_msgs, text_msgs))
}

/// Send an idle notification to the team lead's inbox.
pub fn send_idle_notification(
    agent_name: &str,
    team_name: &str,
    summary: Option<String>,
    completed_task_id: Option<String>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let protocol = StructuredProtocolMessage::IdleNotification {
        from: agent_name.to_string(),
        timestamp: now.clone(),
        idle_reason: Some(IdleReason::Available),
        summary,
        completed_task_id,
        completed_status: None,
    };
    let text = serde_json::to_string(&protocol)?;

    let msg = TeammateMessage {
        from: agent_name.to_string(),
        text,
        timestamp: now,
        read: false,
        color: None,
        summary: Some("idle notification".to_string()),
    };

    write_to_mailbox(team_lead_name(), team_name, msg)?;
    Ok(())
}

/// Handle a shutdown request protocol message.
/// Returns the new state: ShutdownApproved or ShutdownRejected.
/// Default: approve shutdown. In production, model decides via LLM.
pub fn handle_shutdown_request(
    request_id: &str,
    agent_name: &str,
    team_name: &str,
) -> anyhow::Result<TeammateState> {
    let now = chrono::Utc::now().to_rfc3339();

    let approved = true;

    let protocol = if approved {
        StructuredProtocolMessage::ShutdownApproved {
            request_id: request_id.to_string(),
            from: agent_name.to_string(),
            backend_type: None,
            timestamp: now.clone(),
        }
    } else {
        StructuredProtocolMessage::ShutdownRejected {
            request_id: request_id.to_string(),
            from: agent_name.to_string(),
            reason: "Still has pending work".to_string(),
            timestamp: now.clone(),
        }
    };

    let text = serde_json::to_string(&protocol)?;
    let msg = TeammateMessage {
        from: agent_name.to_string(),
        text,
        timestamp: now,
        read: false,
        color: None,
        summary: Some(
            if approved {
                "shutdown approved"
            } else {
                "shutdown rejected"
            }
            .to_string(),
        ),
    };

    write_to_mailbox(team_lead_name(), team_name, msg)?;

    Ok(if approved {
        TeammateState::ShutdownApproved
    } else {
        TeammateState::ShutdownRejected
    })
}

/// Format a prompt from a PriorityInboxResult.
fn format_prompt_from_inbox(inbox: &PriorityInboxResult) -> String {
    let mut parts = Vec::new();

    // Task assignments first (most actionable).
    for entry in &inbox.task_assignments {
        parts.push(format!(
            "New task assigned: {}\n\n{}",
            entry.subject, entry.description
        ));
    }

    // Lead messages next.
    for msg in &inbox.lead_messages {
        parts.push(format!("Message from {}: {}", msg.from, msg.text));
    }

    // Peer messages last.
    for msg in &inbox.peer_messages {
        parts.push(format!("Message from {}: {}", msg.from, msg.text));
    }

    parts.join("\n\n")
}

/// Execute one teammate prompt through the existing synchronous sub-agent tool.
async fn execute_teammate_prompt(
    rt: &TeammateRuntime,
    prompt: String,
) -> Result<Option<String>, String> {
    let tool = SubagentRunTool::new(rt.subagent_manager.clone(), rt.subagent_runtime.clone());
    let mut input = serde_json::json!({
        "prompt": prompt,
        "agent_type": rt.agent_type.as_str(),
        "name": rt.agent_name,
    });
    if let Some(model) = rt.model.as_ref() {
        input["model"] = serde_json::json!(model);
    }
    if let Some(allowed_tools) = rt.allowed_tools.as_ref() {
        input["allowed_tools"] = serde_json::json!(allowed_tools);
    }

    let result = tool
        .execute(input, &rt.tool_context)
        .await
        .map_err(|err| err.to_string())?;
    if !result.success {
        return Err(result.content);
    }
    Ok(Some(result.content))
}

/// Complete a claimed task after successful execution.
async fn mark_task_completed(rt: &TeammateRuntime, task_id: &str, summary: Option<&str>) {
    let metadata = summary.map(|summary| serde_json::json!({ "teammate_summary": summary }));
    let mut manager = rt.task_v2_manager.lock().await;
    let _ = manager.update_task(
        task_id,
        Some(TaskV2Status::Completed),
        None,
        None,
        None,
        None,
        metadata,
        None,
        None,
    );
}

/// Record a task execution failure without inventing a failed task status.
async fn record_task_error(rt: &TeammateRuntime, task_id: &str, error: &str) {
    let mut manager = rt.task_v2_manager.lock().await;
    let _ = manager.update_task(
        task_id,
        None,
        None,
        None,
        None,
        None,
        Some(serde_json::json!({ "last_teammate_error": error })),
        None,
        None,
    );
}

// ---------------------------------------------------------------------------
// Teammate Loop
// ---------------------------------------------------------------------------

/// Run the teammate's continuous prompt loop.
///
/// State machine:
/// 1. Execute the initial spawn prompt once.
/// 2. Wait for next prompt/shutdown/task via priority inbox.
/// 3. Handle shutdown request → approve/reject.
/// 4. Execute one LLM/tool run through SubagentRunTool.
/// 5. Send idle notification → loop back.
pub async fn run_teammate_loop(mut rt: TeammateRuntime) -> TeammateResult {
    let mut state = TeammateState::Initializing;
    let mut summary: Option<String> = None;

    if !rt.initial_prompt.trim().is_empty() && !rt.cancel_token.is_cancelled() {
        state = TeammateState::Active;
        match execute_teammate_prompt(&rt, rt.initial_prompt.clone()).await {
            Ok(result_summary) => {
                summary = result_summary.clone();
                let _ = send_idle_notification(
                    &rt.agent_name,
                    &rt.team_name,
                    result_summary.or_else(|| Some("initial prompt complete".to_string())),
                    None,
                );
            }
            Err(error) => {
                summary = Some(format!("initial prompt failed: {error}"));
                let _ =
                    send_idle_notification(&rt.agent_name, &rt.team_name, summary.clone(), None);
            }
        }
        state = TeammateState::Idle;
    }

    loop {
        if rt.cancel_token.is_cancelled() {
            state = TeammateState::Terminated;
            break;
        }

        let wait_result = wait_for_next_prompt_or_shutdown(&mut rt).await;

        match wait_result {
            WaitResult::Aborted => {
                state = TeammateState::Terminated;
                break;
            }
            WaitResult::ShutdownRequest(entry) => {
                match handle_shutdown_request(&entry.request_id, &rt.agent_name, &rt.team_name) {
                    Ok(TeammateState::ShutdownApproved) => {
                        state = TeammateState::ShutdownApproved;
                        break;
                    }
                    Ok(TeammateState::ShutdownRejected) => {
                        state = TeammateState::Idle;
                        continue;
                    }
                    _ => {
                        state = TeammateState::Idle;
                        continue;
                    }
                }
            }
            WaitResult::NewMessage(inbox) => {
                let prompt = format_prompt_from_inbox(&inbox);
                if prompt.is_empty() {
                    state = TeammateState::Idle;
                    continue;
                }
                state = TeammateState::Active;
                match execute_teammate_prompt(&rt, prompt).await {
                    Ok(result_summary) => {
                        summary = result_summary.clone();
                        let _ = send_idle_notification(
                            &rt.agent_name,
                            &rt.team_name,
                            result_summary.or_else(|| Some("message handled".to_string())),
                            None,
                        );
                    }
                    Err(error) => {
                        summary = Some(format!("message handling failed: {error}"));
                        let _ = send_idle_notification(
                            &rt.agent_name,
                            &rt.team_name,
                            summary.clone(),
                            None,
                        );
                    }
                }
            }
            WaitResult::TaskClaimed(task) => {
                state = TeammateState::Active;
                let prompt = format!(
                    "Complete all open tasks. Start with task #{}:\n\n{}\n\n{}",
                    task.id, task.subject, task.description
                );
                match execute_teammate_prompt(&rt, prompt).await {
                    Ok(result_summary) => {
                        summary = result_summary.clone();
                        mark_task_completed(&rt, &task.id, summary.as_deref()).await;
                        let _ = send_idle_notification(
                            &rt.agent_name,
                            &rt.team_name,
                            result_summary.or_else(|| Some("task complete".to_string())),
                            Some(task.id),
                        );
                    }
                    Err(error) => {
                        summary = Some(format!("task failed: {error}"));
                        record_task_error(&rt, &task.id, &error).await;
                        let _ = send_idle_notification(
                            &rt.agent_name,
                            &rt.team_name,
                            summary.clone(),
                            Some(task.id),
                        );
                    }
                }
            }
        }

        state = TeammateState::Idle;
    }

    TeammateResult {
        agent_id: rt.agent_id.clone(),
        agent_name: rt.agent_name.clone(),
        final_state: state,
        summary,
    }
}

/// Poll the team leader's inbox and return text messages for injection
/// into the leader's conversation as synthetic user messages.
/// Legacy function — the inbox poller now handles this continuously.
#[allow(dead_code)]
pub fn poll_leader_inbox(team_name: &str) -> anyhow::Result<Vec<String>> {
    let (protocol_msgs, text_msgs) = process_inbox_messages(team_lead_name(), team_name)?;

    let protocol_texts: Vec<String> = protocol_msgs
        .iter()
        .map(|p| match p {
            StructuredProtocolMessage::IdleNotification { from, summary, .. } => {
                format!(
                    "Teammate {} is idle. {}",
                    from,
                    summary.as_deref().unwrap_or("")
                )
            }
            StructuredProtocolMessage::ShutdownApproved { from, .. } => {
                format!("Teammate {} approved shutdown.", from)
            }
            StructuredProtocolMessage::ShutdownRejected { from, reason, .. } => {
                format!("Teammate {} rejected shutdown: {}", from, reason)
            }
            _ => "Protocol message from teammate".to_string(),
        })
        .collect();

    let text_contents: Vec<String> = text_msgs
        .iter()
        .map(|m| format!("Message from {}: {}", m.from, m.text))
        .collect();

    Ok([protocol_texts, text_contents].concat())
}
