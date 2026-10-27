//! `request_user_input` — the host-question gate, split out of
//! `host_executor.rs` beside [`super::approval`].
//!
//! The tool's own `execute` body can only error: the question is not a tool
//! effect, it is a request for the host UI. The serial dispatch path routes
//! the call here instead of running the tool, mirroring the approval gate:
//!
//! 1. Parse + validate the payload ([`UserInputRequest::from_value`]) so a
//!    malformed call is answered as an `invalid_input` tool error, never
//!    rendered half-way.
//! 2. Emit [`Event::UserInputRequired`] with the wire tool-call id.
//! 3. Block on the user-input decision channel (cancel-token race, same shape
//!    as approval), matching by id.
//! 4. On `Submitted`, cross-check the answers against the request with
//!    [`UserInputResponse::validate_against`] and feed either the answers or
//!    the validation error back as the tool result. A rejected answer is a
//!    tool error for the model to react to — never a panic and never a turn
//!    abort.
//!
//! Known limitations, recorded beside the behaviour it owns:
//!
//! - Sub-agents cannot call this tool (the subagent registration chain does
//!   not register it), so one receiver serves the main turn; answers for a
//!   different id are discarded rather than parked.
//! - A host with no user-input receiver attached is refused with an
//!   actionable tool error ("ask in plain text") instead of hanging. `exec`
//!   attaches one but has no prompt, so it answers with a cancellation —
//!   the same result a REPL user gets by declining the prompt.

use codesmith_tools::{ToolError, ToolResult};

use crate::engine::approval::UserInputDecision;
use crate::engine::host_executor::HostAgentExecutor;
use crate::events::Event;
use crate::user_input::UserInputRequest;

impl HostAgentExecutor {
    /// Ask the host the model's questions and return its answer as the tool
    /// result for `tool_id`.
    pub(crate) async fn request_user_input(
        &self,
        tool_id: &str,
        input: &serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        let request = UserInputRequest::from_value(input)?;
        let Some(receiver) = &self.user_input else {
            return Ok(ToolResult::error(
                "request_user_input is not available in this host: no interactive prompt is \
                 attached. Ask the user in plain text instead.",
            ));
        };
        if let Some(tx) = &self.event_tx
            && tx
                .send(Event::UserInputRequired {
                    id: tool_id.to_string(),
                    request: request.clone(),
                })
                .await
                .is_err()
        {
            return Err(ToolError::execution_failed(
                "The UI event channel closed before the question could be shown; no answer \
                 was collected. Ask the user in plain text instead.",
            ));
        }

        let mut rx = receiver.lock().await;
        loop {
            let received = {
                let cancelled = async {
                    match self.cancel_token.as_ref() {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    biased;
                    _ = cancelled => {
                        return Err(ToolError::execution_failed(
                            "Request cancelled while awaiting user input",
                        ));
                    }
                    decision = rx.recv() => decision,
                }
            };
            match received {
                None => {
                    return Err(ToolError::execution_failed(
                        "The user-input channel closed before an answer arrived.",
                    ));
                }
                Some(UserInputDecision::Cancelled { id }) if id == tool_id => {
                    return Ok(ToolResult::success(
                        "The user cancelled the request; no answers were provided. Do not re-ask \
                         the same questions — ask in plain text only if you still need the \
                         information.",
                    ));
                }
                Some(UserInputDecision::Submitted { id, response }) if id == tool_id => {
                    return match response.validate_against(&request) {
                        Ok(()) => ToolResult::json(&response).map_err(|err| {
                            ToolError::execution_failed(format!(
                                "could not encode the user's answer: {err}"
                            ))
                        }),
                        Err(err) => Ok(ToolResult::error(format!(
                            "Invalid request_user_input response: {err}. The answer was not \
                             accepted; re-ask the affected question or continue without it."
                        ))),
                    };
                }
                // A decision addressed to another prompt: this executor has a
                // single waiter, so it can only be stale — discard it.
                Some(_) => continue,
            }
        }
    }
}
