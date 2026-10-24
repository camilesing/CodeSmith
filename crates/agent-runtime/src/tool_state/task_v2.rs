//! State types extracted from `crates/tui/src/tools/task_v2.rs`.
//! Tool implementations stay in the CLI.

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
        // Defense-in-depth for the same rule `task_file` documents: the id
        // is model-supplied and reaches `Path::join`, so it must be a
        // single safe path segment — a new caller passing an unsanitized
        // list id must not silently escape `~/.codesmith/tasks`.
        crate::utils::require_safe_path_component(task_list_id)?;
        let base = codesmith_config::codesmith_home()?;
        let task_dir = base.join("tasks").join(task_list_id);
        fs::create_dir_all(&task_dir)?;
        Ok(Self { task_dir })
    }

    fn lock_path(&self) -> PathBuf {
        self.task_dir.join(".lock")
    }

    /// Resolve the on-disk file for a task id. The id is model-supplied and
    /// reaches `Path::join`, so it must be a single safe path segment — this
    /// blocks `../../foo`-style traversal and separator smuggling.
    fn task_file(&self, id: &str) -> anyhow::Result<PathBuf> {
        if !crate::utils::is_safe_path_component(id) {
            return Err(anyhow::anyhow!(
                "invalid task id '{id}': must be a single safe path segment"
            ));
        }
        Ok(self.task_dir.join(format!("{id}.json")))
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
        crate::utils::write_atomic(&self.highwatermark_file(), value.to_string().as_bytes())?;
        Ok(())
    }

    fn read_task_file(&self, id: &str) -> anyhow::Result<TaskV2Record> {
        let path = self.task_file(id)?;
        let content = fs::read_to_string(&path)?;
        let record: TaskV2Record = serde_json::from_str(&content)?;
        Ok(record)
    }

    fn write_task_file(&self, record: &TaskV2Record) -> anyhow::Result<()> {
        let path = self.task_file(&record.id)?;
        let content = serde_json::to_string_pretty(record)?;
        // Atomic temp-file + rename: readers hold no lock, so a truncate-
        // then-write could expose a torn file mid-update.
        crate::utils::write_atomic(&path, content.as_bytes())?;
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
    #[allow(clippy::suspicious_open_options)]
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
    #[allow(clippy::too_many_arguments)]
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
            match self.read_task_file(blocker_id) {
                Ok(mut blocker) => {
                    if !blocker.blocks.contains(&id_str) {
                        blocker.blocks.push(id_str.clone());
                        self.write_task_file(&blocker)?;
                    }
                }
                Err(e) => {
                    tracing::warn!("task link update: skipping blocker {blocker_id}: {e}");
                }
            }
        }

        // Maintain bidirectional links: update blocked tasks' `blocked_by` arrays
        for blocked_id in &blocks {
            match self.read_task_file(blocked_id) {
                Ok(mut blocked) => {
                    if !blocked.blocked_by.contains(&id_str) {
                        blocked.blocked_by.push(id_str.clone());
                        self.write_task_file(&blocked)?;
                    }
                }
                Err(e) => {
                    tracing::warn!("task link update: skipping blocked {blocked_id}: {e}");
                }
            }
        }

        Ok(id_str)
    }

    /// Update an existing task. Supports dependency additions via `add_blocks`
    /// and `add_blocked_by`, which maintain bidirectional links automatically.
    #[allow(clippy::too_many_arguments)]
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
        let prev_status = record.status;

        let mut updated = record;

        if let Some(s) = status {
            // `Deleted` is routed exclusively through `soft_delete_task`:
            // setting it here would hide the task from `list_tasks` /
            // `claim_task` while other tasks keep its id in their
            // `blocks` / `blocked_by` arrays — scheduling logic then waits
            // forever on a hidden, never-completed blocker.
            if s == TaskV2Status::Deleted {
                return Err(anyhow::anyhow!("use soft_delete_task to delete task {id}"));
            }
            // Track timing transitions
            if prev_status == TaskV2Status::Pending && s == TaskV2Status::InProgress {
                updated.started_at = Some(now);
            }
            if s == TaskV2Status::Completed && prev_status != TaskV2Status::Completed {
                updated.completed_at = Some(now);
            }
            // Reverse transitions must not keep stale timestamps.
            if s != TaskV2Status::Completed {
                updated.completed_at = None;
            }
            if s == TaskV2Status::Pending {
                updated.started_at = None;
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
            let serde_json::Value::Object(new_vals) = merge else {
                return Err(anyhow::anyhow!("metadata_merge must be a JSON object"));
            };
            if !updated.metadata.is_object() {
                updated.metadata = json!({});
            }
            if let serde_json::Value::Object(existing) = &mut updated.metadata {
                for (k, v) in new_vals {
                    if v.is_null() {
                        existing.remove(&k);
                    } else {
                        existing.insert(k, v);
                    }
                }
            }
        }

        // Validate self-references in BOTH link lists before touching any
        // files: the loops below persist back-links into other tasks'
        // files as they go, so an input error discovered mid-loop leaves
        // a one-sided link on disk that fails the bidirectional invariant
        // — and fails the same way forever on retry.
        if add_blocks
            .as_ref()
            .is_some_and(|ids| ids.contains(&updated.id))
        {
            return Err(anyhow::anyhow!("task {} cannot block itself", updated.id));
        }
        if add_blocked_by
            .as_ref()
            .is_some_and(|ids| ids.contains(&updated.id))
        {
            return Err(anyhow::anyhow!(
                "task {} cannot be blocked by itself",
                updated.id
            ));
        }

        // Add blocks: update this task's `blocks` and the blocked tasks' `blocked_by`
        if let Some(new_blocks) = add_blocks {
            for block_id in &new_blocks {
                if !updated.blocks.contains(block_id) {
                    updated.blocks.push(block_id.clone());
                    match self.read_task_file(block_id) {
                        Ok(mut blocked) => {
                            if !blocked.blocked_by.contains(&updated.id) {
                                blocked.blocked_by.push(updated.id.clone());
                                self.write_task_file(&blocked)?;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("task link update: skipping blocked {block_id}: {e}");
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
                    match self.read_task_file(blocker_id) {
                        Ok(mut blocker) => {
                            if !blocker.blocks.contains(&updated.id) {
                                blocker.blocks.push(updated.id.clone());
                                self.write_task_file(&blocker)?;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("task link update: skipping blocker {blocker_id}: {e}");
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

    /// List all tasks without acquiring a lock. Lock-free reads are safe
    /// because writers stage into a temp file and rename atomically
    /// (`write_task_file`); callers that mutate around the scan must
    /// already hold the write lock. Excludes deleted tasks.
    fn list_tasks_inner(&self) -> anyhow::Result<Vec<TaskV2Record>> {
        let mut tasks: Vec<TaskV2Record> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.task_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".json") && !name.starts_with('.') {
                    let id_str = name.trim_end_matches(".json");
                    if let Ok(record) = self.read_task_file(id_str)
                        && record.status != TaskV2Status::Deleted
                    {
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

    /// Soft-delete a task: clean up references in other tasks, then physically
    /// remove the task file. The high-water mark prevents ID reuse.
    pub fn soft_delete_task(&mut self, id: &str) -> anyhow::Result<()> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        if let Ok(record) = self.read_task_file(id) {
            // Remove this task's ID from blockers' `blocks` arrays
            for blocker_id in &record.blocked_by {
                match self.read_task_file(blocker_id) {
                    Ok(mut blocker) => {
                        blocker.blocks.retain(|b| b != id);
                        self.write_task_file(&blocker)?;
                    }
                    Err(e) => {
                        tracing::warn!("task link cleanup: skipping blocker {blocker_id}: {e}");
                    }
                }
            }
            // Remove this task's ID from blocked tasks' `blocked_by` arrays
            for blocked_id in &record.blocks {
                match self.read_task_file(blocked_id) {
                    Ok(mut blocked) => {
                        blocked.blocked_by.retain(|b| b != id);
                        self.write_task_file(&blocked)?;
                    }
                    Err(e) => {
                        tracing::warn!("task link cleanup: skipping blocked {blocked_id}: {e}");
                    }
                }
            }
        }

        let path = self.task_file(id)?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Delete a task by ID (raw physical deletion without reference cleanup).
    pub fn delete_task(&mut self, id: &str) -> anyhow::Result<()> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        let path = self.task_file(id)?;
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

    /// Release one task: clear its owner and, if it was left `InProgress`,
    /// reset it to `Pending` so another teammate can claim it.
    ///
    /// Targeted counterpart to [`Self::unassign_teammate_tasks`]: the
    /// blanket unassign strips ownership from every task this agent owns —
    /// including unrelated `InProgress` tasks, which would become
    /// `InProgress` with no owner and then never be claimable
    /// (`claim_task` only takes ownerless tasks).
    pub fn release_task(&mut self, id: &str) -> anyhow::Result<()> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        let mut record = self.read_task_file(id)?;
        if record.status == TaskV2Status::InProgress {
            record.status = TaskV2Status::Pending;
        }
        record.owner = None;
        self.write_task_file(&record)?;
        Ok(())
    }

    /// Unassign all tasks owned by the given agent (set owner to None, and
    /// return still-`InProgress` work to `Pending`).
    /// Used for swarm cleanup when a teammate departs: an ownerless
    /// `InProgress` task would be permanently stranded, because claiming
    /// only accepts `Pending` tasks without an owner.
    pub fn unassign_teammate_tasks(&mut self, agent_name: &str) -> anyhow::Result<usize> {
        let mut lock = self.acquire_write_lock()?;
        let _guard = lock.write()?;

        let mut tasks = self.list_tasks_inner()?;
        let mut count = 0;
        for task in &mut tasks {
            if task.owner.as_deref() == Some(agent_name) {
                if task.status == TaskV2Status::InProgress {
                    task.status = TaskV2Status::Pending;
                }
                task.owner = None;
                self.write_task_file(task)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

/// Shared reference to TaskV2Manager.
///
/// `std::sync::Mutex` is deliberate: the manager's methods are fully
/// synchronous (blocking fs + flock) and the guard is never held across
/// an `.await`, so an async mutex would only misrepresent the critical
/// section (tokio's own guidance).
pub type SharedTaskV2Manager = Arc<std::sync::Mutex<TaskV2Manager>>;

/// Create a new shared TaskV2Manager.
pub fn new_shared_task_v2_manager(task_list_id: &str) -> anyhow::Result<SharedTaskV2Manager> {
    let manager = TaskV2Manager::new(task_list_id)?;
    Ok(Arc::new(std::sync::Mutex::new(manager)))
}

/// Render the verification nudge message.
pub fn render_verification_nudge(completed_count: u32) -> String {
    format!(
        "You've completed {completed_count} tasks without running a verification step. \
         Consider calling `run_tests` or similar verification tools to validate your work \
         before continuing. This is a suggestion, not a requirement."
    )
}

/// Task-completion count that triggers a verification nudge when no task
/// subject/description contains a verification-ish keyword ("verif",
/// "test", "check" — a loose substring heuristic). The count is the total
/// number of completed tasks, not completions since the last verification.
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

    fn temp_manager() -> TaskV2Manager {
        let dir = tempfile::tempdir().expect("tempdir").keep();
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
        let _id1 = mgr
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

    /// A departed owner's `InProgress` work must come back as claimable
    /// `Pending`: an ownerless `InProgress` task is never matched by
    /// `claim_task` and used to be stranded forever.
    #[test]
    fn unassign_returns_in_progress_work_to_pending() {
        let mut mgr = temp_manager();
        let id = mgr
            .create_task(
                "Owned".into(),
                "desc".into(),
                None,
                None,
                Some("agent_x".into()),
                vec![],
                vec![],
                None,
            )
            .unwrap();
        mgr.update_task(
            &id,
            Some(TaskV2Status::InProgress),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        mgr.unassign_teammate_tasks("agent_x").unwrap();

        let task = &mgr.list_tasks().unwrap()[0];
        assert!(task.owner.is_none());
        assert_eq!(task.status, TaskV2Status::Pending);
        // And the returned task is claimable again.
        assert!(mgr.claim_task(&id, "agent_b").is_ok());
    }

    /// `release_task` is the targeted form: it must not touch a sibling
    /// task the same agent still owns.
    #[test]
    fn release_task_targets_one_task() {
        let mut mgr = temp_manager();
        let a = mgr
            .create_task(
                "A".into(),
                "d".into(),
                None,
                None,
                Some("agent_x".into()),
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let b = mgr
            .create_task(
                "B".into(),
                "d".into(),
                None,
                None,
                Some("agent_x".into()),
                vec![],
                vec![],
                None,
            )
            .unwrap();
        mgr.update_task(
            &a,
            Some(TaskV2Status::InProgress),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        mgr.update_task(
            &b,
            Some(TaskV2Status::InProgress),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        mgr.release_task(&a).unwrap();

        let tasks = mgr.list_tasks().unwrap();
        let a_task = tasks.iter().find(|t| t.subject == "A").unwrap();
        let b_task = tasks.iter().find(|t| t.subject == "B").unwrap();
        assert!(a_task.owner.is_none());
        assert_eq!(a_task.status, TaskV2Status::Pending);
        assert_eq!(
            b_task.owner.as_deref(),
            Some("agent_x"),
            "sibling untouched"
        );
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

    #[test]
    fn update_task_metadata_merge_seeds_null_metadata() {
        let mut mgr = temp_manager();
        let id = mgr
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
        let path = mgr.task_file(&id).unwrap();
        let raw = format!(
            r#"{{"id":"{id}","subject":"s","status":"pending","created_at":"2024-01-01T00:00:00Z"}}"#
        );
        fs::write(&path, raw).unwrap();

        let updated = mgr
            .update_task(
                &id,
                None,
                None,
                None,
                None,
                None,
                Some(json!({"k": "v"})),
                None,
                None,
            )
            .unwrap();

        assert_eq!(updated.metadata, json!({"k": "v"}));
    }

    #[test]
    fn update_task_rejects_non_object_metadata_merge() {
        let mut mgr = temp_manager();
        let id = mgr
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

        let err = mgr
            .update_task(
                &id,
                None,
                None,
                None,
                None,
                None,
                Some(json!([1, 2])),
                None,
                None,
            )
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("metadata_merge must be a JSON object")
        );
    }

    #[test]
    fn update_task_reverse_transition_clears_stale_timestamps() {
        let mut mgr = temp_manager();
        let id = mgr
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

        mgr.update_task(
            &id,
            Some(TaskV2Status::InProgress),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let done = mgr
            .update_task(
                &id,
                Some(TaskV2Status::Completed),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(done.completed_at.is_some());

        let reopened = mgr
            .update_task(
                &id,
                Some(TaskV2Status::Pending),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(reopened.completed_at.is_none());
        assert!(reopened.started_at.is_none());
    }

    #[test]
    fn update_task_rejects_deleted_status() {
        // `Deleted` through update_task would hide the task from listing
        // and claiming while leaving dangling blocks/blocked_by links on
        // other tasks; deletion must route through soft_delete_task.
        let mut mgr = temp_manager();
        let id = mgr
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
        let err = mgr
            .update_task(
                &id,
                Some(TaskV2Status::Deleted),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("soft_delete_task"));
    }

    #[test]
    fn update_task_rejects_self_reference() {
        let mut mgr = temp_manager();
        let id = mgr
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

        let err = mgr
            .update_task(
                &id,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(vec![id.clone()]),
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("cannot block itself"));

        let err = mgr
            .update_task(
                &id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(vec![id.clone()]),
            )
            .unwrap_err();
        assert!(err.to_string().contains("cannot be blocked by itself"));
    }

    #[test]
    fn task_file_rejects_traversal_ids() {
        // Model-supplied ids reach `Path::join` — traversal, separators, and
        // dot tricks must be rejected before any file is touched.
        let mgr = temp_manager();
        for bad_id in ["../../foo", "../x", "a/b", "", ".", "..", ".hidden"] {
            assert!(
                mgr.task_file(bad_id).is_err(),
                "task id {bad_id:?} must be rejected"
            );
        }
        // The rejection must also surface through the public API.
        let err = mgr.get_task("../../foo").unwrap_err();
        assert!(
            err.to_string().contains("invalid task id"),
            "error must name the invalid id; got: {err}"
        );
    }

    #[test]
    fn task_file_accepts_safe_ids() {
        let mgr = temp_manager();
        let path = mgr.task_file("valid-id-123").unwrap();
        assert!(path.starts_with(&mgr.task_dir));
        assert!(path.ends_with("valid-id-123.json"));
    }
}
