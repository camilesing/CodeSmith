//! Model-visible ticket tools.
//!
//! Two audiences, one store:
//!
//! - the **worker** gets `ticket_report` — structured status that drives the
//!   ticket's stage and, for `blocked` / a question, wakes the coordinator
//!   mid-run (the worker keeps working; the coordinator relays the question);
//! - the **coordinator** gets the board: `ticket_list` (person-first),
//!   `ticket_read` (full scope, reports, attention, inputs — the spill side
//!   of the inline caps), and `ticket_close` (withdraws the mandate: the
//!   worker's further tool calls are refused by the delegation policy).
//!
//! There is deliberately no `ticket_create`: creation rides the background
//! spawn path (single writer), so a ticket cannot exist without its worker.

use async_trait::async_trait;
use codesmith_agent_runtime::subagent::{
    CompletionKind, SubAgentCompletion, SubAgentCompletionSender,
};
use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str,
};
use serde_json::json;

use super::running_agent_ids;
use super::tickets::{ReportState, SUMMARY_INLINE_LIMIT, SharedTicketStore, TicketOutcome};
use crate::tools::subagent::{SharedSubAgentManager, SubAgentStatus};

/// Parse the `state` argument shared by report-shaped inputs.
fn parse_report_state(value: &str) -> Result<ReportState, ToolError> {
    match value.trim().to_lowercase().as_str() {
        "progress" | "in_progress" | "working" => Ok(ReportState::Progress),
        "waiting" | "idle" => Ok(ReportState::Waiting),
        "ready" | "done" | "complete" => Ok(ReportState::Ready),
        "blocked" | "stuck" => Ok(ReportState::Blocked),
        "failed" | "error" => Ok(ReportState::Failed),
        "dropped" | "aborted" | "not_applicable" => Ok(ReportState::Dropped),
        other => Err(ToolError::invalid_input(format!(
            "unknown ticket state '{other}'; use progress | waiting | ready | blocked | failed | dropped"
        ))),
    }
}

/// The worker-side report tool. Registered only on a coordinator-spawned
/// worker's surface; `agent_id` is the worker's own id (its ticket id).
pub struct TicketReportTool {
    store: SharedTicketStore,
    agent_id: String,
    /// The parent engine's completion channel: a blocked report or an
    /// attention question rides it so the idle-wake / pre-request drain
    /// delivers the card (the worker's turn keeps running).
    parent_tx: Option<SubAgentCompletionSender>,
}

impl TicketReportTool {
    pub fn new(
        store: SharedTicketStore,
        agent_id: String,
        parent_tx: Option<SubAgentCompletionSender>,
    ) -> Self {
        Self {
            store,
            agent_id,
            parent_tx,
        }
    }
}

#[async_trait]
impl ToolSpec for TicketReportTool {
    fn name(&self) -> &'static str {
        "ticket_report"
    }

    fn description(&self) -> &str {
        "Report this ticket's status to the coordinator. States: progress (working, no wake), waiting (nothing left to do), ready (work complete and verified, awaiting review), blocked (cannot proceed), failed (gave up with an error), dropped (nothing to do — closes the ticket silently). Attach `question` whenever the answer must come from a human: it is relayed verbatim and work may continue once answered. Call this instead of burying status in prose — the board's stage is derived from these reports."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "state": {
                    "type": "string",
                    "enum": ["progress", "waiting", "ready", "blocked", "failed", "dropped"],
                    "description": "Structured status; drives the ticket's stage on the coordinator's board"
                },
                "summary": {
                    "type": "string",
                    "description": "One-paragraph status for the board card (first 600 chars shown inline; full text stays readable via ticket_read)"
                },
                "question": {
                    "type": "string",
                    "description": "Optional question for the human, relayed verbatim by the coordinator. Use with blocked, or mid-work when only a human can unblock you"
                }
            },
            "required": ["state", "summary"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let state = parse_report_state(
            optional_str(&input, "state").ok_or_else(|| ToolError::missing_field("state"))?,
        )?;
        let summary =
            optional_str(&input, "summary").ok_or_else(|| ToolError::missing_field("summary"))?;
        // A blank question is no question (the store drops it): it neither
        // wakes the coordinator nor was relayed.
        let question =
            optional_str(&input, "question").filter(|question| !question.trim().is_empty());

        let card = {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.add_report(&self.agent_id, state, summary, question)
        };
        if let Some(card) = card
            && let Some(tx) = &self.parent_tx
        {
            tx.send(SubAgentCompletion {
                agent_id: self.agent_id.clone(),
                kind: CompletionKind::Subagent,
                payload: format!(
                    "A worker ticket needs attention (the worker keeps running).\n{card}"
                ),
                context_patch: None,
            });
        }
        let acknowledged = !matches!(state, ReportState::Progress);
        Ok(ToolResult::success(if acknowledged {
            format!(
                "recorded {}; the coordinator sees it on the next board update{}",
                state.as_str(),
                if question.is_some() {
                    " — your question was relayed"
                } else {
                    ""
                }
            )
        } else {
            "recorded progress; continue working".to_string()
        }))
    }
}

/// The coordinator-side board. Registered only in coordinator mode.
pub struct TicketListTool {
    store: SharedTicketStore,
    manager: SharedSubAgentManager,
}

impl TicketListTool {
    pub fn new(store: SharedTicketStore, manager: SharedSubAgentManager) -> Self {
        Self { store, manager }
    }

    async fn render(&self) -> String {
        // Manager first (async read, short-lived guard): a running-ids
        // snapshot taken without holding the ticket store's sync lock.
        let running = running_agent_ids(&self.manager).await;
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut open = store.open_tickets_with(&|id| running.contains(id));
        if open.is_empty() {
            return "No open tickets.".to_string();
        }
        open.sort_by_key(|(_, stage)| *stage);
        let total = open.len();
        let mut lines: Vec<String> = open
            .iter()
            .map(|(ticket, stage)| {
                let worker = if running.contains(&ticket.id) {
                    "running"
                } else {
                    "settled"
                };
                let mut line = format!(
                    "- {} [{}] {} (worker: {worker}",
                    ticket.id,
                    stage.as_str(),
                    ticket.title
                );
                if let Some(name) = &ticket.session_name {
                    line.push_str(&format!(", name: {name}"));
                }
                line.push(')');
                line
            })
            .collect();
        lines.push(format!(
            "\n{total} open ticket(s). Stages: awaiting-answer > awaiting-review > blocked > \
             failed > working > idle. Route follow-ups for an open ticket to its worker via \
             agent_eval's message parameter; close with ticket_close when done."
        ));
        lines.join("\n")
    }
}

#[async_trait]
impl ToolSpec for TicketListTool {
    fn name(&self) -> &'static str {
        "ticket_list"
    }

    fn description(&self) -> &str {
        "List this session's open coordinator tickets, person-first (awaiting-answer, awaiting-review, blocked, failed, working, idle). Check this before spawning new workers — a follow-up for an existing ticket goes to its worker, not a new spawn."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::success(self.render().await))
    }
}

/// Full ticket detail — the spill side of the inline caps: complete reports,
/// attention question verbatim, inputs, scope (mandate), and pointers into
/// the worker transcript.
pub struct TicketReadTool {
    store: SharedTicketStore,
    manager: SharedSubAgentManager,
}

impl TicketReadTool {
    pub fn new(store: SharedTicketStore, manager: SharedSubAgentManager) -> Self {
        Self { store, manager }
    }
}

#[async_trait]
impl ToolSpec for TicketReadTool {
    fn name(&self) -> &'static str {
        "ticket_read"
    }

    fn description(&self) -> &str {
        "Read one coordinator ticket in full: scope (the mandate as dispatched), every report with full summaries, the open attention question verbatim, and the inputs routed to the worker. The id is the worker's agent id / session name."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Ticket id (the worker's agent id or session name)" }
            },
            "required": ["id"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let requested = optional_str(&input, "id")
            .or_else(|| optional_str(&input, "agent_id"))
            .ok_or_else(|| ToolError::missing_field("id"))?;
        // Resolve session names through the manager, like agent_eval. A ticket
        // id with no manager entry (a restart left the ticket open; workers
        // are not resumable) still names a persisted ticket — fall back to
        // the raw id instead of failing, matching ticket_close.
        let agent_id = {
            let manager = self.manager.read().await;
            manager
                .resolve_agent_ref(requested)
                .unwrap_or_else(|_| requested.to_string())
        };
        // A running worker reads as `working` — the stage the board shows
        // for the same ticket.
        let running = running_agent_ids(&self.manager).await;
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(ticket) = store.get(&agent_id) else {
            return Err(ToolError::execution_failed(format!(
                "no ticket for {requested} (only coordinator-spawned background workers get tickets)"
            )));
        };
        let stage = ticket.stage(running.contains(&ticket.id));
        let mut out = format!(
            "# ticket {id}\ntitle: {title}\nstage: {stage}\ncreated: {created}\n\n## scope (mandate)\n{scope}\n",
            id = ticket.id,
            title = ticket.title,
            stage = stage.as_str(),
            created = ticket.created_at,
            scope = ticket.scope,
        );
        out.push_str("\n## reports\n");
        if ticket.reports.is_empty() {
            out.push_str("(none — the worker ended without a structured report)\n");
        }
        for report in &ticket.reports {
            out.push_str(&format!(
                "- [{}] {} — {}\n",
                report.state.as_str(),
                report.at,
                report.summary
            ));
        }
        match &ticket.attention {
            Some(attention) => {
                let status = if attention.resolved_at.is_none() {
                    "OPEN"
                } else {
                    "resolved"
                };
                out.push_str(&format!(
                    "\n## attention ({status})\n{}\n",
                    attention.question
                ));
            }
            None => out.push_str("\n## attention\n(none)\n"),
        }
        out.push_str("\n## inputs routed to the worker\n");
        if ticket.inputs.is_empty() {
            out.push_str("(none)\n");
        }
        for input in &ticket.inputs {
            out.push_str(&format!("- {} — {}\n", input.at, input.text));
        }
        if let Some(end) = &ticket.ended {
            out.push_str(&format!(
                "\n## ended\n{} (by {})\n",
                end.outcome.as_str(),
                end.by
            ));
        }
        out.push_str(&format!(
            "\nFull worker transcript: agent_eval on {agent_id} (terminal projections carry a \
             transcript_handle for handle_read). Inline cards cap summaries at \
             {SUMMARY_INLINE_LIMIT} chars; this view shows the full text."
        ));
        Ok(ToolResult::success(out))
    }
}

/// Withdraw a ticket's mandate: the ticket ends, and the worker's further
/// tool calls are refused by the delegation policy ("ended ticket; new work
/// needs a new ticket").
pub struct TicketCloseTool {
    store: SharedTicketStore,
    manager: SharedSubAgentManager,
}

impl TicketCloseTool {
    pub fn new(store: SharedTicketStore, manager: SharedSubAgentManager) -> Self {
        Self { store, manager }
    }
}

#[async_trait]
impl ToolSpec for TicketCloseTool {
    fn name(&self) -> &'static str {
        "ticket_close"
    }

    fn description(&self) -> &str {
        "Close a ticket: withdraws its mandate — the worker's further tool calls are refused. Use after reporting the result to the user (ready → synthesize → close), when work is obsolete, or to stop a stray worker. Outcomes: completed (delivered), stopped (withdrawn), failed."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Ticket id (the worker's agent id or session name)" },
                "outcome": {
                    "type": "string",
                    "enum": ["completed", "stopped", "failed"],
                    "description": "Defaults to completed"
                },
                "cancel_worker": {
                    "type": "boolean",
                    "description": "Also cancel the worker's session if still running (default true)"
                }
            },
            "required": ["id"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Session management, not a workspace write — the same declaration
        // the sibling `agent_close` carries. It is NOT `ReadOnly`: closing a
        // ticket ends its mandate and may cancel a running worker.
        vec![
            ToolCapability::ExecutesCode,
            ToolCapability::AllowedInPlanMode,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let requested = optional_str(&input, "id")
            .or_else(|| optional_str(&input, "agent_id"))
            .ok_or_else(|| ToolError::missing_field("id"))?;
        let outcome = match optional_str(&input, "outcome").unwrap_or("completed") {
            "completed" => TicketOutcome::Completed,
            "stopped" => TicketOutcome::Stopped,
            "failed" => TicketOutcome::Failed,
            other => {
                return Err(ToolError::invalid_input(format!(
                    "unknown outcome '{other}'; use completed | stopped | failed"
                )));
            }
        };
        let cancel_worker = optional_bool(&input, "cancel_worker", true)?;

        // Resolve session-name refs through the manager; cancel a still
        // running worker when asked (the same cancel `agent_close` uses) —
        // only once the ticket is confirmed open, so an unknown or
        // already-ended ticket cancels nothing.
        let agent_id = {
            let manager = self.manager.read().await;
            manager
                .resolve_agent_ref(requested)
                .unwrap_or_else(|_| requested.to_string())
        };
        {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !store.end(&agent_id, outcome, "coordinator") {
                return Err(ToolError::execution_failed(format!(
                    "no open ticket for {requested}"
                )));
            }
        }
        // `cancel` answers for any resolved agent, settled or not: only a
        // worker that was running when the ask arrived was cancelled by it.
        let cancelled = cancel_worker && {
            let mut guard = self.manager.write().await;
            let was_running = matches!(
                guard.get_result(&agent_id),
                Ok(snapshot) if snapshot.status == SubAgentStatus::Running
            );
            let _ = guard.cancel(&agent_id);
            was_running
        };
        Ok(ToolResult::success(format!(
            "ticket {agent_id} closed ({}); the worker's further tool calls are refused{}",
            outcome.as_str(),
            if cancelled {
                " and its session was cancelled"
            } else {
                ""
            }
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::tickets::TicketStore;

    fn shared_store() -> SharedTicketStore {
        let mut store = TicketStore::in_memory();
        store.create("agent_w", "", "Fix the flaky login test");
        std::sync::Arc::new(std::sync::Mutex::new(store))
    }

    /// A manager over a temp workspace plus one `Running` worker — the
    /// live-status fixture of the board tools.
    async fn manager_with_running_worker(id: &str) -> (tempfile::TempDir, SharedSubAgentManager) {
        use crate::tools::subagent::SubAgentManager;

        let dir = tempfile::tempdir().expect("tempdir");
        let manager: SharedSubAgentManager = std::sync::Arc::new(tokio::sync::RwLock::new(
            SubAgentManager::new(dir.path().to_path_buf(), 2),
        ));
        manager.write().await.insert_test_live_agent(
            id.to_string(),
            crate::tools::subagent::SubAgentAssignment {
                objective: "Fix the flaky login test".to_string(),
                role: None,
            },
        );
        (dir, manager)
    }

    #[tokio::test]
    async fn report_tool_records_state_and_rejects_unknown_state() {
        let tool = TicketReportTool::new(shared_store(), "agent_w".to_string(), None);
        let result = tool
            .execute(
                json!({ "state": "ready", "summary": "fixed; tests green" }),
                &ToolContext::new("/tmp"),
            )
            .await
            .expect("execute");
        assert!(format!("{result:?}").contains("recorded ready"));

        let err = tool
            .execute(
                json!({ "state": "weird", "summary": "x" }),
                &ToolContext::new("/tmp"),
            )
            .await
            .expect_err("unknown state");
        assert!(err.to_string().contains("unknown ticket state"));
    }

    #[tokio::test]
    async fn report_tool_wakes_parent_on_blocked_with_question() {
        let (tx, mut rx) =
            SubAgentCompletionSender::channel(std::sync::Arc::new(tokio::sync::Notify::new()));
        let store = shared_store();
        let tool = TicketReportTool::new(store, "agent_w".to_string(), Some(tx));
        tool.execute(
            json!({ "state": "blocked", "summary": "need creds", "question": "which tenant?" }),
            &ToolContext::new("/tmp"),
        )
        .await
        .expect("execute");
        let completion = rx.try_recv().expect("wake sent");
        assert!(completion.payload.contains("which tenant?"));
    }

    /// `ready → synthesize → close` runs `ticket_close` on a worker that has
    /// already settled. The reply used to claim "its session was cancelled"
    /// for any agent the id resolved to.
    #[tokio::test]
    async fn closing_a_settled_workers_ticket_claims_no_cancel() {
        let (dir, manager) = manager_with_running_worker("agent_w").await;
        manager
            .write()
            .await
            .cancel("agent_w")
            .expect("settle the worker");
        let tool = TicketCloseTool::new(shared_store(), manager.clone());
        let result = tool
            .execute(json!({ "id": "agent_w" }), &ToolContext::new(dir.path()))
            .await
            .expect("close");
        let text = format!("{result:?}");
        assert!(text.contains("closed (completed)"), "{text}");
        assert!(!text.contains("session was cancelled"), "{text}");
        manager.write().await.abort_test_agent("agent_w");
    }

    /// An agent without an open ticket (an Agent-mode child, or a ticket a
    /// `dropped` report already ended) stays untouched: the cancel used to
    /// run before the ticket check and go unreported behind the error.
    #[tokio::test]
    async fn closing_without_an_open_ticket_cancels_nothing() {
        use crate::tools::subagent::SubAgentStatus;

        let (dir, manager) = manager_with_running_worker("agent_unticketed").await;
        let tool = TicketCloseTool::new(shared_store(), manager.clone());
        let err = tool
            .execute(
                json!({ "id": "agent_unticketed" }),
                &ToolContext::new(dir.path()),
            )
            .await
            .expect_err("no ticket to close");
        assert!(err.to_string().contains("no open ticket"), "{err}");
        let status = manager
            .read()
            .await
            .get_result("agent_unticketed")
            .expect("the agent is still there")
            .status;
        assert_eq!(status, SubAgentStatus::Running);
        manager.write().await.abort_test_agent("agent_unticketed");
    }

    /// A blank `question` is a common way to "fill in" the optional field.
    /// It is no question: the worker is not told it was relayed and the
    /// coordinator is not woken for it.
    #[tokio::test]
    async fn report_tool_treats_a_blank_question_as_none() {
        let (tx, mut rx) =
            SubAgentCompletionSender::channel(std::sync::Arc::new(tokio::sync::Notify::new()));
        let tool = TicketReportTool::new(shared_store(), "agent_w".to_string(), Some(tx));
        let result = tool
            .execute(
                json!({ "state": "waiting", "summary": "nothing left", "question": "  " }),
                &ToolContext::new("/tmp"),
            )
            .await
            .expect("execute");
        let text = format!("{result:?}");
        assert!(text.contains("recorded waiting"), "{text}");
        assert!(!text.contains("relayed"), "{text}");
        assert!(rx.try_recv().is_err(), "a blank question wakes nobody");
    }

    /// A running worker that filed no report reads as `working` — the stage
    /// `ticket_list` shows for the same ticket — not as `idle`.
    #[tokio::test]
    async fn read_derives_the_stage_from_live_worker_status() {
        let (dir, manager) = manager_with_running_worker("agent_w").await;
        let tool = TicketReadTool::new(shared_store(), manager.clone());
        let result = tool
            .execute(json!({ "id": "agent_w" }), &ToolContext::new(dir.path()))
            .await
            .expect("read");
        let text = format!("{result:?}");
        assert!(text.contains("stage: working"), "{text}");
        manager.write().await.abort_test_agent("agent_w");
    }

    #[tokio::test]
    async fn read_falls_back_to_the_persisted_ticket_id() {
        // A restart leaves tickets open while the manager has no live worker
        // to resolve the id through; the persisted row must still read.
        use crate::tools::subagent::SubAgentManager;

        let dir = tempfile::tempdir().expect("tempdir");
        let manager: SharedSubAgentManager = std::sync::Arc::new(tokio::sync::RwLock::new(
            SubAgentManager::new(dir.path().to_path_buf(), 2),
        ));
        let tool = TicketReadTool::new(shared_store(), manager);
        let result = tool
            .execute(json!({ "id": "agent_w" }), &ToolContext::new(dir.path()))
            .await
            .expect("persisted ticket reads");
        assert!(format!("{result:?}").contains("Fix the flaky login test"));
    }
}
