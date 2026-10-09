//! TeamDeleteTool — cleans up team and task directories when the swarm
//! work is complete. Validates no active teammates remain before deletion.

use async_trait::async_trait;
use serde_json::json;

use crate::features::Feature;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::task_v2::TaskV2Manager;
use crate::tools::team::{
    SharedTeamContext, active_teammate_count, active_teammates, delete_team_directories,
    read_team_file, sanitize_name, team_lead_name,
};

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

    fn supports_parallel(&self) -> bool {
        false
    }
    fn is_read_only(&self) -> bool {
        false
    }

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
                None => {
                    return Err(ToolError::invalid_input(
                        "Not in a team. Nothing to delete.",
                    ));
                }
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

        // Clear TeamContext and defensively cancel any stored teammate tokens.
        {
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
}
