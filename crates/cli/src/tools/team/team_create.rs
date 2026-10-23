//! TeamCreateTool — creates a team config, task directory, and registers
//! the current session as team lead.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::task_v2::new_shared_task_v2_manager;
use crate::tools::team::{
    SharedTeamContext, TeamContext, TeamFile, TeamMember, create_team_file_exclusive,
    format_lead_agent_id, sanitize_name, team_lead_name,
};
use codesmith_agent_runtime::features::Feature;

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

        // Check one-team-per-leader constraint, holding the guard until
        // the TeamContext is installed so concurrent execute() calls
        // cannot both pass the check and silently overwrite each other.
        let mut team_guard = self.team_context.lock().await;
        if team_guard.is_some() {
            return Err(ToolError::invalid_input(
                "Already in a team. Only one team per leader session.",
            ));
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
        // An empty sanitized name would resolve the team dir to the teams
        // root itself (`~/.codesmith/teams/`) and clobber its config.json.
        if sanitized.is_empty() {
            return Err(ToolError::invalid_input(
                "Team name must contain at least one alphanumeric character",
            ));
        }
        // A second session creating the same-named team would silently
        // clobber the existing roster (the one-team-per-leader guard above
        // is in-process only). Refuse instead.
        // Creation itself is exclusive (O_EXCL on config.json) — the
        // `.exists()` pre-check was a TOCTOU across sessions, and its
        // `.unwrap_or(false)` silently skipped the guard when home-dir
        // resolution failed.
        let pre_exists = crate::tools::team::team_config_path(&team_name)
            .map(|path| path.exists())
            .unwrap_or(false);
        if pre_exists {
            return Err(ToolError::invalid_input(format!(
                "A team named '{}' already exists; pick a different name or delete the existing team first",
                sanitized
            )));
        }
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

        let team_file_path = match create_team_file_exclusive(&team_file) {
            Ok(path) => path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ToolError::invalid_input(format!(
                    "A team named '{}' already exists; pick a different name or delete the existing team first",
                    sanitized
                )));
            }
            Err(e) => {
                return Err(ToolError::execution_failed(format!(
                    "Failed to create team file: {}",
                    e
                )));
            }
        };

        // Create team-scoped TaskV2Manager. On failure, roll back the
        // on-disk team created above — otherwise every retry hits the
        // "already exists" check and fails permanently until the user
        // manually deletes ~/.codesmith/teams/<name>. Creation was
        // exclusive (O_EXCL), so the rollback cannot clobber another team.
        let task_v2_manager = match new_shared_task_v2_manager(&sanitized) {
            Ok(manager) => manager,
            Err(e) => {
                if let Err(cleanup_err) = crate::tools::team::delete_team_directories(&team_name) {
                    crate::logging::warn(format!(
                        "failed to roll back team '{}' after task-manager failure: {cleanup_err}",
                        team_name
                    ));
                }
                return Err(ToolError::execution_failed(format!(
                    "Failed to create task manager: {}",
                    e
                )));
            }
        };

        let team_ctx = TeamContext {
            team_name: team_name.clone(),
            team_file_path: team_file_path.clone(),
            lead_agent_id: lead_agent_id.clone(),
            task_v2_manager,
            teammates: std::collections::HashMap::new(),
            teammate_cancel_tokens: std::collections::HashMap::new(),
        };

        // `team_guard` has been held since the one-team-per-leader check.
        *team_guard = Some(team_ctx);

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
