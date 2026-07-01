//! `enter_worktree` tool — creates an isolated git worktree and switches session into it.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_tools::{ApprovalRequirement, ToolCapability, ToolError, ToolResult};

use codesmith_agent_runtime::tools::spec::{ToolContext, ToolSpec};

use super::{
    SharedWorktreeSessionState, find_canonical_git_root, generate_random_slug, get_current_branch,
    get_or_create_worktree, validate_worktree_slug,
};

pub struct EnterWorktreeTool {
    worktree_state: SharedWorktreeSessionState,
}

impl EnterWorktreeTool {
    pub fn new(worktree_state: SharedWorktreeSessionState) -> Self {
        Self { worktree_state }
    }
}

#[async_trait]
impl ToolSpec for EnterWorktreeTool {
    fn name(&self) -> &str {
        "enter_worktree"
    }

    fn description(&self) -> &str {
        "Creates an isolated git worktree and switches the session into it. \
         Use this ONLY when explicitly instructed by the user to work in a worktree, \
         or when the user mentions wanting a worktree, working in isolation, or \
         creating a separate branch workspace. This tool creates a new directory \
         at `.codesmith/worktrees/<name>` with its own branch. The session's \
         working directory shifts to the worktree so all file operations happen \
         there. Use `exit_worktree` to leave."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Optional name for the worktree. Each segment may contain only letters, digits, dots, underscores, and dashes; max 64 chars total. A random name is generated if not provided."
                }
            },
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
        // 1. Check if already in a worktree session
        {
            let state = self.worktree_state.lock().unwrap();
            if state.active {
                return Err(ToolError::execution_failed(
                    "Already in a worktree session. Use exit_worktree first before entering another.",
                ));
            }
        }

        // 2. Validate/generate slug
        let slug = match codesmith_agent_runtime::tools::spec::optional_str(&input, "name") {
            Some(name) => {
                validate_worktree_slug(name)?;
                name.to_string()
            }
            None => generate_random_slug(),
        };

        // 3. Resolve canonical git root from context.workspace
        let git_root = find_canonical_git_root(&context.workspace)
            .unwrap_or_else(|| context.workspace.clone());

        // 4. Get current branch for tracking
        let original_branch = get_current_branch(&context.cwd).ok();

        // 5. Create git worktree
        let create_result = get_or_create_worktree(&git_root, &slug)?;

        // 6. Update WorktreeSessionState
        {
            let mut state = self.worktree_state.lock().unwrap();
            state.active = true;
            state.worktree_path = Some(create_result.worktree_path.clone());
            state.worktree_branch = Some(create_result.worktree_branch.clone());
            state.worktree_name = Some(slug.clone());
            state.original_cwd = Some(context.cwd.clone());
            state.original_head_commit = Some(create_result.head_commit.clone());
            state.session_id = None; // session_id not available in ToolContext
        }

        // 7. Build result message
        let branch_info = if create_result.worktree_branch.is_empty() {
            String::new()
        } else {
            format!(" on branch {}", create_result.worktree_branch)
        };
        let existed_note = if create_result.existed {
            " (resuming existing worktree)"
        } else {
            ""
        };
        let message = format!(
            "Created worktree at {}{}{}. The session is now working in the worktree. \
             Use exit_worktree to leave mid-session, or exit the session to be prompted.",
            create_result.worktree_path.display(),
            branch_info,
            existed_note
        );

        Ok(ToolResult::success(message))
    }
}
