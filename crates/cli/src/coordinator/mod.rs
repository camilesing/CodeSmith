//! Coordinator mode's Router/Worker skeleton (DSH super-agent concepts,
//! adapted to CodeSmith's interactive architecture).
//!
//! The coordinator session routes; worker sub-agent sessions execute. This
//! module owns the **ticket** layer that gives delegated work a durable
//! identity: one ticket per background worker spawned from coordinator mode,
//! structured worker reports (`ticket_report`), a coordinator-facing board
//! (`ticket_list` / `ticket_read` / `ticket_close`), and a mandate guard that
//! refuses a closed ticket's worker further tool calls.
//!
//! Flow (all riding existing seams — no loop branches, no new queues):
//!
//! 1. `agent_open` / `agent_spawn` in coordinator mode create a ticket keyed
//!    by the worker's agent id (`tickets::TicketStore::create`).
//! 2. The worker surface gains `ticket_report` (`tools::TicketReportTool`).
//!    A `blocked` report or an attention question sends a
//!    [`SubAgentCompletion`] through the parent channel — the same
//!    idle-wake / pre-request-drain path a settled child uses — so the
//!    coordinator (or an idle session) is woken while the worker keeps
//!    running.
//! 3. The child's terminal completion carries a ticket card
//!    (`tickets::card_for`) instead of only the raw summary, and the
//!    coordinator's first request of each turn gets a one-shot board digest
//!    (the `OpenTicketsNote` seam, agent-runtime `host_services`).
//!
//! # Known limitations
//!
//! - Ticket-per-worker: several workers spawned for one user request are N
//!   tickets, not one task with N assignments (the DSH prototype's model).
//!   The coordinator groups them in its reply; the board does not.
//! - Workers are not resumable across restarts (replacement semantics, the
//!   sub-agent contract). A restart leaves old tickets open at stage `idle`;
//!   the coordinator closes or re-dispatches them manually.
//! - `ticket_report` is advisory to the transcript: a worker that never
//!   calls it still completes — its card is flagged "ended without a status
//!   report" and the coordinator decides via `agent_eval`.
//! - Scope is prompt-enforced, not machine-checked (same stance as the
//!   prototype: the mandate text rides the ticket; code enforces the
//!   ended-ticket tool guard only).

pub mod tickets;
pub mod tools;
pub mod triggers;

use codesmith_agent_runtime::host_services::OpenTicketsNote;
use codesmith_agent_runtime::subagent::SubAgentStatus;

pub use tickets::{SharedTicketStore, TicketStore};

/// The engine seam's provider: the board digest with live worker status.
/// The executor asks at a coordinator turn's first request; the answer
/// rides a `ticket_board` runtime event (append-only history).
pub struct OpenTicketsBoard {
    pub store: SharedTicketStore,
    pub subagent_manager: crate::tools::subagent::SharedSubAgentManager,
}

#[async_trait::async_trait]
impl OpenTicketsNote for OpenTicketsBoard {
    async fn open_tickets_digest(&self) -> Option<String> {
        // Manager first (async, short-lived guard), then the store's sync
        // lock — the two are never held together across an await.
        let running: std::collections::HashSet<String> = {
            let manager = self.subagent_manager.read().await;
            manager
                .list()
                .into_iter()
                .filter(|agent| agent.status == SubAgentStatus::Running)
                .map(|agent| agent.agent_id)
                .collect()
        };
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store.board_digest_with(&|id| running.contains(id))
    }
}
