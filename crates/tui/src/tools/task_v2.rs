//! Task V2 — conversation-scoped task tracking with file-based persistence.
//!
//! Distinct from `task_manager.rs` (durable background jobs), Task V2 tracks
//! in-conversation tasks with pending→in_progress→completed workflow, file
//! persistence, concurrent-safe access via flock, and dependency tracking.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

// === Types ===

/// Task V2 status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskV2Status {
    Pending,
    InProgress,
    Completed,
}

impl TaskV2Status {
    fn from_str_opt(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "in_progress" | "inprogress" => Some(Self::InProgress),
            "completed" | "done" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// A single Task V2 record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskV2Record {
    pub id: String,
    pub subject: String,
    #[serde(default)]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    pub status: TaskV2Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// File-based task list manager with concurrent-safe access.
///
/// Task files live at `~/.codewhale/tasks/{task_list_id}/{task_id}.json`.
/// A `.highwatermark` file prevents ID reuse after deletion.
/// A `.lock` file provides flock-based concurrent access.
#[derive(Debug)]
pub struct TaskV2Manager {
    task_dir: PathBuf,
}

impl TaskV2Manager {
    /// Create a new TaskV2Manager for a given task list ID (typically session ID).
    pub fn new(task_list_id: &str) -> anyhow::Result<Self> {
        let base = codewhale_config::codewhale_home()?;
        let task_dir = base.join("tasks").join(task_list_id);
        fs::create_dir_all(&task_dir)?;
        Ok(Self { task_dir })
    }

    fn lock_path(&self) -> PathBuf {
        self.task_dir.join(".lock")
    }

    fn task_file(&self, id: &str) -> PathBuf {
        self.task_dir.join(format!("{id}.json"))
    }

    fn highwatermark_file(&self) -> PathBuf {
        self.task_dir.join(".highwatermark")
    }

    fn read_highwatermark(&self) -> u64 {
        let path = self.highwatermark_file();
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
    }

    fn write_highwatermark(&self, value: u64) -> anyhow::Result<()> {
        fs::write(self.highwatermark_file(), value.to_string())?;
        Ok(())
    }

    fn read_task_file(&self, id: &str) -> anyhow::Result<TaskV2Record> {
        let path = self.task_file(id);
        let content = fs::read_to_string(&path)?;
        let record: TaskV2Record = serde_json::from_str(&content)?;
        Ok(record)
    }

    fn write_task_file(&self, record: &TaskV2Record) -> anyhow::Result<()> {
        let path = self.task_file(&record.id);
        let content = serde_json::to_string_pretty(record)?;
        fs::write(&path, content)?;
        Ok(())
    }

    /// Find the highest task ID across existing files and the high water mark.
    fn find_highest_id(&self) -> u64 {
        let hwm = self.read_highwatermark();
        let mut max_file_id: u64 = 0;
        if let Ok(entries) = fs::read_dir(&self.task_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".json") && !name.starts_with('.') {
                    let id_str = name.trim_end_matches(".json");
                    if let Ok(id) = id_str.parse::<u64>() {
                        max_file_id = max_file_id.max(id);
                    }
                }
            }
        }
        hwm.max(max_file_id)
    }

    /// Acquire exclusive flock for mutation operations.
    fn acquire_write_lock(&self) -> anyhow::Result<RwLock<fs::File>> {
        let lock_path = self.lock_path();
        // Ensure lock file exists
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)?;
        let file = fs::File::open(&lock_path)?;
        Ok(RwLock::new(file))
    }

    /// Create a new task. Returns the task ID.
    pub fn create_task(
        &mut self,
        subject: String,
        description: String,
        active_form: Option<String>,
        status: Option<TaskV2Status>,
        owner: Option<String>,
        blocked_by: Vec<String>,
        metadata: Option<serde_json::Value>,
    ) -> anyhow::Result<String> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        let highest = self.find_highest_id();
        let id = highest + 1;
        self.write_highwatermark(id)?;

        let now = Utc::now();
        let status = status.unwrap_or(TaskV2Status::Pending);
        let started_at = if status == TaskV2Status::InProgress {
            Some(now)
        } else {
            None
        };

        let record = TaskV2Record {
            id: id.to_string(),
            subject,
            description,
            active_form,
            status,
            owner,
            blocked_by,
            metadata: metadata.unwrap_or(json!({})),
            created_at: now,
            started_at,
            completed_at: None,
        };

        self.write_task_file(&record)?;
        Ok(id.to_string())
    }

    /// Update an existing task.
    pub fn update_task(
        &mut self,
        id: &str,
        status: Option<TaskV2Status>,
        owner: Option<String>,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<Option<String>>,
        metadata_merge: Option<serde_json::Value>,
    ) -> anyhow::Result<TaskV2Record> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        let record = self.read_task_file(id)?;
        let now = Utc::now();

        let mut updated = record.clone();

        if let Some(s) = status {
            // Track timing transitions
            if record.status == TaskV2Status::Pending && s == TaskV2Status::InProgress {
                updated.started_at = Some(now);
            }
            if s == TaskV2Status::Completed && record.status != TaskV2Status::Completed {
                updated.completed_at = Some(now);
            }
            updated.status = s;
        }

        if let Some(o) = owner {
            updated.owner = Some(o);
        }

        if let Some(s) = subject {
            updated.subject = s;
        }

        if let Some(d) = description {
            updated.description = d;
        }

        if let Some(a) = active_form {
            updated.active_form = a;
        }

        if let Some(merge) = metadata_merge {
            // Merge metadata keys
            if let serde_json::Value::Object(existing) = &mut updated.metadata {
                if let serde_json::Value::Object(new_vals) = merge {
                    for (k, v) in new_vals {
                        if v.is_null() {
                            existing.remove(&k);
                        } else {
                            existing.insert(k, v);
                        }
                    }
                }
            }
        }

        self.write_task_file(&updated)?;
        Ok(updated)
    }

    /// Get a single task by ID.
    pub fn get_task(&self, id: &str) -> anyhow::Result<TaskV2Record> {
        self.read_task_file(id)
    }

    /// List all tasks, sorted by ID.
    pub fn list_tasks(&self) -> anyhow::Result<Vec<TaskV2Record>> {
        let mut tasks: Vec<TaskV2Record> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.task_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".json") && !name.starts_with('.') {
                    let id_str = name.trim_end_matches(".json");
                    if let Ok(record) = self.read_task_file(id_str) {
                        tasks.push(record);
                    }
                }
            }
        }
        tasks.sort_by(|a, b| {
            let a_id: u64 = a.id.parse().unwrap_or(0);
            let b_id: u64 = b.id.parse().unwrap_or(0);
            a_id.cmp(&b_id)
        });
        Ok(tasks)
    }

    /// Delete a task by ID.
    pub fn delete_task(&mut self, id: &str) -> anyhow::Result<()> {
        let path = self.task_file(id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

/// Shared reference to TaskV2Manager.
pub type SharedTaskV2Manager = Arc<tokio::sync::Mutex<TaskV2Manager>>;

/// Create a new shared TaskV2Manager.
pub fn new_shared_task_v2_manager(task_list_id: &str) -> anyhow::Result<SharedTaskV2Manager> {
    let manager = TaskV2Manager::new(task_list_id)?;
    Ok(Arc::new(tokio::sync::Mutex::new(manager)))
}

// === Verification Nudge ===

/// Track whether tasks were completed without a verification step.
/// Threshold = 3: after 3 consecutive completions without verification,
/// a nudge message is injected.
const VERIFICATION_NUDGE_THRESHOLD: u32 = 3;

/// Render the verification nudge message.
pub fn render_verification_nudge(completed_count: u32) -> String {
    format!(
        "You've completed {completed_count} tasks without running a verification step. \
         Consider calling `run_tests` or similar verification tools to validate your work \
         before continuing. This is a suggestion, not a requirement."
    )
}

/// Check whether a verification nudge should be emitted based on task list state.
/// Returns the count of completed tasks without verification if >= threshold.
pub fn should_emit_verification_nudge(manager: &TaskV2Manager) -> Option<u32> {
    let tasks = manager.list_tasks().ok()?;
    let completed_count = tasks
        .iter()
        .filter(|t| t.status == TaskV2Status::Completed)
        .count() as u32;

    // Check if any task subject/description mentions verification
    let has_verification = tasks.iter().any(|t| {
        let text = format!("{} {}", t.subject, t.description).to_lowercase();
        text.contains("verif") || text.contains("test") || text.contains("check")
    });

    if completed_count >= VERIFICATION_NUDGE_THRESHOLD && !has_verification {
        Some(completed_count)
    } else {
        None
    }
}

// === TaskV2CreateTool ===

pub struct TaskV2CreateTool {
    manager: SharedTaskV2Manager,
}

impl TaskV2CreateTool {
    pub fn new(manager: SharedTaskV2Manager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSpec for TaskV2CreateTool {
    fn name(&self) -> &'static str {
        "task_create_v2"
    }

    fn description(&self) -> &'static str {
        "Create a structured task with subject, description, and optional dependencies. \
         Tasks persist across turns and support the pending -> in_progress -> completed workflow."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "subject": {
                    "type": "string",
                    "description": "Brief imperative title for the task"
                },
                "description": {
                    "type": "string",
                    "description": "Detailed requirements and context"
                },
                "active_form": {
                    "type": "string",
                    "description": "Present continuous form for spinner display (e.g., 'Running tests')"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "default": "pending",
                    "description": "Initial status"
                },
                "owner": {
                    "type": "string",
                    "description": "Agent name assigned to this task"
                },
                "blocked_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs that must complete before this task"
                },
                "metadata": {
                    "type": "object",
                    "description": "Arbitrary key-value metadata"
                }
            },
            "required": ["subject", "description"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let subject = input
            .get("subject")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("subject"))?
            .to_string();

        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let active_form = input
            .get("active_form")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let status_str = input
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        let status = TaskV2Status::from_str_opt(status_str).unwrap_or(TaskV2Status::Pending);

        let owner = input
            .get("owner")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let blocked_by: Vec<String> = input
            .get("blocked_by")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let metadata = input.get("metadata").cloned();

        let subject_display = subject.clone();
        let mut manager = self.manager.lock().await;
        let task_id = manager
            .create_task(subject, description, active_form, Some(status), owner, blocked_by, metadata)
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to create task: {e}"),
            })?;

        Ok(ToolResult::success(format!(
            "Created task {task_id}: {subject_display}"
        )))
    }
}

// === TaskV2UpdateTool ===

pub struct TaskV2UpdateTool {
    manager: SharedTaskV2Manager,
}

impl TaskV2UpdateTool {
    pub fn new(manager: SharedTaskV2Manager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSpec for TaskV2UpdateTool {
    fn name(&self) -> &'static str {
        "task_update_v2"
    }

    fn description(&self) -> &'static str {
        "Update a task's status, owner, description, or metadata. \
         When marking completed, consider running verification first."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "New status"
                },
                "owner": {
                    "type": "string",
                    "description": "New owner agent name"
                },
                "subject": {
                    "type": "string",
                    "description": "New subject/title"
                },
                "description": {
                    "type": "string",
                    "description": "New description"
                },
                "active_form": {
                    "type": "string",
                    "description": "New present-continuous form for spinner, or null to clear"
                },
                "metadata": {
                    "type": "object",
                    "description": "Metadata keys to merge (null values delete keys)"
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let task_id = input
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("task_id"))?
            .to_string();

        let status = input
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(TaskV2Status::from_str_opt);

        let owner = input
            .get("owner")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let subject = input
            .get("subject")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let active_form = input
            .get("active_form")
            .map(|v| {
                if v.is_null() {
                    Some(None)
                } else {
                    v.as_str().map(|s| Some(s.to_string()))
                }
            })
            .flatten();

        let metadata_merge = input.get("metadata").cloned();

        let mut manager = self.manager.lock().await;
        let updated = manager
            .update_task(
                &task_id,
                status,
                owner,
                subject,
                description,
                active_form,
                metadata_merge,
            )
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to update task {task_id}: {e}"),
            })?;

        Ok(ToolResult::success(format!(
            "Task {} [{}] updated to {}",
            updated.id, updated.subject, serde_json::to_string(&updated.status).unwrap_or_default()
        )))
    }
}

// === TaskV2GetTool ===

pub struct TaskV2GetTool {
    manager: SharedTaskV2Manager,
}

impl TaskV2GetTool {
    pub fn new(manager: SharedTaskV2Manager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSpec for TaskV2GetTool {
    fn name(&self) -> &'static str {
        "task_get_v2"
    }

    fn description(&self) -> &'static str {
        "Get full details of a task by ID."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID to retrieve"
                }
            },
            "required": ["task_id"],
            "additionalProperties": false
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
        let task_id = input
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("task_id"))?;

        let manager = self.manager.lock().await;
        let record = manager
            .get_task(task_id)
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to get task {task_id}: {e}"),
            })?;

        let content = serde_json::to_string_pretty(&record)
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to serialize task: {e}"),
            })?;

        Ok(ToolResult::success(content))
    }
}

// === TaskV2ListTool ===

pub struct TaskV2ListTool {
    manager: SharedTaskV2Manager,
}

impl TaskV2ListTool {
    pub fn new(manager: SharedTaskV2Manager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSpec for TaskV2ListTool {
    fn name(&self) -> &'static str {
        "task_list_v2"
    }

    fn description(&self) -> &'static str {
        "List all tasks with summary information. Tasks are sorted by ID."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
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
        _input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let manager = self.manager.lock().await;
        let tasks = manager
            .list_tasks()
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to list tasks: {e}"),
            })?;

        if tasks.is_empty() {
            return Ok(ToolResult::success("No tasks."));
        }

        let mut lines: Vec<String> = Vec::new();
        for t in &tasks {
            let status_sym = match t.status {
                TaskV2Status::Pending => "○",
                TaskV2Status::InProgress => "◎",
                TaskV2Status::Completed => "●",
            };
            let owner_str = t.owner.as_deref().unwrap_or("unassigned");
            let blocked = if t.blocked_by.is_empty() {
                String::new()
            } else {
                format!(" (blocked by: {})", t.blocked_by.join(", "))
            };
            lines.push(format!(
                "{} {} [{}] {} — {}{blocked}",
                status_sym, t.id, t.subject, serde_json::to_string(&t.status).unwrap_or_default(), owner_str
            ));
        }

        Ok(ToolResult::success(lines.join("\n")))
    }
}