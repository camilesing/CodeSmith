//! TeamDeleteTool — cleans up team and task directories when the swarm
//! work is complete. Validates no active teammates remain before deletion.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::task_v2::TaskV2Manager;
use crate::tools::team::{
    SharedTeamContext, active_teammate_count, active_teammates, delete_team_directories,
    read_team_file, sanitize_name, team_lead_name,
};
use codesmith_agent_runtime::features::Feature;

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
        "Remove a team and its task directories — the session's own, or the \
         one named by team_name when it was left on disk by an earlier \
         session. Validates no active teammates remain before deletion. \
         Removes git worktrees, team directory, and task directory."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "team_name": {
                    "type": "string",
                    "description": "Delete the team with this name instead of the session's own — \
                                    for a team an earlier session left on disk."
                }
            },
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

        // The session's own team, unless the caller names one: a team an
        // earlier session left on disk blocks its name forever otherwise
        // (`team_create` says "delete the existing team first"), because
        // nothing joins or resumes a team.
        let session_team = self
            .team_context
            .lock()
            .await
            .as_ref()
            .map(|ctx| ctx.team_name.clone());
        let team_name = match input.get("team_name").and_then(|v| v.as_str()) {
            Some(name) => name.trim().to_string(),
            None => session_team
                .clone()
                .ok_or_else(|| ToolError::invalid_input("Not in a team. Nothing to delete."))?,
        };
        if team_name.is_empty() {
            return Err(ToolError::invalid_input("team_name must not be empty"));
        }

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

        // Destroy git worktrees for members that have them. `worktree_path`
        // comes from a JSON file any file-write tool can edit, so validate
        // the shape before handing it to git. Failures are surfaced instead
        // of swallowed: `git worktree remove` refuses dirty worktrees
        // without --force, and that refusal is the uncommitted-work
        // protection, so it must stay visible.
        let mut worktree_errors: Vec<String> = Vec::new();
        for member in &team_file.members {
            let Some(wt_path) = &member.worktree_path else {
                continue;
            };
            if let Err(reason) = validate_worktree_path(wt_path) {
                crate::logging::warn(format!(
                    "team_delete: skipping suspicious worktree_path for {}: {} ({})",
                    member.name, reason, wt_path
                ));
                continue;
            }
            match std::process::Command::new("git")
                .args(["worktree", "remove", "--"])
                .arg(wt_path)
                .output()
            {
                Ok(out) if out.status.success() => {}
                Ok(out) => worktree_errors.push(format!(
                    "{}: {}",
                    wt_path,
                    String::from_utf8_lossy(&out.stderr).trim()
                )),
                Err(e) => worktree_errors.push(format!("{wt_path}: {e}")),
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

        delete_team_directories(&team_name).map_err(|e| {
            ToolError::execution_failed(format!("Failed to delete team directories: {}", e))
        })?;

        // Clear TeamContext and defensively cancel any stored teammate tokens
        // — only when the session was in the team that was deleted.
        if session_team.as_deref() == Some(team_name.as_str()) {
            let mut tc = self.team_context.lock().await;
            if let Some(ctx) = tc.as_mut() {
                for token in ctx.teammate_cancel_tokens.values() {
                    token.cancel();
                }
                ctx.teammate_cancel_tokens.clear();
                ctx.teammates.clear();
            }
            *tc = None;
        }

        let mut result = json!({"deleted_team": team_name});
        if !worktree_errors.is_empty() {
            result["worktree_errors"] = json!(worktree_errors);
        }
        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

/// A member `worktree_path` must look like one the worktree tool created:
/// absolute, no `..` components, and located under a `.codesmith/worktrees`
/// directory. The team file is LLM-writable JSON, so an arbitrary path must
/// never reach `git worktree remove` unvalidated.
fn validate_worktree_path(path: &str) -> Result<(), String> {
    use std::path::{Component, Path};
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("not an absolute path".to_string());
    }
    // The `..` scan must cover the whole path — accepting on the marker pair
    // alone would let `/repo/.codesmith/worktrees/../../etc` through.
    let mut components = path.components().peekable();
    let mut under_worktrees = false;
    while let Some(component) = components.next() {
        if let Component::ParentDir = component {
            return Err("contains a `..` component".to_string());
        }
        if !under_worktrees
            && let Component::Normal(name) = component
            && name == ".codesmith"
            && matches!(components.peek(), Some(Component::Normal(next)) if *next == "worktrees")
        {
            under_worktrees = true;
        }
    }
    if under_worktrees {
        Ok(())
    } else {
        Err("not under a .codesmith/worktrees directory".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::validate_worktree_path;

    #[test]
    fn worktree_path_accepts_tool_shaped_paths() {
        assert!(validate_worktree_path("/repo/.codesmith/worktrees/alpha").is_ok());
        assert!(validate_worktree_path("/repo/.codesmith/worktrees/team-x-beta").is_ok());
    }

    #[test]
    fn worktree_path_rejects_escaping_or_foreign_paths() {
        assert!(validate_worktree_path("relative/.codesmith/worktrees/x").is_err());
        assert!(validate_worktree_path("/repo/.codesmith/worktrees/../../etc").is_err());
        assert!(validate_worktree_path("/etc/passwd").is_err());
        assert!(validate_worktree_path("/some/other/repo").is_err());
    }

    #[cfg(test)]
    mod execute_tests {
        // Async tests hold the test-env mutex across awaits.
        #![allow(clippy::await_holding_lock)]

        use crate::tools::spec::{RuntimeToolServices, ToolContext, ToolSpec};
        use crate::tools::task_v2::new_shared_task_v2_manager;
        use crate::tools::team::team_file::{
            TeamFile, TeamMember, create_team_file, format_lead_agent_id,
        };
        use crate::tools::team::{SharedTeamContext, TeamContext, new_shared_team_context};
        use codesmith_agent_runtime::features::Feature;
        use codesmith_agent_runtime::test_support::{ScopedCodeSmithHome, lock_test_env};
        use serde_json::json;

        use super::super::{ApprovalRequirement, TeamDeleteTool, ToolCapability};

        fn context(features_on: bool) -> ToolContext {
            let mut ctx = ToolContext::new("/tmp");
            ctx.runtime = RuntimeToolServices::default();
            if features_on {
                ctx.features.enable(Feature::AgentTeams);
            }
            ctx
        }

        fn make_team_file(name: &str, workers: usize) -> TeamFile {
            let members = (1..=workers)
                .map(|i| TeamMember {
                    agent_id: format!("worker{i}@t"),
                    name: format!("worker{i}"),
                    agent_type: None,
                    model: None,
                    prompt: None,
                    color: None,
                    joined_at: 0,
                    cwd: "/tmp".to_string(),
                    worktree_path: None,
                    session_id: None,
                    is_active: true,
                })
                .collect();
            TeamFile {
                name: name.to_string(),
                description: None,
                created_at: 0,
                lead_agent_id: format_lead_agent_id(name),
                lead_session_id: None,
                team_allowed_paths: None,
                members,
            }
        }

        async fn in_team(name: &str) -> SharedTeamContext {
            let team_context = new_shared_team_context();
            {
                let mut slot = team_context.lock().await;
                *slot = Some(TeamContext {
                    team_name: name.to_string(),
                    team_file_path: std::path::PathBuf::new(),
                    lead_agent_id: format_lead_agent_id(name),
                    task_v2_manager: new_shared_task_v2_manager(name).expect("manager"),
                    teammates: std::collections::HashMap::new(),
                    teammate_cancel_tokens: std::collections::HashMap::new(),
                });
            }
            team_context
        }

        #[test]
        fn team_delete_metadata_surface() {
            let tool = TeamDeleteTool::new(new_shared_team_context());
            assert_eq!(tool.name(), "team_delete");
            assert!(tool.description().contains("active teammates"));
            let schema = tool.input_schema();
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], false);
            let team_name = &schema["properties"]["team_name"];
            assert_eq!(team_name["type"], "string");
            assert!(
                team_name["description"]
                    .as_str()
                    .unwrap_or("")
                    .contains("earlier session"),
                "{schema}"
            );
            assert_eq!(tool.approval_requirement(), ApprovalRequirement::Auto);
            assert_eq!(tool.capabilities(), vec![ToolCapability::WritesFiles]);
            assert!(!tool.supports_parallel());
            assert!(!tool.is_read_only());
        }

        #[tokio::test]
        async fn team_delete_guards_refuse_featureless_or_teamless_callers() {
            let _guard = lock_test_env();
            let _home = ScopedCodeSmithHome::new();
            let no_team = TeamDeleteTool::new(new_shared_team_context());

            let err = no_team
                .execute(json!({}), &context(false))
                .await
                .expect_err("feature disabled");
            assert!(err.to_string().contains("agent_teams"), "{err}");

            let err = no_team
                .execute(json!({}), &context(true))
                .await
                .expect_err("no team");
            assert!(
                err.to_string().contains("Not in a team. Nothing to delete"),
                "{err}"
            );
        }

        /// A team an earlier session left on disk could be neither deleted
        /// (no team in this session) nor re-created (its name was taken).
        #[tokio::test]
        async fn team_delete_removes_a_team_left_on_disk_by_an_earlier_session() {
            let _guard = lock_test_env();
            let _home = ScopedCodeSmithHome::new();
            create_team_file(&make_team_file("del-stale", 0)).expect("team");
            let tool = TeamDeleteTool::new(new_shared_team_context());

            let result = tool
                .execute(json!({"team_name": "del-stale"}), &context(true))
                .await
                .expect("a stale team deletes by name");

            assert!(
                crate::tools::team::read_team_file("del-stale").is_err(),
                "roster removed from disk"
            );
            let payload: serde_json::Value = serde_json::from_str(&result.content).expect("json");
            assert_eq!(payload["deleted_team"], "del-stale");
        }

        /// Deleting some other team by name must not end this session's own
        /// team: the context is cleared only for the team that was deleted.
        #[tokio::test]
        async fn deleting_another_team_by_name_keeps_the_session_in_its_own() {
            let _guard = lock_test_env();
            let _home = ScopedCodeSmithHome::new();
            create_team_file(&make_team_file("del-other", 0)).expect("team");
            let team_context = in_team("del-keep").await;
            let tool = TeamDeleteTool::new(team_context.clone());

            tool.execute(json!({"team_name": "del-other"}), &context(true))
                .await
                .expect("another team deletes by name");
            assert!(
                crate::tools::team::read_team_file("del-other").is_err(),
                "the named team is removed"
            );
            assert_eq!(
                team_context
                    .lock()
                    .await
                    .as_ref()
                    .map(|ctx| ctx.team_name.as_str()),
                Some("del-keep"),
                "the session stays in its own team"
            );
        }

        #[tokio::test]
        async fn team_delete_refuses_while_teammates_remain() {
            let _guard = lock_test_env();
            let _home = ScopedCodeSmithHome::new();
            create_team_file(&make_team_file("del-busy", 2)).expect("team");
            let tool = TeamDeleteTool::new(in_team("del-busy").await);

            let err = tool
                .execute(json!({}), &context(true))
                .await
                .expect_err("active teammates block deletion");
            let message = err.to_string();
            assert!(message.contains("2 active teammates remain"), "{message}");
            assert!(message.contains("worker1, worker2"), "{message}");
        }

        #[tokio::test]
        async fn team_delete_removes_directories_and_clears_the_session_context() {
            let _guard = lock_test_env();
            let _home = ScopedCodeSmithHome::new();
            create_team_file(&make_team_file("del-idle", 0)).expect("team");
            // Park a cancellable token to prove the cleanup cancels it.
            let team_context = in_team("del-idle").await;
            let token = tokio_util::sync::CancellationToken::new();
            {
                let mut slot = team_context.lock().await;
                slot.as_mut()
                    .expect("team")
                    .teammate_cancel_tokens
                    .insert("worker-gone".to_string(), token.clone());
            }
            let tool = TeamDeleteTool::new(team_context.clone());

            let result = tool
                .execute(json!({}), &context(true))
                .await
                .expect("lead-only team deletes");
            let payload: serde_json::Value = serde_json::from_str(&result.content).expect("json");
            assert_eq!(payload["deleted_team"], "del-idle");
            assert!(
                payload.get("worktree_errors").is_none(),
                "no worktrees, no errors"
            );

            // Team directories are gone.
            assert!(
                crate::tools::team::read_team_file("del-idle").is_err(),
                "roster removed from disk"
            );

            // The session context and cancel tokens are cleared.
            assert!(team_context.lock().await.is_none(), "team context cleared");
            assert!(token.is_cancelled(), "parked teammate tokens cancelled");
        }
    }
}
