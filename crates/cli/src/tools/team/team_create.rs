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
        // is in-process only). Refuse instead. Creation itself is
        // exclusive (O_EXCL on config.json) and its `AlreadyExists` arm
        // returns the same message — the old `.exists()` pre-check was a
        // TOCTOU across sessions, and its `.unwrap_or(false)` silently
        // skipped the guard when home-dir resolution failed. Home-dir
        // resolution failures now surface from `create_team_file_exclusive`
        // as execution_failed with context.
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
                // O_EXCL proved any config.json at this path is ours, so
                // rolling back a partial creation (config written while
                // inboxes/ or the task dir failed) is safe — otherwise every
                // retry for this name fails "already exists" until the user
                // manually deletes ~/.codesmith/teams/<name>.
                if let Err(cleanup_err) = crate::tools::team::delete_team_directories(&team_name) {
                    crate::logging::warn(format!(
                        "failed to roll back partially-created team '{}': {cleanup_err}",
                        team_name
                    ));
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::{RuntimeToolServices, ToolSpec};
    use codesmith_agent_runtime::test_support::{ScopedCodeSmithHome, lock_test_env};

    fn context(features_on: bool) -> ToolContext {
        let mut ctx = ToolContext::new("/tmp");
        ctx.runtime = RuntimeToolServices::default();
        if features_on {
            ctx.features.enable(Feature::AgentTeams);
        }
        ctx
    }

    #[test]
    fn team_create_metadata_surface() {
        let team_context = crate::tools::team::new_shared_team_context();
        let tool = TeamCreateTool::new(team_context);
        assert_eq!(tool.name(), "team_create");
        assert!(tool.description().contains("team lead"));
        let schema = tool.input_schema();
        assert_eq!(
            schema["required"]
                .as_array()
                .and_then(|r| r.first())
                .and_then(|v| v.as_str()),
            Some("team_name")
        );
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Auto);
        assert_eq!(tool.capabilities(), vec![ToolCapability::WritesFiles]);
        assert!(!tool.supports_parallel());
        assert!(!tool.is_read_only());
    }

    #[tokio::test]
    async fn team_create_guards_refuse_bad_input() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let team_context = crate::tools::team::new_shared_team_context();
        let tool = TeamCreateTool::new(team_context.clone());

        let err = tool
            .execute(json!({"team_name": "x"}), &context(false))
            .await
            .expect_err("feature disabled");
        assert!(err.to_string().contains("agent_teams"), "{err}");

        let err = tool
            .execute(json!({}), &context(true))
            .await
            .expect_err("missing team_name");
        assert!(err.to_string().contains("team_name"), "{err}");

        let err = tool
            .execute(json!({"team_name": ""}), &context(true))
            .await
            .expect_err("empty name sanitizes to nothing");
        assert!(
            err.to_string().contains("at least one alphanumeric"),
            "{err}"
        );

        // One team per leader session: an occupied context refuses.
        {
            let mut slot = team_context.lock().await;
            *slot = Some(crate::tools::team::TeamContext {
                team_name: "existing".to_string(),
                team_file_path: std::path::PathBuf::new(),
                lead_agent_id: format_lead_agent_id("existing"),
                task_v2_manager: new_shared_task_v2_manager("existing").expect("manager"),
                teammates: std::collections::HashMap::new(),
                teammate_cancel_tokens: std::collections::HashMap::new(),
            });
        }
        let err = tool
            .execute(json!({"team_name": "second"}), &context(true))
            .await
            .expect_err("one team per session");
        assert!(err.to_string().contains("Only one team"), "{err}");
    }

    #[tokio::test]
    async fn team_create_happy_path_registers_lead_and_refuses_duplicates() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let team_context = crate::tools::team::new_shared_team_context();
        let tool = TeamCreateTool::new(team_context.clone());

        let result = tool
            .execute(
                json!({
                    "team_name": "Cov Team!",
                    "description": "coverage team",
                    "agent_type": "reviewer"
                }),
                &context(true),
            )
            .await
            .expect("team created");
        let payload: serde_json::Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(payload["team_name"], "Cov Team!");
        assert_eq!(payload["sanitized_name"], "cov-team-");
        assert_eq!(
            payload["task_list_id"], "cov-team-",
            "the task list keys on the sanitized name"
        );
        assert_eq!(payload["lead_agent_id"], format_lead_agent_id("Cov Team!"));

        // The roster is on disk with the lead as its first member.
        let team_file = crate::tools::team::read_team_file("Cov Team!").expect("roster");
        assert_eq!(team_file.members.len(), 1);
        assert_eq!(team_file.members[0].name, team_lead_name());
        assert_eq!(team_file.members[0].agent_type.as_deref(), Some("reviewer"));

        // The context is installed for the session.
        assert!(
            team_context.lock().await.is_some(),
            "create registers the leader session"
        );

        // A second session creating the same-named team is refused.
        let other_context = crate::tools::team::new_shared_team_context();
        let other = TeamCreateTool::new(other_context);
        let err = other
            .execute(json!({"team_name": "Cov Team!"}), &context(true))
            .await
            .expect_err("duplicate team name");
        assert!(err.to_string().contains("already exists"), "{err}");
    }
}
