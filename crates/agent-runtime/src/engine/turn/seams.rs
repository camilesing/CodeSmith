//! Turn-loop cross-cutting seams — the cancel-token checkpoints and the
//! steer drain — split out of `host_executor.rs` (codebase-health §1).
//!
//! These are the two guardrails that cut across every phase of the step loop
//! rather than belonging to one of them. This module owns their helpers
//! ([`HostAgentExecutor::is_cancelled`] and the steer push / drain methods);
//! the checkpoint call sites themselves stay in the code that owns the
//! surrounding phase (the map below says where each lives).
//!
//! **steer** ([`HostAgentExecutor::drain_steers`]) — lets a user inject
//! additional text input into an in-flight turn. At the top of each step
//! (before the LLM request), queued steers are drained via `try_recv` and each
//! becomes a `user` message in the transcript so the model re-reads them on
//! this step's request — mirroring `handle_deepseek_turn`'s top-of-loop drain.
//! The receiver is
//! `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>` —
//! interior-mutable because `AgentExecutor::run` is `&self` while `try_recv`
//! takes `&mut self` (same pattern as the LSP flush's `pending` accumulator);
//! tokio mutex (not std) so the guard may also cross the blocking
//! `recv().await` in the sub-agent blocking hold's `biased select!` steer arm.
//! The three secondary drain sites are all absorbed ✅: the **mid-stream
//! buffer** ([`HostAgentExecutor::reduce_stream`] `try_recv`s into a per-step
//! `pending_steers` vec after each stream event, flushed post-stream /
//! post-tool via [`HostAgentExecutor::flush_pending_steers`]), the
//! **post-stream resume** (the no-tool-calls arm flushes + resumes), and the
//! **blocking `recv` during sub-agent hold** (the hold's own `biased select!`
//! arms).
//!
//! **cancel-token** (guardrail 10) — the **first cross-cutting guardrail**. An
//! optional [`CancellationToken`](tokio_util::sync::CancellationToken) (`None`
//! ⇒ all cancel checks are no-ops) mirrors production's seven
//! turn-cancellation checkpoints. When set, a cancelled turn surfaces
//! [`StopReason::Interrupted`](codesmith_agent::callback::StopReason::Interrupted)
//! (distinct from `Error`) so the host can show "cancelled" rather than
//! "failed". The checkpoint map:
//!
//! - **Checkpoint A** (loop-top, before `max_steps`, in `run_inner`) bounds
//!   all `continue` loops (capacity `RetryStep`, reactive
//!   `RecoveredContextOverflow`, subagent resume);
//! - **Checkpoint B** (stream-open race, in `turn::stream`) races the token
//!   against `create_message_stream` in a `biased select!` so a cancelled turn
//!   aborts before the stream opens;
//! - **Checkpoint C** (the `Empty` arm, in `turn::stream`) aborts a
//!   transparent retry;
//! - **Checkpoint D** (post-stream `Complete`/`Partial`, in `turn::stream`)
//!   discards already-produced content;
//! - **Checkpoint G** (post-tool-loop, before `loop_guard_halt`, in
//!   `run_inner`) lets a tool-triggered cancel take priority over a loop-guard
//!   halt;
//! - the **approval cancel race** (in `turn::approval`) breaks out of the
//!   blocking `recv().await` via `select!` (the tool records an error result,
//!   then Checkpoint G catches the cancel);
//! - the **steer stale-drain**
//!   ([`HostAgentExecutor::drain_stale_steers`]) is a `pub` host-side method —
//!   the host calls it before `run` (mirrors `handle_send_message`'s `while
//!   rx_steer.try_recv().is_ok() {}`), not inside the turn loop.
//!
//! The subagent **blocking hold** cancel race (Checkpoint E) is absorbed ✅ —
//! it lives in the hold's own `biased select!` cancel arm. See "Known gaps in
//! subagent" in `host_executor.rs` for per-guardrail cancel status.

use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message};

use crate::engine::host_executor::HostAgentExecutor;
use crate::engine::summarize_text;

impl HostAgentExecutor {
    /// Whether the turn has been cancelled. `None` cancel token ⇒ never
    /// cancelled (embeds/tests that don't need cancellation). Mirrors production
    /// `self.cancel_token.is_cancelled()` checks at every turn-loop checkpoint.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel_token.as_ref().is_some_and(|t| t.is_cancelled())
    }

    /// Push a steer message into the transcript, observing against the shared
    /// working set and wrapping in `<turn_meta>` when a
    /// [`TurnMetaProbe`](crate::engine::host_executor::TurnMetaProbe) is
    /// present (production); plain text otherwise. Shared by the pre-request
    /// drain ([`drain_steers`](Self::drain_steers)), the sub-agent blocking-hold
    /// steer arm, and the mid-stream buffer flush
    /// ([`flush_pending_steers`](Self::flush_pending_steers)) — single source for
    /// the observe + enrich + push logic so the three push sites cannot drift.
    /// Sync: [`ChatHistory::push`] is sync and `observe_user_message` /
    /// `enrich_user_text_message` are sync (the lock on the shared working set
    /// is never held across an `await`).
    pub(crate) fn push_steer_message(&self, steer: String, history: &mut dyn ChatHistory) {
        let message = match &self.turn_meta {
            Some(probe) => {
                probe.observe_user_message(&steer);
                probe.enrich_user_text_message(steer)
            }
            None => Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: steer,
                    cache_control: None,
                }],
            },
        };
        history.push(message);
    }

    /// Flush mid-stream-buffered steers into the transcript (observe + enrich +
    /// push). No status — "Steer input queued" was already emitted during
    /// buffering in [`reduce_stream`](Self::reduce_stream). Returns the count
    /// flushed so callers can decide whether to resume the turn. Mirrors
    /// production's `pending_steers.drain(..)` at `handle_deepseek_turn`
    /// (post-stream no-tools — caller resumes on a fresh step) and
    /// `:2632-2637` (post-tool — caller falls through to the next step).
    pub(crate) fn flush_pending_steers(
        &self,
        pending: &mut Vec<String>,
        history: &mut dyn ChatHistory,
    ) -> usize {
        let count = pending.len();
        for steer in pending.drain(..) {
            self.push_steer_message(steer, history);
        }
        count
    }

    /// (1) per-step pre-request seam — drain queued steer inputs into the
    /// transcript as `user` messages so the model sees them before its next
    /// request. Mirrors `handle_deepseek_turn`'s top-of-loop steer drain
    /// (`handle_deepseek_turn`): `try_recv` loop → trim → skip-empty → push a
    /// `user` message → emit status. `try_recv` is non-blocking — this only
    /// drains what's already queued; it never waits for new input.
    ///
    /// When a [`TurnMetaProbe`](crate::engine::host_executor::TurnMetaProbe) is
    /// wired in (production), this mirrors production's
    /// `working_set.observe_user_message(text, &workspace)` +
    /// `user_text_message_with_turn_metadata` wrap for each drained steer
    /// (observe before the move, then enrich). Embeds/tests (`probe` absent)
    /// push plain text — the pre-slice-22 behavior. The mid-stream buffer
    /// drain site (inside [`reduce_stream`](HostAgentExecutor::reduce_stream)'s
    /// `try_recv` + [`flush_pending_steers`](HostAgentExecutor::flush_pending_steers))
    /// is now absorbed ✅; the blocking `recv` during the sub-agent hold is
    /// enriched via the hold's own steer arm.
    pub(crate) async fn drain_steers(&self, history: &mut dyn ChatHistory) {
        let Some(rx) = &self.steer else {
            return;
        };
        loop {
            // `try_recv` is synchronous and non-blocking — the tokio mutex
            // guard is taken and dropped within this block, never across an
            // `await` (matching the LSP flush pattern). The lock is
            // uncontended (single consumer) so `.await` is effectively instant.
            let steer = {
                let mut guard = rx.lock().await;
                match guard.try_recv() {
                    Ok(s) => s,
                    // Empty or disconnected — nothing more to drain this step.
                    Err(_) => break,
                }
            };
            let steer = steer.trim().to_string();
            if steer.is_empty() {
                continue;
            }
            // Compute the status preview before moving `steer` into the
            // message (mirrors production's `steer.clone()` + summarize).
            let status = format!("Steer input accepted: {}", summarize_text(&steer, 120));
            self.push_steer_message(steer, history);
            self.emit_status(status).await;
        }
    }

    /// Drain stale steer inputs from a previous (possibly cancelled) turn.
    /// Mirrors production's `while self.rx_steer.try_recv().is_ok() {}` at the
    /// start of `handle_send_message` (`engine/mod.rs:1013-1014`) — a per-turn
    /// reset so steers queued during an interrupted previous turn don't leak
    /// into the new turn. Unlike [`drain_steers`](Self::drain_steers), this
    /// **discards** (does not inject into the transcript): stale steers are not
    /// the user's intent for this turn. Async — the tokio mutex guard is
    /// taken and dropped within the loop, never across another `await` (the
    /// steer receiver migrated from `std::sync::Mutex` to `tokio::sync::Mutex`
    /// in the blocking-hold slice so the `biased select!` steer arm can hold
    /// the guard across `recv().await`).
    ///
    /// **Host-side concern:** the host calls this BEFORE
    /// `AgentExecutor::run`, not inside the turn loop. Calling it inside
    /// `run_inner` would discard steers the host queued for the current turn
    /// before calling `run`.
    pub async fn drain_stale_steers(&self) {
        let Some(rx) = &self.steer else {
            return;
        };
        let mut guard = rx.lock().await;
        while guard.try_recv().is_ok() {}
    }
}
