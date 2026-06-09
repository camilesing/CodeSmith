//! TeamDeleteTool — cleans up team and task directories when the swarm
//! work is complete. Validates no active teammates remain before deletion.

use async_trait::async_trait;
use serde_json::json;

use crate::features::Feature;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::team::{
    SharedTeamContext, sanitize_name, team_lead_name,
    read_team_file, delete_team_directories,
    active_teammate_count, active_teammates,
};
use crate::tools::task_v2::TaskV2Manager;

pub struct TeamDeleteTool {
    team_context: SharedTeamContext,
}

impl TeamDeleteTool {
    pub fn new(team_context: SharedTeamContext) -> Self {
        Self { team_context }
    }
}

#[async_trait]
impl ToolSpec for TeamDeleteTool {
    fn name(&self) -> &'static str {
        "team_delete"
    }

    fn description(&self) -> &'static str {
        "Remove the current team and its task directories. \
         Validates no active teammates remain before deletion. \
         Removes git worktrees, team directory, and task directory."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool { false }
    fn is_read_only(&self) -> bool { false }

    async fn execute(
        &self,
        _input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        if !context.features.enabled(Feature::AgentTeams) {
            return Err(ToolError::not_available("agent_teams feature is disabled"));
        }

        let team_name = {
            let tc = self.team_context.lock().await;
            match tc.as_ref() {
                Some(ctx) => ctx.team_name.clone(),
                None => return Err(ToolError::invalid_input("Not in a team. Nothing to delete.")),
            }
        };

        let team_file = read_team_file(&team_name)
            .map_err(|e| ToolError::execution_failed(format!("Failed to read team file: {}", e)))?;

        let active = active_teammate_count(&team_file);
        if active > 0 {
            let names: Vec<String> = active_teammates(&team_file)
                .iter()
                .map(|m| m.name.clone())
                .collect();
            return Err(ToolError::invalid_input(format!(
                "Cannot delete team: {} active teammates remain: {}",
                active,
                names.join(", ")
            )));
        }

        // Destroy git worktrees for members that have them.
        for member in &team_file.members {
            if let Some(wt_path) = &member.worktree_path {
                let _ = std::process::Command::new("git")
                    .args(["worktree", "remove", wt_path])
                    .output();
            }
        }

        // Unassign stale tasks for all former teammates.
        let task_list_id = sanitize_name(&team_name);
        if let Ok(mut manager) = TaskV2Manager::new(&task_list_id) {
            for member in &team_file.members {
                if member.name != team_lead_name() {
                    let _ = manager.unassign_teammate_tasks(&member.name);
                }
            }
        }

        delete_team_directories(&team_name)
            .map_err(|e| ToolError::execution_failed(format!("Failed to delete team directories: {}", e)))?;

        // Clear TeamContext.
        {
            let mut tc = self.team_context.lock().await;
            *tc = None;
        }

        let result = json!({"deleted_team": team_name});
        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}