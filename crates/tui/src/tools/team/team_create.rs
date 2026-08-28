//! TeamCreateTool — creates a team config, task directory, and registers
//! the current session as team lead.

use async_trait::async_trait;
use serde_json::json;

use crate::features::Feature;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::task_v2::new_shared_task_v2_manager;
use crate::tools::team::{
    SharedTeamContext, TeamContext, TeamFile, TeamMember, create_team_file, format_lead_agent_id,
    sanitize_name, team_lead_name,
};

pub struct TeamCreateTool {
    team_context: SharedTeamContext,
}

impl TeamCreateTool {
    pub fn new(team_context: SharedTeamContext) -> Self {
        Self { team_context }
    }
}

#[async_trait]
impl ToolSpec for TeamCreateTool {
    fn name(&self) -> &'static str {
        "team_create"
    }

    fn description(&self) -> &'static str {
        "Create a new team for coordinating multiple agents. \
         Creates a team config file, a shared task directory, and registers \
         the current session as the team lead. Only one team per leader session."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "team_name": {
                    "type": "string",
                    "description": "Name for the new team"
                },
                "description": {
                    "type": "string",
                    "description": "Team purpose/description"
                },
                "agent_type": {
                    "type": "string",
                    "description": "Role of the team lead (e.g., researcher, test-runner)"
                }
            },
            "required": ["team_name"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        false
    }
    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        if !context.features.enabled(Feature::AgentTeams) {
            return Err(ToolError::not_available("agent_teams feature is disabled"));
        }

        // Check one-team-per-leader constraint.
        {
            let tc = self.team_context.lock().await;
            if tc.is_some() {
                return Err(ToolError::invalid_input(
                    "Already in a team. Only one team per leader session.",
                ));
            }
        }

        let team_name = input
            .get("team_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::missing_field("team_name"))?
            .to_string();

        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let agent_type = input
            .get("agent_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let sanitized = sanitize_name(&team_name);
        let lead_agent_id = format_lead_agent_id(&team_name);
        let now = chrono::Utc::now().timestamp_millis();

        let leader_member = TeamMember {
            agent_id: lead_agent_id.clone(),
            name: team_lead_name().to_string(),
            agent_type,
            model: None,
            prompt: None,
            color: None,
            joined_at: now,
            cwd: context.workspace.to_string_lossy().to_string(),
            worktree_path: None,
            session_id: None,
            is_active: true,
        };

        let team_file = TeamFile {
            name: team_name.clone(),
            description,
            created_at: now,
            lead_agent_id: lead_agent_id.clone(),
            lead_session_id: None,
            team_allowed_paths: None,
            members: vec![leader_member],
        };

        let team_file_path = create_team_file(&team_file).map_err(|e| {
            ToolError::execution_failed(format!("Failed to create team file: {}", e))
        })?;

        // Create team-scoped TaskV2Manager.
        let task_v2_manager = new_shared_task_v2_manager(&sanitized).map_err(|e| {
            ToolError::execution_failed(format!("Failed to create task manager: {}", e))
        })?;

        let team_ctx = TeamContext {
            team_name: team_name.clone(),
            team_file_path: team_file_path.clone(),
            lead_agent_id: lead_agent_id.clone(),
            task_v2_manager,
            teammates: std::collections::HashMap::new(),
            teammate_cancel_tokens: std::collections::HashMap::new(),
        };

        {
            let mut tc = self.team_context.lock().await;
            *tc = Some(team_ctx);
        }

        let result = json!({
            "team_name": team_name,
            "sanitized_name": sanitized,
            "team_file_path": team_file_path.to_string_lossy(),
            "lead_agent_id": lead_agent_id,
            "task_list_id": sanitized,
        });

        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}
