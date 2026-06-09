//! Teammate lifecycle — in-process teammate loop with idle/shutdown protocol.
//!
//! Each teammate runs its own mini-engine loop as a tokio task. The state
//! machine handles: Initializing → Active → Idle → (new message → Active)
//! or (shutdown_request → ShutdownPending → Terminated/Idle).
//!
//! The actual LLM call per iteration will delegate to the existing
//! `SubAgentRuntime` / `run_subagent` infrastructure once a per-step
//! API is available. For now, the coordination and state machine are
//! complete; the LLM execution step is marked as a TODO placeholder.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::tools::task_v2::SharedTaskV2Manager;
use crate::tools::team::{
    TeammateMessage, StructuredProtocolMessage,
    read_unread_messages, mark_messages_as_read, parse_structured_protocol,
    is_structured_protocol_message, write_to_mailbox, team_lead_name,
};

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
}

/// Process unread inbox messages, separating protocol messages from plain text.
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
        idle_reason: None,
        summary,
        completed_task_id,
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
        summary: Some(if approved { "shutdown approved" } else { "shutdown rejected" }.to_string()),
    };

    write_to_mailbox(team_lead_name(), team_name, msg)?;

    Ok(if approved {
        TeammateState::ShutdownApproved
    } else {
        TeammateState::ShutdownRejected
    })
}

/// Run the teammate's continuous prompt loop.
///
/// State machine:
/// 1. Check cancel_token → Terminated if cancelled
/// 2. Read inbox → handle protocol (shutdown/task assignment) → inject text
/// 3. Execute one LLM step via SubAgentRuntime (TODO: per-step API)
/// 4. If no tool calls → Idle, send IdleNotification
/// 5. Loop back to step 1
///
/// The LLM execution step currently delegates to the existing `run_subagent`
/// as a one-shot call per iteration. A future per-step API will allow
/// tighter integration with the teammate's message history and compaction.
pub async fn run_teammate_loop(mut rt: TeammateRuntime) -> TeammateResult {
    let mut state = TeammateState::Initializing;
    let mut current_prompt = rt.initial_prompt.clone();

    loop {
        // Check cancellation.
        if rt.cancel_token.is_cancelled() {
            state = TeammateState::Terminated;
            break;
        }

        // Drain inbox messages.
        let (protocol_msgs, text_msgs) = match process_inbox_messages(
            &rt.agent_name, &rt.team_name,
        ) {
            Ok((p, t)) => (p, t),
            Err(_) => (Vec::new(), Vec::new()),
        };

        // Handle protocol messages.
        for protocol in &protocol_msgs {
            match protocol {
                StructuredProtocolMessage::ShutdownRequest { request_id, .. } => {
                    match handle_shutdown_request(
                        request_id,
                        &rt.agent_name,
                        &rt.team_name,
                    ) {
                        Ok(TeammateState::ShutdownApproved) => {
                            state = TeammateState::ShutdownApproved;
                            break;
                        }
                        Ok(TeammateState::ShutdownRejected) => {
                            state = TeammateState::Idle;
                        }
                        _ => {}
                    }
                }
                StructuredProtocolMessage::TaskAssignment { subject, description, .. } => {
                    current_prompt = format!(
                        "New task assigned: {}\n\n{}",
                        subject, description
                    );
                    state = TeammateState::Active;
                }
                _ => {}
            }
        }

        if state == TeammateState::ShutdownApproved || state == TeammateState::Terminated {
            break;
        }

        // Build prompt from text messages.
        if !text_msgs.is_empty() {
            let text_content: Vec<String> = text_msgs
                .iter()
                .map(|m| format!("Message from {}: {}", m.from, m.text))
                .collect();
            current_prompt = text_content.join("\n\n");
            state = TeammateState::Active;
        }

        // If idle with no messages, poll and wait.
        if state == TeammateState::Idle && text_msgs.is_empty() && protocol_msgs.is_empty() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        // TODO: Execute one LLM step here using SubAgentRuntime.
        // This will be implemented once a per-step API is available
        // that accepts an existing message history + prompt and returns
        // tool calls + assistant text for a single turn.
        //
        // For now, mark the step as complete and go idle.
        state = TeammateState::Idle;

        let _ = send_idle_notification(
            &rt.agent_name,
            &rt.team_name,
            Some("step placeholder".to_string()),
            None,
        );

        // Reset prompt for next iteration.
        current_prompt.clear();
    }

    TeammateResult {
        agent_id: rt.agent_id.clone(),
        agent_name: rt.agent_name.clone(),
        final_state: state,
        summary: None,
    }
}

/// Poll the team leader's inbox and return text messages for injection
/// into the leader's conversation as synthetic user messages.
pub fn poll_leader_inbox(team_name: &str) -> anyhow::Result<Vec<String>> {
    let (protocol_msgs, text_msgs) = process_inbox_messages(
        team_lead_name(), team_name,
    )?;

    let protocol_texts: Vec<String> = protocol_msgs.iter().map(|p| {
        match p {
            StructuredProtocolMessage::IdleNotification { from, summary, .. } => {
                format!("Teammate {} is idle. {}", from, summary.as_deref().unwrap_or(""))
            }
            StructuredProtocolMessage::ShutdownApproved { from, .. } => {
                format!("Teammate {} approved shutdown.", from)
            }
            StructuredProtocolMessage::ShutdownRejected { from, reason, .. } => {
                format!("Teammate {} rejected shutdown: {}", from, reason)
            }
            _ => format!("Protocol message from teammate"),
        }
    }).collect();

    let text_contents: Vec<String> = text_msgs.iter()
        .map(|m| format!("Message from {}: {}", m.from, m.text))
        .collect();

    Ok([protocol_texts, text_contents].concat())
}