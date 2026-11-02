//! Unified inbox vocabulary — one set of terms for the three delivery
//! mechanisms that already exist in the kernel (P1-1).
//!
//! CodeSmith does not have DSH's `ReactLoopInbox` data structure, and P1-1
//! deliberately does not add one: the mechanisms below already implement
//! the semantics, and a second queue next to them would be a second inbox
//! wearing a hat. What was missing was the *vocabulary* — three names for
//! one concept. The vocabulary lives here as the classification every
//! mechanism maps onto; the one piece of code it owns is the wake gate
//! ([`followup_wake_pending`]) the idle-wake decision consumes.
//!
//! # The vocabulary
//!
//! **Target** — when an entry is delivered:
//!
//! | Target | Claimed at | Claimed by |
//! |---|---|---|
//! | `NextTurn` | the turn boundary (loop top, idle) | the op loop's `select!` (`engine/mod.rs`) — a real op, a latched wake ([`idle_wake_op`](super::idle_wake)), or a goal round |
//! | `NextStep` | the step boundary (pre-request) | the executor's pre-request drain (sub-agent / shell-job completions) and the steer drain (`turn/seams.rs`) |
//!
//! **Delivery** — how an entry wakes the loop:
//!
//! | Delivery | Wakes? | Mechanism today |
//! |---|---|---|
//! | `Followup` | yes — synthesizes/continues a turn when nothing is running | sub-agent + shell-job completions (`SubAgentCompletionSender::send` pokes the `Notify`); goal rounds; team inbox dispatch (`Op::TeamInboxDispatch`) |
//! | `Steer` | yes — interrupts the current step's stream and lands at the next boundary | the steer channel (`rx_steer`): REPL queued messages flushed mid-turn, plan-approval feedback |
//! | `Inject` | no — queues silently, delivered at its target's boundary | REPL messages typed while a turn runs (queued host-side, then steered or re-queued); runtime-API prompts to a busy thread |
//!
//! # Latched wake
//!
//! A `Followup` poke that lands while the loop is *not* waiting in the
//! `select!` (a turn is running, or a maintenance op — compaction, purge,
//! edit-rollback, task cancels — is executing in a match arm) is
//! **latched**: `tokio::sync::Notify` stores the permit, and the next idle
//! wait consumes it and re-runs the wake decision at the convergence point.
//! The wake is never lost to a maintenance window; the regression is
//! pinned by `wake_arriving_during_a_maintenance_op_is_latched_and_replayed`
//! (cli engine tests).
//!
//! # Known limitations
//!
//! - Targets are not per-agent addressable: completions deliver to the
//!   parent main turn (the spawning session); team dispatches go through
//!   the leader poller on the same op seam. A DSH-style addressable team /
//!   workflow inbox is future work on this seam, not present vocabulary.
//! - Sub-agent transcripts keep no journal (P0-2 open), so inbox entries
//!   carry no journal generation; delivery correctness rests on the
//!   payload itself. This slice deliberately opens no new journal hole.
//! - `Steer` with no live turn is the host's call, by design: the runtime
//!   API rejects it (409), the REPL keeps the text on its queue and sends
//!   it as the next turn — `Followup`, never a silent drop.
//! - The `Inject` path relies on host-side queues (the REPL's queued
//!   messages); the engine's `rx_op` backlog is the inject queue for API
//!   hosts. There is no engine-side inject buffer to inspect.

/// The idle-wake decision's gate (P1-1): whether the inbox holds any
/// followup-class entry worth synthesizing a turn for. Today those entries
/// are the parked sub-agent / shell-job completions — the two producers the
/// round-10 wake seam already serves — so this is the generalization of
/// `parked > 0` under the inbox vocabulary: a new wakeup-flagged producer
/// registers here, and only here.
pub(crate) fn followup_wake_pending(parked_completions: usize) -> bool {
    parked_completions > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wake_gate_is_exactly_parked_followups() {
        assert!(!followup_wake_pending(0));
        assert!(followup_wake_pending(1));
        assert!(followup_wake_pending(7));
    }
}
