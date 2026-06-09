//! SendMessageTool — inter-teammate messaging with file-based mailbox delivery.
//!
//! Supports plain text DMs, broadcast (to: "*"), and structured protocol
//! messages (shutdown_request, plan_approval_response, etc.).

use async_trait::async_trait;
use serde_json::json;

use crate::features::Feature;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::team::{
    SharedTeamContext, TeammateMessage, team_lead_name,
    read_team_file, write_to_mailbox,
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
         (shutdown_request, shutdown_response, plan_approval_response). \
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
                                        "plan_approval_response"
                                    ]
                                },
                                "request_id": { "type": "string" },
                                "approve": { "type": "boolean" },
                                "reason": { "type": "string" },
                                "feedback": { "type": "string" }
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

    fn supports_parallel(&self) -> bool { false }
    fn is_read_only(&self) -> bool { false }

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
                Some(ctx) => (ctx.team_name.clone(), team_lead_name().to_string()),
                None => return Err(ToolError::invalid_input("Not in a team. Cannot send messages.")),
            }
        };

        // Determine message text: plain string or structured protocol JSON.
        let message_val = input.get("message").ok_or_else(|| ToolError::missing_field("message"))?;
        let text = if message_val.is_string() {
            message_val.as_str().unwrap().to_string()
        } else {
            let mut obj = message_val.as_object().cloned().unwrap_or_default();
            obj.insert("from".to_string(), json!(sender_name));
            obj.insert("timestamp".to_string(), json!(chrono::Utc::now().to_rfc3339()));

            // Add request_id for shutdown_request if missing.
            if let Some(type_val) = obj.get("type").and_then(|v| v.as_str()) {
                if type_val == "shutdown_request" && !obj.contains_key("request_id") {
                    obj.insert("request_id".to_string(), json!(format!("req-{}", chrono::Utc::now().timestamp_millis())));
                }
            }

            serde_json::to_string(&obj)
                .map_err(|e| ToolError::execution_failed(format!("Failed to serialize protocol message: {}", e)))?
        };

        let team_msg = TeammateMessage {
            from: sender_name.clone(),
            text,
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            color: None,
            summary,
        };

        if recipient == "*" {
            // Broadcast to all non-lead teammates.
            let team_file = read_team_file(&team_name)
                .map_err(|e| ToolError::execution_failed(format!("Failed to read team file: {}", e)))?;

            let mut delivered = Vec::new();
            for member in &team_file.members {
                if member.name != team_lead_name() && member.is_active {
                    write_to_mailbox(&member.name, &team_name, team_msg.clone())
                        .map_err(|e| ToolError::execution_failed(format!(
                            "Failed to deliver to {}: {}", member.name, e
                        )))?;
                    delivered.push(member.name.clone());
                }
            }

            let result = json!({"broadcast": true, "delivered_to": delivered});
            return ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()));
        }

        // Single recipient DM.
        write_to_mailbox(&recipient, &team_name, team_msg)
            .map_err(|e| ToolError::execution_failed(format!(
                "Failed to deliver to {}: {}", recipient, e
            )))?;

        let result = json!({"delivered_to": recipient});
        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}