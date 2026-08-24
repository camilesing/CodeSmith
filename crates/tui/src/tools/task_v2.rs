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
pub use codesmith_agent_runtime::tool_state::task_v2::*;

// === Types ===

// === Verification Nudge ===

/// Track whether tasks were completed without a verification step.
/// Threshold = 3: after 3 consecutive completions without verification,
/// a nudge message is injected.
const VERIFICATION_NUDGE_THRESHOLD: u32 = 3;

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
        let mut manager = self.manager.lock().await;
        let task_id = manager
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
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to create task: {e}"),
            })?;

        // Fire TaskCreated hook (can block creation with exit code 2)
        if let Some(hook_executor) = &context.runtime.hook_executor
            && hook_executor.has_hooks_for_event(crate::hooks::HookEvent::TaskCreated) {
                let hook_ctx = crate::hooks::HookContext::new()
                    .with_tool_name("task_create_v2")
                    .with_task_id(&task_id)
                    .with_task_subject(&subject_display)
                    .with_task_status(status_str);
                let results =
                    hook_executor.execute(crate::hooks::HookEvent::TaskCreated, &hook_ctx);
                for result in &results {
                    if result.exit_code == Some(2) {
                        // Rollback: delete the just-created task
                        manager
                            .delete_task(&task_id)
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

        let status = input
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(TaskV2Status::from_str_opt);

        // Handle deletion as a special case
        if status == Some(TaskV2Status::Deleted) {
            let mut manager = self.manager.lock().await;
            manager
                .soft_delete_task(&task_id)
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

        let active_form = input
            .get("active_form")
            .and_then(|v| {
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
                add_blocks,
                add_blocked_by,
            )
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("Failed to update task {task_id}: {e}"),
            })?;

        // Fire TaskCompleted hook when status transitions to completed (observer-only)
        if completed
            && let Some(hook_executor) = &context.runtime.hook_executor
                && hook_executor.has_hooks_for_event(crate::hooks::HookEvent::TaskCompleted) {
                    let hook_ctx = crate::hooks::HookContext::new()
                        .with_tool_name("task_update_v2")
                        .with_task_id(&updated.id)
                        .with_task_subject(&updated.subject)
                        .with_task_status("completed");
                    let _ =
                        hook_executor.execute(crate::hooks::HookEvent::TaskCompleted, &hook_ctx);
                }

        // Send mailbox notification when owner changes
        if let Some(mailbox) = &context.runtime.task_mailbox {
            let owner_input = input.get("owner").and_then(|v| v.as_str());
            if owner_input.is_some() {
                mailbox.send(
                    crate::tools::subagent::mailbox::MailboxMessage::TaskAssigned {
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

        let manager = self.manager.lock().await;
        let record = manager
            .get_task(task_id)
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
