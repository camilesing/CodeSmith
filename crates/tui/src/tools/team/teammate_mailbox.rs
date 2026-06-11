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

/// Idle reason variants — why a teammate went idle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IdleReason {
    /// Turn completed normally, available for new work.
    Available,
    /// Interrupted by external signal (cancel, etc).
    Interrupted,
    /// Failed during turn execution.
    Failed,
}

/// Structured protocol messages carried inside TeammateMessage.text as JSON.
/// Parsed when `is_structured_protocol_message()` returns true.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        backend_type: Option<String>,
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
        idle_reason: Option<IdleReason>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed_status: Option<String>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
        timestamp: String,
    },
    /// Leader pushes permission rule updates to a worker.
    TeamPermissionUpdate {
        from: String,
        allowed_tools: Vec<String>,
        denied_tools: Vec<String>,
        timestamp: String,
    },
    /// Leader changes a worker's permission mode. Only valid from team-lead.
    ModeSetRequest {
        from: String,
        permission_mode: String,
        timestamp: String,
    },
    /// Worker requests network access permission (sandbox).
    SandboxPermissionRequest {
        request_id: String,
        agent_id: String,
        tool_name: String,
        tool_use_id: String,
        domain: String,
        description: String,
    },
    /// Leader responds to sandbox permission request.
    SandboxPermissionResponse {
        request_id: String,
        subtype: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
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
                    | "team_permission_update"
                    | "mode_set_request"
                    | "sandbox_permission_request"
                    | "sandbox_permission_response"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{lock_test_env, ScopedCodeWhaleHome};
    use crate::tools::team::team_file::{create_team_file, TeamFile, TeamMember, format_lead_agent_id, team_lead_name};

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

    fn make_message(from: &str, text: &str) -> TeammateMessage {
        TeammateMessage {
            from: from.to_string(),
            text: text.to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            read: false,
            color: None,
            summary: None,
        }
    }

    #[test]
    fn is_structured_protocol_message_true_for_known_types() {
        let types = [
            "shutdown_request", "shutdown_approved", "shutdown_rejected",
            "idle_notification", "task_assignment",
            "permission_request", "permission_response",
            "plan_approval_request", "plan_approval_response",
            "team_permission_update", "mode_set_request",
            "sandbox_permission_request", "sandbox_permission_response",
        ];
        for t in types {
            let json = format!("{{\"type\":\"{t}\"}}");
            assert!(is_structured_protocol_message(&json), "expected true for {t}");
        }
    }

    #[test]
    fn is_structured_protocol_message_false_for_plain_text() {
        assert!(!is_structured_protocol_message("hello teammate"));
        assert!(!is_structured_protocol_message(""));
    }

    #[test]
    fn is_structured_protocol_message_false_for_json_without_type() {
        assert!(!is_structured_protocol_message("{\"from\":\"x\"}"));
    }

    #[test]
    fn parse_structured_protocol_roundtrips_shutdown_request() {
        let msg = StructuredProtocolMessage::ShutdownRequest {
            request_id: "req-1".to_string(),
            from: "leader".to_string(),
            reason: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        };
        let text = serde_json::to_string(&msg).expect("serialize");
        let parsed = parse_structured_protocol(&text).expect("parse");
        assert!(matches!(parsed, StructuredProtocolMessage::ShutdownRequest { .. }));
    }

    #[test]
    fn parse_structured_protocol_returns_none_for_garbage() {
        assert_eq!(parse_structured_protocol("{bad json"), None);
        assert_eq!(parse_structured_protocol("plain text"), None);
    }

    #[test]
    fn read_mailbox_returns_empty_for_missing_file() {
        let _guard = lock_test_env();
        let _home = ScopedCodeWhaleHome::new();
        create_team_file(&make_team_file("mb-test")).expect("team");

        let msgs = read_mailbox("nonexistent-agent", "mb-test").expect("read");
        assert!(msgs.is_empty());
    }

    #[test]
    fn write_to_mailbox_creates_and_appends() {
        let _guard = lock_test_env();
        let _home = ScopedCodeWhaleHome::new();
        create_team_file(&make_team_file("mb-write")).expect("team");

        write_to_mailbox("worker1", "mb-write", make_message("leader", "hello")).expect("write1");
        write_to_mailbox("worker1", "mb-write", make_message("leader", "world")).expect("write2");

        let msgs = read_mailbox("worker1", "mb-write").expect("read");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text, "hello");
        assert_eq!(msgs[1].text, "world");
    }

    #[test]
    fn write_to_mailbox_concurrent_no_message_loss() {
        let _guard = lock_test_env();
        let _home = ScopedCodeWhaleHome::new();
        create_team_file(&make_team_file("mb-concurrent")).expect("team");

        let team_name = "mb-concurrent".to_string();
        let threads: Vec<std::thread::JoinHandle<()>> = (0..5)
            .map(|i| {
                let tn = team_name.clone();
                std::thread::spawn(move || {
                    let msg = TeammateMessage {
                        from: format!("t{i}"),
                        text: format!("msg-{i}"),
                        timestamp: "2026-01-01T00:00:00Z".to_string(),
                        read: false,
                        color: None,
                        summary: None,
                    };
                    write_to_mailbox("target", &tn, msg).expect("write");
                })
            })
            .collect();
        for t in threads {
            t.join().expect("thread join");
        }

        let msgs = read_mailbox("target", "mb-concurrent").expect("read");
        assert_eq!(msgs.len(), 5, "no messages lost in concurrent writes");
    }

    #[test]
    fn mark_messages_as_read_marks_all() {
        let _guard = lock_test_env();
        let _home = ScopedCodeWhaleHome::new();
        create_team_file(&make_team_file("mb-read")).expect("team");

        write_to_mailbox("worker1", "mb-read", make_message("leader", "msg1")).expect("write");
        mark_messages_as_read("worker1", "mb-read").expect("mark read");

        let msgs = read_mailbox("worker1", "mb-read").expect("read");
        assert!(msgs.iter().all(|m| m.read));
    }

    #[test]
    fn clear_mailbox_empties_inbox() {
        let _guard = lock_test_env();
        let _home = ScopedCodeWhaleHome::new();
        create_team_file(&make_team_file("mb-clear")).expect("team");

        write_to_mailbox("worker1", "mb-clear", make_message("leader", "msg1")).expect("write");
        clear_mailbox("worker1", "mb-clear").expect("clear");

        let msgs = read_mailbox("worker1", "mb-clear").expect("read");
        assert!(msgs.is_empty());
    }
}