//! Model-visible automation tools over `AutomationManager`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::automation_manager::{
    AutomationStatus, CreateAutomationRequest, UpdateAutomationRequest,
};
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, optional_u64, required_str,
};

pub struct AutomationCreateTool;
pub struct AutomationListTool;
pub struct AutomationReadTool;
pub struct AutomationUpdateTool;
pub struct AutomationPauseTool;
pub struct AutomationResumeTool;
pub struct AutomationDeleteTool;
pub struct AutomationRunTool;

#[async_trait]
impl ToolSpec for AutomationCreateTool {
    fn name(&self) -> &'static str {
        "automation_create"
    }

    fn description(&self) -> &'static str {
        "Create a durable scheduled automation. Creation requires approval and recurrence is constrained to supported HOURLY/WEEKLY RRULE forms. Runs enqueue normal durable tasks."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "prompt": { "type": "string" },
                "rrule": {
                    "type": "string",
                    "description": "Supported: FREQ=HOURLY;INTERVAL=N[;BYDAY=MO,TU] or FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=30"
                },
                "cwds": { "type": "array", "items": { "type": "string" } },
                "paused": { "type": "boolean", "default": false }
            },
            "required": ["name", "prompt", "rrule"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let manager = context
            .runtime
            .automations
            .as_ref()
            .ok_or_else(|| ToolError::not_available("AutomationManager is not attached"))?;
        let req = CreateAutomationRequest {
            name: required_str(&input, "name")?.to_string(),
            prompt: required_str(&input, "prompt")?.to_string(),
            rrule: required_str(&input, "rrule")?.to_string(),
            cwds: cwds_from_input(&input)?,
            status: Some(
                if input
                    .get("paused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    AutomationStatus::Paused
                } else {
                    AutomationStatus::Active
                },
            ),
        };
        let automation = manager
            .create_automation(req)
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        ToolResult::json(&automation).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[async_trait]
impl ToolSpec for AutomationListTool {
    fn name(&self) -> &'static str {
        "automation_list"
    }

    fn description(&self) -> &'static str {
        "List durable automations with status, next run, and last run timestamps."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 50 }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let manager = context
            .runtime
            .automations
            .as_ref()
            .ok_or_else(|| ToolError::not_available("AutomationManager is not attached"))?;
        let mut automations = manager
            .list_automations()
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        automations.truncate(optional_u64(&input, "limit", 50)?.clamp(1, 100) as usize);
        ToolResult::json(&automations).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[async_trait]
impl ToolSpec for AutomationReadTool {
    fn name(&self) -> &'static str {
        "automation_read"
    }

    fn description(&self) -> &'static str {
        "Read one durable automation plus recent run records."
    }

    fn input_schema(&self) -> Value {
        automation_id_schema(true)
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let manager = context
            .runtime
            .automations
            .as_ref()
            .ok_or_else(|| ToolError::not_available("AutomationManager is not attached"))?;
        let id = required_str(&input, "automation_id")?;
        let automation = manager
            .get_automation(id)
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        let runs = manager
            .list_runs(id, Some(20))
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        ToolResult::json(&json!({ "automation": automation, "recent_runs": runs }))
            .map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[async_trait]
impl ToolSpec for AutomationUpdateTool {
    fn name(&self) -> &'static str {
        "automation_update"
    }

    fn description(&self) -> &'static str {
        "Update a durable automation. Requires approval; recurrence remains constrained to supported RRULE forms."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "automation_id": { "type": "string" },
                "name": { "type": "string" },
                "prompt": { "type": "string" },
                "rrule": { "type": "string" },
                "cwds": { "type": "array", "items": { "type": "string" } },
                "status": { "type": "string", "enum": ["active", "paused"] }
            },
            "required": ["automation_id"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let manager = context
            .runtime
            .automations
            .as_ref()
            .ok_or_else(|| ToolError::not_available("AutomationManager is not attached"))?;
        let status = match optional_str(&input, "status") {
            None => None,
            Some("paused") => Some(AutomationStatus::Paused),
            Some("active") => Some(AutomationStatus::Active),
            // The schema's enum is model-facing only — a typo like
            // "Paused" or "activ" used to silently resume unattended
            // scheduled runs instead of failing loudly.
            Some(other) => {
                return Err(ToolError::invalid_input(format!(
                    "field 'status' must be \"active\" or \"paused\", got: {other}"
                )));
            }
        };
        let req = UpdateAutomationRequest {
            name: optional_str(&input, "name").map(ToString::to_string),
            prompt: optional_str(&input, "prompt").map(ToString::to_string),
            rrule: optional_str(&input, "rrule").map(ToString::to_string),
            // Presence means "replace the list": `cwds: null` is rejected by
            // `cwds_from_input` instead of silently clearing the configured
            // working directories.
            cwds: if input.get("cwds").is_some() {
                Some(cwds_from_input(&input)?)
            } else {
                None
            },
            status,
        };
        let automation = manager
            .update_automation(required_str(&input, "automation_id")?, req)
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        ToolResult::json(&automation).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

macro_rules! write_automation_tool {
    ($ty:ident, $name:literal, $desc:literal, $method:ident) => {
        #[async_trait]
        impl ToolSpec for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            fn description(&self) -> &'static str {
                $desc
            }
            fn input_schema(&self) -> Value {
                automation_id_schema(true)
            }
            fn capabilities(&self) -> Vec<ToolCapability> {
                vec![ToolCapability::RequiresApproval]
            }
            fn approval_requirement(&self) -> ApprovalRequirement {
                ApprovalRequirement::Required
            }
            fn defer_loading(&self) -> bool {
                true
            }

            async fn execute(
                &self,
                input: Value,
                context: &ToolContext,
            ) -> Result<ToolResult, ToolError> {
                let manager =
                    context.runtime.automations.as_ref().ok_or_else(|| {
                        ToolError::not_available("AutomationManager is not attached")
                    })?;
                let automation = manager
                    .$method(required_str(&input, "automation_id")?)
                    .await
                    .map_err(|e| ToolError::execution_failed(e.to_string()))?;
                ToolResult::json(&automation)
                    .map_err(|e| ToolError::execution_failed(e.to_string()))
            }
        }
    };
}

write_automation_tool!(
    AutomationPauseTool,
    "automation_pause",
    "Pause a durable automation. Requires approval.",
    pause_automation
);
write_automation_tool!(
    AutomationResumeTool,
    "automation_resume",
    "Resume a paused durable automation. Requires approval.",
    resume_automation
);
write_automation_tool!(
    AutomationDeleteTool,
    "automation_delete",
    "Delete a durable automation and its run history. Requires approval.",
    delete_automation
);

#[async_trait]
impl ToolSpec for AutomationRunTool {
    fn name(&self) -> &'static str {
        "automation_run"
    }

    fn description(&self) -> &'static str {
        "Run an automation now. The run enqueues a normal durable task and returns linked task/thread/turn ids as they become available."
    }

    fn input_schema(&self) -> Value {
        automation_id_schema(true)
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let manager = context
            .runtime
            .automations
            .as_ref()
            .ok_or_else(|| ToolError::not_available("AutomationManager is not attached"))?;
        let task_manager = context
            .runtime
            .task_manager
            .as_ref()
            .ok_or_else(|| ToolError::not_available("TaskManager is not attached"))?;
        let run = manager
            .run_now(required_str(&input, "automation_id")?, task_manager)
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        ToolResult::json(&run).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

fn automation_id_schema(require_id: bool) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "automation_id": { "type": "string" }
        },
        "additionalProperties": false
    });
    if require_id {
        schema["required"] = json!(["automation_id"]);
    }
    schema
}

/// Parse a `Vec<String>` field, rejecting wrong-typed shapes instead of
/// silently defaulting or dropping items: `cwds: ["/tmp", 42]` used to
/// persist as `["/tmp"]`, and a JSON `null` used to yield `Ok(vec![])` which
/// `automation_update`'s presence check turned into "clear the configured
/// working directories" — diverging from how `required_str`/`required_u64`
/// treat wrong-typed fields.
fn string_array(input: &Value, field: &str) -> Result<Vec<String>, ToolError> {
    match input.get(field) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let s = item.as_str().ok_or_else(|| {
                    ToolError::invalid_input(format!(
                        "field '{field}' must contain only strings, got: {item}"
                    ))
                })?;
                out.push(s.to_string());
            }
            Ok(out)
        }
        Some(other) => Err(ToolError::invalid_input(format!(
            "field '{field}' must be an array of strings, got: {other}"
        ))),
    }
}

/// Parse the `cwds` working-directory list. Entries become the workspace of
/// unattended, auto-approved runs downstream, so every entry must be an
/// absolute path — a relative path would resolve against wherever the
/// scheduler happens to run, granting the run an unintended directory.
fn cwds_from_input(input: &Value) -> Result<Vec<PathBuf>, ToolError> {
    string_array(input, "cwds")?
        .into_iter()
        .map(|raw| {
            let path = PathBuf::from(&raw);
            if !path.is_absolute() {
                return Err(ToolError::invalid_input(format!(
                    "cwds entries must be absolute paths, got: {raw:?}"
                )));
            }
            Ok(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::ToolSpec;

    #[test]
    fn create_schema_exposes_rrule() {
        let schema = AutomationCreateTool.input_schema();
        assert!(schema["properties"]["rrule"].is_object());
        assert_eq!(schema["required"][0], "name");
    }

    #[test]
    fn string_array_rejects_wrong_typed_shapes() {
        let ok = json!({ "cwds": ["/tmp/a", "/tmp/b"] });
        assert_eq!(
            string_array(&ok, "cwds").unwrap(),
            vec!["/tmp/a".to_string(), "/tmp/b".to_string()]
        );
        let absent = json!({});
        assert!(string_array(&absent, "cwds").unwrap().is_empty());

        let mixed = json!({ "cwds": ["/tmp", 42, null] });
        assert!(string_array(&mixed, "cwds").is_err(), "non-string items");
        let null = json!({ "cwds": null });
        assert!(string_array(&null, "cwds").is_err(), "null must not clear");
        let scalar = json!({ "cwds": "/tmp" });
        assert!(string_array(&scalar, "cwds").is_err(), "non-array");
    }

    #[test]
    fn cwds_must_be_absolute() {
        let ok = json!({ "cwds": ["/work/repo"] });
        assert_eq!(
            cwds_from_input(&ok).unwrap(),
            vec![PathBuf::from("/work/repo")]
        );

        let rel = json!({ "cwds": ["repo"] });
        let err = cwds_from_input(&rel).expect_err("relative path must be rejected");
        assert!(err.to_string().contains("absolute"));
    }
}
