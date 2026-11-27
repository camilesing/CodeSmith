//! Task V2 — conversation-scoped task tracking with file-based persistence.
//!
//! Distinct from `task_manager.rs` (durable background jobs), Task V2 tracks
//! in-conversation tasks with pending→in_progress→completed workflow, file
//! persistence, concurrent-safe access via flock, and dependency tracking.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
pub use codesmith_agent_runtime::tool_state::task_v2::{
    SharedTaskV2Manager, TaskV2Manager, TaskV2Record, TaskV2Status, new_shared_task_v2_manager,
};

// === Types ===

// === Verification Nudge ===

/// Track whether tasks were completed without a verification step.
/// Threshold = 3: after 3 consecutive completions without verification,
/// a nudge message is injected.
#[allow(dead_code)]
const VERIFICATION_NUDGE_THRESHOLD: u32 = 3;

/// A task worker's panic or cancellation surfaces as a tool failure that
/// names it.
fn worker_failed(err: tokio::task::JoinError) -> ToolError {
    ToolError::ExecutionFailed {
        message: format!("task worker failed: {err}"),
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
                "blocks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs that this task blocks (reverse dependency)"
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
        context: &ToolContext,
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

        // Tool input is not schema-validated, so the enum in `input_schema`
        // is documentation only: an unknown word is refused here instead of
        // silently creating a pending task. `deleted` is not a state a new
        // task can be in (`list_tasks` would hide it forever).
        let status_str = match input.get("status") {
            None => "pending".to_string(),
            Some(v) => v
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| ToolError::invalid_input(format!("invalid status {v}")))?,
        };
        let status = match TaskV2Status::from_str_opt(&status_str) {
            Some(TaskV2Status::Deleted) => {
                return Err(ToolError::invalid_input(
                    "invalid status \"deleted\": a new task cannot start deleted",
                ));
            }
            Some(status) => status,
            None => {
                return Err(ToolError::invalid_input(format!(
                    "invalid status {status_str:?}"
                )));
            }
        };

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

        let blocks: Vec<String> = input
            .get("blocks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let metadata = input.get("metadata").cloned();

        let subject_display = subject.clone();
        // The store is blocking fs + flock work under a std Mutex: every
        // manager call runs on the blocking pool, and the hook — a child
        // process the executor waits on, up to its timeout — never runs
        // with the manager lock held.
        let task_id = {
            let manager = self.manager.clone();
            tokio::task::spawn_blocking(move || {
                manager
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .create_task(
                        subject,
                        description,
                        active_form,
                        Some(status),
                        owner,
                        blocked_by,
                        blocks,
                        metadata,
                    )
            })
            .await
            .map_err(worker_failed)?
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to create task: {e}"),
            })?
        };

        // Fire TaskCreated hook (can block creation with exit code 2)
        if let Some(hook_executor) = &context.runtime.hook_executor
            && hook_executor.has_hooks_for_event(crate::hooks::HookEvent::TaskCreated)
        {
            let hook_ctx = crate::hooks::HookContext::new()
                .with_tool_name("task_create_v2")
                .with_task_id(&task_id)
                .with_task_subject(&subject_display)
                .with_task_status(status_str.as_str());
            let hook_executor = std::sync::Arc::clone(hook_executor);
            let results = tokio::task::spawn_blocking(move || {
                hook_executor.execute(crate::hooks::HookEvent::TaskCreated, &hook_ctx)
            })
            .await
            .map_err(worker_failed)?;
            for result in &results {
                if result.exit_code == Some(2) {
                    // Rollback: remove the just-created task AND the links
                    // it added to its blockers — the raw delete left task
                    // 1 saying `blocks: ["2"]` for a task that no longer
                    // exists.
                    let manager = self.manager.clone();
                    let task_id = task_id.clone();
                    tokio::task::spawn_blocking(move || {
                        manager
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .soft_delete_task(&task_id)
                    })
                    .await
                    .map_err(worker_failed)?
                    .map_err(|e| ToolError::ExecutionFailed {
                        message: format!("Failed to rollback task creation: {e}"),
                    })?;
                    let reason = result.stderr.lines().next().unwrap_or("no reason given");
                    return Ok(ToolResult::error(format!(
                        "Task creation blocked by hook: {reason}"
                    )));
                }
            }
        }

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
        "Update a task's status, owner, description, metadata, or dependencies. \
         Set status to 'deleted' to permanently remove the task and clean up references. \
         Use add_blocks/add_blocked_by to establish dependency links. \
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
                    "enum": ["pending", "in_progress", "completed", "deleted"],
                    "description": "New status. 'deleted' permanently removes the task and cleans up references."
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
                },
                "add_blocks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs to add to this task's blocks list (tasks this task blocks)"
                },
                "add_blocked_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs to add to this task's blocked_by list (tasks that must complete first)"
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
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let task_id = input
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("task_id"))?
            .to_string();

        // An unknown status is refused: ignored, the update reported
        // success and the task was never marked done.
        let status = match input.get("status") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .and_then(TaskV2Status::from_str_opt)
                    .ok_or_else(|| ToolError::invalid_input(format!("invalid status {v}")))?,
            ),
        };

        // Handle deletion as a special case (the store is blocking file
        // work: on the pool)
        if status == Some(TaskV2Status::Deleted) {
            let manager = self.manager.clone();
            let task_id_owned = task_id.to_string();
            tokio::task::spawn_blocking(move || {
                manager
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .soft_delete_task(&task_id_owned)
            })
            .await
            .map_err(worker_failed)?
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to delete task {task_id}: {e}"),
            })?;
            return Ok(ToolResult::success(format!(
                "Task {task_id} deleted and references cleaned up"
            )));
        }

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

        let active_form = input.get("active_form").and_then(|v| {
            if v.is_null() {
                Some(None)
            } else {
                v.as_str().map(|s| Some(s.to_string()))
            }
        });

        let metadata_merge = input.get("metadata").cloned();

        let add_blocks: Option<Vec<String>> = input
            .get("add_blocks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            });

        let add_blocked_by: Option<Vec<String>> = input
            .get("add_blocked_by")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            });

        let completed = status == Some(TaskV2Status::Completed);

        let updated = {
            let manager = self.manager.clone();
            let task_id_owned = task_id.clone();
            tokio::task::spawn_blocking(move || {
                manager
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .update_task(
                        &task_id_owned,
                        status,
                        owner,
                        subject,
                        description,
                        active_form,
                        metadata_merge,
                        add_blocks,
                        add_blocked_by,
                    )
            })
            .await
            .map_err(worker_failed)?
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to update task {task_id}: {e}"),
            })?
        };

        // Fire TaskCompleted hook when status transitions to completed
        // (observer-only). A child process the executor waits on: on the
        // pool, and with no manager guard held.
        if completed
            && let Some(hook_executor) = &context.runtime.hook_executor
            && hook_executor.has_hooks_for_event(crate::hooks::HookEvent::TaskCompleted)
        {
            let hook_ctx = crate::hooks::HookContext::new()
                .with_tool_name("task_update_v2")
                .with_task_id(&updated.id)
                .with_task_subject(&updated.subject)
                .with_task_status("completed");
            let hook_executor = std::sync::Arc::clone(hook_executor);
            let _ = tokio::task::spawn_blocking(move || {
                hook_executor.execute(crate::hooks::HookEvent::TaskCompleted, &hook_ctx)
            })
            .await;
        }

        // Send mailbox notification when owner changes
        if let Some(mailbox) = &context.runtime.task_mailbox {
            let owner_input = input.get("owner").and_then(|v| v.as_str());
            if owner_input.is_some() {
                mailbox.send(
                    codesmith_agent_runtime::mailbox::MailboxMessage::TaskAssigned {
                        agent_id: updated.owner.clone().unwrap_or_default(),
                        task_id: updated.id.clone(),
                        task_subject: updated.subject.clone(),
                    },
                );
            }
        }

        Ok(ToolResult::success(format!(
            "Task {} [{}] updated to {}",
            updated.id,
            updated.subject,
            serde_json::to_string(&updated.status).unwrap_or_default()
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

        let manager = self.manager.clone();
        let task_id_owned = task_id.to_string();
        let record = tokio::task::spawn_blocking(move || {
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_task(&task_id_owned)
        })
        .await
        .map_err(worker_failed)?
        .map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to get task {task_id}: {e}"),
        })?;

        let content =
            serde_json::to_string_pretty(&record).map_err(|e| ToolError::ExecutionFailed {
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
        let manager = self.manager.clone();
        let tasks = tokio::task::spawn_blocking(move || {
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .list_tasks()
        })
        .await
        .map_err(worker_failed)?
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
                TaskV2Status::Deleted => "✕",
            };
            let owner_str = t.owner.as_deref().unwrap_or("unassigned");
            let deps = if t.blocked_by.is_empty() && t.blocks.is_empty() {
                String::new()
            } else {
                let mut parts = Vec::new();
                if !t.blocked_by.is_empty() {
                    parts.push(format!("blocked by: {}", t.blocked_by.join(", ")));
                }
                if !t.blocks.is_empty() {
                    parts.push(format!("blocks: {}", t.blocks.join(", ")));
                }
                format!(" ({})", parts.join(", "))
            };
            lines.push(format!(
                "{} {} [{}] {} — {}{deps}",
                status_sym,
                t.id,
                t.subject,
                serde_json::to_string(&t.status).unwrap_or_default(),
                owner_str
            ));
        }

        Ok(ToolResult::success(lines.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    // Async tests hold the test-env mutex across awaits.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use codesmith_agent_runtime::test_support::{EnvVarGuard, lock_test_env};

    /// `TaskV2Manager` persists under `codesmith_home()/tasks/{list_id}` —
    /// redirect `CODESMITH_HOME` into a scratch dir so no test touches the
    /// real user state. The env lock serializes env-mutating tests.
    struct ScratchEnv {
        _home: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
        _home_guard: EnvVarGuard,
    }

    fn scratch_env() -> ScratchEnv {
        let lock = lock_test_env();
        let home = tempfile::tempdir().expect("tempdir");
        let codesmith_home = home.path().join(".codesmith");
        std::fs::create_dir_all(&codesmith_home).expect("create codesmith home");
        let home_guard = EnvVarGuard::set("CODESMITH_HOME", &codesmith_home);
        ScratchEnv {
            _home: home,
            _lock: lock,
            _home_guard: home_guard,
        }
    }

    /// One fresh manager per test: a unique list id keeps task files from
    /// colliding even if two managers ever coexist under the same home.
    fn scratch_manager(label: &str) -> SharedTaskV2Manager {
        let list_id = format!("v2-test-{label}-{}", std::process::id());
        new_shared_task_v2_manager(&list_id).expect("shared task v2 manager")
    }

    /// None of the Task V2 tools touch the workspace in a hook-less,
    /// mailbox-less context — a throwaway path keeps the context hermetic.
    fn scratch_context() -> ToolContext {
        let tmp = tempfile::tempdir().expect("tempdir");
        ToolContext::new(tmp.keep())
    }

    #[test]
    fn task_v2_tool_surfaces_name_schema_and_capabilities() {
        let _env = scratch_env();
        let manager = scratch_manager("surface");

        let create = TaskV2CreateTool::new(manager.clone());
        let update = TaskV2UpdateTool::new(manager.clone());
        let get = TaskV2GetTool::new(manager.clone());
        let list = TaskV2ListTool::new(manager.clone());

        assert_eq!(create.name(), "task_create_v2");
        assert_eq!(update.name(), "task_update_v2");
        assert_eq!(get.name(), "task_get_v2");
        assert_eq!(list.name(), "task_list_v2");

        let create_schema = create.input_schema();
        assert_eq!(create_schema["required"][0], "subject");
        assert_eq!(create_schema["required"][1], "description");
        assert!(
            create_schema["properties"]["blocked_by"].is_object(),
            "create schema declares dependencies: {create_schema}"
        );
        let update_schema = update.input_schema();
        assert_eq!(update_schema["required"][0], "task_id");
        assert!(
            update_schema["properties"]["status"]["enum"]
                .as_array()
                .expect("status enum")
                .iter()
                .any(|v| v == "deleted"),
            "update schema advertises the deleted special case"
        );
        let get_schema = get.input_schema();
        assert_eq!(get_schema["required"][0], "task_id");
        assert!(
            list.input_schema()["properties"]
                .as_object()
                .unwrap()
                .is_empty(),
            "list takes no properties"
        );

        // Persistence tools write files; read tools stay read-only.
        assert!(create.capabilities().contains(&ToolCapability::WritesFiles));
        assert!(update.capabilities().contains(&ToolCapability::WritesFiles));
        assert!(get.capabilities().contains(&ToolCapability::ReadOnly));
        assert!(list.capabilities().contains(&ToolCapability::ReadOnly));

        // Auto-approved: task tracking is conversation-scoped bookkeeping.
        assert!(matches!(
            create.approval_requirement(),
            ApprovalRequirement::Auto
        ));
        assert!(matches!(
            update.approval_requirement(),
            ApprovalRequirement::Auto
        ));
        assert!(matches!(
            get.approval_requirement(),
            ApprovalRequirement::Auto
        ));
        assert!(matches!(
            list.approval_requirement(),
            ApprovalRequirement::Auto
        ));

        assert!(create.description().contains("pending -> in_progress"));
        assert!(update.description().contains("deleted"));
        assert!(!get.description().is_empty());
        assert!(!list.description().is_empty());
    }

    #[tokio::test]
    async fn task_v2_create_update_get_round_trip() {
        let _env = scratch_env();
        let manager = scratch_manager("roundtrip");
        let ctx = scratch_context();

        let create = TaskV2CreateTool::new(manager.clone());
        let created = create
            .execute(
                json!({
                    "subject": "Land the parser fix",
                    "description": "Fix the off-by-one in tokenize()",
                    "active_form": "Landing the parser fix",
                    "status": "in_progress",
                    "owner": "worker-1",
                    "blocked_by": ["9"],
                    "blocks": ["3"],
                    "metadata": {"priority": "high"}
                }),
                &ctx,
            )
            .await
            .expect("create should succeed");
        assert_eq!(created.content, "Created task 1: Land the parser fix");

        let record = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_task("1")
            .expect("task persisted");
        assert_eq!(record.subject, "Land the parser fix");
        assert_eq!(record.status, TaskV2Status::InProgress);
        assert_eq!(record.owner.as_deref(), Some("worker-1"));
        assert_eq!(record.blocked_by, vec!["9".to_string()]);
        assert_eq!(record.blocks, vec!["3".to_string()]);
        assert_eq!(record.metadata["priority"], "high");
        assert!(record.started_at.is_some(), "in_progress stamps started_at");

        // Update to completed; the success line reports the new status.
        let update = TaskV2UpdateTool::new(manager.clone());
        let updated = update
            .execute(
                json!({
                    "task_id": "1",
                    "status": "completed",
                    "metadata": {"priority": "high", "verified": true}
                }),
                &ctx,
            )
            .await
            .expect("update should succeed");
        assert!(updated.content.contains("completed"), "{}", updated.content);
        let record = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_task("1")
            .expect("task still present");
        assert_eq!(record.status, TaskV2Status::Completed);
        assert!(record.completed_at.is_some());
        assert_eq!(record.metadata["verified"], true, "metadata merges");

        // Get returns the full serialized record.
        let get = TaskV2GetTool::new(manager.clone());
        let fetched = get
            .execute(json!({"task_id": "1"}), &ctx)
            .await
            .expect("get should succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&fetched.content).expect("get content is JSON");
        assert_eq!(parsed["subject"], "Land the parser fix");
        assert_eq!(parsed["status"], "completed");
    }

    #[tokio::test]
    async fn task_v2_create_requires_subject() {
        let _env = scratch_env();
        let manager = scratch_manager("missing-subject");
        let ctx = scratch_context();
        let err = TaskV2CreateTool::new(manager)
            .execute(json!({"description": "no subject"}), &ctx)
            .await
            .expect_err("subject is required");
        assert!(err.to_string().contains("subject"), "{err}");
    }

    #[tokio::test]
    async fn task_v2_update_requires_task_id() {
        let _env = scratch_env();
        let manager = scratch_manager("missing-task-id");
        let ctx = scratch_context();
        let err = TaskV2UpdateTool::new(manager)
            .execute(json!({"status": "completed"}), &ctx)
            .await
            .expect_err("task_id is required");
        assert!(err.to_string().contains("task_id"), "{err}");
        let err = TaskV2GetTool::new(scratch_manager("get-no-id"))
            .execute(json!({}), &ctx)
            .await
            .expect_err("get also requires task_id");
        assert!(err.to_string().contains("task_id"), "{err}");
    }

    /// Tool input is not schema-validated, so a slip like `in-progress`
    /// used to be ignored: the update reported success and nothing changed.
    #[tokio::test]
    async fn task_v2_rejects_an_unknown_status_on_update_and_create() {
        let _env = scratch_env();
        let manager = scratch_manager("status");
        let ctx = scratch_context();
        TaskV2CreateTool::new(manager.clone())
            .execute(json!({"subject": "s", "description": "d"}), &ctx)
            .await
            .expect("create");

        let err = TaskV2UpdateTool::new(manager.clone())
            .execute(json!({"task_id": "1", "status": "in-progress"}), &ctx)
            .await
            .expect_err("a hyphenated status is refused");
        assert!(err.to_string().contains("invalid status"), "{err}");
        let record = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_task("1")
            .expect("task 1 survives");
        assert_eq!(record.status, TaskV2Status::Pending, "nothing changed");

        let err = TaskV2CreateTool::new(manager.clone())
            .execute(
                json!({"subject": "s2", "description": "d", "status": "complete"}),
                &ctx,
            )
            .await
            .expect_err("an unknown create status is refused");
        assert!(err.to_string().contains("invalid status"), "{err}");
        let err = TaskV2CreateTool::new(manager.clone())
            .execute(
                json!({"subject": "s3", "description": "d", "status": "deleted"}),
                &ctx,
            )
            .await
            .expect_err("a new task cannot start deleted");
        assert!(err.to_string().contains("deleted"), "{err}");
    }

    /// A `task_created` hook that blocks a creation (exit 2) rolls the
    /// task back — including the link it added to its blocker. The raw
    /// delete left task 1 saying `blocks: ["2"]` for a task that was gone.
    #[tokio::test]
    async fn task_v2_hook_blocked_creation_leaves_no_dangling_link() {
        let _env = scratch_env();
        let manager = scratch_manager("rollback");
        TaskV2CreateTool::new(manager.clone())
            .execute(
                json!({"subject": "blocker", "description": "d"}),
                &scratch_context(),
            )
            .await
            .expect("task 1 is created with no hook attached");

        let tmp = tempfile::tempdir().expect("tempdir");
        let script = tmp.path().join("block.sh");
        std::fs::write(&script, "exit 2\n").expect("hook script");
        let mut ctx = scratch_context();
        ctx.runtime.hook_executor = Some(std::sync::Arc::new(crate::hooks::HookExecutor::new(
            crate::hooks::HooksConfig {
                hooks: vec![crate::hooks::Hook::new(
                    codesmith_agent_runtime::hooks::HookEvent::TaskCreated,
                    &format!("sh {}", script.display()),
                )],
                enabled: true,
                default_timeout_secs: None,
                working_dir: None,
            },
            tmp.path().to_path_buf(),
        )));

        let blocked = TaskV2CreateTool::new(manager.clone())
            .execute(
                json!({"subject": "blocked", "description": "d", "blocked_by": ["1"]}),
                &ctx,
            )
            .await
            .expect("a blocked creation is a tool result, not an error");
        assert!(
            blocked.content.contains("blocked by hook"),
            "{}",
            blocked.content
        );

        let store = manager.lock().unwrap_or_else(|e| e.into_inner());
        let blocker = store.get_task("1").expect("task 1 survives");
        assert!(
            blocker.blocks.is_empty(),
            "no dangling link: {:?}",
            blocker.blocks
        );
        assert!(
            store.get_task("2").is_err(),
            "the blocked task is not left on disk"
        );
    }

    #[tokio::test]
    async fn task_v2_update_delete_removes_task_and_cleans_up() {
        let _env = scratch_env();
        let manager = scratch_manager("delete");
        let ctx = scratch_context();

        TaskV2CreateTool::new(manager.clone())
            .execute(
                json!({"subject": "temporary", "description": "gone soon"}),
                &ctx,
            )
            .await
            .expect("create");
        let deleted = TaskV2UpdateTool::new(manager.clone())
            .execute(json!({"task_id": "1", "status": "deleted"}), &ctx)
            .await
            .expect("delete should succeed");
        assert_eq!(deleted.content, "Task 1 deleted and references cleaned up");
        assert!(
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .list_tasks()
                .expect("list")
                .is_empty(),
            "soft-deleted tasks disappear from the listing"
        );
    }

    #[tokio::test]
    async fn task_v2_get_unknown_task_fails() {
        let _env = scratch_env();
        let manager = scratch_manager("get-unknown");
        let ctx = scratch_context();
        let err = TaskV2GetTool::new(manager)
            .execute(json!({"task_id": "404"}), &ctx)
            .await
            .expect_err("unknown task id");
        assert!(err.to_string().contains("404"), "{err}");
    }

    #[tokio::test]
    async fn task_v2_list_renders_statuses_deps_and_empty_state() {
        let _env = scratch_env();
        let manager = scratch_manager("list");
        let ctx = scratch_context();

        let list = TaskV2ListTool::new(manager.clone());
        let empty = list.execute(json!({}), &ctx).await.expect("empty list");
        assert_eq!(empty.content, "No tasks.");

        TaskV2CreateTool::new(manager.clone())
            .execute(
                json!({
                    "subject": "blocked work",
                    "description": "waits on task 2",
                    "blocked_by": ["2"],
                    "owner": "worker-9"
                }),
                &ctx,
            )
            .await
            .expect("create blocked task");
        TaskV2CreateTool::new(manager.clone())
            .execute(
                json!({
                    "subject": "in flight",
                    "description": "running now",
                    "status": "in_progress"
                }),
                &ctx,
            )
            .await
            .expect("create in-progress task");

        let rendered = list.execute(json!({}), &ctx).await.expect("list");
        // One line per task, sorted by id, with status glyph + deps + owner.
        let lines: Vec<&str> = rendered.content.lines().collect();
        assert_eq!(lines.len(), 2, "{}", rendered.content);
        assert!(lines[0].contains("○"), "task 1 pending: {}", lines[0]);
        assert!(lines[0].contains("blocked by: 2"), "{}", lines[0]);
        assert!(lines[0].contains("worker-9"), "{}", lines[0]);
        assert!(lines[1].contains("◎"), "task 2 in progress: {}", lines[1]);
        assert!(lines[1].contains("unassigned"), "{}", lines[1]);
    }

    #[tokio::test]
    async fn task_v2_update_adds_dependency_links() {
        let _env = scratch_env();
        let manager = scratch_manager("deps");
        let ctx = scratch_context();

        for subject in ["first", "second"] {
            TaskV2CreateTool::new(manager.clone())
                .execute(
                    json!({"subject": subject, "description": "dep fixture"}),
                    &ctx,
                )
                .await
                .expect("create");
        }
        TaskV2UpdateTool::new(manager.clone())
            .execute(json!({"task_id": "2", "add_blocks": ["1"]}), &ctx)
            .await
            .expect("link");
        let record = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_task("2")
            .expect("task 2");
        assert_eq!(record.blocks, vec!["1".to_string()]);
        // The reverse edge lands on task 1's blocked_by.
        let record = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_task("1")
            .expect("task 1");
        assert_eq!(record.blocked_by, vec!["2".to_string()]);
    }
}
