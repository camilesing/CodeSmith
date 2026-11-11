//! Ticket store — durable identity for coordinator-delegated work.
//!
//! One ticket per background worker spawned from coordinator mode; the
//! ticket id IS the worker's agent id, so every board row doubles as the
//! session ref `agent_eval` / `agent_close` accept (chain of recall). Stage
//! is *derived*, never stored — the reference prototype's rule: state that
//! follows facts survives restarts and cannot drift from them.
//!
//! Persistence mirrors the sub-agent state file (`subagents.v1.json`):
//! best-effort JSON under `<workspace>/.codesmith/state/`, loaded lazily on
//! first access so a non-coordinator session pays nothing.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Shared handle threaded from the engine host to the spawn path, the
/// worker's tool surface, and the coordinator's board tools.
pub type SharedTicketStore = std::sync::Arc<std::sync::Mutex<TicketStore>>;

const TICKETS_STATE_FILE: &str = "tickets.v1.json";
const TICKETS_SCHEMA_VERSION: u32 = 1;
/// Inline budget for a report summary on a card or digest line — the
/// prototype's "inline & spill": full text stays in the store, reachable
/// through `ticket_read`.
pub const SUMMARY_INLINE_LIMIT: usize = 600;
const TITLE_LIMIT: usize = 80;
const DIGEST_MAX_LINES: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ticket {
    /// The worker's agent id — also the session ref for agent_eval/close.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// First line of the objective, capped — the board label.
    pub title: String,
    /// The full objective as dispatched: the mandate text. Scope is
    /// prompt-enforced (see module docs), never machine-checked.
    pub scope: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<TicketReport>,
    /// An open question to the user. Work may continue while it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
    /// Follow-up inputs routed to the worker (agent_eval messages).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<TicketInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<TicketEnd>,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TicketReport {
    pub state: ReportState,
    pub summary: String,
    pub at: String,
}

/// Structured worker status. Drives the derived stage; the coordinator never
/// guesses "waiting on a person" out of free text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportState {
    Progress,
    Waiting,
    Ready,
    Blocked,
    Failed,
    Dropped,
}

impl ReportState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::Waiting => "waiting",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Dropped => "dropped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attention {
    pub question: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TicketInput {
    pub text: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TicketEnd {
    pub outcome: TicketOutcome,
    /// Who ended it: "coordinator" (ticket_close), "worker" (dropped
    /// report), or "cancel" (agent_close / task_stop).
    pub by: String,
    pub at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TicketOutcome {
    Completed,
    Stopped,
    Failed,
    Dropped,
}

impl TicketOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Dropped => "dropped",
        }
    }
}

/// The person-first board stage, first match wins (derived, not stored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TicketStage {
    AwaitingAnswer,
    AwaitingReview,
    Blocked,
    Failed,
    Working,
    Idle,
}

impl TicketStage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingAnswer => "awaiting-answer",
            Self::AwaitingReview => "awaiting-review",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Working => "working",
            Self::Idle => "idle",
        }
    }
}

impl Ticket {
    /// Derived stage given the worker's liveness (`worker_running` from the
    /// sub-agent manager snapshot).
    #[must_use]
    pub fn stage(&self, worker_running: bool) -> TicketStage {
        if self.ended.is_some() {
            // Ended tickets sort last; the board filters them out anyway.
            return TicketStage::Idle;
        }
        if self
            .attention
            .as_ref()
            .is_some_and(|attention| attention.resolved_at.is_none())
        {
            return TicketStage::AwaitingAnswer;
        }
        match self.reports.last().map(|report| report.state) {
            Some(ReportState::Ready) => TicketStage::AwaitingReview,
            Some(ReportState::Blocked) => TicketStage::Blocked,
            Some(ReportState::Failed) => TicketStage::Failed,
            _ if worker_running => TicketStage::Working,
            _ => TicketStage::Idle,
        }
    }
}

/// Derive a ticket title from the spawn objective: first non-empty line,
/// capped.
#[must_use]
pub fn title_from_objective(objective: &str) -> String {
    let first = objective
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(untitled)");
    if first.chars().count() <= TITLE_LIMIT {
        first.to_string()
    } else {
        let mut capped: String = first.chars().take(TITLE_LIMIT).collect();
        capped.push('…');
        capped
    }
}

fn cap_inline(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.trim().to_string()
    } else {
        let mut capped: String = text.chars().take(limit).collect();
        capped.push('…');
        capped
    }
}

/// Lazily-loaded, best-effort-persisted ticket set. All mutation methods
/// persist before returning; callers hold the outer mutex (sync fs write of
/// a small JSON file, the `subagents.v1.json` pattern).
#[derive(Debug, Default)]
pub struct TicketStore {
    /// `None` until first access — a session that never touches tickets
    /// never reads the file.
    loaded: bool,
    tickets: Vec<Ticket>,
    /// `None` = in-memory only (tests, non-workspace hosts).
    path: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedTickets {
    schema_version: u32,
    tickets: Vec<Ticket>,
}

impl TicketStore {
    /// Store persisting under `<workspace>/.codesmith/state/tickets.v1.json`.
    #[must_use]
    pub fn for_workspace(workspace: &Path) -> Self {
        Self {
            loaded: false,
            tickets: Vec::new(),
            path: Some(
                workspace
                    .join(".codesmith")
                    .join("state")
                    .join(TICKETS_STATE_FILE),
            ),
        }
    }

    /// In-memory store for tests and hosts without a workspace.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    fn load_if_needed(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let Some(path) = &self.path else { return };
        let Ok(text) = std::fs::read_to_string(path) else {
            return; // first run or unreadable — start empty
        };
        match serde_json::from_str::<PersistedTickets>(&text) {
            Ok(state) if state.schema_version == TICKETS_SCHEMA_VERSION => {
                self.tickets = state.tickets;
            }
            Ok(state) => {
                tracing::warn!(
                    "ticket state schema {} != {}; ignoring persisted tickets at {}",
                    state.schema_version,
                    TICKETS_SCHEMA_VERSION,
                    path.display()
                );
            }
            Err(err) => {
                tracing::warn!(
                    "failed to parse ticket state at {}: {err}; starting empty",
                    path.display()
                );
            }
        }
    }

    fn persist(&self) {
        let Some(path) = &self.path else { return };
        let state = PersistedTickets {
            schema_version: TICKETS_SCHEMA_VERSION,
            tickets: self.tickets.clone(),
        };
        // Same atomic tmp+rename the sub-agent state uses: a crash mid-write
        // must not truncate the board (load treats a parse error as empty,
        // which would drop every ticket).
        if let Err(err) = crate::tools::subagent::write_json_atomic(path, &state) {
            tracing::warn!("failed to persist ticket state: {err:#}");
        }
    }

    /// Create the ticket for a spawned worker. Idempotent by id.
    pub fn create(&mut self, id: &str, session_name: &str, objective: &str) {
        self.load_if_needed();
        if self.tickets.iter().any(|ticket| ticket.id == id) {
            return;
        }
        let now = Utc::now().to_rfc3339();
        let session_name = (!session_name.is_empty()).then(|| session_name.to_string());
        self.tickets.push(Ticket {
            id: id.to_string(),
            session_name,
            title: title_from_objective(objective),
            scope: objective.to_string(),
            created_at: now.clone(),
            reports: Vec::new(),
            attention: None,
            inputs: Vec::new(),
            ended: None,
            updated_at: now,
        });
        self.persist();
    }

    /// Record a structured worker report. Returns `Some(attention_card)` —
    /// the payload to wake the parent with — when the report is `blocked` or
    /// carries a question; other states ride the worker's natural
    /// completion. A `dropped` report ends the ticket immediately (the
    /// silent abort).
    pub fn add_report(
        &mut self,
        id: &str,
        state: ReportState,
        summary: &str,
        question: Option<&str>,
    ) -> Option<String> {
        self.load_if_needed();
        let now = Utc::now().to_rfc3339();
        let ticket = self.tickets.iter_mut().find(|ticket| ticket.id == id)?;
        if ticket.ended.is_some() {
            return None; // closed ticket: no wake, no state change
        }
        ticket.reports.push(TicketReport {
            state,
            summary: summary.to_string(),
            at: now.clone(),
        });
        if let Some(question) = question.filter(|question| !question.trim().is_empty()) {
            ticket.attention = Some(Attention {
                question: question.to_string(),
                at: now.clone(),
                resolved_at: None,
            });
        }
        ticket.updated_at = now.clone();
        if state == ReportState::Dropped {
            ticket.ended = Some(TicketEnd {
                outcome: TicketOutcome::Dropped,
                by: "worker".to_string(),
                at: now,
            });
            self.persist();
            return None;
        }
        // Build the wake card under the mutable borrow, persist after it
        // ends.
        let wake = state == ReportState::Blocked || question.is_some();
        let card = wake.then(|| {
            let stage = ticket.stage(true);
            let mut card = format!(
                "[ticket {id}] \"{title}\" — stage: {stage}\n",
                title = ticket.title,
                stage = stage.as_str()
            );
            if let Some(Attention { question, .. }) = &ticket.attention {
                card.push_str(&format!("attention (answer via agent_eval): {question}\n"));
            }
            card.push_str(&format!(
                "last report ({state}): {summary}",
                state = state.as_str(),
                summary = cap_inline(summary, SUMMARY_INLINE_LIMIT)
            ));
            card
        });
        self.persist();
        card
    }

    /// Record a follow-up input routed to the worker (agent_eval message);
    /// delivering an answer resolves an open attention question.
    pub fn add_input(&mut self, id: &str, text: &str) {
        self.load_if_needed();
        let Some(ticket) = self.tickets.iter_mut().find(|ticket| ticket.id == id) else {
            return;
        };
        if ticket.ended.is_some() {
            return;
        }
        let now = Utc::now().to_rfc3339();
        ticket.inputs.push(TicketInput {
            text: text.to_string(),
            at: now.clone(),
        });
        if let Some(attention) = ticket.attention.as_mut()
            && attention.resolved_at.is_none()
        {
            attention.resolved_at = Some(now.clone());
        }
        ticket.updated_at = now;
        self.persist();
    }

    /// End a ticket. Idempotent; returns false when unknown or already ended.
    pub fn end(&mut self, id: &str, outcome: TicketOutcome, by: &str) -> bool {
        self.load_if_needed();
        let Some(ticket) = self.tickets.iter_mut().find(|ticket| ticket.id == id) else {
            return false;
        };
        if ticket.ended.is_some() {
            return false;
        }
        ticket.ended = Some(TicketEnd {
            outcome,
            by: by.to_string(),
            at: Utc::now().to_rfc3339(),
        });
        ticket.updated_at = Utc::now().to_rfc3339();
        self.persist();
        true
    }

    /// Whether the ticket exists and is ended — the mandate guard's check.
    #[must_use]
    pub fn is_ended(&mut self, id: &str) -> Option<bool> {
        self.load_if_needed();
        self.tickets
            .iter()
            .find(|ticket| ticket.id == id)
            .map(|ticket| ticket.ended.is_some())
    }

    #[must_use]
    pub fn get(&mut self, id: &str) -> Option<Ticket> {
        self.load_if_needed();
        self.tickets.iter().find(|ticket| ticket.id == id).cloned()
    }

    /// Open tickets with live worker status, person-first stage order.
    /// `is_running` answers from the sub-agent manager snapshot (the digest
    /// seam supplies it; `false` renders a manager-less view).
    #[must_use]
    pub fn open_tickets_with(
        &mut self,
        is_running: &dyn Fn(&str) -> bool,
    ) -> Vec<(Ticket, TicketStage)> {
        self.load_if_needed();
        self.tickets
            .iter()
            .filter(|ticket| ticket.ended.is_none())
            .map(|ticket| (ticket.clone(), ticket.stage(is_running(&ticket.id))))
            .collect()
    }

    /// The one-turn board digest for the coordinator's first request: one
    /// line per open ticket, person-first, capped. `None` when the board is
    /// clear (no message pushed — short content costs no tokens).
    #[must_use]
    pub fn board_digest_with(&mut self, is_running: &dyn Fn(&str) -> bool) -> Option<String> {
        let mut open = self.open_tickets_with(is_running);
        if open.is_empty() {
            return None;
        }
        open.sort_by_key(|(_, stage)| *stage);
        let total = open.len();
        let mut lines: Vec<String> = open
            .iter()
            .take(DIGEST_MAX_LINES)
            .map(|(ticket, stage)| {
                format!(
                    "- {id} [{stage}] {title}",
                    id = ticket.id,
                    stage = stage.as_str(),
                    title = ticket.title
                )
            })
            .collect();
        if total > DIGEST_MAX_LINES {
            lines.push(format!(
                "… and {} more (ticket_list)",
                total - DIGEST_MAX_LINES
            ));
        }
        Some(lines.join("\n"))
    }

    /// The completion card for a settled worker: stage, last report
    /// (capped), attention, and the no-report flag. `None` when the agent
    /// has no ticket (non-coordinator lineage).
    #[must_use]
    pub fn card_for(&mut self, id: &str, worker_terminal_status: &str) -> Option<String> {
        let ticket = self.get(id)?;
        let stage = ticket.stage(false);
        let mut card = format!(
            "[ticket {id}] \"{title}\" — stage: {stage}, worker: {status}\n",
            title = ticket.title,
            stage = stage.as_str(),
            status = worker_terminal_status
        );
        match ticket.reports.last() {
            Some(report) => card.push_str(&format!(
                "last report ({state}): {summary}",
                state = report.state.as_str(),
                summary = cap_inline(&report.summary, SUMMARY_INLINE_LIMIT)
            )),
            None => card.push_str(
                "no structured status report was filed (worker ended without ticket_report); \
                 read the transcript via agent_eval before deciding",
            ),
        }
        if let Some(Attention {
            question,
            resolved_at: None,
            ..
        }) = &ticket.attention
        {
            card.push_str(&format!("\nattention still open: {question}"));
        }
        if let Some(end) = &ticket.ended {
            card.push_str(&format!(
                "\nticket ended: {outcome} (by {by})",
                outcome = end.outcome.as_str(),
                by = end.by
            ));
        }
        Some(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_ticket() -> TicketStore {
        let mut store = TicketStore::in_memory();
        store.create(
            "agent_a",
            "auth",
            "Investigate the auth middleware.\nMore detail",
        );
        store
    }

    #[test]
    fn create_is_idempotent_and_titles_from_first_line() {
        let mut store = store_with_ticket();
        store.create("agent_a", "", "Different objective");
        let ticket = store.get("agent_a").expect("ticket exists");
        assert_eq!(ticket.title, "Investigate the auth middleware.");
        assert_eq!(
            ticket.scope,
            "Investigate the auth middleware.\nMore detail"
        );
    }

    #[test]
    fn stage_priority_attention_over_review_over_blocked() {
        let mut store = store_with_ticket();
        let ticket = store.get("agent_a").unwrap();
        assert_eq!(ticket.stage(true), TicketStage::Working);

        // ready → awaiting review (even though worker may still be running)
        store.add_report("agent_a", ReportState::Ready, "done", None);
        let ticket = store.get("agent_a").unwrap();
        assert_eq!(ticket.stage(true), TicketStage::AwaitingReview);

        // an open attention question outranks everything
        store.add_report(
            "agent_a",
            ReportState::Progress,
            "waiting on info",
            Some("which env?"),
        );
        let ticket = store.get("agent_a").unwrap();
        assert_eq!(ticket.stage(false), TicketStage::AwaitingAnswer);

        // delivering the answer resolves it
        store.add_input("agent_a", "use staging");
        let ticket = store.get("agent_a").unwrap();
        assert_eq!(ticket.stage(false), TicketStage::Idle); // last report is progress, worker not running
    }

    #[test]
    fn blocked_report_wakes_with_attention_card() {
        let mut store = store_with_ticket();
        let card = store
            .add_report(
                "agent_a",
                ReportState::Blocked,
                "cannot reproduce",
                Some("which version?"),
            )
            .expect("wake card");
        assert!(card.contains("stage: awaiting-answer"));
        assert!(card.contains("attention"));
        assert!(card.contains("which version?"));

        // progress never wakes
        let mut store = store_with_ticket();
        assert!(
            store
                .add_report("agent_a", ReportState::Progress, "halfway", None)
                .is_none()
        );
    }

    #[test]
    fn dropped_report_ends_ticket_and_further_reports_are_ignored() {
        let mut store = store_with_ticket();
        assert!(
            store
                .add_report("agent_a", ReportState::Dropped, "not a defect", None)
                .is_none()
        );
        assert_eq!(store.is_ended("agent_a"), Some(true));
        assert!(
            store
                .add_report("agent_a", ReportState::Ready, "late", None)
                .is_none()
        );
        let ticket = store.get("agent_a").unwrap();
        assert_eq!(ticket.reports.len(), 1);
    }

    #[test]
    fn card_flags_missing_report_and_ends() {
        let mut store = store_with_ticket();
        let card = store.card_for("agent_a", "completed").expect("card");
        assert!(card.contains("no structured status report"));

        store.end("agent_a", TicketOutcome::Stopped, "cancel");
        let card = store.card_for("agent_a", "cancelled").expect("card");
        assert!(card.contains("ticket ended: stopped (by cancel)"));
        assert!(store.card_for("agent_unknown", "completed").is_none());
    }

    #[test]
    fn digest_orders_person_first_and_caps_lines() {
        let mut store = TicketStore::in_memory();
        for i in 0..10 {
            store.create(&format!("agent_{i}"), "", &format!("objective {i}"));
        }
        store.create("agent_blocked", "", "blocked thing");
        store.add_report("agent_blocked", ReportState::Blocked, "stuck", None);
        let digest = store.board_digest_with(&|_| false).expect("digest");
        let lines: Vec<&str> = digest.lines().collect();
        assert_eq!(lines.len(), DIGEST_MAX_LINES + 1); // cap + overflow line
        assert!(lines[0].contains("agent_blocked"));
        assert!(lines[0].contains("[blocked]"));
        assert!(lines.last().unwrap().contains("ticket_list"));

        // a running worker derives `working` from the is_running answer
        store.create("agent_live", "", "live objective");
        let digest = store
            .board_digest_with(&|id| id == "agent_live")
            .expect("digest");
        assert!(digest.contains("[working] live objective"));

        // empty board → None (no message pushed)
        let mut empty = TicketStore::in_memory();
        assert!(empty.board_digest_with(&|_| false).is_none());
    }

    #[test]
    fn persistence_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = TicketStore::for_workspace(dir.path());
        store.create("agent_p", "", "persisted objective");
        store.add_report("agent_p", ReportState::Ready, "ready summary", None);

        let mut reloaded = TicketStore::for_workspace(dir.path());
        let ticket = reloaded.get("agent_p").expect("persisted ticket");
        assert_eq!(ticket.scope, "persisted objective");
        assert_eq!(ticket.reports.len(), 1);
        assert_eq!(ticket.stage(false), TicketStage::AwaitingReview);
    }

    #[test]
    fn title_caps_at_limit() {
        let long = "x".repeat(200);
        assert_eq!(title_from_objective(&long).chars().count(), TITLE_LIMIT + 1);
        assert_eq!(title_from_objective("\n\n  second line  "), "second line");
    }
}
