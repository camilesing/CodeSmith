//! Idle wake — the op loop's synthesized next turn (round 10 TODO-69).
//!
//! The seam's three roles:
//!
//! - **signal** — [`crate::subagent::SubAgentCompletionSender::send`] pokes
//!   the engine's `Arc<Notify>` after enqueueing a completion. The
//!   completion stays queued in the parent-completion channel; this is a
//!   side-band signal, not a second queue (no inbox framework). A poke that
//!   lands while a turn is running leaves a stored permit; the next idle
//!   wait consumes it and re-checks the channel, where the running turn's
//!   own drains have usually already consumed the completion — the wake is
//!   then a silent no-op.
//! - **decision** — [`Engine::idle_wake_op`]: synthesize only when the host
//!   opted in (`EngineConfig::idle_wake`), no op is waiting (a user-queued
//!   message must never be delayed by a synthesized turn; its own turn's
//!   pre-request drain delivers the completions anyway), a client exists,
//!   and completions are actually parked. Parameters come from the last
//!   real `Op::SendMessage` snapshot, so a synthesized turn runs under the
//!   session's current mode / model / approval posture.
//! - **delivery** — the synthesized op goes through the *same*
//!   `Op::SendMessage` arm as a user message; the executor's pre-request
//!   drain (`HostAgentExecutor::drain_subagent_completions`, seam 1)
//!   delivers the parked completions as
//!   `<codesmith:runtime_event kind="subagent_completion">` user messages
//!   ahead of the first request. This module never touches the completion
//!   payload itself.
//!
//! ## Known limitations
//!
//! - A host that turns `idle_wake` on must drain the event channel while
//!   idle (REPL `unattended::IdlePump`, runtime-API idle monitor). A host
//!   that cannot (exec exits after one turn; embeds; ACP until its
//!   prompt-outcome resolution is turn-scoped) leaves it off — the event
//!   channel's intentional `MessageDelta` backpressure would otherwise
//!   stall a synthesized turn mid-stream once 256 events queue up.
//! - The synthesized turn's wake notice is a real user-role message (it
//!   enters the transcript and the session journal, per "model-visible
//!   means logged"); it is NOT prefix-cache-frozen content.

use super::RUNTIME_EVENT_PREFIX;
use crate::engine::Engine;
use crate::ops::Op;

impl Engine {
    /// Decide and build the idle-wake turn. `None` = no synthesis: returns
    /// the op loop to its idle wait (the `Notify` permit was consumed, so
    /// this cannot spin). Called only from the op loop's idle select arm —
    /// the engine is definitionally not running a turn here.
    pub(crate) async fn idle_wake_op(&self) -> Option<Op> {
        if !self.config.idle_wake || !self.rx_op.is_empty() {
            return None;
        }
        // No client, no turn — a synthesized op would only fail loudly.
        self.llm_client.as_ref()?;
        let parked = self.rx_subagent_completion.lock().await.len();
        if parked == 0 {
            return None;
        }
        let mut op = self.last_send_op.clone()?;
        if let Op::SendMessage { content, .. } = &mut op {
            *content = idle_wake_message(parked);
        }
        tracing::debug!(
            target: "codesmith::engine",
            "idle wake: synthesizing turn with {parked} parked sub-agent completion(s)"
        );
        Some(op)
    }
}

/// The user-role content of the synthesized turn. Framed as an internal
/// runtime event (same envelope as the completion sentinels the drain
/// appends right after it) so the model knows this is coordination
/// scaffolding, not fresh user input. `parked` is the completion count the
/// drain is about to deliver.
pub(crate) fn idle_wake_message(parked: usize) -> String {
    let plural = if parked == 1 { "" } else { "s" };
    format!(
        "{RUNTIME_EVENT_PREFIX} kind=\"idle_wake\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. While the session was idle, {parked} \
background sub-agent completion{plural} landed; they follow this message as \
subagent_completion runtime events. Use them to continue coordinating the task — \
read results via `agent_eval` where needed, decide follow-up work, and report progress \
to the user. Do not explain the sentinel protocol and do not quote the raw XML unless \
the user explicitly asks to debug sub-agent internals.\n\
</codesmith:runtime_event>",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_message_is_internal_and_names_the_count() {
        let text = idle_wake_message(2);
        assert!(text.contains("kind=\"idle_wake\""));
        assert!(text.contains("internal runtime event, not user input"));
        assert!(text.contains("2 background sub-agent completions landed"));
        assert!(text.ends_with("</codesmith:runtime_event>"));
    }
}
