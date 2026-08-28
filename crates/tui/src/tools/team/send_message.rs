//! SendMessageTool — inter-teammate messaging with file-based mailbox delivery.
//!
//! Supports plain text DMs, broadcast (to: "*"), and structured protocol
//! messages dispatched to dedicated handlers from protocol_handlers.rs.

use async_trait::async_trait;
use serde_json::json;

use crate::features::Feature;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::team::protocol_handlers::{
    handle_plan_approval_auto_approve, handle_plan_approval_rejection, handle_shutdown_rejection,
    handle_shutdown_request,
};
use crate::tools::team::{
    SharedTeamContext, TeammateMessage, read_team_file, team_lead_name, write_to_mailbox,
};

pub struct SendMessageTool {
    team_context: SharedTeamContext,
}

impl SendMessageTool {
    pub fn new(team_context: SharedTeamContext) -> Self {
        Self { team_context }
    }
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
                    context
                        .runtime
                        .team_sender
                        .clone()
                        .unwrap_or_else(|| team_lead_name().to_string()),
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
                // Write approval protocol to teammate mailbox (informational).
                // Actual cancellation happens via inbox poller on leader side.
                let now = chrono::Utc::now().to_rfc3339();
                let protocol_text = serde_json::to_string(&json!({
                    "type": "shutdown_approved",
                    "request_id": request_id,
                    "from": sender_name,
                    "timestamp": now,
                }))
                .map_err(|e| ToolError::execution_failed(format!("Serialize failed: {}", e)))?;
                write_to_mailbox(
                    &recipient,
                    &team_name,
                    TeammateMessage {
                        from: sender_name.clone(),
                        text: protocol_text,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        read: false,
                        color: None,
                        summary: Some("shutdown approved".to_string()),
                    },
                )
                .map_err(|e| ToolError::execution_failed(format!("Delivery failed: {}", e)))?;
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
                let now = chrono::Utc::now().to_rfc3339();
                let protocol_text = serde_json::to_string(&json!({
                    "type": "mode_set_request",
                    "from": sender_name,
                    "permission_mode": permission_mode,
                    "timestamp": now,
                }))
                .map_err(|e| ToolError::execution_failed(format!("Serialize failed: {}", e)))?;
                write_to_mailbox(
                    &recipient,
                    &team_name,
                    TeammateMessage {
                        from: sender_name.clone(),
                        text: protocol_text,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        read: false,
                        color: None,
                        summary: Some(format!("mode set: {}", permission_mode)),
                    },
                )
                .map_err(|e| ToolError::execution_failed(format!("Delivery failed: {}", e)))?;
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
                let now = chrono::Utc::now().to_rfc3339();
                let protocol_text = serde_json::to_string(&json!({
                    "type": "team_permission_update",
                    "from": sender_name,
                    "allowed_tools": allowed_tools,
                    "denied_tools": denied_tools,
                    "timestamp": now,
                }))
                .map_err(|e| ToolError::execution_failed(format!("Serialize failed: {}", e)))?;
                write_to_mailbox(
                    &recipient,
                    &team_name,
                    TeammateMessage {
                        from: sender_name.clone(),
                        text: protocol_text,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        read: false,
                        color: None,
                        summary: Some("permission update".to_string()),
                    },
                )
                .map_err(|e| ToolError::execution_failed(format!("Delivery failed: {}", e)))?;
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
                forwarded.insert(
                    "timestamp".to_string(),
                    json!(chrono::Utc::now().to_rfc3339()),
                );
                let protocol_text = serde_json::to_string(&forwarded)
                    .map_err(|e| ToolError::execution_failed(format!("Serialize failed: {}", e)))?;
                write_to_mailbox(
                    &recipient,
                    &team_name,
                    TeammateMessage {
                        from: sender_name.clone(),
                        text: protocol_text,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        read: false,
                        color: None,
                        summary: Some(format!("sandbox permission: {}", type_str)),
                    },
                )
                .map_err(|e| ToolError::execution_failed(format!("Delivery failed: {}", e)))?;
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
            // Broadcast to all non-lead teammates.
            let team_file = read_team_file(team_name).map_err(|e| {
                ToolError::execution_failed(format!("Failed to read team file: {}", e))
            })?;

            let mut delivered = Vec::new();
            for member in &team_file.members {
                if member.name != team_lead_name() && member.is_active {
                    write_to_mailbox(&member.name, team_name, team_msg.clone()).map_err(|e| {
                        ToolError::execution_failed(format!(
                            "Failed to deliver to {}: {}",
                            member.name, e
                        ))
                    })?;
                    delivered.push(member.name.clone());
                }
            }

            return ToolResult::json(&json!({"broadcast": true, "delivered_to": delivered}))
                .map_err(|e| ToolError::execution_failed(e.to_string()));
        }

        // Single recipient DM.
        write_to_mailbox(recipient, team_name, team_msg).map_err(|e| {
            ToolError::execution_failed(format!("Failed to deliver to {}: {}", recipient, e))
        })?;

        ToolResult::json(&json!({"delivered_to": recipient}))
            .map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}
