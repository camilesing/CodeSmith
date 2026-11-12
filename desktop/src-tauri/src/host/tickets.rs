//! Coordinator ticket board: the desktop's shared-task projection.
//!
//! Coordinator mode persists one ticket per spawned worker at
//! `<workspace>/.codesmith/state/tickets.v1.json` (the runtime's
//! `crates/cli/src/coordinator/tickets.rs` store). The vendored client's
//! shared-task panel reads the `agentTeam` projection, so this module maps
//! those durable ticket records onto that wire shape. A read never loads an
//! engine: the board is a durable record, and a session whose engine was
//! evicted still shows its tickets.
//!
//! The mapping is the one the runtime's own board uses, minus worker
//! liveness (the store derives `working` from the live sub-agent manager):
//! the last structured report already separates pending, in-progress and
//! completed work, and the card's warning glyph only needs `ready`.
//!
//! Known limitations:
//! - a workspace the bridge cannot read yields no projection (the board is
//!   absent), never a fabricated row;
//! - stopped, failed and dropped tickets are hidden — the shared-task
//!   vocabulary has no failure state; a completed ticket stays visible;
//! - live updates ride the follow stream's 2s poll, not a runtime push, so a
//!   `progress` report that never wakes the coordinator appears on the next
//!   poll;
//! - the board follows the durable tickets, not the thread mode: a session
//!   that left coordinator still shows tickets it already created.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

const TICKETS_STATE_FILE: &str = "tickets.v1.json";
const TICKETS_SCHEMA_VERSION: u32 = 1;

/// How often a followed session re-reads its ticket file (the board's live
/// tail). The store persists before every mutation returns, so the file is
/// the durable truth; the poll only notices it.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Deserialize)]
struct PersistedTickets {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    tickets: Vec<Ticket>,
}

#[derive(Debug, Deserialize)]
struct Ticket {
    id: String,
    #[serde(default)]
    session_name: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    reports: Vec<TicketReport>,
    #[serde(default)]
    attention: Option<Attention>,
    #[serde(default)]
    ended: Option<TicketEnd>,
}

#[derive(Debug, Deserialize)]
struct TicketReport {
    #[serde(default)]
    state: String,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
struct Attention {
    #[serde(default)]
    resolved_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TicketEnd {
    #[serde(default)]
    outcome: String,
}

/// The ticket store's path for one workspace.
#[must_use]
pub fn state_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".codesmith")
        .join("state")
        .join(TICKETS_STATE_FILE)
}

/// Read one workspace's board value; `None` when the file is absent,
/// unreadable, malformed, or holds no visible ticket.
pub async fn projection(workspace: &Path) -> Option<Value> {
    let bytes = tokio::fs::read(state_path(workspace)).await.ok()?;
    let state: PersistedTickets = serde_json::from_slice(&bytes).ok()?;
    if state.schema_version != TICKETS_SCHEMA_VERSION {
        return None;
    }
    map_tickets(&state.tickets)
}

/// Map the durable tickets onto the client's `agentTeam` projection shape.
fn map_tickets(tickets: &[Ticket]) -> Option<Value> {
    let tasks: Vec<Value> = tickets.iter().filter_map(task_row).collect();
    if tasks.is_empty() {
        return None;
    }
    Some(json!({ "enabled": true, "tasks": tasks }))
}

/// One visible ticket as a shared-task row.
fn task_row(ticket: &Ticket) -> Option<Value> {
    if let Some(end) = &ticket.ended {
        // The shared-task panel has no stopped/failed state; hiding those
        // keeps the board from claiming a failure was a completion.
        return (end.outcome == "completed").then(|| task_json(ticket, "completed", true));
    }
    let last = ticket.reports.last();
    let awaiting_answer = ticket
        .attention
        .as_ref()
        .is_some_and(|attention| attention.resolved_at.is_none());
    let (status, ready) = match last.map(|report| report.state.as_str()) {
        // A ready report is complete work awaiting review.
        Some("ready") => ("completed", true),
        // Blocked/failed work and an open question need the human: the
        // panel's warning glyph is the pending + not-ready state.
        Some("blocked") | Some("failed") => ("pending", false),
        _ if awaiting_answer => ("pending", false),
        _ => ("in_progress", true),
    };
    Some(task_json(ticket, status, ready))
}

/// The wire row. `ownerId`/`ownerName` are the worker's ticket id and spawn
/// name, so the panel's owner line names the worker rather than the model's
/// objective.
fn task_json(ticket: &Ticket, status: &str, ready: bool) -> Value {
    let subject = ticket.title.trim();
    let subject = if subject.is_empty() {
        ticket.id.as_str()
    } else {
        subject
    };
    let description = if !ticket.scope.trim().is_empty() {
        ticket.scope.clone()
    } else {
        ticket
            .reports
            .last()
            .map(|report| report.summary.clone())
            .unwrap_or_default()
    };
    let owner = ticket
        .session_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(&ticket.id);
    json!({
        "id": ticket.id,
        "revision": u64::try_from(ticket.reports.len()).unwrap_or(u64::MAX).saturating_add(1),
        "subject": subject,
        "description": description,
        "status": status,
        "ownerId": ticket.id,
        "ownerName": owner,
        "blockedBy": [],
        "writeScopes": [],
        "ready": ready,
        "writeScopeWarnings": [],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(id: &str, reports: &[(&str, &str)], ended: Option<&str>) -> Ticket {
        Ticket {
            id: id.to_string(),
            session_name: Some(format!("{id}-name")),
            title: format!("{id} title"),
            scope: format!("{id} scope"),
            reports: reports
                .iter()
                .map(|(state, summary)| TicketReport {
                    state: (*state).to_string(),
                    summary: (*summary).to_string(),
                })
                .collect(),
            attention: None,
            ended: ended.map(|outcome| TicketEnd {
                outcome: outcome.to_string(),
            }),
        }
    }

    #[test]
    fn open_tickets_map_to_the_shared_task_states() {
        let tasks = [
            ticket("working", &[], None),
            ticket("progress", &[("progress", "moving")], None),
            ticket("review", &[("ready", "done")], None),
            ticket("blocked", &[("blocked", "stuck")], None),
            ticket("failed", &[("failed", "broke")], None),
            ticket("completed", &[("ready", "done")], Some("completed")),
            ticket("stopped", &[], Some("stopped")),
            ticket("dropped", &[], Some("dropped")),
        ];
        let board = map_tickets(&tasks).expect("visible rows");
        let rows = board["tasks"].as_array().expect("tasks array");
        let by_id = |id: &str| {
            rows.iter()
                .find(|row| row["id"] == json!(id))
                .unwrap_or_else(|| panic!("missing row {id}"))
        };
        assert_eq!(by_id("working")["status"], json!("in_progress"));
        assert_eq!(by_id("progress")["status"], json!("in_progress"));
        assert_eq!(by_id("review")["status"], json!("completed"));
        assert_eq!(by_id("blocked")["status"], json!("pending"));
        assert_eq!(by_id("blocked")["ready"], json!(false));
        assert_eq!(by_id("failed")["status"], json!("pending"));
        assert_eq!(by_id("failed")["ready"], json!(false));
        assert_eq!(by_id("completed")["status"], json!("completed"));
        // Stopped and dropped tickets are not completion and not shown.
        assert!(rows.iter().all(|row| row["id"] != json!("stopped")));
        assert!(rows.iter().all(|row| row["id"] != json!("dropped")));
        assert_eq!(by_id("working")["ownerName"], json!("working-name"));
        assert_eq!(by_id("working")["description"], json!("working scope"));
    }

    #[test]
    fn an_open_question_is_a_pending_warning_until_answered() {
        let mut asking = ticket("asking", &[("progress", "waiting")], None);
        asking.attention = Some(Attention { resolved_at: None });
        let board = map_tickets(&[asking]).expect("visible row");
        assert_eq!(board["tasks"][0]["status"], json!("pending"));
        assert_eq!(board["tasks"][0]["ready"], json!(false));

        let mut answered = ticket("answered", &[("progress", "waiting")], None);
        answered.attention = Some(Attention {
            resolved_at: Some("2026-11-12T00:00:00Z".to_string()),
        });
        let board = map_tickets(&[answered]).expect("visible row");
        assert_eq!(board["tasks"][0]["status"], json!("in_progress"));
        assert_eq!(board["tasks"][0]["ready"], json!(true));
    }

    #[test]
    fn no_visible_ticket_answers_an_absent_projection() {
        assert_eq!(map_tickets(&[]), None);
        assert_eq!(
            map_tickets(&[ticket("stopped", &[], Some("stopped"))]),
            None
        );
    }

    #[tokio::test]
    async fn a_failed_store_read_is_absent_not_fabricated() {
        let root = std::env::temp_dir().join(format!(
            "cds-tickets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            projection(&root).await,
            None,
            "a workspace without a ticket file has no board"
        );
        let bad = state_path(&root);
        std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
        std::fs::write(&bad, b"not json").unwrap();
        assert_eq!(projection(&root).await, None);
        std::fs::write(
            &bad,
            br#"{"schema_version":1,"tickets":[{"id":"t","title":"T","scope":"S"}]}"#,
        )
        .unwrap();
        let value = projection(&root).await.expect("a valid store projects");
        assert_eq!(value["tasks"][0]["status"], json!("in_progress"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
