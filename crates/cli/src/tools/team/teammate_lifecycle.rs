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
    // The manager's calls are blocking fs+flock under a std Mutex (never
    // held across an await), so the whole claim runs off the runtime
    // worker thread.
    let manager = task_v2_manager.clone();
    let agent = agent_name.to_string();
    tokio::task::spawn_blocking(move || claim_next_task_blocking(&agent, &manager))
        .await
        .ok()
        .flatten()
}

/// The blocking half of [`try_claim_next_task`].
fn claim_next_task_blocking(
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
            // Release only this claim: the blanket unassign also strips
            // ownership from unrelated in-progress tasks of the same agent.
            if let Err(release_err) = manager.release_task(&task.id) {
                crate::logging::warn(format!(
                    "failed to release task {} after status-update failure: {release_err}",
                    task.id
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
    /// Carries the rest of the scan batch: the scanner already marked every
    /// message read, so the caller must drain `lead_messages`,
    /// `peer_messages`, and `task_assignments` before acting on shutdown or
    /// they are lost on every path (approved: the teammate exits;
    /// rejected: the next scan finds nothing unread).
    ShutdownRequest(ShutdownRequestEntry, PriorityInboxResult),
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

        // The scan is synchronous flock-guarded file I/O; keep it off the
        // runtime worker thread (this poll runs every 500ms per teammate).
        let scan = {
            let (agent, team) = (rt.agent_name.clone(), rt.team_name.clone());
            tokio::task::spawn_blocking(move || scan_inbox_with_priority(&agent, &team))
        };
        let mut inbox = match scan.await {
            Ok(Ok(i)) => {
                scan_failures = 0;
                i
            }
            Ok(Err(err)) => {
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
            Err(err) => {
                scan_failures += 1;
                if scan_failures == 1 || scan_failures.is_multiple_of(20) {
                    crate::logging::warn(format!(
                        "teammate '{}' inbox scan worker failed (attempt {scan_failures}): {err}",
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

        // Apply plan approval responses.
        for resp in &inbox.plan_approval_responses {
            if resp.approved {
                rt.permission_mode = resp
                    .permission_mode
                    .clone()
                    .unwrap_or_else(|| "auto".to_string());
            } else if let Some(feedback) = &resp.feedback {
                // A leader's rejection used to be silently discarded: the
                // teammate kept operating under a rejected plan with no idea
                // why. Surface the feedback as a lead message so it rides
                // the next prompt.
                inbox.lead_messages.push(TeammateMessage {
                    from: team_lead_name().to_string(),
                    text: format!(
                        "Your plan (request {}) was rejected by the lead: {feedback}",
                        resp.request_id
                    ),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    read: true,
                    color: None,
                    summary: Some("plan rejection feedback".to_string()),
                });
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

        // Shutdown request has highest priority, and carries the rest of
        // the batch (already marked read) so the caller can drain it.
        if let Some(entry) = inbox.shutdown_request.clone() {
            return WaitResult::ShutdownRequest(entry, inbox);
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
    // The manager lookup is blocking flock-guarded file I/O.
    let owns_in_progress = {
        let manager = rt.task_v2_manager.clone();
        let agent = agent_name.to_string();
        match tokio::task::spawn_blocking(move || {
            let manager = manager.lock().unwrap_or_else(|e| e.into_inner());
            manager.list_tasks().map(|tasks| {
                tasks.iter().any(|task| {
                    task.owner.as_deref() == Some(agent.as_str())
                        && matches!(task.status, crate::tools::task_v2::TaskV2Status::InProgress)
                })
            })
        })
        .await
        {
            Ok(Ok(owns)) => owns,
            // Fail closed: with ownership unknown, approving shutdown could
            // strand in-progress tasks (their release path assumes the
            // departure is legitimate). Rejecting keeps the teammate alive
            // and lets shutdown be retried once the store is readable.
            Ok(Err(err)) => {
                crate::logging::warn(format!(
                    "teammate '{agent_name}' cannot inspect tasks during shutdown: {err}"
                ));
                return Err(err.context("task state unknown; shutdown deferred"));
            }
            Err(err) => {
                crate::logging::warn(format!(
                    "teammate '{agent_name}' shutdown task inspection failed: {err}"
                ));
                return Err(
                    anyhow::Error::new(err).context("task state unknown; shutdown deferred")
                );
            }
        }
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

    // The prompt run is the bulk of the loop's wall time; without this
    // select, a hung tool call or model stream blocked cancellation (and
    // shutdown) until it finished on its own.
    let result = tokio::select! {
        r = tool.execute(input, &rt.tool_context) => r.map_err(|err| err.to_string())?,
        _ = rt.cancel_token.cancelled() => {
            return Err("teammate cancelled during prompt execution".to_string());
        }
    };
    if !result.success {
        return Err(result.content);
    }
    Ok(Some(result.content))
}

/// Complete a claimed task after successful execution.
async fn mark_task_completed(rt: &TeammateRuntime, task_id: &str, summary: Option<&str>) {
    let metadata = summary.map(|summary| serde_json::json!({ "teammate_summary": summary }));
    // The manager's calls are blocking fs+flock under a std Mutex; the
    // companion record_task_error below takes the same off-thread path.
    let manager = rt.task_v2_manager.clone();
    let id = task_id.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let mut manager = manager.lock().unwrap_or_else(|e| e.into_inner());
        manager.update_task(
            &id,
            Some(TaskV2Status::Completed),
            None,
            None,
            None,
            None,
            metadata,
            None,
            None,
        )
    })
    .await;
    if let Err(err) = result.map_err(anyhow::Error::new).and_then(|r| r) {
        // Leaving the task `InProgress` + owned busy-locks this teammate
        // out of every future claim (`claim_task` rejects agents with an
        // in-progress task), so fall back to the same recovery an execution
        // error takes: reset to `Pending`, clear the owner.
        crate::logging::warn(format!(
            "teammate '{}' failed to mark task {task_id} completed: {err}",
            rt.agent_name
        ));
        record_task_error(rt, task_id, &format!("mark completed failed: {err}")).await;
    }
}

/// Record a task execution failure: reset the task to `Pending` and clear
/// the owner (with the error as metadata). The old None-status version
/// left the task `InProgress` and owned by this agent — `claim_task`'s
/// busy check then rejected every future claim after ONE transient
/// failure, permanently idling the teammate (`try_claim_next_task`
/// swallows the error and just keeps polling).
async fn record_task_error(rt: &TeammateRuntime, task_id: &str, error: &str) {
    let manager = rt.task_v2_manager.clone();
    let id = task_id.to_string();
    let error = error.to_string();
    // `release_task` targets this one task: the blanket unassign used here
    // before also strips ownership from any *other* in-progress task this
    // agent owns, and ownerless in-progress tasks are never claimable.
    let result = tokio::task::spawn_blocking(move || {
        let mut manager = manager.lock().unwrap_or_else(|e| e.into_inner());
        manager.update_task(
            &id,
            Some(crate::tools::task_v2::TaskV2Status::Pending),
            None,
            None,
            None,
            None,
            Some(serde_json::json!({ "last_teammate_error": error })),
            None,
            None,
        )?;
        manager.release_task(&id)
    })
    .await;
    if let Err(err) = result.map_err(anyhow::Error::new).and_then(|r| r) {
        crate::logging::warn(format!(
            "failed to reset task {task_id} after teammate error: {err}"
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
            WaitResult::ShutdownRequest(entry, inbox) => {
                // The scanner already marked the whole batch read: drain its
                // messages and assignments before acting on shutdown, or
                // they are lost on every path (approved: the teammate exits;
                // rejected: the next scan finds nothing unread).
                let pending_prompt = format_prompt_from_inbox(&inbox);
                if !pending_prompt.is_empty() {
                    match execute_teammate_prompt(&rt, pending_prompt).await {
                        Ok(result_summary) => {
                            summary = result_summary.clone();
                            let _ = send_idle_notification(
                                &rt.agent_name,
                                &rt.team_name,
                                result_summary.or_else(|| Some("messages handled".to_string())),
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
                match handle_shutdown_request(&rt, &entry.request_id, &rt.agent_name, &rt.team_name)
                    .await
                {
                    Ok(TeammateState::ShutdownApproved) => {
                        // Release owned tasks before exiting — the manager
                        // provides `unassign_teammate_tasks` for exactly
                        // this; skipping it stranded owned tasks.
                        {
                            let mut manager =
                                rt.task_v2_manager.lock().unwrap_or_else(|e| e.into_inner());
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

    // ---- coverage slice: priority inbox scan, prompt formatting, permission
    // resolution, task claiming, shutdown handling, and the wait loop — all
    // against tempdir stores, no engine turns. ----

    use crate::tools::team::new_shared_permission_registry;
    use crate::tools::team::team_file::{
        TeamFile, TeamMember, create_team_file, format_lead_agent_id,
    };
    use codesmith_agent_runtime::test_support::{ScopedCodeSmithHome, lock_test_env};
    use uuid::Uuid;

    fn wide_team_file(name: &str) -> TeamFile {
        TeamFile {
            name: name.to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: format_lead_agent_id(name),
            lead_session_id: None,
            team_allowed_paths: None,
            members: vec![TeamMember {
                agent_id: "worker1@t".to_string(),
                name: "worker1".to_string(),
                agent_type: None,
                model: None,
                prompt: None,
                color: None,
                joined_at: 0,
                cwd: "/tmp".to_string(),
                worktree_path: None,
                session_id: None,
                is_active: true,
            }],
        }
    }

    fn seed(agent: &str, team: &str, from: &str, text: &str) {
        write_to_mailbox(
            agent,
            team,
            TeammateMessage {
                from: from.to_string(),
                text: text.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                read: false,
                color: None,
                summary: None,
            },
        )
        .expect("seed message");
    }

    fn protocol_text(protocol: StructuredProtocolMessage) -> String {
        serde_json::to_string(&protocol).expect("serialize protocol")
    }

    #[test]
    fn scan_inbox_with_priority_classifies_every_message_family() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("scan-team")).expect("team");
        let now = chrono::Utc::now().to_rfc3339();

        seed(
            "worker1",
            "scan-team",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::ShutdownRequest {
                request_id: "req-s1".to_string(),
                from: team_lead_name().to_string(),
                reason: Some("done".to_string()),
                timestamp: now.clone(),
            }),
        );
        seed(
            "worker1",
            "scan-team",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::PermissionResponse {
                request_id: "req-p1".to_string(),
                subtype: "allow".to_string(),
                error: None,
            }),
        );
        seed(
            "worker1",
            "scan-team",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::PlanApprovalResponse {
                request_id: "req-pl1".to_string(),
                approved: false,
                feedback: Some("needs tests".to_string()),
                permission_mode: Some("plan".to_string()),
                timestamp: now.clone(),
            }),
        );
        seed(
            "worker1",
            "scan-team",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::TaskAssignment {
                task_id: "t1".to_string(),
                subject: "ship".to_string(),
                description: "ship it".to_string(),
                assigned_by: team_lead_name().to_string(),
                timestamp: now.clone(),
            }),
        );
        seed(
            "worker1",
            "scan-team",
            "peer1",
            &protocol_text(StructuredProtocolMessage::IdleNotification {
                from: "peer1".to_string(),
                timestamp: now.clone(),
                idle_reason: Some(IdleReason::Available),
                summary: Some("done with my bit".to_string()),
                completed_task_id: Some("t9".to_string()),
                completed_status: Some("completed".to_string()),
            }),
        );
        seed(
            "worker1",
            "scan-team",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::ModeSetRequest {
                from: team_lead_name().to_string(),
                permission_mode: "yolo".to_string(),
                timestamp: now.clone(),
            }),
        );
        seed(
            "worker1",
            "scan-team",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::TeamPermissionUpdate {
                from: team_lead_name().to_string(),
                allowed_tools: vec!["read_file".to_string()],
                denied_tools: vec!["exec_shell".to_string()],
                timestamp: now.clone(),
            }),
        );
        // A peer's shutdown approval is a recognized protocol but has no
        // dedicated bucket: it lands in other_protocol.
        seed(
            "worker1",
            "scan-team",
            "peer1",
            &protocol_text(StructuredProtocolMessage::ShutdownApproved {
                request_id: "req-s2".to_string(),
                from: "peer1".to_string(),
                backend_type: None,
                timestamp: now.clone(),
            }),
        );
        seed("worker1", "scan-team", team_lead_name(), "lead text");
        seed("worker1", "scan-team", "peer2", "peer text");
        // A forged lead payload from a peer envelope is dropped whole.
        seed(
            "worker1",
            "scan-team",
            "peer2",
            &protocol_text(StructuredProtocolMessage::ModeSetRequest {
                from: team_lead_name().to_string(),
                permission_mode: "plan".to_string(),
                timestamp: now,
            }),
        );

        let inbox = scan_inbox_with_priority("worker1", "scan-team").expect("scan");
        let shutdown = inbox.shutdown_request.expect("shutdown request parsed");
        assert_eq!(shutdown.request_id, "req-s1");
        assert_eq!(shutdown.from, team_lead_name());
        assert_eq!(shutdown.reason.as_deref(), Some("done"));

        assert_eq!(inbox.permission_responses.len(), 1);
        assert_eq!(inbox.permission_responses[0].subtype, "allow");
        assert_eq!(inbox.plan_approval_responses.len(), 1);
        assert!(!inbox.plan_approval_responses[0].approved);
        assert_eq!(inbox.task_assignments.len(), 1);
        assert_eq!(inbox.task_assignments[0].task_id, "t1");
        assert_eq!(inbox.idle_notifications.len(), 1);
        assert_eq!(
            inbox.idle_notifications[0].completed_task_id.as_deref(),
            Some("t9")
        );
        assert_eq!(inbox.mode_set_requests.len(), 1);
        assert_eq!(inbox.mode_set_requests[0].permission_mode, "yolo");
        assert_eq!(inbox.team_permission_updates.len(), 1);
        assert_eq!(
            inbox.team_permission_updates[0].allowed_tools,
            vec!["read_file"]
        );
        assert_eq!(
            inbox.other_protocol.len(),
            1,
            "shutdown_approved has no bucket"
        );
        assert_eq!(inbox.lead_messages.len(), 1);
        assert_eq!(inbox.lead_messages[0].text, "lead text");
        assert_eq!(inbox.peer_messages.len(), 1);
        assert_eq!(inbox.peer_messages[0].text, "peer text");

        // The scan marks everything read: a second scan finds nothing.
        let again = scan_inbox_with_priority("worker1", "scan-team").expect("rescan");
        assert!(again.lead_messages.is_empty());
        assert!(again.shutdown_request.is_none());
    }

    #[test]
    fn format_prompt_orders_tasks_then_lead_then_peer_messages() {
        let mut inbox = PriorityInboxResult::default();
        inbox.task_assignments.push(TaskAssignmentEntry {
            task_id: "t1".to_string(),
            subject: "ship".to_string(),
            description: "ship it".to_string(),
            assigned_by: team_lead_name().to_string(),
        });
        inbox.lead_messages.push(TeammateMessage {
            from: team_lead_name().to_string(),
            text: "lead asks".to_string(),
            timestamp: "t".to_string(),
            read: false,
            color: None,
            summary: None,
        });
        inbox.peer_messages.push(TeammateMessage {
            from: "peer".to_string(),
            text: "peer says".to_string(),
            timestamp: "t".to_string(),
            read: false,
            color: None,
            summary: None,
        });

        let prompt = format_prompt_from_inbox(&inbox);
        let task_at = prompt.find("New task assigned: ship").expect("task first");
        let lead_at = prompt.find("lead asks").expect("lead second");
        let peer_at = prompt.find("peer says").expect("peer last");
        assert!(task_at < lead_at && lead_at < peer_at, "{prompt}");
        assert!(
            prompt.contains("ship it"),
            "task description rides the prompt"
        );

        assert_eq!(
            format_prompt_from_inbox(&PriorityInboxResult::default()),
            "",
            "an empty inbox formats an empty prompt"
        );
    }

    #[test]
    fn resolve_permission_responses_routes_allow_and_denies_unknown_subtypes() {
        let registry = new_shared_permission_registry();
        let allow_rx = {
            let mut reg = registry.lock().unwrap();
            reg.register("req-ok".to_string())
        };
        let deny_rx = {
            let mut reg = registry.lock().unwrap();
            reg.register("req-weird".to_string())
        };

        resolve_permission_responses(
            &[
                PermissionResponseEntry {
                    request_id: "req-ok".to_string(),
                    subtype: "success".to_string(),
                    error: None,
                },
                PermissionResponseEntry {
                    request_id: "req-weird".to_string(),
                    subtype: "garbage".to_string(),
                    error: Some("boom".to_string()),
                },
            ],
            &registry,
        );

        assert_eq!(allow_rx.blocking_recv(), Ok(PermissionDecision::Allow));
        match deny_rx.blocking_recv().expect("deny decision") {
            PermissionDecision::Deny { reason } => {
                assert_eq!(reason.as_deref(), Some("boom"))
            }
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn process_inbox_messages_splits_protocol_from_text() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("proc-team")).expect("team");
        seed(
            "worker1",
            "proc-team",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::ModeSetRequest {
                from: team_lead_name().to_string(),
                permission_mode: "plan".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            }),
        );
        seed("worker1", "proc-team", "peer2", "plain words");

        let (protocol, text) = process_inbox_messages("worker1", "proc-team").expect("process");
        assert_eq!(protocol.len(), 1);
        assert!(matches!(
            &protocol[0],
            StructuredProtocolMessage::ModeSetRequest { .. }
        ));
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].text, "plain words");

        // Read-marking applies here too.
        let (protocol, text) = process_inbox_messages("worker1", "proc-team").expect("again");
        assert!(protocol.is_empty() && text.is_empty());
    }

    #[test]
    fn send_idle_notification_writes_the_protocol_to_the_lead_inbox() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("idle-team")).expect("team");

        send_idle_notification(
            "worker1",
            "idle-team",
            Some("finished the sweep".to_string()),
            Some("t7".to_string()),
        )
        .expect("idle notification");

        let lead =
            crate::tools::team::teammate_mailbox::read_mailbox(team_lead_name(), "idle-team")
                .expect("lead");
        assert_eq!(lead.len(), 1);
        let parsed = parse_structured_protocol_from(&lead[0].text, &lead[0].from)
            .expect("idle protocol parses");
        match parsed {
            StructuredProtocolMessage::IdleNotification {
                from,
                idle_reason,
                summary,
                completed_task_id,
                ..
            } => {
                assert_eq!(from, "worker1");
                assert_eq!(idle_reason, Some(IdleReason::Available));
                assert_eq!(summary.as_deref(), Some("finished the sweep"));
                assert_eq!(completed_task_id.as_deref(), Some("t7"));
            }
            other => panic!("expected idle notification, got {other:?}"),
        }
    }

    #[test]
    fn poll_leader_inbox_formats_protocol_and_text_messages() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("poll-team")).expect("team");
        let now = chrono::Utc::now().to_rfc3339();

        seed(
            team_lead_name(),
            "poll-team",
            "worker1",
            &protocol_text(StructuredProtocolMessage::IdleNotification {
                from: "worker1".to_string(),
                timestamp: now.clone(),
                idle_reason: Some(IdleReason::Available),
                summary: Some("standing by".to_string()),
                completed_task_id: None,
                completed_status: None,
            }),
        );
        seed(
            team_lead_name(),
            "poll-team",
            "worker1",
            &protocol_text(StructuredProtocolMessage::ShutdownApproved {
                request_id: "req-a".to_string(),
                from: "worker1".to_string(),
                backend_type: None,
                timestamp: now.clone(),
            }),
        );
        seed(
            team_lead_name(),
            "poll-team",
            "worker1",
            &protocol_text(StructuredProtocolMessage::ShutdownRejected {
                request_id: "req-b".to_string(),
                from: "worker1".to_string(),
                reason: "unfinished work".to_string(),
                timestamp: now.clone(),
            }),
        );
        seed(
            team_lead_name(),
            "poll-team",
            "worker2",
            &protocol_text(StructuredProtocolMessage::TaskAssignment {
                task_id: "t1".to_string(),
                subject: "s".to_string(),
                description: "d".to_string(),
                assigned_by: "worker2".to_string(),
                timestamp: now,
            }),
        );
        seed(team_lead_name(), "poll-team", "worker2", "plain update");

        let lines = poll_leader_inbox("poll-team").expect("poll");
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(
            lines[0].contains("Teammate worker1 is idle. standing by"),
            "line 0: {:?}",
            lines[0]
        );
        assert!(
            lines[1].contains("Teammate worker1 approved shutdown"),
            "line 1: {:?}",
            lines[1]
        );
        assert!(
            lines[2].contains("Teammate worker1 rejected shutdown: unfinished work"),
            "line 2: {:?}",
            lines[2]
        );
        assert_eq!(lines[3], "Protocol message from teammate");
        assert_eq!(lines[4], "Message from worker2: plain update");
    }

    // ---- task claiming + lifecycle state machine on tempdir stores ----

    fn shared_tasks(team: &str) -> SharedTaskV2Manager {
        crate::tools::task_v2::new_shared_task_v2_manager(team).expect("task manager")
    }

    fn create_task(
        manager: &SharedTaskV2Manager,
        subject: &str,
        owner: Option<&str>,
        blocked_by: Vec<String>,
    ) -> String {
        let mut guard = manager.lock().unwrap();
        guard
            .create_task(
                subject.to_string(),
                format!("{subject} body"),
                None,
                None,
                owner.map(ToString::to_string),
                blocked_by,
                Vec::new(),
                None,
            )
            .expect("create task")
    }

    #[tokio::test]
    async fn try_claim_next_task_skips_owned_and_blocked_tasks() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("claim-team")).expect("team");
        let tasks = shared_tasks("claim-team");

        create_task(&tasks, "owned", Some("someone-else"), Vec::new());
        let blocker = create_task(&tasks, "blocker", Some("someone-else"), Vec::new());
        create_task(&tasks, "blocked", None, vec![blocker.clone()]);
        let free = create_task(&tasks, "free", None, Vec::new());

        let claimed = try_claim_next_task("worker1", &tasks).await.expect("claim");
        assert_eq!(
            claimed.subject, "free",
            "owned and blocked tasks are skipped"
        );
        assert_eq!(claimed.owner.as_deref(), Some("worker1"));
        {
            // The returned record is the claim snapshot; the store reflects
            // the in-progress arming the claim performs.
            let guard = tasks.lock().unwrap();
            let stored = guard.get_task(&claimed.id).expect("task");
            assert_eq!(
                stored.status,
                TaskV2Status::InProgress,
                "claim arms the task"
            );
        }

        // The blocked task is still unreachable while its blocker is
        // unresolved, and worker1 is busy with `free`.
        assert!(
            try_claim_next_task("worker1", &tasks).await.is_none(),
            "nothing left to claim"
        );

        // Completing both the claim and the blocker unblocks the dependent task.
        {
            let mut guard = tasks.lock().unwrap();
            guard
                .update_task(
                    &free,
                    Some(TaskV2Status::Completed),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("complete free");
            guard
                .update_task(
                    &blocker,
                    Some(TaskV2Status::Completed),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("complete blocker");
        }
        let next = try_claim_next_task("worker1", &tasks)
            .await
            .expect("unblocked task claimable");
        assert_eq!(next.subject, "blocked");
    }

    /// Caller must hold `lock_test_env()` — constructing a context touches
    /// env-derived state. Not reentrant: the guard is the caller's.
    fn teammate_runtime(team: &str, agent: &str, tasks: SharedTaskV2Manager) -> TeammateRuntime {
        let workspace = std::env::temp_dir().join(format!("teammate-lifecycle-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let context = ToolContext::new(workspace.clone());
        let config = crate::config::Config {
            api_key: Some("test-key".to_string()),
            ..crate::config::Config::default()
        };
        let client = crate::core::engine::resolve_llm_client_with_secrets(
            &config,
            &crate::no_keyring_secrets(),
        )
        .expect("stub client constructs");
        let subagent_manager = crate::tools::subagent::new_shared_subagent_manager(workspace, 4);
        let subagent_runtime = SubAgentRuntime::new(
            client,
            "deepseek-chat".to_string(),
            context.clone(),
            true,
            None,
            subagent_manager.clone(),
        );
        TeammateRuntime {
            agent_id: format!("{agent}@{team}"),
            agent_name: agent.to_string(),
            team_name: team.to_string(),
            color: None,
            cancel_token: CancellationToken::new(),
            task_v2_manager: tasks,
            initial_prompt: String::new(),
            permission_mode: "auto".to_string(),
            permission_registry: new_shared_permission_registry(),
            subagent_manager,
            subagent_runtime,
            tool_context: context,
            agent_type: SubAgentType::Team,
            model: None,
            allowed_tools: None,
        }
    }

    #[tokio::test]
    async fn handle_shutdown_request_approves_idle_and_rejects_busy_teammates() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("sd-team")).expect("team");
        let tasks = shared_tasks("sd-team");
        let rt = teammate_runtime("sd-team", "worker1", tasks.clone());

        // Idle: no owned in-progress work, shutdown approves.
        let state = handle_shutdown_request(&rt, "req-1", "worker1", "sd-team")
            .await
            .expect("shutdown decision");
        assert_eq!(state, TeammateState::ShutdownApproved);
        let lead = crate::tools::team::teammate_mailbox::read_mailbox(team_lead_name(), "sd-team")
            .expect("lead");
        assert!(
            lead.iter()
                .any(|msg| msg.text.contains("shutdown_approved") && msg.text.contains("req-1")),
            "approval lands in the lead's mailbox"
        );

        // Busy: an owned in-progress task makes the teammate refuse to die.
        let busy = create_task(&tasks, "wip", None, Vec::new());
        {
            let mut guard = tasks.lock().unwrap();
            guard.claim_task(&busy, "worker1").expect("claim");
            guard
                .update_task(
                    &busy,
                    Some(TaskV2Status::InProgress),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("in progress");
        }
        let state = handle_shutdown_request(&rt, "req-2", "worker1", "sd-team")
            .await
            .expect("shutdown decision");
        assert_eq!(state, TeammateState::ShutdownRejected);
        let lead = crate::tools::team::teammate_mailbox::read_mailbox(team_lead_name(), "sd-team")
            .expect("lead");
        assert!(
            lead.iter()
                .any(|msg| msg.text.contains("shutdown_rejected")
                    && msg.text.contains("pending work")),
            "rejection lands with the reason"
        );
    }

    #[tokio::test]
    async fn mark_task_completed_and_record_task_error_reset_task_state() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("mt-team")).expect("team");
        let tasks = shared_tasks("mt-team");
        let rt = teammate_runtime("mt-team", "worker1", tasks.clone());

        let done = create_task(&tasks, "done", None, Vec::new());
        {
            let mut guard = tasks.lock().unwrap();
            guard.claim_task(&done, "worker1").expect("claim");
        }
        mark_task_completed(&rt, &done, Some("all green")).await;
        {
            let guard = tasks.lock().unwrap();
            let task = guard.get_task(&done).expect("task");
            assert_eq!(task.status, TaskV2Status::Completed);
            assert_eq!(
                task.metadata.get("teammate_summary"),
                Some(&serde_json::json!("all green"))
            );
        }

        // An execution error resets the task to pending and unowned.
        let failed = create_task(&tasks, "failed", None, Vec::new());
        {
            let mut guard = tasks.lock().unwrap();
            guard.claim_task(&failed, "worker1").expect("claim");
        }
        record_task_error(&rt, &failed, "tool exploded").await;
        {
            let guard = tasks.lock().unwrap();
            let task = guard.get_task(&failed).expect("task");
            assert_eq!(task.status, TaskV2Status::Pending, "reset for retry");
            assert_eq!(task.owner, None, "ownership released");
            assert!(
                task.metadata
                    .get("last_teammate_error")
                    .is_some_and(|e| e.as_str() == Some("tool exploded")),
                "error recorded in metadata"
            );
        }
    }

    #[tokio::test]
    async fn wait_loop_returns_aborted_on_a_cancelled_token() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("wait-abort")).expect("team");
        let tasks = shared_tasks("wait-abort");
        let mut rt = teammate_runtime("wait-abort", "worker1", tasks);
        rt.cancel_token.cancel();

        match wait_for_next_prompt_or_shutdown(&mut rt).await {
            WaitResult::Aborted => {}
            _ => panic!("expected Aborted from a cancelled token"),
        }
    }

    #[tokio::test]
    async fn wait_loop_returns_new_message_for_lead_text() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("wait-msg")).expect("team");
        let tasks = shared_tasks("wait-msg");
        let mut rt = teammate_runtime("wait-msg", "worker1", tasks);
        seed("worker1", "wait-msg", team_lead_name(), "get to work");

        match wait_for_next_prompt_or_shutdown(&mut rt).await {
            WaitResult::NewMessage(inbox) => {
                assert_eq!(inbox.lead_messages.len(), 1);
                assert_eq!(inbox.lead_messages[0].text, "get to work");
            }
            _ => panic!("expected NewMessage from the seeded inbox"),
        }
    }

    #[tokio::test]
    async fn wait_loop_prioritizes_shutdown_and_carries_the_batch() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("wait-sd")).expect("team");
        let tasks = shared_tasks("wait-sd");
        let mut rt = teammate_runtime("wait-sd", "worker1", tasks);
        seed("worker1", "wait-sd", "peer2", "meanwhile");
        seed(
            "worker1",
            "wait-sd",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::ShutdownRequest {
                request_id: "req-w1".to_string(),
                from: team_lead_name().to_string(),
                reason: Some("wrap up".to_string()),
                timestamp: chrono::Utc::now().to_rfc3339(),
            }),
        );

        match wait_for_next_prompt_or_shutdown(&mut rt).await {
            WaitResult::ShutdownRequest(entry, inbox) => {
                assert_eq!(entry.request_id, "req-w1");
                assert_eq!(inbox.peer_messages.len(), 1, "batch rides the result");
            }
            _ => panic!("expected ShutdownRequest to outrank text messages"),
        }
    }

    #[tokio::test]
    async fn wait_loop_claims_a_pending_task_when_idle() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("wait-task")).expect("team");
        let tasks = shared_tasks("wait-task");
        let mut rt = teammate_runtime("wait-task", "worker1", tasks.clone());
        let id = create_task(&tasks, "claimable", None, Vec::new());

        match wait_for_next_prompt_or_shutdown(&mut rt).await {
            WaitResult::TaskClaimed(task) => assert_eq!(task.id, id),
            _ => panic!("expected TaskClaimed from the idle task list"),
        }
    }

    #[tokio::test]
    async fn wait_loop_applies_plan_mode_permission_and_team_updates() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("wait-apply")).expect("team");
        let tasks = shared_tasks("wait-apply");
        let mut rt = teammate_runtime("wait-apply", "worker1", tasks);
        let now = chrono::Utc::now().to_rfc3339();

        // A pending permission request gets resolved by the batch.
        let permission_rx = {
            let mut reg = rt.permission_registry.lock().unwrap();
            reg.register("req-perm".to_string())
        };
        seed(
            "worker1",
            "wait-apply",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::PermissionResponse {
                request_id: "req-perm".to_string(),
                subtype: "allow".to_string(),
                error: None,
            }),
        );
        // An approved plan flips the permission mode to the response's.
        seed(
            "worker1",
            "wait-apply",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::PlanApprovalResponse {
                request_id: "req-pl-ok".to_string(),
                approved: true,
                feedback: None,
                permission_mode: Some("plan".to_string()),
                timestamp: now.clone(),
            }),
        );
        // A mode set request from the lead overrides afterwards.
        seed(
            "worker1",
            "wait-apply",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::ModeSetRequest {
                from: team_lead_name().to_string(),
                permission_mode: "yolo".to_string(),
                timestamp: now.clone(),
            }),
        );
        // A team permission update initializes the allowlist.
        seed(
            "worker1",
            "wait-apply",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::TeamPermissionUpdate {
                from: team_lead_name().to_string(),
                allowed_tools: vec!["read_file".to_string(), "web_search".to_string()],
                denied_tools: vec![],
                timestamp: now.clone(),
            }),
        );
        // Text so the wait returns NewMessage.
        seed("worker1", "wait-apply", team_lead_name(), "go");

        match wait_for_next_prompt_or_shutdown(&mut rt).await {
            WaitResult::NewMessage(inbox) => {
                assert_eq!(inbox.lead_messages.len(), 1);
            }
            _ => panic!("expected NewMessage from the seeded inbox"),
        }
        assert_eq!(
            rt.permission_mode, "yolo",
            "mode set wins over plan approval"
        );
        assert_eq!(
            rt.allowed_tools,
            Some(vec!["read_file".to_string(), "web_search".to_string()]),
            "team permission update seeds the allowlist"
        );
        assert_eq!(
            permission_rx.await,
            Ok(PermissionDecision::Allow),
            "same-batch permission responses resolve before returning"
        );
    }

    #[tokio::test]
    async fn wait_loop_surfaces_plan_rejection_feedback_as_a_lead_message() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("wait-reject")).expect("team");
        let tasks = shared_tasks("wait-reject");
        let mut rt = teammate_runtime("wait-reject", "worker1", tasks);
        seed(
            "worker1",
            "wait-reject",
            team_lead_name(),
            &protocol_text(StructuredProtocolMessage::PlanApprovalResponse {
                request_id: "req-pl-no".to_string(),
                approved: false,
                feedback: Some("add coverage".to_string()),
                permission_mode: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
            }),
        );

        match wait_for_next_prompt_or_shutdown(&mut rt).await {
            WaitResult::NewMessage(inbox) => {
                let feedback = inbox
                    .lead_messages
                    .iter()
                    .map(|msg| msg.text.as_str())
                    .collect::<Vec<_>>();
                assert!(
                    feedback
                        .iter()
                        .any(|text| text.contains("rejected by the lead: add coverage")),
                    "{feedback:?}"
                );
            }
            _ => panic!("expected NewMessage from the seeded inbox"),
        }
    }

    #[tokio::test]
    async fn run_teammate_loop_reports_terminated_on_immediate_cancel() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&wide_team_file("loop-cancel")).expect("team");
        let tasks = shared_tasks("loop-cancel");
        let mut rt = teammate_runtime("loop-cancel", "worker1", tasks);
        rt.cancel_token.cancel();
        rt.initial_prompt = String::new();

        let result = run_teammate_loop(rt).await;
        assert_eq!(result.final_state, TeammateState::Terminated);
        assert_eq!(result.agent_name, "worker1");
        assert!(result.summary.is_none(), "no prompt ever ran");
    }
}
