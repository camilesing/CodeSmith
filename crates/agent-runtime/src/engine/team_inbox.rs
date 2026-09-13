//! Team inbox dispatch handler — processes InboxDispatch from the inbox poller.

use crate::engine::Engine;
use crate::events::Event;
use crate::team::{InboxDispatch, handle_shutdown_approval};
use crate::utils::{defuse_closing_tag, escape_prompt_attr};

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
            InboxDispatch::TeammateMessage {
                from,
                text,
                summary,
            } => {
                // `from` / `summary` land inside double-quoted attributes and
                // `text` inside the element body — all three are teammate-
                // supplied, so escape the attributes and defuse any
                // `</teammate-message` sequence that would close the frame
                // early and smuggle trailing content outside it.
                let xml = format!(
                    "<teammate-message teammate_id=\"{}\" summary=\"{}\">\n{}\n</teammate-message>",
                    escape_prompt_attr(&from),
                    escape_prompt_attr(summary.as_deref().unwrap_or_default()),
                    defuse_closing_tag(&text, "teammate-message")
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
            InboxDispatch::ShutdownApprovalAction {
                from,
                request_id,
                backend_type: _,
            } => {
                // Find and cancel the teammate's CancellationToken.
                // Remove from team file and unassign tasks.
                if let Some(shared_tc) = self.config.team_context.as_ref() {
                    let mut team_ctx = shared_tc.lock().await;
                    if let Some(ctx) = team_ctx.as_mut() {
                        let hook_request_id = request_id.clone();
                        let hook_from = from.clone();
                        let team_name = ctx.team_name.clone();
                        let cancel_tokens = ctx.teammate_cancel_tokens.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            handle_shutdown_approval(
                                &hook_request_id,
                                &hook_from,
                                &team_name,
                                &cancel_tokens,
                            )
                        })
                        .await;
                        ctx.teammate_cancel_tokens.remove(&from);
                        ctx.teammates.retain(|_, info| info.name != from);
                    }
                }
                let msg = format!("Teammate {} has shut down (request {}).", from, request_id);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::ShutdownRejectionInfo {
                from,
                request_id,
                reason,
            } => {
                let msg = format!(
                    "Teammate {} rejected shutdown (request {}): {}",
                    from, request_id, reason
                );
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::PermissionRequestPending {
                agent_id,
                tool_name,
                ..
            } => {
                let msg = format!("{} needs permission for {}", agent_id, tool_name);
                let _ = self.tx_event.send(Event::status(msg)).await;
                // TODO: Route to approval dialog when UI supports it.
            }
            InboxDispatch::PermissionResponseReceived {
                request_id,
                subtype,
                ..
            } => {
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
            InboxDispatch::ModeSetRequestAction {
                from,
                permission_mode,
            } => {
                let msg = format!("Mode set request from {}: {}", from, permission_mode);
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            InboxDispatch::TeamPermissionUpdateInfo {
                from,
                allowed_tools,
                ..
            } => {
                let msg = format!(
                    "Permission update from {}: allowed {}",
                    from,
                    allowed_tools.join(", ")
                );
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
            // Shutdown requests from teammates are rare on leader inbox —
            // normally teammates receive shutdown requests, not send them here.
            // Handle defensively by logging.
            InboxDispatch::ShutdownRequestMessage {
                from,
                request_id,
                reason,
            } => {
                let msg = format!(
                    "Shutdown request from {} (request {}): {:?}",
                    from, request_id, reason
                );
                let _ = self.tx_event.send(Event::status(msg)).await;
            }
        }
    }
}
