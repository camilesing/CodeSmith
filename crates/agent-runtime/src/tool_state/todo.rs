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
    /// a transcript. Every `todo_write` / `checklist_write` call replaces
    /// the whole list, so the fold is last-write-wins over assistant
    /// `ToolUse` blocks. A malformed write is skipped (the fold keeps the
    /// last coherent state) — recorded history is evidence, not a veto.
    /// Rebuilt ids/statuses match live semantics (`add` re-enforces the
    /// single-in-progress rule).
    pub fn rebuild_from_messages(messages: &[codesmith_agent::models::Message]) -> Self {
        use codesmith_agent::models::ContentBlock;

        let mut list = Self::new();
        for message in messages {
            if message.role != "assistant" {
                continue;
            }
            for block in &message.content {
                let ContentBlock::ToolUse { name, input, .. } = block else {
                    continue;
                };
                if name != "todo_write" && name != "checklist_write" {
                    continue;
                }
                let Some(todos) = input.get("todos").and_then(|t| t.as_array()) else {
                    continue;
                };
                let mut rebuilt = Self::new();
                let mut coherent = true;
                for item in todos {
                    let (Some(content), Some(status)) = (
                        item.get("content").and_then(|v| v.as_str()),
                        item.get("status")
                            .and_then(|v| v.as_str())
                            .and_then(TodoStatus::from_str),
                    ) else {
                        coherent = false;
                        break;
                    };
                    rebuilt.add(content.to_string(), status);
                }
                if coherent {
                    list = rebuilt;
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

    /// Transcript helper: an assistant message carrying one tool call.
    #[cfg(test)]
    pub(crate) fn assistant_tool_use(
        name: &str,
        input: serde_json::Value,
    ) -> codesmith_agent::models::Message {
        codesmith_agent::models::Message {
            role: "assistant".to_string(),
            content: vec![codesmith_agent::models::ContentBlock::ToolUse {
                id: "call_x".to_string(),
                name: name.to_string(),
                input,
                caller: None,
            }],
        }
    }

    #[test]
    fn todo_projection_last_write_wins() {
        let messages = vec![
            assistant_tool_use(
                "todo_write",
                serde_json::json!({"todos": [
                    {"content": "a", "status": "pending"},
                    {"content": "b", "status": "in_progress"},
                ]}),
            ),
            assistant_tool_use(
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
    fn todo_projection_skips_malformed_write() {
        let messages = vec![
            assistant_tool_use(
                "todo_write",
                serde_json::json!({"todos": [{"content": "ok", "status": "pending"}]}),
            ),
            // Malformed: unknown status — the fold keeps the last coherent list.
            assistant_tool_use(
                "todo_write",
                serde_json::json!({"todos": [{"content": "bad", "status": "wat"}]}),
            ),
            // Unrelated tool calls and user messages are ignored.
            codesmith_agent::models::Message {
                role: "user".to_string(),
                content: vec![codesmith_agent::models::ContentBlock::Text {
                    text: "hi".to_string(),
                    cache_control: None,
                }],
            },
            assistant_tool_use("read_file", serde_json::json!({"path": "x"})),
        ];
        let list = TodoList::rebuild_from_messages(&messages);
        let snap = list.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].content, "ok");
    }
}
