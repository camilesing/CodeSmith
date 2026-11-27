//! `revert_turn` — agent-callable tool that rewinds the workspace to a
//! prior pre-turn snapshot.
//!
//! The model invokes this when the user says something like "undo the
//! last edit" or "roll back". It mirrors `/rewind` but speaks JSON and
//! takes a turn-offset (default 1 = previous turn) instead of a list
//! index, so the model doesn't have to count entries.
//!
//! Approval is `Required` because this mutates the workspace.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_agent_runtime::snapshot::SnapshotRepo;
use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64,
};

/// Default offset: revert the most-recent turn (i.e. the last `pre-turn:*`
/// snapshot in history).
const DEFAULT_OFFSET: u64 = 1;
/// Hard cap so the model can't ask to roll back to the dawn of time.
const MAX_OFFSET: u64 = 50;

pub struct RevertTurnTool;

#[async_trait]
impl ToolSpec for RevertTurnTool {
    fn name(&self) -> &str {
        "revert_turn"
    }

    fn description(&self) -> &str {
        "Roll back the workspace files to the snapshot taken before a recent turn. \
         Use when the user explicitly asks to undo, revert, or roll back the most recent edits. \
         `turn_offset` is 1-based: 1 reverts the most recent turn, 2 reverts the previous one, \
         and so on (max 50). Conversation history is NOT modified — only working-tree files are \
         restored from the side-git snapshot repo."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "turn_offset": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_OFFSET,
                    "description": "How many turns back to revert (default 1)."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let offset = optional_u64(&input, "turn_offset", DEFAULT_OFFSET)?;
        if offset == 0 || offset > MAX_OFFSET {
            return Err(ToolError::invalid_input(format!(
                "turn_offset must be between 1 and {MAX_OFFSET}; got {offset}",
            )));
        }

        let workspace = context.workspace.clone();
        let label = format!("revert_turn(offset={offset})");
        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let repo = SnapshotRepo::open_or_init(&workspace)
                .map_err(|e| format!("Snapshot repo init failed: {e}"))?;
            // Snapshot the current tree first. It is the safety net
            // `/rewind` takes — `restore` only removes paths HEAD knows, so
            // files created since the last snapshot (earlier in this turn,
            // or by hand) would survive the revert — and it stages those
            // files, so the boundary comparison below sees them (`git diff
            // <commit>` ignores files the index does not know). A failed
            // safety-net snapshot is a warning, not a blocker — but one
            // the caller must see: the pre-revert state is in no snapshot,
            // and files created since the last one are invisible to the
            // boundary comparison below.
            let safety_warning = match repo.snapshot("before revert_turn") {
                Ok(_) => String::new(),
                Err(err) => {
                    tracing::warn!(target: "snapshot", "revert_turn safety snapshot failed: {err}");
                    format!(
                        " WARNING: the pre-revert state could not be snapshotted ({err}) — \
                         this revert cannot be undone, and files created since the last \
                         snapshot may have been left in place."
                    )
                }
            };
            // Find pre-turn:* snapshots only — those mark the start of
            // each turn, which is the right rollback target. We pull a
            // generous list and filter so the model's `turn_offset` is
            // counted in turns, not raw snapshots.
            let snapshots = repo
                .list((MAX_OFFSET as usize).saturating_mul(2) + 16)
                .map_err(|e| format!("Snapshot list failed: {e}"))?;
            let mut pre_turns: Vec<_> = snapshots
                .into_iter()
                .filter(|s| s.label.starts_with("pre-turn:"))
                .collect();
            // Newest first. A pre-turn snapshot that already matches the
            // working tree is a boundary nothing changed after: the calling
            // turn's own (the engine takes it before the first request) and
            // any turn that edited no files. Restoring to it is a no-op, so
            // the offset counts past those.
            let mut skipped = 0;
            while let Some(newest) = pre_turns.first() {
                let matches = repo
                    .work_tree_matches_snapshot(&newest.id)
                    .map_err(|e| format!("Snapshot comparison failed: {e}"))?;
                if !matches {
                    break;
                }
                pre_turns.remove(0);
                skipped += 1;
            }
            let target = pre_turns
                .get((offset - 1) as usize)
                .ok_or_else(|| {
                    if skipped > 0 && pre_turns.is_empty() {
                        "NoSnapshotForTurn: every pre-turn snapshot already matches the current \
                         workspace; no earlier turn boundary differs from it."
                            .to_string()
                    } else if skipped > 0 {
                        format!(
                            "Only {} pre-turn snapshot(s) differ from the current workspace; \
                             turn_offset={offset} is out of range.",
                            pre_turns.len(),
                        )
                    } else {
                        format!(
                            "Only {} pre-turn snapshot(s) exist; turn_offset={offset} is out of range.",
                            pre_turns.len(),
                        )
                    }
                })?
                .clone();
            if repo
                .work_tree_matches_snapshot(&target.id)
                .map_err(|e| format!("Snapshot comparison failed: {e}"))?
            {
                return Err(format!(
                    "NoSnapshotForTurn: target '{}' ({}) already matches the current workspace. \
                     Revert operates at completed turn boundaries; there is no distinct later snapshot to restore.",
                    target.label,
                    short_sha(target.id.as_str()),
                ));
            }
            repo.restore(&target.id)
                .map_err(|e| format!("Restore failed: {e}"))?;
            Ok(format!(
                "{label}: restored '{}' ({}). Workspace files reverted; conversation unchanged.{safety_warning}",
                target.label,
                short_sha(target.id.as_str()),
            ))
        })
        .await
        .map_err(|e| ToolError::execution_failed(format!("revert_turn join failed: {e}")))?;

        match result {
            Ok(msg) => Ok(ToolResult::success(msg)),
            Err(e) => Ok(ToolResult::error(e)),
        }
    }
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code)]
    use super::*;
    use codesmith_agent_runtime::test_support::lock_test_env;
    use std::sync::MutexGuard;
    use tempfile::tempdir;

    /// Pins HOME to a tempdir for the duration of the test under the
    /// process-wide env mutex (`codesmith_agent_runtime::test_support::lock_test_env`).
    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: MutexGuard<'static, ()>,
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: process-wide lock still held.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }
    fn scoped_home(home: &std::path::Path) -> HomeGuard {
        let lock = lock_test_env();
        let prev = std::env::var_os("HOME");
        // SAFETY: serialised by the global env lock.
        unsafe {
            std::env::set_var("HOME", home);
        }
        HomeGuard { prev, _lock: lock }
    }

    #[tokio::test]
    async fn revert_turn_default_offset_restores_pre_turn_one() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        // Setup: create pre-turn:1, post-turn:1 with file modifications.
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"original").unwrap();
        repo.snapshot("pre-turn:1").unwrap();
        std::fs::write(workspace.join("a.txt"), b"modified").unwrap();
        repo.snapshot("post-turn:1").unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(r.success, "expected success: {r:?}");

        let content = std::fs::read_to_string(workspace.join("a.txt")).unwrap();
        assert_eq!(content, "original");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn revert_turn_reports_a_failed_safety_snapshot() {
        use std::os::unix::fs::PermissionsExt;

        // An unreadable file makes `git add -A` fail, so the pre-revert
        // snapshot cannot be taken; the restore itself still works. The
        // result has to say so: this revert cannot be undone.
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"original").unwrap();
        repo.snapshot("pre-turn:1").unwrap();
        std::fs::write(workspace.join("a.txt"), b"modified").unwrap();
        let unreadable = workspace.join("unreadable.txt");
        std::fs::write(&unreadable, b"secret").unwrap();
        let mut perms = std::fs::metadata(&unreadable).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&unreadable, perms).unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        std::fs::remove_file(&unreadable).unwrap();

        assert!(r.success, "{r:?}");
        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "original"
        );
        assert!(
            r.content.contains("could not be snapshotted"),
            "{}",
            r.content
        );
    }

    #[tokio::test]
    async fn revert_turn_removes_files_created_after_the_target_snapshot() {
        // The calling turn created new.txt after its pre-turn snapshot.
        // `restore` only deletes paths the snapshot repo's HEAD knows, so
        // without a snapshot of the current tree the new file survived.
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"original").unwrap();
        repo.snapshot("pre-turn:1").unwrap();
        std::fs::write(workspace.join("new.txt"), b"created this turn").unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(r.success, "expected success: {r:?}");

        assert!(
            !workspace.join("new.txt").exists(),
            "the new file is reverted too"
        );
    }

    #[tokio::test]
    async fn revert_turn_keeps_a_calling_turn_that_only_created_files() {
        // Turn 1 rewrote a.txt; the calling turn then created new.txt and
        // nothing else. A file the index does not know yet is invisible to
        // the boundary comparison, so without the safety snapshot first the
        // calling turn's own boundary read as unchanged — and offset 1 undid
        // turn 1 as well.
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"original").unwrap();
        repo.snapshot("pre-turn:1").unwrap();
        std::fs::write(workspace.join("a.txt"), b"modified").unwrap();
        repo.snapshot("post-turn:1").unwrap();
        repo.snapshot("pre-turn:2").unwrap();
        std::fs::write(workspace.join("new.txt"), b"created this turn").unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(r.success, "expected success: {r:?}");

        assert!(
            !workspace.join("new.txt").exists(),
            "the calling turn's new file is undone"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "modified",
            "turn 1 stays as it was"
        );
    }

    #[tokio::test]
    async fn revert_turn_default_offset_counts_past_the_calling_turns_own_snapshot() {
        // The live sequence: turn 1 edits, turn 2 changes nothing, and
        // turn 3 opens with its own pre-turn snapshot before the model calls
        // the tool. Both boundaries match the working tree, so offset 1 has
        // to land on the start of turn 1 — the latest turn that changed files.
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"original").unwrap();
        repo.snapshot("pre-turn:1").unwrap();
        std::fs::write(workspace.join("a.txt"), b"modified").unwrap();
        repo.snapshot("post-turn:1").unwrap();
        repo.snapshot("pre-turn:2").unwrap();
        repo.snapshot("post-turn:2").unwrap();
        repo.snapshot("pre-turn:3").unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(r.success, "expected success: {r:?}");

        let content = std::fs::read_to_string(workspace.join("a.txt")).unwrap();
        assert_eq!(content, "original");
    }

    #[tokio::test]
    async fn revert_turn_offset_two_reaches_the_turn_before_the_last_edit() {
        // Turn 1 adds b.txt, turn 2 rewrites a.txt, turn 3 (the caller) has
        // only its own pre-turn snapshot. Offset 2 must skip the caller's
        // boundary and then count two real turns back.
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"base").unwrap();
        repo.snapshot("pre-turn:1").unwrap();
        std::fs::write(workspace.join("b.txt"), b"one").unwrap();
        repo.snapshot("post-turn:1").unwrap();
        repo.snapshot("pre-turn:2").unwrap();
        std::fs::write(workspace.join("a.txt"), b"two").unwrap();
        repo.snapshot("post-turn:2").unwrap();
        repo.snapshot("pre-turn:3").unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool
            .execute(json!({"turn_offset": 2}), &ctx)
            .await
            .expect("execute");
        assert!(r.success, "expected success: {r:?}");

        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "base",
            "turn 2 is undone"
        );
        assert!(
            !workspace.join("b.txt").exists(),
            "turn 1 is undone too: the file did not exist before it"
        );
    }

    #[tokio::test]
    async fn revert_turn_invalid_offset_rejected() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace);
        let r = tool.execute(json!({"turn_offset": 0}), &ctx).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn revert_turn_rejects_snapshot_matching_current_workspace() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"unchanged").unwrap();
        repo.snapshot("pre-turn:1").unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace);
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(!r.success);
        assert!(r.content.contains("NoSnapshotForTurn"), "{}", r.content);
    }

    #[tokio::test]
    async fn revert_turn_no_snapshots_returns_error_result() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace);
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(!r.success);
        assert!(r.content.contains("out of range"));
    }
}
