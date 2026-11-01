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
use crate::engine::{Engine, TurnEnd};
use crate::events::TurnOutcomeStatus;
use crate::ops::Op;
use crate::tool_state::goal::{
    GoalSnapshot, MAX_GOAL_ROUNDS, RoundDecision, render_continuation_prompt,
};

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

impl Engine {
    /// Goal round driver (round 10 TODO-71): after a cleanly-ended
    /// `SendMessage` turn with an active goal, queue the next round as
    /// another synthesized turn — or pause the goal when the token budget
    /// or round cap is exhausted. Same wake seam as the idle wake: the
    /// synthesized op rides the ordinary `Op::SendMessage` path, so it is
    /// journaled, pumped, and approval-gated like any turn. Deliberately
    /// only the `SendMessage` arm (not `/edit`): an edited turn's clean end
    /// does not auto-drive.
    pub(crate) async fn goal_round_after_turn(&self, end: TurnEnd) {
        if !self.config.idle_wake || end.status != TurnOutcomeStatus::Completed {
            return;
        }
        // Real ops always outrank a synthesized round; and parked
        // completions win over the goal — their wake turn re-runs this
        // check at its own end.
        if !self.rx_op.is_empty() || self.llm_client.is_none() {
            return;
        }
        if !self.rx_subagent_completion.lock().await.is_empty() {
            return;
        }
        let Some(mut op) = self.last_send_op.clone() else {
            return;
        };
        let decision = {
            let mut goal = self
                .config
                .goal_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !goal.is_active() {
                return;
            }
            goal.next_round(u64::from(end.input_tokens), u64::from(end.output_tokens))
        };
        match decision {
            RoundDecision::Drive(round) => {
                let snapshot: GoalSnapshot = self
                    .config
                    .goal_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .snapshot();
                if let Op::SendMessage { content, .. } = &mut op {
                    *content = render_continuation_prompt(&snapshot, round, MAX_GOAL_ROUNDS);
                }
                let _ = self.tx_op.send(op).await;
            }
            RoundDecision::Pause(reason) => {
                self.config
                    .goal_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pause(reason);
                let _ = self
                    .tx_event
                    .send(crate::events::Event::status(format!(
                        "Goal paused: {reason} (raise the token budget or /goal resume to continue)"
                    )))
                    .await;
            }
        }
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
