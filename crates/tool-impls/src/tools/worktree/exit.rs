//! `exit_worktree` tool — exits a worktree session and restores original working directory.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_tools::{
    ApprovalRequirement, ToolCapability, ToolError, ToolResult, optional_str, required_str,
};

use codesmith_agent_runtime::tools::spec::{ToolContext, ToolSpec};

use super::{
    SharedWorktreeSessionState, cleanup_worktree, count_worktree_changes, find_canonical_git_root,
};

pub struct ExitWorktreeTool {
    worktree_state: SharedWorktreeSessionState,
}

impl ExitWorktreeTool {
    pub fn new(worktree_state: SharedWorktreeSessionState) -> Self {
        Self { worktree_state }
    }
}

#[async_trait]
impl ToolSpec for ExitWorktreeTool {
    fn name(&self) -> &str {
        "exit_worktree"
    }

    fn description(&self) -> &str {
        "Exits a worktree session created by enter_worktree and restores the \
         original working directory. Choose 'keep' to preserve the worktree \
         files on disk, or 'remove' to delete both the worktree directory and \
         branch. If the worktree has uncommitted changes or new commits, \
         'remove' requires discard_changes=true as confirmation — otherwise \
         the tool refuses to prevent accidental data loss. This tool only \
         operates on worktrees created by enter_worktree in the current session."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["keep", "remove"],
                    "description": "\"keep\" leaves the worktree and branch on disk; \"remove\" deletes both."
                },
                "discard_changes": {
                    "type": "boolean",
                    "description": "Required true when action is \"remove\" and the worktree has uncommitted files or unmerged commits. The tool will refuse and list them otherwise."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = required_str(&input, "action")?;
        if action != "keep" && action != "remove" {
            return Err(ToolError::execution_failed(format!(
                "Invalid action \"{action}\". Must be \"keep\" or \"remove\"."
            )));
        }
        let discard_changes = codesmith_agent_runtime::tools::spec::optional_bool(&input, "discard_changes", false);

        // 1. Check if worktree session is active
        let (worktree_path, worktree_branch, original_cwd, original_head_commit) = {
            let state = self.worktree_state.lock().unwrap();
            if !state.active {
                return Ok(ToolResult::success(
                    "No-op: there is no active enter_worktree session to exit. \
                     This tool only operates on worktrees created by enter_worktree \
                     in the current session. No filesystem changes were made.",
                ));
            }
            (
                state.worktree_path.clone().unwrap_or_default(),
                state.worktree_branch.clone(),
                state
                    .original_cwd
                    .clone()
                    .unwrap_or_else(|| context.workspace.clone()),
                state.original_head_commit.clone(),
            )
        };

        // 2. Safety check for "remove" action
        if action == "remove" && !discard_changes {
            let summary = count_worktree_changes(&worktree_path, &original_head_commit);
            match summary {
                None => {
                    // Fail-closed: can't determine state, refuse
                    return Ok(ToolResult::success(format!(
                        "Could not verify worktree state at {}. \
                             Refusing to remove without explicit confirmation. \
                             Re-invoke with discard_changes: true to proceed — \
                             or use action: \"keep\" to preserve the worktree.",
                        worktree_path.display()
                    )));
                }
                Some(s) if s.changed_files > 0 || s.commits > 0 => {
                    let parts: Vec<String> = Vec::new();
                    let mut parts = parts;
                    if s.changed_files > 0 {
                        let file_word = if s.changed_files == 1 {
                            "file"
                        } else {
                            "files"
                        };
                        parts.push(format!("{} uncommitted {}", s.changed_files, file_word));
                    }
                    if s.commits > 0 {
                        let commit_word = if s.commits == 1 { "commit" } else { "commits" };
                        let branch_note = worktree_branch
                            .as_deref()
                            .map(|b| format!(" on {b}"))
                            .unwrap_or_default();
                        parts.push(format!("{} {}{}", s.commits, commit_word, branch_note));
                    }
                    return Ok(ToolResult::success(format!(
                        "Worktree has {}. Removing will discard this work permanently. \
                             Confirm with the user, then re-invoke with discard_changes: true — \
                             or use action: \"keep\" to preserve the worktree.",
                        parts.join(" and ")
                    )));
                }
                _ => {} // No changes, safe to proceed
            }
        }

        // 3. Perform action
        if action == "keep" {
            // Clear state but keep files on disk
            self.worktree_state.lock().unwrap().active = false;
            self.worktree_state.lock().unwrap().worktree_path = None;
            self.worktree_state.lock().unwrap().worktree_branch = None;
            self.worktree_state.lock().unwrap().worktree_name = None;
            self.worktree_state.lock().unwrap().original_cwd = None;
            self.worktree_state.lock().unwrap().original_head_commit = None;
            self.worktree_state.lock().unwrap().session_id = None;

            let branch_info = worktree_branch
                .as_deref()
                .map(|b| format!(" on branch {b}"))
                .unwrap_or_default();

            return Ok(ToolResult::success(format!(
                "Exited worktree. Your work is preserved at {}{}. \
                     Session is now back in {}.",
                worktree_path.display(),
                branch_info,
                original_cwd.display()
            )));
        }

        // action == "remove"
        let git_root = find_canonical_git_root(&context.workspace)
            .unwrap_or_else(|| context.workspace.clone());

        cleanup_worktree(&worktree_path, &worktree_branch, &git_root)?;

        // Clear state
        {
            let mut state = self.worktree_state.lock().unwrap();
            state.active = false;
            state.worktree_path = None;
            state.worktree_branch = None;
            state.worktree_name = None;
            state.original_cwd = None;
            state.original_head_commit = None;
            state.session_id = None;
        }

        Ok(ToolResult::success(format!(
            "Exited and removed worktree at {}. Session is now back in {}.",
            worktree_path.display(),
            original_cwd.display()
        )))
    }
}
