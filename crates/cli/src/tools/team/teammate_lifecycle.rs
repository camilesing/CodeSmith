//! Teammate lifecycle — in-process teammate loop with priority inbox,
//! task claiming, permission resolution, and plan mode transition.
//!
//! State machine: Initializing → Active → Idle → (new message → Active)
//! or (shutdown_request → ShutdownPending → Terminated/Idle).
//!
//! Priority inbox scanning: shutdown > lead messages > peer messages >
//! permission responses > plan approval responses > task assignments.
#![allow(dead_code)]

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
    mark_messages_as_read, parse_structured_protocol_from, read_unread_messages, team_lead_name,
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
            // Envelope-checked parse: the payload's embedded sender must
            // match the mailbox envelope's actual `from`, or a sender-less
            // leader response must come from the lead — content alone
            // would let any teammate forge leader-only directives.
            if let Some(proto) = parse_structured_protocol_from(&msg.text, &msg.from) {
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
    let mut manager = task_v2_manager.lock().unwrap_or_else(|e| e.into_inner());

    let tasks = match manager.list_tasks() {
        Ok(t) => t,
        Err(err) => {
            crate::logging::warn(format!(
                "teammate '{agent_name}' failed to list tasks: {err}"
            ));
            return None;
        }
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
            Err(err) => {
                crate::logging::warn(format!(
                    "teammate '{agent_name}' failed to claim task {}: {err}",
                    task.id
                ));
                continue;
            }
        };

        // Set status to in_progress. If the status write fails, the claim
        // already persisted ownership — release it or the task is stranded
        // forever (every claimer skips tasks with an owner, and this loop
        // only returns tasks it just claimed).
        if let Err(err) = manager.update_task(
            &task.id,
            Some(TaskV2Status::InProgress),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ) {
            crate::logging::warn(format!(
                "failed to mark task {} in_progress after claim: {err}",
                task.id
            ));
            if let Err(unassign_err) = manager.unassign_teammate_tasks(agent_name) {
                crate::logging::warn(format!(
                    "failed to unassign tasks of '{agent_name}' after status-update failure: \
                     {unassign_err}"
                ));
            }
            continue;
        }

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
    let mut scan_failures: u32 = 0;

    loop {
        interval.tick().await;

        if rt.cancel_token.is_cancelled() {
            return WaitResult::Aborted;
        }

        let inbox = match scan_inbox_with_priority(&rt.agent_name, &rt.team_name) {
            Ok(i) => {
                scan_failures = 0;
                i
            }
            Err(err) => {
                // A persistent scan failure (bad permissions, unreadable
                // inbox) used to make the teammate look alive while doing
                // nothing, with no diagnostic anywhere. Log the first
                // failure and then every 20th (10s at this tick).
                scan_failures += 1;
                if scan_failures == 1 || scan_failures.is_multiple_of(20) {
                    crate::logging::warn(format!(
                        "teammate '{}' inbox scan failed (attempt {scan_failures}): {err}",
                        rt.agent_name
                    ));
                }
                continue;
            }
        };

        // Resolve pending permission responses BEFORE the shutdown
        // early-return: scan_inbox_with_priority already marked everything
        // read, so returning first used to permanently drop same-batch
        // permission responses / plan approvals / mode-set requests and
        // leave their waiters hanging.
        resolve_permission_responses(&inbox.permission_responses, &rt.permission_registry);

        // Shutdown request has highest priority.
        if let Some(entry) = inbox.shutdown_request {
            return WaitResult::ShutdownRequest(entry);
        }

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

        // Apply team permission updates (lead-only protocol) so the next
        // execute_teammate_prompt honors them — they used to be collected
        // and silently discarded, leaving the protocol effectively dead.
        apply_team_permission_updates(
            &rt.agent_name,
            &mut rt.allowed_tools,
            &inbox.team_permission_updates,
        );

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

/// Apply lead-authored `team_permission_update` messages to a teammate's
/// tool allowlist: add `allowed_tools`, remove `denied_tools`.
fn apply_team_permission_updates(
    agent_name: &str,
    allowed_tools: &mut Option<Vec<String>>,
    updates: &[TeamPermissionUpdateEntry],
) {
    for upd in updates {
        if upd.from != team_lead_name() {
            continue;
        }
        if allowed_tools.is_none() && upd.allowed_tools.is_empty() && !upd.denied_tools.is_empty() {
            // An unrestricted teammate cannot honor a pure deny-list
            // without knowing the full tool universe; applying it as an
            // empty allowlist would lock it out of everything.
            crate::logging::warn(format!(
                "team permission update for '{agent_name}' carries only denied tools while \
                 the teammate is unrestricted; ignoring"
            ));
            continue;
        }
        let allowed = allowed_tools.get_or_insert_with(Vec::new);
        for tool in &upd.allowed_tools {
            if !allowed.contains(tool) {
                allowed.push(tool.clone());
            }
        }
        allowed.retain(|t| !upd.denied_tools.contains(t));
    }
}

/// Resolve permission responses from inbox by matching to oneshot channels.
fn resolve_permission_responses(
    responses: &[PermissionResponseEntry],
    registry: &SharedPermissionRequestRegistry,
) {
    // Recover from poisoning instead of unwrapping: a panic in any other
    // holder of this mutex used to take the whole teammate loop down.
    let mut reg = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for resp in responses {
        let decision = if resp.subtype == "success" || resp.subtype == "allow" {
            PermissionDecision::Allow
        } else {
            // Log before denying: an unrecognized subtype (protocol drift,
            // a typo on the leader side) used to become an unexplained
            // permission denial with no trace.
            crate::logging::warn(format!(
                "permission response '{}' has unrecognized subtype {:?}; resolving to Deny",
                resp.request_id, resp.subtype
            ));
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
            // Same envelope check as scan_inbox_with_priority.
            if let Some(protocol) = parse_structured_protocol_from(&msg.text, &msg.from) {
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
pub async fn handle_shutdown_request(
    rt: &TeammateRuntime,
    request_id: &str,
    agent_name: &str,
    team_name: &str,
) -> anyhow::Result<TeammateState> {
    let now = chrono::Utc::now().to_rfc3339();

    // Default-approve, but reject when this teammate still owns
    // in-progress work — the hardcoded `true` made the reject branch dead
    // code and approved away stranded tasks (their owner then disappears).
    let owns_in_progress = {
        let manager = rt.task_v2_manager.lock().unwrap_or_else(|e| e.into_inner());
        manager
            .list_tasks()
            .map(|tasks| {
                tasks.iter().any(|task| {
                    task.owner.as_deref() == Some(agent_name)
                        && matches!(task.status, crate::tools::task_v2::TaskV2Status::InProgress)
                })
            })
            .unwrap_or(false)
    };
    let approved = !owns_in_progress;

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
    let mut manager = rt.task_v2_manager.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(err) = manager.update_task(
        task_id,
        Some(TaskV2Status::Completed),
        None,
        None,
        None,
        None,
        metadata,
        None,
        None,
    ) {
        // A discarded failure used to leave the task `InProgress` on disk
        // while the agent believed it complete, with no signal anywhere.
        crate::logging::warn(format!(
            "teammate '{}' failed to mark task {task_id} completed: {err}",
            rt.agent_name
        ));
    }
}

/// Record a task execution failure: reset the task to `Pending` and clear
/// the owner (with the error as metadata). The old None-status version
/// left the task `InProgress` and owned by this agent — `claim_task`'s
/// busy check then rejected every future claim after ONE transient
/// failure, permanently idling the teammate (`try_claim_next_task`
/// swallows the error and just keeps polling).
async fn record_task_error(rt: &TeammateRuntime, task_id: &str, error: &str) {
    let mut manager = rt.task_v2_manager.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(err) = manager.update_task(
        task_id,
        Some(crate::tools::task_v2::TaskV2Status::Pending),
        None,
        None,
        None,
        None,
        Some(serde_json::json!({ "last_teammate_error": error })),
        None,
        None,
    ) {
        crate::logging::warn(format!(
            "failed to reset task {task_id} after teammate error: {err}"
        ));
    }
    // `update_task`'s owner parameter is set-only (None = unchanged), so
    // ownership is cleared through the manager's unassign helper — the
    // busy check in `claim_task` blocks on ANY in-progress ownership.
    if let Err(err) = manager.unassign_teammate_tasks(&rt.agent_name) {
        crate::logging::warn(format!(
            "failed to unassign {} after task error: {err}",
            rt.agent_name
        ));
    }
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
    let mut summary: Option<String> = None;

    if !rt.initial_prompt.trim().is_empty() && !rt.cancel_token.is_cancelled() {
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
    }

    // Each iteration exits with `break <state>` carrying the intended
    // final state — intermediate `Active` assignments used to be dead
    // writes overwritten by a trailing `state = Idle` (hidden behind
    // `#![allow(unused_assignments)]`), so the loop could never report
    // `Active` externally.
    let final_state = loop {
        if rt.cancel_token.is_cancelled() {
            break TeammateState::Terminated;
        }

        let wait_result = wait_for_next_prompt_or_shutdown(&mut rt).await;

        match wait_result {
            WaitResult::Aborted => break TeammateState::Terminated,
            WaitResult::ShutdownRequest(entry) => {
                match handle_shutdown_request(&rt, &entry.request_id, &rt.agent_name, &rt.team_name)
                    .await
                {
                    Ok(TeammateState::ShutdownApproved) => {
                        // Release owned tasks before exiting — the manager
                        // provides `unassign_teammate_tasks` for exactly
                        // this; skipping it stranded owned tasks.
                        {
                            let mut manager = rt.task_v2_manager.lock().unwrap_or_else(|e| e.into_inner());
                            let _ = manager.unassign_teammate_tasks(&rt.agent_name);
                        }
                        break TeammateState::ShutdownApproved;
                    }
                    // Rejected or handler error: the teammate keeps working.
                    _ => continue,
                }
            }
            WaitResult::NewMessage(inbox) => {
                let prompt = format_prompt_from_inbox(&inbox);
                if prompt.is_empty() {
                    continue;
                }
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
    };

    TeammateResult {
        agent_id: rt.agent_id.clone(),
        agent_name: rt.agent_name.clone(),
        final_state,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn update(from: &str, allowed: &[&str], denied: &[&str]) -> TeamPermissionUpdateEntry {
        TeamPermissionUpdateEntry {
            from: from.to_string(),
            allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
            denied_tools: denied.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn permission_updates_add_and_remove_from_existing_allowlist() {
        let mut allowed = Some(vec!["read".to_string(), "bash".to_string()]);
        apply_team_permission_updates(
            "w1",
            &mut allowed,
            &[
                update(team_lead_name(), &["web_search"], &["bash"]),
                // Non-lead updates are ignored.
                update("peer", &["rm"], &[]),
            ],
        );
        assert_eq!(
            allowed,
            Some(vec!["read".to_string(), "web_search".to_string()])
        );
    }

    #[test]
    fn permission_update_initializes_allowlist_for_unrestricted_teammate() {
        let mut allowed = None;
        apply_team_permission_updates(
            "w1",
            &mut allowed,
            &[update(team_lead_name(), &["read", "bash"], &[])],
        );
        assert_eq!(allowed, Some(vec!["read".to_string(), "bash".to_string()]));
    }

    #[test]
    fn deny_only_update_is_ignored_for_unrestricted_teammate() {
        // Applying a pure deny-list to an unrestricted teammate as an empty
        // allowlist would lock it out of everything.
        let mut allowed = None;
        apply_team_permission_updates(
            "w1",
            &mut allowed,
            &[update(team_lead_name(), &[], &["rm"])],
        );
        assert_eq!(allowed, None);
    }
}
