//! `enter_worktree` tool — creates an isolated git worktree and switches session into it.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_tools::{ApprovalRequirement, ToolCapability, ToolError, ToolResult};

use codesmith_agent_runtime::tools::spec::{ToolContext, ToolSpec};

use super::{
    SharedWorktreeSessionState, WorktreeCreateResult, find_canonical_git_root,
    generate_random_slug, get_or_create_worktree, validate_worktree_slug,
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
            let state = self.worktree_state.lock().map_err(|_| {
                ToolError::execution_failed(
                    "worktree session state lock is poisoned (a prior update panicked)",
                )
            })?;
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

        // 3-4. Resolve the canonical git root and create the worktree. All of
        // this is blocking git subprocess + fs work (`git worktree add` can
        // take seconds on a large repo), so it runs on the blocking pool
        // instead of the async executor.
        let workspace_root = context.workspace.clone();
        let slug_for_task = slug.clone();
        let create_result =
            tokio::task::spawn_blocking(move || -> Result<WorktreeCreateResult, ToolError> {
                // 3. Resolve canonical git root from context.workspace. Fail
                // fast with an actionable message instead of silently
                // falling back to the workspace path, which only surfaces
                // later as a raw `git worktree add` "not a git repository"
                // stderr.
                let git_root = find_canonical_git_root(&workspace_root).ok_or_else(|| {
                    ToolError::execution_failed(
                        "enter_worktree requires a git repository, but no git root was found \
                         for the current workspace",
                    )
                })?;

                // 4. Create git worktree
                get_or_create_worktree(&git_root, &slug_for_task)
            })
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("worktree creation task failed: {e}"))
            })??;

        // 6. Update WorktreeSessionState. Re-verify under the lock: the
        // initial `!state.active` check ran before the (slow) git/process
        // work above, and a concurrent enter/exit interleaving in between
        // would have its state silently overwritten here (orphaning a
        // worktree/branch, or resurrecting a cleared session with a stale
        // original_cwd).
        {
            let mut state = self.worktree_state.lock().map_err(|_| {
                ToolError::execution_failed(
                    "worktree session state lock is poisoned (a prior update panicked)",
                )
            })?;
            if state.active {
                return Err(ToolError::execution_failed(
                    "Already in a worktree session. Use exit_worktree first before entering another.",
                ));
            }
            state.active = true;
            state.worktree_path = Some(create_result.worktree_path.clone());
            state.worktree_branch = Some(create_result.worktree_branch.clone());
            state.worktree_name = Some(slug.clone());
            state.original_cwd = Some(context.cwd.clone());
            // For a RESUMED worktree, `create_result.head_commit` is the
            // worktree's CURRENT HEAD — storing it as the session baseline
            // made exit_worktree's `rev-list original..HEAD` count report
            // 0 for all prior-session commits, so `action: "remove"` could
            // delete earlier work (dir + `git branch -D`) without
            // `discard_changes`. Use the branch point when derivable.
            let baseline = if create_result.existed {
                merge_base_baseline(&create_result.worktree_path, &create_result.worktree_branch)
                    .unwrap_or_else(|| create_result.head_commit.clone())
            } else {
                create_result.head_commit.clone()
            };
            state.original_head_commit = Some(baseline);
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

/// Baseline for a RESUMED worktree: the merge-base of the worktree branch
/// and the default branch, so prior-session commits stay visible to
/// exit_worktree's `rev-list original..HEAD` safety check. Best-effort —
/// callers fall back to the current HEAD when git cannot answer.
fn merge_base_baseline(worktree_path: &std::path::Path, worktree_branch: &str) -> Option<String> {
    if worktree_branch.is_empty() {
        return None;
    }
    let default_branch = super::get_default_branch(worktree_path).ok()?;
    let output = super::run_git(
        worktree_path,
        &["merge-base", worktree_branch, &default_branch],
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (sha.len() == 40).then_some(sha)
}
