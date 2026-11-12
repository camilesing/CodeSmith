//! Child-session journal provider over the runtime thread store.
//!
//! Seam: [`codesmith_agent_runtime::child_journal::ChildSessionJournalFactory`]
//! (definition in the framework crate). This module is the provider for
//! runtime-API threads. A child is a normal [`ThreadRecord`] with
//! `origin = "subagent"`; its turns/items/events are the same records the
//! HTTP API already replays, so the desktop bridge opens a child conversation
//! through the existing `session/follow` path.
//!
//! Replacement semantics: one run is one turn. A restart marks a still-running
//! child `interrupted` and keeps every item (see
//! `RuntimeThreadManager::recover_interrupted_state`); nothing here ever
//! resumes a child.
//!
//! Known limitations:
//! - Assistant text is recorded per settled model response, not per stream
//!   delta; a reconnect mid-step sees the last completed step.
//! - Forked children record their assignment as the first user message, not
//!   the inherited parent prefix.
//! - Parallel calls of the same tool name settle their results in start order
//!   (the `Callback` seam carries no call id into `on_tool_end`).
//! - The thread store is the authoritative record; there is no snapshot file
//!   cache for child threads (the snapshot cache is for saved sessions).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use codesmith_agent_runtime::child_journal::{
    ChildSessionDescriptor, ChildSessionEvent, ChildSessionJournal, ChildSessionJournalFactory,
    ChildSessionStatus,
};
use codesmith_agent_runtime::models::{ContentBlock, Usage};

use super::{
    CURRENT_RUNTIME_SCHEMA_VERSION, RuntimeThreadStore, RuntimeTurnStatus, ThreadRecord,
    TurnItemKind, TurnItemLifecycleStatus, TurnItemRecord, TurnRecord, summarize_text,
    turn_completed_payload,
};

/// Builds one journal per in-process child. Shared by the engine host.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeChildJournalFactory {
    store: Arc<RuntimeThreadStore>,
}

impl RuntimeChildJournalFactory {
    pub(crate) fn new(store: Arc<RuntimeThreadStore>) -> Self {
        Self { store }
    }
}

impl ChildSessionJournalFactory for RuntimeChildJournalFactory {
    fn open(&self, descriptor: ChildSessionDescriptor) -> Option<Arc<dyn ChildSessionJournal>> {
        Some(Arc::new(RuntimeChildJournal {
            store: Arc::clone(&self.store),
            descriptor,
            state: Mutex::new(JournalState::default()),
        }))
    }
}

#[derive(Default)]
struct JournalState {
    turn_id: Option<String>,
    descriptor_seq: Option<u64>,
    /// Open tool-call item ids keyed by tool name, in start order.
    open_tools: HashMap<String, VecDeque<String>>,
    finished: bool,
}

struct RuntimeChildJournal {
    store: Arc<RuntimeThreadStore>,
    descriptor: ChildSessionDescriptor,
    state: Mutex<JournalState>,
}

impl std::fmt::Debug for RuntimeChildJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeChildJournal")
            .field("agent_id", &self.descriptor.agent_id)
            .field("parent_id", &self.descriptor.parent_id)
            .finish()
    }
}

#[async_trait::async_trait]
impl ChildSessionJournal for RuntimeChildJournal {
    async fn record(&self, event: ChildSessionEvent) {
        let mut state = self.state.lock().await;
        match event {
            ChildSessionEvent::Started => self.start(&mut state).await,
            ChildSessionEvent::UserMessage { text } => self.user_message(&state, text).await,
            ChildSessionEvent::AssistantMessage { content } => {
                self.assistant_message(&state, &content).await
            }
            ChildSessionEvent::ToolStarted { id, name, input } => {
                self.tool_started(&mut state, id, name, input).await
            }
            ChildSessionEvent::ToolFinished { name, ok, content } => {
                self.tool_finished(&mut state, &name, ok, &content).await
            }
            ChildSessionEvent::ApprovalRequested {
                id,
                tool_name,
                path,
            } => {
                let Some(turn_id) = state.turn_id.as_deref() else {
                    return;
                };
                self.append(
                    turn_id,
                    "subagent.approval_requested",
                    json!({
                        "id": id,
                        "tool": tool_name,
                        "path": path.display().to_string(),
                    }),
                )
                .await;
            }
            ChildSessionEvent::ApprovalDecided { id, allowed } => {
                let Some(turn_id) = state.turn_id.as_deref() else {
                    return;
                };
                self.append(
                    turn_id,
                    "subagent.approval_decided",
                    json!({ "id": id, "allowed": allowed }),
                )
                .await;
            }
            ChildSessionEvent::Usage(usage) => self.usage(&state, usage).await,
            ChildSessionEvent::Finished {
                status,
                final_text,
                usage,
            } => self.finish(&mut state, status, final_text, usage).await,
        }
    }
}

impl RuntimeChildJournal {
    async fn start(&self, state: &mut JournalState) {
        if state.turn_id.is_some() {
            return;
        }
        let now = Utc::now();
        let parent = self.store.load_thread(&self.descriptor.parent_id).ok();
        let mut thread = parent
            .as_ref()
            .map(|parent| ThreadRecord {
                schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                id: self.descriptor.agent_id.clone(),
                created_at: now,
                updated_at: now,
                model: self.descriptor.model.clone(),
                workspace: self.descriptor.workspace.clone(),
                mode: parent.mode.clone(),
                allow_shell: parent.allow_shell,
                trust_mode: parent.trust_mode,
                auto_approve: parent.auto_approve,
                latest_turn_id: None,
                latest_response_bookmark: None,
                archived: false,
                system_prompt: None,
                task_id: parent.task_id.clone(),
                title: Some(display_name(&self.descriptor.name)),
                coherence_state: Default::default(),
                queue: Vec::new(),
                origin: Some("subagent".to_string()),
                parent_id: Some(self.descriptor.parent_id.clone()),
                agent_type: Some(self.descriptor.agent_type.clone()),
                spawn_depth: Some(self.descriptor.depth),
                subagent_mode: Some(self.descriptor.mode.as_str().to_string()),
                subagent_status: Some("running".to_string()),
                subagent_descriptor_seq: None,
            })
            .unwrap_or_else(|| ThreadRecord {
                schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                id: self.descriptor.agent_id.clone(),
                created_at: now,
                updated_at: now,
                model: self.descriptor.model.clone(),
                workspace: self.descriptor.workspace.clone(),
                mode: "agent".to_string(),
                allow_shell: false,
                trust_mode: false,
                auto_approve: false,
                latest_turn_id: None,
                latest_response_bookmark: None,
                archived: false,
                system_prompt: None,
                task_id: None,
                title: Some(display_name(&self.descriptor.name)),
                coherence_state: Default::default(),
                queue: Vec::new(),
                origin: Some("subagent".to_string()),
                parent_id: Some(self.descriptor.parent_id.clone()),
                agent_type: Some(self.descriptor.agent_type.clone()),
                spawn_depth: Some(self.descriptor.depth),
                subagent_mode: Some(self.descriptor.mode.as_str().to_string()),
                subagent_status: Some("running".to_string()),
                subagent_descriptor_seq: None,
            });
        // A child identity is a fresh run. If an id somehow already exists,
        // keep its record and open a new turn rather than overwriting the
        // durable transcript.
        if let Ok(existing) = self.store.load_thread(&thread.id) {
            thread = existing;
            thread.updated_at = now;
            thread.subagent_status = Some("running".to_string());
        }

        let turn_id = format!("turn_{}", &Uuid::new_v4().to_string()[..8]);
        let turn = TurnRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: turn_id.clone(),
            thread_id: thread.id.clone(),
            status: RuntimeTurnStatus::InProgress,
            input_summary: summarize_text(&self.descriptor.assignment, 280),
            created_at: now,
            started_at: Some(now),
            ended_at: None,
            duration_ms: None,
            usage: None,
            error: None,
            item_ids: Vec::new(),
            steer_count: 0,
            tool_catalog: None,
            base_url: None,
        };
        thread.latest_turn_id = Some(turn_id.clone());
        thread.updated_at = now;
        if let Err(err) = self.store.save_thread(&thread) {
            tracing::warn!(target: "codesmith_subagents", ?err, "child thread save failed");
        }
        if let Err(err) = self.store.save_turn(&turn) {
            tracing::warn!(target: "codesmith_subagents", ?err, "child turn save failed");
        }
        state.turn_id = Some(turn_id.clone());

        // Descriptor first: the desktop projection folds the child's identity
        // and seq from this event.
        match self
            .store
            .append_event(
                &thread.id,
                Some(&turn_id),
                None,
                "subagent.descriptor",
                json!({
                    "id": thread.id.clone(),
                    "parentSessionId": self.descriptor.parent_id.clone(),
                    "mode": self.descriptor.mode.as_str(),
                    "label": thread.title.clone(),
                    "agentType": self.descriptor.agent_type.clone(),
                    "depth": self.descriptor.depth,
                }),
            )
            .await
        {
            Ok(record) => {
                state.descriptor_seq = Some(record.seq);
                thread.subagent_descriptor_seq = Some(record.seq);
                if let Err(err) = self.store.save_thread(&thread) {
                    tracing::warn!(target: "codesmith_subagents", ?err, "child descriptor save failed");
                }
            }
            Err(err) => {
                tracing::warn!(target: "codesmith_subagents", ?err, "child descriptor append failed");
            }
        }
        self.append(&turn_id, "turn.started", json!({})).await;
        self.user_message(state, self.descriptor.assignment.clone())
            .await;
    }

    async fn user_message(&self, state: &JournalState, text: String) {
        let Some(turn_id) = state.turn_id.as_deref() else {
            return;
        };
        self.append(turn_id, "turn.steered", json!({ "input": text }))
            .await;
        let item = self.new_item(
            turn_id,
            TurnItemKind::UserMessage,
            TurnItemLifecycleStatus::Completed,
            &summarize_text(&text, 280),
            Some(text.clone()),
            None,
            true,
        );
        self.save_item(turn_id, &item).await;
        self.emit_item(turn_id, "item.started", &item, None).await;
        self.emit_item(turn_id, "item.completed", &item, None).await;
    }

    async fn assistant_message(&self, state: &JournalState, content: &[ContentBlock]) {
        let Some(turn_id) = state.turn_id.as_deref() else {
            return;
        };
        for block in content {
            match block {
                ContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                    let item = self.new_item(
                        turn_id,
                        TurnItemKind::AgentMessage,
                        TurnItemLifecycleStatus::Completed,
                        &summarize_text(text, 280),
                        Some(text.clone()),
                        None,
                        true,
                    );
                    self.save_item(turn_id, &item).await;
                    self.emit_item(turn_id, "item.started", &item, None).await;
                    self.emit_item(turn_id, "item.completed", &item, None).await;
                }
                ContentBlock::Thinking { thinking } if !thinking.trim().is_empty() => {
                    let item = self.new_item(
                        turn_id,
                        TurnItemKind::AgentReasoning,
                        TurnItemLifecycleStatus::Completed,
                        &summarize_text(thinking, 280),
                        Some(thinking.clone()),
                        None,
                        true,
                    );
                    self.save_item(turn_id, &item).await;
                    self.emit_item(turn_id, "item.started", &item, None).await;
                    self.emit_item(turn_id, "item.completed", &item, None).await;
                }
                // Tool calls are materialized by `ToolStarted`, which carries
                // the wire call id.
                _ => {}
            }
        }
    }

    async fn tool_started(
        &self,
        state: &mut JournalState,
        id: String,
        name: String,
        input: serde_json::Value,
    ) {
        let Some(turn_id) = state.turn_id.as_deref() else {
            return;
        };
        let item = self.new_item(
            turn_id,
            TurnItemKind::ToolCall,
            TurnItemLifecycleStatus::InProgress,
            &name,
            Some(name.clone()),
            Some(json!({ "arguments": input.clone(), "call_id": id })),
            false,
        );
        self.save_item(turn_id, &item).await;
        self.emit_item(
            turn_id,
            "item.started",
            &item,
            Some(json!({ "name": name, "input": input })),
        )
        .await;
        state
            .open_tools
            .entry(name)
            .or_default()
            .push_back(item.id.clone());
    }

    async fn tool_finished(&self, state: &mut JournalState, name: &str, ok: bool, content: &str) {
        let Some(turn_id) = state.turn_id.as_deref() else {
            return;
        };
        let item_id = state.open_tools.get_mut(name).and_then(VecDeque::pop_front);
        let Some(item_id) = item_id else {
            // A tool result without a recorded start (an alternate loop that
            // skips `on_llm_end`): keep the result on its own item rather
            // than dropping it.
            let item = self.new_item(
                turn_id,
                TurnItemKind::ToolCall,
                if ok {
                    TurnItemLifecycleStatus::Completed
                } else {
                    TurnItemLifecycleStatus::Failed
                },
                name,
                Some(content.to_string()),
                None,
                false,
            );
            self.save_item(turn_id, &item).await;
            self.emit_item(turn_id, "item.started", &item, None).await;
            self.emit_item(
                turn_id,
                if ok { "item.completed" } else { "item.failed" },
                &item,
                None,
            )
            .await;
            return;
        };
        let Ok(mut item) = self.store.load_item(&item_id) else {
            return;
        };
        item.status = if ok {
            TurnItemLifecycleStatus::Completed
        } else {
            TurnItemLifecycleStatus::Failed
        };
        item.detail = Some(content.to_string());
        item.ended_at = Some(Utc::now());
        self.save_item(turn_id, &item).await;
        self.emit_item(
            turn_id,
            if ok { "item.completed" } else { "item.failed" },
            &item,
            None,
        )
        .await;
    }

    async fn usage(&self, state: &JournalState, usage: Usage) {
        let Some(turn_id) = state.turn_id.as_deref() else {
            return;
        };
        if let Ok(mut turn) = self.store.load_turn(turn_id) {
            turn.usage = Some(usage.clone());
            if let Err(err) = self.store.save_turn(&turn) {
                tracing::warn!(target: "codesmith_subagents", ?err, "child usage save failed");
            }
        }
        self.append(turn_id, "subagent.usage", json!(usage)).await;
    }

    async fn finish(
        &self,
        state: &mut JournalState,
        status: ChildSessionStatus,
        final_text: Option<String>,
        usage: Usage,
    ) {
        if state.finished {
            return;
        }
        state.finished = true;
        let Some(turn_id) = state.turn_id.clone() else {
            return;
        };
        let now = Utc::now();
        // Close any tool call the child left open (cancel/failure).
        let open_items: Vec<String> = state
            .open_tools
            .drain()
            .flat_map(|(_, items)| items)
            .collect();
        for item_id in open_items {
            if let Ok(mut item) = self.store.load_item(&item_id) {
                item.status = TurnItemLifecycleStatus::Interrupted;
                item.ended_at = Some(now);
                if item.detail.as_deref().unwrap_or_default().trim().is_empty() {
                    item.detail = Some(status.as_str().to_string());
                }
                self.save_item(&turn_id, &item).await;
                self.emit_item(&turn_id, "item.failed", &item, None).await;
            }
        }

        let runtime_status = match &status {
            ChildSessionStatus::Completed => RuntimeTurnStatus::Completed,
            ChildSessionStatus::Cancelled => RuntimeTurnStatus::Canceled,
            ChildSessionStatus::Failed(_) => RuntimeTurnStatus::Failed,
        };
        if let Ok(mut turn) = self.store.load_turn(&turn_id) {
            turn.status = runtime_status;
            turn.ended_at = Some(now);
            if let Some(started_at) = turn.started_at {
                turn.duration_ms = Some(
                    now.signed_duration_since(started_at)
                        .num_milliseconds()
                        .max(0) as u64,
                );
            }
            turn.usage = Some(usage.clone());
            if let ChildSessionStatus::Failed(message) = &status {
                turn.error = Some(message.clone());
            }
            if let Err(err) = self.store.save_turn(&turn) {
                tracing::warn!(target: "codesmith_subagents", ?err, "child turn finish save failed");
            }
            self.append(
                &turn_id,
                "turn.completed",
                turn_completed_payload(&mut turn.clone()),
            )
            .await;
        }
        self.append(
            &turn_id,
            "subagent.finished",
            json!({
                "status": status.as_str(),
                "final_text": final_text,
                "usage": usage,
            }),
        )
        .await;

        if let Ok(mut thread) = self.store.load_thread(&self.descriptor.agent_id) {
            thread.subagent_status = Some(status.as_str().to_string());
            thread.updated_at = now;
            if let Err(err) = self.store.save_thread(&thread) {
                tracing::warn!(target: "codesmith_subagents", ?err, "child thread finish save failed");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn new_item(
        &self,
        turn_id: &str,
        kind: TurnItemKind,
        status: TurnItemLifecycleStatus,
        summary: &str,
        detail: Option<String>,
        metadata: Option<serde_json::Value>,
        settled: bool,
    ) -> TurnItemRecord {
        let now = Utc::now();
        TurnItemRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: format!("item_{}", &Uuid::new_v4().to_string()[..8]),
            turn_id: turn_id.to_string(),
            kind,
            status,
            summary: summary.to_string(),
            detail,
            metadata,
            artifact_refs: Vec::new(),
            started_at: Some(now),
            ended_at: settled.then_some(now),
        }
    }

    async fn save_item(&self, turn_id: &str, item: &TurnItemRecord) {
        if let Err(err) = self.store.save_item(item) {
            tracing::warn!(target: "codesmith_subagents", ?err, "child item save failed");
            return;
        }
        if let Ok(mut turn) = self.store.load_turn(turn_id)
            && !turn.item_ids.iter().any(|id| id == &item.id)
        {
            turn.item_ids.push(item.id.clone());
            if let Err(err) = self.store.save_turn(&turn) {
                tracing::warn!(target: "codesmith_subagents", ?err, "child turn item index save failed");
            }
        }
    }

    async fn emit_item(
        &self,
        turn_id: &str,
        event: &str,
        item: &TurnItemRecord,
        tool: Option<serde_json::Value>,
    ) {
        let mut payload = json!({ "item": item });
        if let Some(tool) = tool {
            payload["tool"] = tool;
        }
        if let Err(err) = self
            .store
            .append_event(
                &self.descriptor.agent_id,
                Some(turn_id),
                Some(&item.id),
                event,
                payload,
            )
            .await
        {
            tracing::warn!(
                target: "codesmith_subagents",
                agent_id = %self.descriptor.agent_id,
                %event,
                ?err,
                "child item journal append failed"
            );
        }
    }

    async fn append(&self, turn_id: &str, event: &str, payload: serde_json::Value) {
        if let Err(err) = self
            .store
            .append_event(
                &self.descriptor.agent_id,
                Some(turn_id),
                None,
                event,
                payload,
            )
            .await
        {
            tracing::warn!(
                target: "codesmith_subagents",
                agent_id = %self.descriptor.agent_id,
                %event,
                ?err,
                "child journal append failed"
            );
        }
    }
}

/// A non-empty, bounded display label for the catalog.
fn display_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "sub-agent".to_string();
    }
    summarize_text(trimmed, 120)
}

/// The journal directory's parent thread id helper (test convenience).
#[cfg(test)]
pub(crate) fn descriptor_for_test(
    agent_id: &str,
    parent_id: &str,
    mode: codesmith_agent_runtime::child_journal::ChildSessionMode,
    assignment: &str,
) -> ChildSessionDescriptor {
    ChildSessionDescriptor {
        agent_id: agent_id.to_string(),
        parent_id: parent_id.to_string(),
        agent_type: "general".to_string(),
        depth: 1,
        name: "test child".to_string(),
        mode,
        model: "deepseek-v4-flash".to_string(),
        workspace: std::path::PathBuf::from("."),
        assignment: assignment.to_string(),
    }
}
