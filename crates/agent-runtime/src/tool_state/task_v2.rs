//! State types extracted from `crates/tui/src/tools/task_v2.rs`.
//! Tool implementations stay in tui.

use chrono::{DateTime, Utc};
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

/// Task V2 status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskV2Status {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

impl TaskV2Status {
    pub fn from_str_opt(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "in_progress" | "inprogress" => Some(Self::InProgress),
            "completed" | "done" => Some(Self::Completed),
            "deleted" => Some(Self::Deleted),
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
    pub blocks: Vec<String>,
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
/// Task files live at `~/.codesmith/tasks/{task_list_id}/{task_id}.json`.
/// A `.highwatermark` file prevents ID reuse after deletion.
/// A `.lock` file provides flock-based concurrent access.
#[derive(Debug)]
pub struct TaskV2Manager {
    task_dir: PathBuf,
}

impl TaskV2Manager {
    /// Create a new TaskV2Manager for a given task list ID (typically session ID).
    pub fn new(task_list_id: &str) -> anyhow::Result<Self> {
        let base = codesmith_config::codesmith_home()?;
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
    /// When `blocked_by` references other tasks, their `blocks` arrays are
    /// updated to include the new task's ID (bidirectional dependency).
    pub fn create_task(
        &mut self,
        subject: String,
        description: String,
        active_form: Option<String>,
        status: Option<TaskV2Status>,
        owner: Option<String>,
        blocked_by: Vec<String>,
        blocks: Vec<String>,
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

        let id_str = id.to_string();
        let record = TaskV2Record {
            id: id_str.clone(),
            subject,
            description,
            active_form,
            status,
            owner,
            blocked_by: blocked_by.clone(),
            blocks: blocks.clone(),
            metadata: metadata.unwrap_or(json!({})),
            created_at: now,
            started_at,
            completed_at: None,
        };

        self.write_task_file(&record)?;

        // Maintain bidirectional links: update blockers' `blocks` arrays
        for blocker_id in &blocked_by {
            if let Ok(mut blocker) = self.read_task_file(blocker_id) {
                if !blocker.blocks.contains(&id_str) {
                    blocker.blocks.push(id_str.clone());
                    self.write_task_file(&blocker)?;
                }
            }
        }

        // Maintain bidirectional links: update blocked tasks' `blocked_by` arrays
        for blocked_id in &blocks {
            if let Ok(mut blocked) = self.read_task_file(blocked_id) {
                if !blocked.blocked_by.contains(&id_str) {
                    blocked.blocked_by.push(id_str.clone());
                    self.write_task_file(&blocked)?;
                }
            }
        }

        Ok(id_str)
    }

    /// Update an existing task. Supports dependency additions via `add_blocks`
    /// and `add_blocked_by`, which maintain bidirectional links automatically.
    pub fn update_task(
        &mut self,
        id: &str,
        status: Option<TaskV2Status>,
        owner: Option<String>,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<Option<String>>,
        metadata_merge: Option<serde_json::Value>,
        add_blocks: Option<Vec<String>>,
        add_blocked_by: Option<Vec<String>>,
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

        // Add blocks: update this task's `blocks` and the blocked tasks' `blocked_by`
        if let Some(new_blocks) = add_blocks {
            for block_id in &new_blocks {
                if !updated.blocks.contains(block_id) {
                    updated.blocks.push(block_id.clone());
                    if let Ok(mut blocked) = self.read_task_file(block_id) {
                        if !blocked.blocked_by.contains(&updated.id) {
                            blocked.blocked_by.push(updated.id.clone());
                            self.write_task_file(&blocked)?;
                        }
                    }
                }
            }
        }

        // Add blocked_by: update this task's `blocked_by` and the blockers' `blocks`
        if let Some(new_blocked_by) = add_blocked_by {
            for blocker_id in &new_blocked_by {
                if !updated.blocked_by.contains(blocker_id) {
                    updated.blocked_by.push(blocker_id.clone());
                    if let Ok(mut blocker) = self.read_task_file(blocker_id) {
                        if !blocker.blocks.contains(&updated.id) {
                            blocker.blocks.push(updated.id.clone());
                            self.write_task_file(&blocker)?;
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

    /// List all tasks, sorted by ID. Excludes soft-deleted tasks.
    pub fn list_tasks(&self) -> anyhow::Result<Vec<TaskV2Record>> {
        self.list_tasks_inner()
    }

    /// List all tasks without acquiring a lock (caller must hold the write lock).
    /// Excludes deleted tasks.
    fn list_tasks_inner(&self) -> anyhow::Result<Vec<TaskV2Record>> {
        let mut tasks: Vec<TaskV2Record> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.task_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".json") && !name.starts_with('.') {
                    let id_str = name.trim_end_matches(".json");
                    if let Ok(record) = self.read_task_file(id_str) {
                        if record.status != TaskV2Status::Deleted {
                            tasks.push(record);
                        }
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

    /// Soft-delete a task: clean up references in other tasks, then physically
    /// remove the task file. The high-water mark prevents ID reuse.
    pub fn soft_delete_task(&mut self, id: &str) -> anyhow::Result<()> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        if let Ok(record) = self.read_task_file(id) {
            // Remove this task's ID from blockers' `blocks` arrays
            for blocker_id in &record.blocked_by {
                if let Ok(mut blocker) = self.read_task_file(blocker_id) {
                    blocker.blocks.retain(|b| b != id);
                    self.write_task_file(&blocker)?;
                }
            }
            // Remove this task's ID from blocked tasks' `blocked_by` arrays
            for blocked_id in &record.blocks {
                if let Ok(mut blocked) = self.read_task_file(blocked_id) {
                    blocked.blocked_by.retain(|b| b != id);
                    self.write_task_file(&blocked)?;
                }
            }
        }

        let path = self.task_file(id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Delete a task by ID (raw physical deletion without reference cleanup).
    pub fn delete_task(&mut self, id: &str) -> anyhow::Result<()> {
        let path = self.task_file(id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Claim a task by setting the owner. Fails if the agent already owns
    /// an in-progress task in this task list (busy check).
    pub fn claim_task(&mut self, id: &str, agent_name: &str) -> anyhow::Result<TaskV2Record> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        // Busy check: does the agent already own an in-progress task?
        let tasks = self.list_tasks_inner()?;
        let busy = tasks.iter().any(|t| {
            t.owner.as_deref() == Some(agent_name)
                && t.status == TaskV2Status::InProgress
                && t.id != id
        });
        if busy {
            return Err(anyhow::anyhow!(
                "Agent '{}' already has an in-progress task; cannot claim task {}",
                agent_name,
                id
            ));
        }

        let mut record = self.read_task_file(id)?;
        record.owner = Some(agent_name.to_string());
        self.write_task_file(&record)?;
        Ok(record)
    }

    /// Unassign all tasks owned by the given agent (set owner to None).
    /// Used for swarm cleanup when a teammate departs.
    pub fn unassign_teammate_tasks(&mut self, agent_name: &str) -> anyhow::Result<usize> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        let mut tasks = self.list_tasks_inner()?;
        let mut count = 0;
        for task in &mut tasks {
            if task.owner.as_deref() == Some(agent_name) {
                task.owner = None;
                self.write_task_file(task)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

/// Shared reference to TaskV2Manager.
pub type SharedTaskV2Manager = Arc<tokio::sync::Mutex<TaskV2Manager>>;

/// Create a new shared TaskV2Manager.
pub fn new_shared_task_v2_manager(task_list_id: &str) -> anyhow::Result<SharedTaskV2Manager> {
    let manager = TaskV2Manager::new(task_list_id)?;
    Ok(Arc::new(tokio::sync::Mutex::new(manager)))
}

/// Render the verification nudge message.
pub fn render_verification_nudge(completed_count: u32) -> String {
    format!(
        "You've completed {completed_count} tasks without running a verification step. \
         Consider calling `run_tests` or similar verification tools to validate your work \
         before continuing. This is a suggestion, not a requirement."
    )
}

/// Track whether tasks were completed without a verification step.
/// Threshold = 3: after 3 consecutive completions without verification,
/// a nudge message is injected.
const VERIFICATION_NUDGE_THRESHOLD: u32 = 3;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_manager() -> TaskV2Manager {
        let dir = tempfile::tempdir().expect("tempdir").into_path();
        let task_dir = dir.join("tasks").join("test_session");
        fs::create_dir_all(&task_dir).expect("create task dir");
        TaskV2Manager { task_dir }
    }

    #[test]
    fn create_task_returns_incrementing_ids() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "Task A".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let id2 = mgr
            .create_task(
                "Task B".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let n1: u64 = id1.parse().unwrap();
        let n2: u64 = id2.parse().unwrap();
        assert_eq!(n2, n1 + 1);
    }

    #[test]
    fn create_task_with_blocked_by_updates_blocks() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "Blocker".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let id2 = mgr
            .create_task(
                "Blocked".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![id1.clone()],
                vec![],
                None,
            )
            .unwrap();

        let blocker = mgr.get_task(&id1).unwrap();
        assert!(blocker.blocks.contains(&id2));
    }

    #[test]
    fn create_task_with_blocks_updates_blocked_by() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "First".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let id2 = mgr
            .create_task(
                "Second".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![id1.clone()],
                None,
            )
            .unwrap();

        let blocked = mgr.get_task(&id1).unwrap();
        assert!(blocked.blocked_by.contains(&id2));
    }

    #[test]
    fn update_task_add_blocks_bidirectional() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "A".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let id2 = mgr
            .create_task(
                "B".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();

        let updated = mgr
            .update_task(
                &id1,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(vec![id2.clone()]),
                None,
            )
            .unwrap();

        assert!(updated.blocks.contains(&id2));
        let task_b = mgr.get_task(&id2).unwrap();
        assert!(task_b.blocked_by.contains(&id1));
    }

    #[test]
    fn update_task_add_blocked_by_bidirectional() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "A".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let id2 = mgr
            .create_task(
                "B".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();

        let updated = mgr
            .update_task(
                &id2,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(vec![id1.clone()]),
            )
            .unwrap();

        assert!(updated.blocked_by.contains(&id1));
        let task_a = mgr.get_task(&id1).unwrap();
        assert!(task_a.blocks.contains(&id2));
    }

    #[test]
    fn soft_delete_cleans_up_references() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "Blocker".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let id2 = mgr
            .create_task(
                "Blocked".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![id1.clone()],
                vec![],
                None,
            )
            .unwrap();

        // Before delete: blocker has task2 in blocks
        let before = mgr.get_task(&id1).unwrap();
        assert!(before.blocks.contains(&id2));

        mgr.soft_delete_task(&id2).unwrap();

        // After delete: blocker's blocks no longer references deleted task
        let after = mgr.get_task(&id1).unwrap();
        assert!(!after.blocks.contains(&id2));

        // Deleted task file is physically removed
        assert!(mgr.get_task(&id2).is_err());
    }

    #[test]
    fn claim_task_fails_when_agent_busy() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "Busy task".into(),
                "desc".into(),
                None,
                Some(TaskV2Status::InProgress),
                Some("agent_a".into()),
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let id2 = mgr
            .create_task(
                "New task".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();

        let result = mgr.claim_task(&id2, "agent_a");
        assert!(result.is_err());
    }

    #[test]
    fn claim_task_succeeds_when_agent_free() {
        let mut mgr = temp_manager();
        let id1 = mgr
            .create_task(
                "Free task".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();

        let result = mgr.claim_task(&id1, "agent_a");
        assert!(result.is_ok());
        let task = result.unwrap();
        assert_eq!(task.owner.as_deref(), Some("agent_a"));
    }

    #[test]
    fn unassign_teammate_tasks_clears_owner() {
        let mut mgr = temp_manager();
        mgr.create_task(
            "Task 1".into(),
            "desc".into(),
            None,
            None,
            Some("agent_x".into()),
            vec![],
            vec![],
            None,
        )
        .unwrap();
        mgr.create_task(
            "Task 2".into(),
            "desc".into(),
            None,
            None,
            Some("agent_y".into()),
            vec![],
            vec![],
            None,
        )
        .unwrap();

        let count = mgr.unassign_teammate_tasks("agent_x").unwrap();
        assert_eq!(count, 1);

        let tasks = mgr.list_tasks().unwrap();
        let x_task = tasks.iter().find(|t| t.subject == "Task 1").unwrap();
        assert!(x_task.owner.is_none());
        let y_task = tasks.iter().find(|t| t.subject == "Task 2").unwrap();
        assert_eq!(y_task.owner.as_deref(), Some("agent_y"));
    }

    #[test]
    fn list_tasks_excludes_deleted() {
        let mut mgr = temp_manager();
        mgr.create_task(
            "Visible".into(),
            "desc".into(),
            None,
            None,
            None,
            vec![],
            vec![],
            None,
        )
        .unwrap();
        let id2 = mgr
            .create_task(
                "To delete".into(),
                "desc".into(),
                None,
                None,
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();

        mgr.soft_delete_task(&id2).unwrap();

        let tasks = mgr.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].subject, "Visible");
    }

    #[test]
    fn status_deleted_parsed_from_string() {
        assert_eq!(
            TaskV2Status::from_str_opt("deleted"),
            Some(TaskV2Status::Deleted)
        );
        assert_eq!(
            TaskV2Status::from_str_opt("DELETED"),
            Some(TaskV2Status::Deleted)
        );
    }

    #[test]
    fn blocks_field_deserializes_with_default() {
        let json = r#"{"id":"1","subject":"test","description":"","status":"pending","blocked_by":[],"metadata":{},"created_at":"2024-01-01T00:00:00Z"}"#;
        let record: TaskV2Record = serde_json::from_str(json).unwrap();
        assert!(record.blocks.is_empty());
    }
}
