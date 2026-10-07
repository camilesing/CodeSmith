//! Todo list shared state.
//!
//! State types extracted from `crates/tui/src/tools/todo.rs`;
//! the tool implementations stay in tui.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Status for a todo item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
        }
    }

    /// Parse a string into a todo status.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" | "inprogress" => Some(TodoStatus::InProgress),
            "completed" | "done" => Some(TodoStatus::Completed),
            _ => None,
        }
    }
}

/// A single todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: u32,
    pub content: String,
    pub status: TodoStatus,
}

/// Snapshot of a todo list for display or serialization.
#[derive(Debug, Clone, Serialize)]
pub struct TodoListSnapshot {
    pub items: Vec<TodoItem>,
    pub completion_pct: u8,
    pub in_progress_id: Option<u32>,
}

/// Mutable list of todo items with helper operations.
#[derive(Debug, Clone, Default)]
pub struct TodoList {
    items: Vec<TodoItem>,
    next_id: u32,
}

impl TodoList {
    /// Create an empty todo list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
        }
    }

    /// Event-sourcing slice 3 — the todo projection: rebuild the list from
    /// a transcript by folding every checklist tool call that actually
    /// executed (see [`super::executed_tool_call_ids`]). Mirrors the live
    /// tools registered by `with_todo_tool` (tool-impls `todo.rs`):
    /// `*_write` replaces the whole list, `*_add` appends, `*_update`
    /// updates one status, and a missing or unrecognized `status` defaults
    /// to pending — so the rebuild matches what the live list held. A write
    /// whose item has a non-string `content` is skipped (the fold keeps the
    /// last coherent state). Rebuilt ids/statuses match live semantics
    /// (`add` re-enforces the single-in-progress rule).
    pub fn rebuild_from_messages(messages: &[codesmith_agent::models::Message]) -> Self {
        use codesmith_agent::models::ContentBlock;

        let executed = super::executed_tool_call_ids(messages);

        let mut list = Self::new();
        for message in messages {
            if message.role != "assistant" {
                continue;
            }
            for block in &message.content {
                let ContentBlock::ToolUse {
                    id, name, input, ..
                } = block
                else {
                    continue;
                };
                if !executed.contains(id.as_str()) {
                    continue;
                }
                match name.as_str() {
                    "todo_write" | "checklist_write" => {
                        let Some(todos) = input.get("todos").and_then(|t| t.as_array()) else {
                            continue;
                        };
                        let mut rebuilt = Self::new();
                        let mut coherent = true;
                        for item in todos {
                            let Some(content) = item.get("content").and_then(|v| v.as_str()) else {
                                coherent = false;
                                break;
                            };
                            let status = item
                                .get("status")
                                .and_then(|v| v.as_str())
                                .and_then(TodoStatus::from_str)
                                .unwrap_or(TodoStatus::Pending);
                            rebuilt.add(content.to_string(), status);
                        }
                        if coherent {
                            list = rebuilt;
                        }
                    }
                    "todo_add" | "checklist_add" => {
                        let Some(content) = input.get("content").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let status = input
                            .get("status")
                            .and_then(|v| v.as_str())
                            .and_then(TodoStatus::from_str)
                            .unwrap_or(TodoStatus::Pending);
                        list.add(content.to_string(), status);
                    }
                    "todo_update" | "checklist_update" => {
                        let (Some(item_id), Some(status)) = (
                            input
                                .get("id")
                                .and_then(|v| v.as_u64())
                                .and_then(|v| u32::try_from(v).ok()),
                            input
                                .get("status")
                                .and_then(|v| v.as_str())
                                .and_then(TodoStatus::from_str),
                        ) else {
                            continue;
                        };
                        list.update_status(item_id, status);
                    }
                    _ => continue,
                }
            }
        }
        list
    }

    /// Return a snapshot of the list with computed metrics.
    #[must_use]
    pub fn snapshot(&self) -> TodoListSnapshot {
        TodoListSnapshot {
            items: self.items.clone(),
            completion_pct: self.completion_percentage(),
            in_progress_id: self.in_progress_id(),
        }
    }

    /// Add a new todo item.
    pub fn add(&mut self, content: String, status: TodoStatus) -> TodoItem {
        let status = match status {
            TodoStatus::InProgress => {
                self.set_single_in_progress(None);
                TodoStatus::InProgress
            }
            other => other,
        };

        let item = TodoItem {
            id: self.next_id,
            content,
            status,
        };
        self.next_id += 1;
        self.items.push(item.clone());
        item
    }

    /// Update an item's status by id.
    pub fn update_status(&mut self, id: u32, status: TodoStatus) -> Option<TodoItem> {
        let mut updated: Option<TodoItem> = None;
        if status == TodoStatus::InProgress {
            self.set_single_in_progress(Some(id));
        }
        for item in &mut self.items {
            if item.id == id {
                item.status = status;
                updated = Some(item.clone());
                break;
            }
        }
        updated
    }

    /// Compute completion percentage for the list.
    #[must_use]
    pub fn completion_percentage(&self) -> u8 {
        if self.items.is_empty() {
            return 0;
        }
        let total = self.items.len();
        let completed = self
            .items
            .iter()
            .filter(|item| item.status == TodoStatus::Completed)
            .count();
        let percent = completed.saturating_mul(100);
        let percent = (percent + total / 2) / total;
        u8::try_from(percent).unwrap_or(u8::MAX)
    }

    /// Return the id of the in-progress item, if any.
    #[must_use]
    pub fn in_progress_id(&self) -> Option<u32> {
        self.items
            .iter()
            .find(|item| item.status == TodoStatus::InProgress)
            .map(|item| item.id)
    }

    /// Clear all todo items.
    pub fn clear(&mut self) {
        self.items.clear();
        self.next_id = 1;
    }

    fn set_single_in_progress(&mut self, allow_id: Option<u32>) {
        for item in &mut self.items {
            if Some(item.id) != allow_id && item.status == TodoStatus::InProgress {
                item.status = TodoStatus::Pending;
            }
        }
    }
}

/// Shared reference to a `TodoList` for use across tools
pub type SharedTodoList = Arc<Mutex<TodoList>>;

/// Create a new shared `TodoList`
pub fn new_shared_todo_list() -> SharedTodoList {
    Arc::new(Mutex::new(TodoList::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{assistant_tool_use, tool_result};

    #[test]
    fn todo_projection_last_write_wins() {
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "todo_write",
                serde_json::json!({"todos": [
                    {"content": "a", "status": "pending"},
                    {"content": "b", "status": "in_progress"},
                ]}),
            ),
            tool_result("call_2", false),
            assistant_tool_use(
                "call_2",
                "checklist_write",
                serde_json::json!({"todos": [
                    {"content": "a", "status": "completed"},
                    {"content": "c", "status": "pending"},
                ]}),
            ),
        ];
        let list = TodoList::rebuild_from_messages(&messages);
        let snap = list.snapshot();
        assert_eq!(snap.items.len(), 2);
        assert_eq!(snap.items[0].content, "a");
        assert!(snap.items[0].status == TodoStatus::Completed);
        assert_eq!(snap.items[1].content, "c");
    }

    #[test]
    fn todo_projection_defaults_unrecognized_status_like_live_tool() {
        // The live TodoWriteTool adds the item as pending when `status` is
        // missing or unrecognized — the rebuild must match, not skip the
        // write (that would restore a stale list on reload).
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "todo_write",
                serde_json::json!({"todos": [{"content": "ok", "status": "pending"}]}),
            ),
            tool_result("call_2", false),
            assistant_tool_use(
                "call_2",
                "todo_write",
                serde_json::json!({"todos": [{"content": "bad", "status": "wat"}]}),
            ),
        ];
        let list = TodoList::rebuild_from_messages(&messages);
        let snap = list.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].content, "bad");
        assert!(snap.items[0].status == TodoStatus::Pending);
    }

    #[test]
    fn todo_projection_folds_add_and_update() {
        // `with_todo_tool` registers add/update against the same shared
        // list; a session that used them must reload to the same state.
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "checklist_write",
                serde_json::json!({"todos": [
                    {"content": "a", "status": "pending"},
                    {"content": "b", "status": "pending"},
                ]}),
            ),
            tool_result("call_2", false),
            assistant_tool_use(
                "call_2",
                "checklist_add",
                serde_json::json!({"content": "c"}),
            ),
            tool_result("call_3", false),
            assistant_tool_use(
                "call_3",
                "todo_update",
                serde_json::json!({"id": 1, "status": "in_progress"}),
            ),
        ];
        let list = TodoList::rebuild_from_messages(&messages);
        let snap = list.snapshot();
        assert_eq!(snap.items.len(), 3);
        assert_eq!(snap.items[2].content, "c");
        assert!(snap.items[0].status == TodoStatus::InProgress);
        assert_eq!(snap.in_progress_id, Some(1));
    }

    #[test]
    fn todo_projection_skips_errored_and_unpaired_calls() {
        // Blocked/errored calls carry an error result; an interrupted turn
        // persists the ToolUse with no result at all. Neither mutated the
        // live list, so neither folds.
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "todo_write",
                serde_json::json!({"todos": [{"content": "ok", "status": "pending"}]}),
            ),
            // Errored write: not folded.
            tool_result("call_2", true),
            assistant_tool_use(
                "call_2",
                "todo_write",
                serde_json::json!({"todos": [{"content": "err", "status": "pending"}]}),
            ),
            // Errored add: not folded.
            tool_result("call_3", true),
            assistant_tool_use(
                "call_3",
                "checklist_add",
                serde_json::json!({"content": "err-add"}),
            ),
            // Unpaired write (interrupted turn): not folded.
            assistant_tool_use(
                "call_4",
                "todo_write",
                serde_json::json!({"todos": [{"content": "dangling", "status": "pending"}]}),
            ),
            // Unrelated tool calls and user messages are ignored.
            assistant_tool_use("call_5", "read_file", serde_json::json!({"path": "x"})),
            codesmith_agent::models::Message {
                role: "user".to_string(),
                content: vec![codesmith_agent::models::ContentBlock::Text {
                    text: "hi".to_string(),
                    cache_control: None,
                }],
            },
        ];
        let list = TodoList::rebuild_from_messages(&messages);
        let snap = list.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].content, "ok");
    }

    #[test]
    fn todo_projection_skips_write_with_non_string_content() {
        let messages = vec![
            tool_result("call_1", false),
            assistant_tool_use(
                "call_1",
                "todo_write",
                serde_json::json!({"todos": [{"content": "ok", "status": "pending"}]}),
            ),
            tool_result("call_2", false),
            assistant_tool_use(
                "call_2",
                "todo_write",
                serde_json::json!({"todos": [{"content": 42, "status": "pending"}]}),
            ),
        ];
        let list = TodoList::rebuild_from_messages(&messages);
        let snap = list.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].content, "ok");
    }
}
