//! File-based teammate mailbox — inter-agent message delivery via inbox files.
//!
//! Inbox files live at `~/.codewhale/teams/{sanitized_name}/inboxes/{agent_name}.json`.
//! Each inbox is a JSON array of TeammateMessage entries. Concurrent writes
//! use flock via a separate `.lock` file (same pattern as TaskV2Manager).

use std::fs;
use std::path::PathBuf;

use fd_lock::RwLock;
use serde::{Deserialize, Serialize};

use super::team_file::{team_dir, sanitize_name};

/// A single message in a teammate's file-based inbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateMessage {
    /// Sender's agent name.
    pub from: String,
    /// Message body — plain text or JSON-encoded StructuredProtocolMessage.
    pub text: String,
    /// ISO 8601 timestamp.
    #[serde(default = "default_timestamp")]
    pub timestamp: String,
    /// Whether the recipient has read this message.
    #[serde(default)]
    pub read: bool,
    /// Optional color for UI rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Optional short summary for preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

fn default_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Structured protocol messages carried inside TeammateMessage.text as JSON.
/// Parsed when `is_structured_protocol_message()` returns true.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StructuredProtocolMessage {
    /// Leader requests teammate shutdown; model decides approve/reject.
    ShutdownRequest {
        request_id: String,
        from: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        timestamp: String,
    },
    /// Teammate approved shutdown request.
    ShutdownApproved {
        request_id: String,
        from: String,
        timestamp: String,
    },
    /// Teammate rejected shutdown request.
    ShutdownRejected {
        request_id: String,
        from: String,
        reason: String,
        timestamp: String,
    },
    /// Teammate finished its turn and is waiting for next prompt.
    IdleNotification {
        from: String,
        timestamp: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        idle_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_task_id: Option<String>,
    },
    /// Leader assigns a task to a teammate.
    TaskAssignment {
        task_id: String,
        subject: String,
        description: String,
        assigned_by: String,
        timestamp: String,
    },
    /// Teammate requests leader permission for a tool call.
    PermissionRequest {
        request_id: String,
        agent_id: String,
        tool_name: String,
        tool_use_id: String,
        description: String,
    },
    /// Leader responds to a permission request.
    PermissionResponse {
        request_id: String,
        subtype: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Teammate submits a plan for leader approval.
    PlanApprovalRequest {
        from: String,
        timestamp: String,
        plan_file_path: String,
        request_id: String,
    },
    /// Leader responds to a plan approval request.
    PlanApprovalResponse {
        request_id: String,
        approved: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
        timestamp: String,
    },
}

/// Check whether a message text contains a structured protocol payload.
/// True when the text starts with `{` and contains a `"type"` key with a
/// known protocol message type value.
pub fn is_structured_protocol_message(text: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(type_val) = val.get("type").and_then(|v| v.as_str()) {
            return matches!(
                type_val,
                "shutdown_request"
                    | "shutdown_approved"
                    | "shutdown_rejected"
                    | "idle_notification"
                    | "task_assignment"
                    | "permission_request"
                    | "permission_response"
                    | "plan_approval_request"
                    | "plan_approval_response"
            );
        }
    }
    false
}

/// Parse a structured protocol message from text. Returns None if text
/// is not a valid protocol JSON.
pub fn parse_structured_protocol(text: &str) -> Option<StructuredProtocolMessage> {
    if !is_structured_protocol_message(text) {
        return None;
    }
    serde_json::from_str(text.trim()).ok()
}

/// Path to a teammate's inbox file.
fn inbox_path(agent_name: &str, team_name: &str) -> anyhow::Result<PathBuf> {
    let dir = team_dir(team_name)?;
    Ok(dir.join("inboxes").join(format!("{}.json", sanitize_name(agent_name))))
}

/// Path to the flock lock file for a teammate's inbox.
fn inbox_lock_path(agent_name: &str, team_name: &str) -> anyhow::Result<PathBuf> {
    Ok(inbox_path(agent_name, team_name)?.with_extension("lock"))
}

/// Acquire exclusive flock on the inbox lock file. The guard holds the lock
/// for the duration of the mutation operation (same pattern as TaskV2).
fn acquire_inbox_lock(agent_name: &str, team_name: &str) -> anyhow::Result<RwLock<fs::File>> {
    let lock_path = inbox_lock_path(agent_name, team_name)?;
    // Ensure lock file exists.
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    let file = fs::File::open(&lock_path)?;
    Ok(RwLock::new(file))
}

/// Read all messages from a teammate's inbox file.
pub fn read_mailbox(agent_name: &str, team_name: &str) -> anyhow::Result<Vec<TeammateMessage>> {
    let path = inbox_path(agent_name, team_name)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let json = fs::read_to_string(&path)?;
    let messages: Vec<TeammateMessage> = serde_json::from_str(&json)?;
    Ok(messages)
}

/// Read only unread messages from a teammate's inbox.
pub fn read_unread_messages(agent_name: &str, team_name: &str) -> anyhow::Result<Vec<TeammateMessage>> {
    let messages = read_mailbox(agent_name, team_name)?;
    Ok(messages.into_iter().filter(|m| !m.read).collect())
}

/// Write a message to a teammate's inbox file with flock-based locking.
pub fn write_to_mailbox(
    recipient_name: &str,
    team_name: &str,
    message: TeammateMessage,
) -> anyhow::Result<()> {
    let path = inbox_path(recipient_name, team_name)?;

    // Ensure inbox file exists.
    if !path.exists() {
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent)?;
        fs::write(&path, "[]")?;
    }

    // Acquire exclusive lock via separate .lock file.
    let mut lock = acquire_inbox_lock(recipient_name, team_name)?;
    let _guard = lock.write()?;

    // Read current messages, append new one, write back.
    let existing: Vec<TeammateMessage> = {
        let content = fs::read_to_string(&path)?;
        serde_json::from_str(&content).unwrap_or_default()
    };
    let mut updated = existing;
    updated.push(message);
    let json = serde_json::to_string(&updated)?;
    fs::write(&path, json)?;

    Ok(())
}

/// Mark all messages in a teammate's inbox as read.
pub fn mark_messages_as_read(agent_name: &str, team_name: &str) -> anyhow::Result<()> {
    let path = inbox_path(agent_name, team_name)?;
    if !path.exists() {
        return Ok(());
    }

    let mut lock = acquire_inbox_lock(agent_name, team_name)?;
    let _guard = lock.write()?;

    let existing: Vec<TeammateMessage> = {
        let content = fs::read_to_string(&path)?;
        serde_json::from_str(&content).unwrap_or_default()
    };
    let updated: Vec<TeammateMessage> = existing
        .into_iter()
        .map(|mut m| {
            m.read = true;
            m
        })
        .collect();
    let json = serde_json::to_string(&updated)?;
    fs::write(&path, json)?;

    Ok(())
}

/// Clear a teammate's inbox (set to empty array).
pub fn clear_mailbox(agent_name: &str, team_name: &str) -> anyhow::Result<()> {
    let path = inbox_path(agent_name, team_name)?;
    if path.exists() {
        fs::write(&path, "[]")?;
    }
    Ok(())
}