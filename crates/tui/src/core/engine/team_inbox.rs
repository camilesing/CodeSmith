//! Team inbox dispatch handler — processes InboxDispatch from the inbox poller.

use std::collections::HashMap;

use crate::core::engine::Engine;
use crate::core::events::Event;
use crate::tools::team::{InboxDispatch, proto_shutdown_approval};

impl Engine {
    /// Handle a team inbox dispatch from the inbox poller background task.
    ///
    /// Each dispatch type is processed according to the team protocol:
    /// - TeammateMessage → inject as `<teammate-message>` XML synthetic user message
    /// - ShutdownApprovalAction → cancel teammate token, remove from team, unassign tasks
    /// - PermissionRequestPending → route to approval dialog
    /// - PlanApprovalAutoApprove → informational (already auto-approved by poller)
    /// - IdleNotificationInfo → informational display
    /// - ModeSetRequestAction → informational (teammate applies locally)
    /// - TeamPermissionUpdateInfo → informational display
    pub async fn handle_team_inbox_dispatch(&mut self, dispatch: InboxDispatch) {
        match dispatch {
            InboxDispatch::TeammateMessage { from, text, summary } => {
                let xml = format!(
                    "<teammate-message teammate_id=\"{from}\" summary=\"{}\">\n{text}\n</teammate-message>",
                    summary.unwrap_or_default()
                );
                // Inject as synthetic user message.
                let msg = crate::models::Message {
                    role: "user".to_string(),
                    content: vec![crate::models::ContentBlock::Text {
                        text: xml,
                        cache_control: None,
                    }],
                };
                self.session.messages.push(msg);
                self.session.rebuild_working_set();
                let _ = self.emit_session_updated().await;
            }
            InboxDispatch::ShutdownApprovalAction { from, request_id, backend_type: _ } => {
                // Find and cancel the teammate's CancellationToken.
                // Remove from team file and unassign tasks.
                if let Some(shared_tc) = self.config.team_context.as_ref() {
                    let team_ctx = shared_tc.lock().await;
                    if let Some(ctx) = team_ctx.as_ref() {
                        let _ = proto_shutdown_approval(
                            &request_id, &from, &ctx.team_name, &HashMap::new(),
                        );
                    }
                }
                let msg = format!("Teammate {} has shut down (request {}).", from, request_id);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::ShutdownRejectionInfo { from, request_id, reason } => {
                let msg = format!("Teammate {} rejected shutdown (request {}): {}", from, request_id, reason);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::PermissionRequestPending { request_id, agent_id, tool_name, .. } => {
                let msg = format!("{} needs permission for {}", agent_id, tool_name);
                let _ = self.tx_event.send(Event::status(msg)).await;
                // TODO: Route to approval dialog when UI supports it.
            }
            InboxDispatch::PermissionResponseReceived { request_id, subtype, .. } => {
                let msg = format!("Permission response for {}: {}", request_id, subtype);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::PlanApprovalAutoApprove { from, request_id } => {
                let msg = format!("Plan auto-approved for {} (request {})", from, request_id);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::IdleNotificationInfo { from, summary, .. } => {
                let msg = format!("Teammate {} is idle. {}", from, summary.unwrap_or_default());
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::ModeSetRequestAction { from, permission_mode } => {
                let msg = format!("Mode set request from {}: {}", from, permission_mode);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::TeamPermissionUpdateInfo { from, allowed_tools, .. } => {
                let msg = format!("Permission update from {}: allowed {}", from, allowed_tools.join(", "));
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            // Shutdown requests from teammates are rare on leader inbox —
            // normally teammates receive shutdown requests, not send them here.
            // Handle defensively by logging.
            InboxDispatch::ShutdownRequestMessage { from, request_id, reason } => {
                let msg = format!("Shutdown request from {} (request {}): {:?}", from, request_id, reason);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
        }
    }
}