//! SendMessageTool — inter-teammate messaging with file-based mailbox delivery.
//!
//! Supports plain text DMs, broadcast (to: "*"), and structured protocol
//! messages dispatched to dedicated handlers from protocol_handlers.rs.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::team::protocol_handlers::{
    handle_plan_approval_auto_approve, handle_plan_approval_rejection, handle_shutdown_rejection,
    handle_shutdown_request,
};
use crate::tools::team::teammate_mailbox::{parse_structured_protocol_from, read_mailbox};
use crate::tools::team::{
    SharedTeamContext, StructuredProtocolMessage, TeammateMessage, find_member_by_name,
    read_team_file, team_lead_name, write_to_mailbox,
};
use codesmith_agent_runtime::features::Feature;

/// Attribution used when a calling context has no runtime-injected team
/// identity. Such contexts must never be treated as the lead.
const UNKNOWN_SENDER: &str = "unknown-sender";

/// Request ids this process has already answered (shutdown and plan
/// approval responses): a request may be approved/rejected at most once, so
/// an id still sitting in the append-only mailbox file cannot be replayed
/// after it was answered. Known limitation: process-local, and the
/// check-then-act window to the mailbox write remains — durable,
/// cross-process consumption needs state in the mailbox store itself.
static HANDLED_REQUEST_IDS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn request_already_handled(request_id: &str) -> bool {
    HANDLED_REQUEST_IDS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(request_id)
}

fn mark_request_handled(request_id: &str) {
    HANDLED_REQUEST_IDS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(request_id.to_string());
}

/// Protocol actions only the team lead may exercise.
const LEAD_ONLY_ACTIONS: [&str; 5] = [
    "shutdown_request",
    "plan_approval_response",
    "team_permission_update",
    "mode_set_request",
    // A leader-authored response type (see StructuredProtocolMessage): a
    // member forging a permission grant/rejection could answer another
    // teammate's pending sandbox permission request.
    "sandbox_permission_response",
];

/// Verify `sender` may exercise `action` against the team roster.
///
/// `team_sender` is injected by the runtime at spawn time (teammates) or at
/// App construction (the lead), so the model cannot forge it through tool
/// input. Plain mailbox files remain the underlying transport trust
/// boundary — this check governs the tool path.
fn authorize_protocol_action(action: &str, sender: &str, team_name: &str) -> Result<(), ToolError> {
    if sender == UNKNOWN_SENDER {
        return Err(ToolError::invalid_input(format!(
            "Refusing '{action}' from an unidentified sender: this context has no team identity"
        )));
    }
    let team_file = read_team_file(team_name)
        .map_err(|e| ToolError::execution_failed(format!("Failed to read team roster: {}", e)))?;
    let is_lead = sender == team_lead_name();
    let is_member = find_member_by_name(&team_file, sender).is_some_and(|member| member.is_active);
    // Name-squatting guard: authorization keys on display names (the
    // runtime injects the agent NAME as `team_sender`). A teammate spawned
    // with the display name "team-lead" would pass `is_lead` above and
    // gain every lead-only action — shutdowns, plan approvals, permission
    // grants. The real lead is not a roster member (its identity lives in
    // `lead_agent_id`), so any roster entry carrying the lead name is a
    // squatter: the name no longer proves lead identity, and we refuse
    // rather than guess which "team-lead" is real.
    if is_lead && find_member_by_name(&team_file, team_lead_name()).is_some() {
        return Err(ToolError::invalid_input(format!(
            "a roster member of team '{team_name}' is named 'team-lead'; the lead name no \
             longer proves lead identity — refusing lead authorization"
        )));
    }
    if !is_lead && !is_member {
        return Err(ToolError::invalid_input(format!(
            "Sender '{sender}' is not an active member of team '{team_name}'"
        )));
    }
    if LEAD_ONLY_ACTIONS.contains(&action) && !is_lead {
        return Err(ToolError::invalid_input(format!(
            "'{action}' is a lead-only protocol action; sender '{sender}' is not the team lead"
        )));
    }
    Ok(())
}

pub struct SendMessageTool {
    team_context: SharedTeamContext,
}

impl SendMessageTool {
    pub fn new(team_context: SharedTeamContext) -> Self {
        Self { team_context }
    }
}

/// Validate that `recipient` names the lead or an active teammate before
/// delivery — a typo'd or nonexistent name used to "succeed" by creating an
/// inbox file nobody polls, misleading the caller into believing the
/// message was delivered (distinct names can also collide after
/// `sanitize_name`, silently redirecting messages).
fn ensure_known_recipient(recipient: &str, team_name: &str) -> Result<(), ToolError> {
    if recipient == "*" {
        return Ok(());
    }
    let team_file = read_team_file(team_name)
        .map_err(|e| ToolError::execution_failed(format!("Failed to read team roster: {}", e)))?;
    let known = recipient == team_lead_name()
        || find_member_by_name(&team_file, recipient).is_some_and(|m| m.is_active);
    if known {
        Ok(())
    } else {
        Err(ToolError::invalid_input(format!(
            "Recipient '{}' is not an active member of team '{}'",
            recipient, team_name
        )))
    }
}

/// Serialize a protocol payload and deliver it to a teammate mailbox.
/// Both the payload and the envelope carry the SAME timestamp — the
/// branches used to call `Utc::now()` twice, so the two could disagree.
fn deliver_protocol(
    recipient: &str,
    team_name: &str,
    sender: &str,
    mut protocol: serde_json::Value,
    summary: &str,
) -> Result<(), ToolError> {
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(obj) = protocol.as_object_mut() {
        obj.insert("timestamp".to_string(), json!(now));
    }
    let text = serde_json::to_string(&protocol)
        .map_err(|e| ToolError::execution_failed(format!("Serialize failed: {}", e)))?;
    write_to_mailbox(
        recipient,
        team_name,
        TeammateMessage {
            from: sender.to_string(),
            text,
            timestamp: now,
            read: false,
            color: None,
            summary: Some(summary.to_string()),
        },
    )
    .map_err(|e| ToolError::execution_failed(format!("Delivery failed: {}", e)))
}

#[async_trait]
impl ToolSpec for SendMessageTool {
    fn name(&self) -> &'static str {
        "send_message"
    }

    fn description(&self) -> &'static str {
        "Send a message to an agent teammate. Supports plain text messages, \
         broadcast (to: \"*\"), and structured protocol messages \
         (shutdown_request, shutdown_response, plan_approval_response, \
         mode_set_request, team_permission_update, sandbox_permission_request/response). \
         Messages are delivered via file-based mailbox."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Recipient: teammate name, or \"*\" for broadcast to all teammates"
                },
                "summary": {
                    "type": "string",
                    "description": "5-10 word preview summary (for plain text messages)"
                },
                "message": {
                    "description": "Plain text string or structured protocol object",
                    "oneOf": [
                        { "type": "string" },
                        {
                            "type": "object",
                            "properties": {
                                "type": {
                                    "type": "string",
                                    "enum": [
                                        "shutdown_request",
                                        "shutdown_approved",
                                        "shutdown_rejected",
                                        "plan_approval_response",
                                        "mode_set_request",
                                        "team_permission_update",
                                        "sandbox_permission_request",
                                        "sandbox_permission_response"
                                    ]
                                },
                                "request_id": { "type": "string" },
                                "approve": { "type": "boolean" },
                                "reason": { "type": "string" },
                                "feedback": { "type": "string" },
                                "permission_mode": { "type": "string" },
                                "allowed_tools": { "type": "array", "items": { "type": "string" } },
                                "denied_tools": { "type": "array", "items": { "type": "string" } },
                                "domain": { "type": "string" },
                                "tool_name": { "type": "string" },
                                "tool_use_id": { "type": "string" },
                                "description": { "type": "string" }
                            },
                            "required": ["type"]
                        }
                    ]
                }
            },
            "required": ["to", "message"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        false
    }
    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        if !context.features.enabled(Feature::AgentTeams) {
            return Err(ToolError::not_available("agent_teams feature is disabled"));
        }

        let recipient = input
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("to"))?
            .to_string();

        let summary = input
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let (team_name, sender_name) = {
            let tc = self.team_context.lock().await;
            match tc.as_ref() {
                Some(ctx) => (
                    ctx.team_name.clone(),
                    // Runtime-injected identity. A missing identity is
                    // attributed as unknown — never silently as the lead.
                    context
                        .runtime
                        .team_sender
                        .clone()
                        .unwrap_or_else(|| UNKNOWN_SENDER.to_string()),
                ),
                None => {
                    return Err(ToolError::invalid_input(
                        "Not in a team. Cannot send messages.",
                    ));
                }
            }
        };

        let message_val = input
            .get("message")
            .ok_or_else(|| ToolError::missing_field("message"))?;

        // Plain text string — write as regular TeammateMessage.
        if message_val.is_string() {
            let text = message_val.as_str().unwrap().to_string();
            return self.send_plain_text(&recipient, &team_name, &sender_name, &text, summary);
        }

        // Structured protocol — dispatch to handler.
        let obj = message_val
            .as_object()
            .ok_or_else(|| ToolError::invalid_input("Message must be a string or object"))?;

        let type_str = obj
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("type in message object"))?;

        // Protocol actions carry privilege (plan approvals, shutdowns,
        // permission grants) — authorize the sender against the roster
        // before dispatching, then confirm the recipient exists so a
        // typo'd name cannot "deliver" into an inbox nobody polls.
        authorize_protocol_action(type_str, &sender_name, &team_name)?;
        ensure_known_recipient(&recipient, &team_name)?;

        match type_str {
            "shutdown_request" => {
                let reason = obj
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let request_id =
                    handle_shutdown_request(&sender_name, &recipient, &team_name, reason).map_err(
                        |e| ToolError::execution_failed(format!("Shutdown request failed: {}", e)),
                    )?;
                ToolResult::json(&json!({
                    "success": true,
                    "message": format!("Shutdown request sent to {}. Request ID: {}", recipient, request_id),
                    "request_id": request_id,
                    "target": recipient,
                })).map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            "shutdown_approved" => {
                let request_id = obj
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::missing_field("request_id"))?
                    .to_string();
                // The approval must be bound to a real, outstanding shutdown
                // request issued to THIS sender — an unbound, caller-supplied
                // request_id would let a member approve a fabricated request
                // or one issued to a different teammate. Approvals are also
                // only meaningful addressed to the lead (the consumer side).
                if recipient != team_lead_name() {
                    return Err(ToolError::invalid_input(
                        "shutdown_approved must be addressed to the team lead",
                    ));
                }
                let mailbox = read_mailbox(&sender_name, &team_name).map_err(|e| {
                    ToolError::execution_failed(format!("Failed to read mailbox: {e}"))
                })?;
                let pending = mailbox.iter().any(|msg| {
                    matches!(
                        parse_structured_protocol_from(&msg.text, &msg.from),
                        Some(StructuredProtocolMessage::ShutdownRequest {
                            request_id: id,
                            from,
                            ..
                        }) if id == request_id && from == team_lead_name()
                    )
                });
                if !pending {
                    return Err(ToolError::invalid_input(format!(
                        "No outstanding shutdown request '{request_id}' issued to '{sender_name}'"
                    )));
                }
                if request_already_handled(&request_id) {
                    return Err(ToolError::invalid_input(format!(
                        "Shutdown request '{request_id}' was already answered"
                    )));
                }
                // Write approval protocol to teammate mailbox (informational).
                // Actual cancellation happens via inbox poller on leader side.
                deliver_protocol(
                    &recipient,
                    &team_name,
                    &sender_name,
                    json!({
                        "type": "shutdown_approved",
                        "request_id": request_id.clone(),
                        "from": sender_name,
                    }),
                    "shutdown approved",
                )?;
                mark_request_handled(&request_id);
                ToolResult::json(&json!({
                    "success": true,
                    "message": format!("Shutdown approved for {}", recipient),
                    "request_id": request_id,
                }))
                .map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            "shutdown_rejected" => {
                let request_id = obj
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::missing_field("request_id"))?
                    .to_string();
                let reason = obj
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("No reason provided")
                    .to_string();
                if request_already_handled(&request_id) {
                    return Err(ToolError::invalid_input(format!(
                        "Shutdown request '{request_id}' was already answered"
                    )));
                }
                handle_shutdown_rejection(
                    &request_id,
                    &sender_name,
                    &recipient,
                    &team_name,
                    reason,
                )
                .map_err(|e| {
                    ToolError::execution_failed(format!("Shutdown rejection failed: {}", e))
                })?;
                mark_request_handled(&request_id);
                ToolResult::json(&json!({
                    "success": true,
                    "message": format!("Shutdown rejected for {}", recipient),
                    "request_id": request_id,
                }))
                .map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            "plan_approval_response" => {
                let request_id = obj
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::missing_field("request_id"))?
                    .to_string();
                let approved = obj
                    .get("approve")
                    .and_then(|v| v.as_bool())
                    .ok_or_else(|| ToolError::missing_field("approve"))?;
                // The approval must be bound to a real, outstanding
                // PlanApprovalRequest issued BY the plan owner (the
                // recipient) to this responder — a stale or fabricated id
                // would otherwise write an approved response (carrying the
                // caller-chosen permission_mode) into the teammate's
                // mailbox, where the inbox poller applies it to whatever
                // plan happens to be pending.
                let mailbox = read_mailbox(&sender_name, &team_name).map_err(|e| {
                    ToolError::execution_failed(format!("Failed to read mailbox: {e}"))
                })?;
                let pending = mailbox.iter().any(|msg| {
                    matches!(
                        parse_structured_protocol_from(&msg.text, &msg.from),
                        Some(StructuredProtocolMessage::PlanApprovalRequest {
                            request_id: id,
                            from,
                            ..
                        }) if id == request_id && from == recipient
                    )
                });
                if !pending {
                    return Err(ToolError::invalid_input(format!(
                        "No outstanding plan approval request '{request_id}' from '{recipient}' \
                         issued to '{sender_name}'"
                    )));
                }
                if request_already_handled(&request_id) {
                    return Err(ToolError::invalid_input(format!(
                        "Plan approval request '{request_id}' was already answered"
                    )));
                }
                if approved {
                    let permission_mode = obj
                        .get("permission_mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("auto")
                        .to_string();
                    handle_plan_approval_auto_approve(
                        &request_id,
                        &recipient,
                        &team_name,
                        &permission_mode,
                    )
                    .map_err(|e| {
                        ToolError::execution_failed(format!("Plan approval failed: {}", e))
                    })?;
                    mark_request_handled(&request_id);
                    ToolResult::json(&json!({
                        "success": true,
                        "message": format!("Plan approved for {}", recipient),
                        "request_id": request_id,
                    }))
                    .map_err(|e| ToolError::execution_failed(e.to_string()))
                } else {
                    let feedback = obj
                        .get("feedback")
                        .and_then(|v| v.as_str())
                        .unwrap_or("No feedback provided")
                        .to_string();
                    handle_plan_approval_rejection(&request_id, &recipient, &team_name, feedback)
                        .map_err(|e| {
                        ToolError::execution_failed(format!("Plan rejection failed: {}", e))
                    })?;
                    mark_request_handled(&request_id);
                    ToolResult::json(&json!({
                        "success": true,
                        "message": format!("Plan rejected for {}", recipient),
                        "request_id": request_id,
                    }))
                    .map_err(|e| ToolError::execution_failed(e.to_string()))
                }
            }
            "mode_set_request" => {
                let permission_mode = obj
                    .get("permission_mode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::missing_field("permission_mode"))?
                    .to_string();
                deliver_protocol(
                    &recipient,
                    &team_name,
                    &sender_name,
                    json!({
                        "type": "mode_set_request",
                        "from": sender_name,
                        "permission_mode": permission_mode,
                    }),
                    &format!("mode set: {}", permission_mode),
                )?;
                ToolResult::json(&json!({
                    "success": true,
                    "message": format!("Mode set request sent to {}: {}", recipient, permission_mode),
                })).map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            "team_permission_update" => {
                let allowed_tools: Vec<String> = obj
                    .get("allowed_tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let denied_tools: Vec<String> = obj
                    .get("denied_tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                deliver_protocol(
                    &recipient,
                    &team_name,
                    &sender_name,
                    json!({
                        "type": "team_permission_update",
                        "from": sender_name,
                        "allowed_tools": allowed_tools,
                        "denied_tools": denied_tools,
                    }),
                    "permission update",
                )?;
                ToolResult::json(&json!({
                    "success": true,
                    "message": format!("Permission update sent to {}", recipient),
                }))
                .map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            "sandbox_permission_request" | "sandbox_permission_response" => {
                // Forward as-is — the inbox poller will classify and route.
                let mut forwarded = obj.clone();
                forwarded.insert("from".to_string(), json!(sender_name));
                deliver_protocol(
                    &recipient,
                    &team_name,
                    &sender_name,
                    serde_json::Value::Object(forwarded),
                    &format!("sandbox permission: {}", type_str),
                )?;
                ToolResult::json(&json!({
                    "success": true,
                    "message": format!("Sandbox permission {} sent to {}", type_str, recipient),
                }))
                .map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            _ => Err(ToolError::invalid_input(format!(
                "Unknown protocol type: {}",
                type_str
            ))),
        }
    }
}

impl SendMessageTool {
    /// Send a plain text message to a single recipient or broadcast.
    fn send_plain_text(
        &self,
        recipient: &str,
        team_name: &str,
        sender_name: &str,
        text: &str,
        summary: Option<String>,
    ) -> Result<ToolResult, ToolError> {
        let team_msg = TeammateMessage {
            from: sender_name.to_string(),
            text: text.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            color: None,
            summary,
        };

        if recipient == "*" {
            // Broadcast to all non-lead teammates. Partial failures are
            // collected and reported — earlier deliveries stand, and the
            // caller sees exactly who did and did not receive the message.
            let team_file = read_team_file(team_name).map_err(|e| {
                ToolError::execution_failed(format!("Failed to read team file: {}", e))
            })?;

            let mut delivered = Vec::new();
            let mut failed = serde_json::Map::new();
            for member in &team_file.members {
                if member.name != team_lead_name() && member.is_active {
                    match write_to_mailbox(&member.name, team_name, team_msg.clone()) {
                        Ok(()) => delivered.push(member.name.clone()),
                        Err(e) => {
                            failed.insert(member.name.clone(), json!(e.to_string()));
                        }
                    }
                }
            }

            if delivered.is_empty() && !failed.is_empty() {
                return Err(ToolError::execution_failed(format!(
                    "Broadcast failed for all recipients: {:?}",
                    failed
                )));
            }

            let mut payload = json!({"broadcast": true, "delivered_to": delivered});
            if !failed.is_empty() {
                payload
                    .as_object_mut()
                    .expect("payload is an object")
                    .insert("failed".to_string(), serde_json::Value::Object(failed));
            }
            return ToolResult::json(&payload)
                .map_err(|e| ToolError::execution_failed(e.to_string()));
        }

        // Single recipient DM — check the roster so a typo'd name cannot
        // silently "deliver" into an inbox nobody polls.
        ensure_known_recipient(recipient, team_name)?;
        write_to_mailbox(recipient, team_name, team_msg).map_err(|e| {
            ToolError::execution_failed(format!("Failed to deliver to {}: {}", recipient, e))
        })?;

        ToolResult::json(&json!({"delivered_to": recipient}))
            .map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    // The env lock (`lock_test_env`) must span the whole async test body:
    // `ScopedCodeSmithHome` redirects `CODESMITH_HOME` and the awaited tool
    // executions read team files through it, so the guard is deliberately
    // held across await points. These tests run on `#[tokio::test]`'s
    // current-thread runtime, so no other task polled on that thread can
    // contend for the std mutex while it is held.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::tools::spec::RuntimeToolServices;
    use crate::tools::team::team_file::{
        TeamFile, TeamMember, create_team_file, format_lead_agent_id,
    };
    use crate::tools::team::teammate_mailbox::read_mailbox;
    use codesmith_agent_runtime::test_support::{ScopedCodeSmithHome, lock_test_env};

    fn make_team(name: &str) -> TeamFile {
        TeamFile {
            name: name.to_string(),
            description: None,
            created_at: 1234567890,
            lead_agent_id: format_lead_agent_id(name),
            lead_session_id: None,
            team_allowed_paths: None,
            members: vec![TeamMember {
                agent_id: "worker@t".to_string(),
                name: "worker1".to_string(),
                agent_type: None,
                model: None,
                prompt: None,
                color: None,
                joined_at: 1234567890,
                cwd: "/tmp".to_string(),
                worktree_path: None,
                session_id: None,
                is_active: true,
            }],
        }
    }

    async fn setup() -> (SendMessageTool, SharedTeamContext) {
        // Caller must hold lock_test_env() — the env lock is not reentrant.
        let team_context = crate::tools::team::new_shared_team_context();
        {
            let mut slot = team_context.lock().await;
            *slot = Some(crate::tools::team::TeamContext {
                team_name: "auth-test".to_string(),
                team_file_path: std::path::PathBuf::new(),
                lead_agent_id: format_lead_agent_id("auth-test"),
                task_v2_manager: crate::tools::task_v2::new_shared_task_v2_manager("auth-test")
                    .expect("task manager"),
                teammates: std::collections::HashMap::new(),
                teammate_cancel_tokens: std::collections::HashMap::new(),
            });
        }
        (SendMessageTool::new(team_context.clone()), team_context)
    }

    fn context_with_sender(sender: Option<String>) -> ToolContext {
        let runtime = RuntimeToolServices {
            team_sender: sender,
            ..RuntimeToolServices::default()
        };
        let mut ctx = ToolContext::new("/tmp");
        ctx.runtime = runtime;
        ctx.features.enable(Feature::AgentTeams);
        ctx
    }

    fn shutdown_request_input() -> serde_json::Value {
        json!({
            "to": "worker1",
            "message": {"type": "shutdown_request", "reason": "done"}
        })
    }

    #[tokio::test]
    async fn unknown_sender_cannot_exercise_protocol_actions() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        let err = tool
            .execute(shutdown_request_input(), &context_with_sender(None))
            .await
            .expect_err("unknown sender must be denied");
        assert!(
            err.to_string().contains("unidentified sender"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn teammate_cannot_exercise_lead_only_actions() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        let err = tool
            .execute(
                shutdown_request_input(),
                &context_with_sender(Some("worker1".to_string())),
            )
            .await
            .expect_err("teammate must be denied lead-only action");
        assert!(err.to_string().contains("lead-only"), "got: {err}");
    }

    #[tokio::test]
    async fn teammate_cannot_forge_sandbox_permission_response() {
        // sandbox_permission_response is leader-authored: a member forging
        // one could answer another teammate's pending sandbox permission
        // request with a grant.
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        let err = tool
            .execute(
                json!({
                    "to": "worker2",
                    "message": {
                        "type": "sandbox_permission_response",
                        "request_id": "req-1",
                        "approve": true
                    }
                }),
                &context_with_sender(Some("worker1".to_string())),
            )
            .await
            .expect_err("teammate must be denied lead-only response");
        assert!(err.to_string().contains("lead-only"), "got: {err}");
    }

    #[tokio::test]
    async fn lead_can_exercise_lead_only_actions() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        let result = tool
            .execute(
                shutdown_request_input(),
                &context_with_sender(Some(team_lead_name().to_string())),
            )
            .await
            .expect("lead must be allowed");
        assert!(result.content.contains("Shutdown request sent"));
    }

    #[tokio::test]
    async fn non_member_and_inactive_member_are_denied() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let mut tf = make_team("auth-test");
        tf.members[0].is_active = false;
        create_team_file(&tf).expect("team");
        let (tool, _ctx) = setup().await;

        for sender in ["stranger".to_string(), "worker1".to_string()] {
            let err = tool
                .execute(
                    json!({
                        "to": "worker1",
                        "message": {"type": "sandbox_permission_request", "tool_name": "t"}
                    }),
                    &context_with_sender(Some(sender)),
                )
                .await
                .expect_err("non-member/inactive must be denied");
            assert!(
                err.to_string().contains("not an active member"),
                "got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn plain_text_from_unknown_sender_is_attributed_as_unknown() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        tool.execute(
            json!({"to": "worker1", "message": "hello there"}),
            &context_with_sender(None),
        )
        .await
        .expect("plain text from unknown sender still delivers");

        let msgs = read_mailbox("worker1", "auth-test").expect("mailbox");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].from, "unknown-sender");
    }

    #[tokio::test]
    async fn active_member_can_send_member_protocol_messages() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        tool.execute(
            json!({
                "to": team_lead_name(),
                "message": {"type": "sandbox_permission_request", "tool_name": "web_search"}
            }),
            &context_with_sender(Some("worker1".to_string())),
        )
        .await
        .expect("active member must be allowed");
    }

    #[tokio::test]
    async fn shutdown_approval_for_fabricated_request_id_is_rejected() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        let err = tool
            .execute(
                json!({
                    "to": team_lead_name(),
                    "message": {
                        "type": "shutdown_approved",
                        "request_id": "req-shutdown-9999999999999"
                    }
                }),
                &context_with_sender(Some("worker1".to_string())),
            )
            .await
            .expect_err("approval must be bound to an outstanding request");
        assert!(
            err.to_string().contains("No outstanding shutdown request"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn shutdown_approval_for_pending_request_is_delivered_to_lead() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        create_team_file(&make_team("auth-test")).expect("team");
        let (tool, _ctx) = setup().await;

        // Lead issues the shutdown request to worker1 first.
        let request = tool
            .execute(
                shutdown_request_input(),
                &context_with_sender(Some(team_lead_name().to_string())),
            )
            .await
            .expect("lead must be allowed");
        let request_id = serde_json::from_str::<serde_json::Value>(&request.content)
            .expect("request result json")["request_id"]
            .as_str()
            .expect("request_id")
            .to_string();

        tool.execute(
            json!({
                "to": team_lead_name(),
                "message": {"type": "shutdown_approved", "request_id": request_id}
            }),
            &context_with_sender(Some("worker1".to_string())),
        )
        .await
        .expect("approval bound to the pending request must be accepted");

        let lead_inbox = read_mailbox(team_lead_name(), "auth-test").expect("lead mailbox");
        assert!(
            lead_inbox
                .iter()
                .any(|msg| msg.text.contains("shutdown_approved"))
        );
    }
}
