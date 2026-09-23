//! File freshness tracking for read-before-edit validation.
//!
//! Editing tools (`edit_file`, `write_file`, `fim_edit`, `apply_patch`) wrapped
//! by [`FreshnessWrappedTool`] reject files that were never read in this
//! session, or that changed on disk since their last read/write. This kills
//! the "edited from stale context" failure mode where the model rewrites a
//! file based on remembered — no longer current — contents.
//!
//! The tracker is deliberately cheap: mtime + len per path, no content
//! hashing. Same-second same-length external edits can slip through on
//! filesystems with coarse mtime granularity; that trade-off keeps the hot
//! read path allocation-free. Tracking is per-engine and shared across
//! turns; [`crate::EngineConfig`] carries the handle so every per-turn
//! registry wraps against the same state.
//!
//! Gated by `[features].file_freshness` (default on). The gate is evaluated
//! at execution time from `ToolContext::features`, so wrapping a tool has no
//! effect when the feature is disabled — no re-registration needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::Value;

use super::spec::{ToolContext, ToolError, ToolResult, ToolSpec};
use crate::features::Feature;

/// On-disk fingerprint of a file at the time it was last read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileState {
    mtime: SystemTime,
    len: u64,
}

fn probe(path: &Path) -> Option<FileState> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileState {
        mtime: meta.modified().ok()?,
        len: meta.len(),
    })
}

/// Per-engine map of workspace paths to their last-known on-disk state.
/// Cheap to clone; all clones share one state map.
#[derive(Debug, Clone, Default)]
pub struct FileFreshnessTracker {
    states: Arc<Mutex<HashMap<PathBuf, FileState>>>,
}

impl FileFreshnessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the current on-disk state of `path` as known-fresh. Called after
    /// a successful read (the model has seen the contents) or a successful
    /// write (the tool result showed the diff).
    pub fn record_read(&self, path: &Path) {
        if let Some(state) = probe(path) {
            self.states
                .lock()
                .expect("freshness map lock")
                .insert(path.to_path_buf(), state);
        }
    }

    /// Alias of [`Self::record_read`]: after a write, the on-disk state is
    /// again known to the model, so subsequent edits validate cleanly without
    /// a forced re-read between chained edit calls.
    pub fn record_write(&self, path: &Path) {
        self.record_read(path);
    }

    /// Validate that `path` may be edited: it was read (or written) in this
    /// session and has not changed on disk since. Non-existent files pass —
    /// creating a new file needs no prior read.
    pub fn validate(&self, path: &Path) -> Result<(), String> {
        let Some(current) = probe(path) else {
            return Ok(());
        };
        let states = self.states.lock().expect("freshness map lock");
        match states.get(path) {
            None => Err(format!(
                "File has not been read in this session: {}. Read it with read_file first so edits are based on current contents.",
                path.display()
            )),
            Some(known) if *known == current => Ok(()),
            Some(_) => Err(format!(
                "File changed on disk since it was last read: {}. Re-read it with read_file before editing to avoid overwriting external changes.",
                path.display()
            )),
        }
    }

    /// Paths whose on-disk fingerprint no longer matches the last known
    /// state — externally modified since their last read/write
    /// (P2-5 `staleReadFileStateHint`). The model's picture of those
    /// files has expired; callers surface the list so the model re-reads
    /// *before* its next edit gets rejected by [`Self::validate`].
    pub fn detect_changed(&self) -> Vec<PathBuf> {
        let states = self.states.lock().expect("freshness map lock");
        states
            .iter()
            .filter_map(|(path, known)| {
                let current = probe(path)?;
                (*known != current).then(|| path.clone())
            })
            .collect()
    }
}

/// Which freshness behavior a wrapped tool needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreshnessRole {
    /// Record the target path as read after a successful call.
    Read,
    /// Validate target paths before executing; record them as fresh after
    /// success.
    Write,
    /// After a successful shell call, re-stat every tracked file and append
    /// a stale-read hint naming the ones that changed (P2-5
    /// `staleReadFileStateHint`): the model's picture of those files just
    /// expired, and re-reading beats getting rejected at the next edit.
    ShellProbe,
}

/// `ToolSpec` decorator that adds read-before-edit freshness validation to a
/// file tool. All catalog-facing methods (name, description, schema,
/// capabilities, ...) delegate verbatim, so the model-visible surface is
/// unchanged.
pub struct FreshnessWrappedTool {
    inner: Arc<dyn ToolSpec>,
    tracker: FileFreshnessTracker,
    role: FreshnessRole,
}

/// Wrap `tool` with freshness tracking when its name is one of the tracked
/// file tools; otherwise return it unchanged.
pub fn wrap_if_freshness_eligible(
    tool: Arc<dyn ToolSpec>,
    tracker: FileFreshnessTracker,
) -> Arc<dyn ToolSpec> {
    let role = match tool.name() {
        "read_file" => FreshnessRole::Read,
        "edit_file" | "write_file" | "fim_edit" | "apply_patch" => FreshnessRole::Write,
        // Shell-family tools can mutate any tracked file via sed -i,
        // generators, formatters, ...
        "exec_shell" | "exec_shell_wait" | "task_shell_start" | "task_shell_wait"
        | "task_shell_output" => FreshnessRole::ShellProbe,
        _ => return tool,
    };
    Arc::new(FreshnessWrappedTool {
        inner: tool,
        tracker,
        role,
    })
}

/// The `path` input field shared by the single-file tools.
fn input_path(input: &Value) -> Option<String> {
    input.get("path").and_then(Value::as_str).map(str::to_owned)
}

/// Every workspace path an `apply_patch` call may touch: the explicit `path`
/// field, `changes[].path` full-replacement entries, and file headers parsed
/// out of the unified-diff `patch` text. Deduplicated, order-preserving.
fn apply_patch_target_paths(input: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(p) = input_path(input) {
        paths.push(p);
    }
    if let Some(changes) = input.get("changes").and_then(Value::as_array) {
        for change in changes {
            if let Some(p) = change.get("path").and_then(Value::as_str) {
                paths.push(p.to_owned());
            }
        }
    }
    if let Some(patch) = input.get("patch").and_then(Value::as_str) {
        for line in patch.lines() {
            let target = if let Some(rest) = line.strip_prefix("diff --git ") {
                // `diff --git a/foo.rs b/foo.rs` — the destination side.
                rest.split_whitespace().nth(1)
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                Some(rest.trim())
            } else {
                line.strip_prefix("--- ").map(str::trim)
            };
            if let Some(target) = target {
                let target = target
                    .strip_prefix("b/")
                    .or_else(|| target.strip_prefix("a/"))
                    .unwrap_or(target);
                paths.push(target.to_owned());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| !p.is_empty() && seen.insert(p.clone()));
    paths
}

fn target_paths(tool_name: &str, input: &Value) -> Vec<String> {
    if tool_name == "apply_patch" {
        apply_patch_target_paths(input)
    } else {
        input_path(input).into_iter().collect()
    }
}

#[async_trait]
impl ToolSpec for FreshnessWrappedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn input_schema(&self) -> Value {
        self.inner.input_schema()
    }

    fn output_schema(&self) -> Value {
        self.inner.output_schema()
    }

    fn validate_input(&self, input: &Value, context: &ToolContext) -> Result<(), ToolError> {
        self.inner.validate_input(input, context)
    }

    fn capabilities(&self) -> Vec<super::spec::ToolCapability> {
        self.inner.capabilities()
    }

    fn approval_requirement(&self) -> super::spec::ApprovalRequirement {
        self.inner.approval_requirement()
    }

    fn approval_requirement_for_input(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> super::spec::ApprovalRequirement {
        self.inner.approval_requirement_for_input(input, context)
    }

    fn is_interactive(&self, input: &Value) -> bool {
        self.inner.is_interactive(input)
    }

    fn supports_parallel(&self) -> bool {
        self.inner.supports_parallel()
    }

    fn defer_loading(&self) -> bool {
        self.inner.defer_loading()
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        if !context.features.enabled(Feature::FileFreshness) {
            return self.inner.execute(input, context).await;
        }
        let paths = target_paths(self.inner.name(), &input);
        let resolved: Vec<PathBuf> = paths
            .iter()
            .filter_map(|p| context.resolve_path(p).ok())
            .collect();

        if self.role == FreshnessRole::Write {
            for path in &resolved {
                if let Err(message) = self.tracker.validate(path) {
                    tracing::warn!(
                        tool = self.inner.name(),
                        path = %path.display(),
                        "file freshness validation rejected the edit"
                    );
                    return Err(ToolError::execution_failed(message));
                }
            }
        }

        let result = self.inner.execute(input, context).await?;
        if result.success {
            for path in &resolved {
                if self.role == FreshnessRole::Read {
                    self.tracker.record_read(path);
                } else {
                    self.tracker.record_write(path);
                }
            }
            // P2-5 staleReadFileStateHint: a successful shell call may have
            // rewritten tracked files (sed -i, generators, formatters).
            // Naming the stale ones now — instead of waiting for the next
            // edit's rejection — turns a passive gate into an active hint,
            // saving a failed tool round.
            if self.role == FreshnessRole::ShellProbe
                && let Some(hint) = stale_read_hint(&self.tracker)
            {
                let mut result = result;
                result.content.push_str(&hint);
                return Ok(result);
            }
        }
        Ok(result)
    }
}

/// Render the stale-read hint for a shell result: the changed paths
/// (bounded to five, then an "and N more" tail), byte-stable formatting.
fn stale_read_hint(tracker: &FileFreshnessTracker) -> Option<String> {
    let mut changed = tracker.detect_changed();
    if changed.is_empty() {
        return None;
    }
    changed.sort();
    let total = changed.len();
    let shown: Vec<String> = changed
        .iter()
        .take(5)
        .map(|p| p.display().to_string())
        .collect();
    let tail = total
        .checked_sub(5)
        .filter(|more| *more > 0)
        .map(|more| format!(", and {more} more"))
        .unwrap_or_default();
    Some(format!(
        "\n\n[Files changed on disk since last read (possibly via this command) — read_file them again before editing: {}{}]",
        shown.join(", "),
        tail,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn touch_sample(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "hello\n").expect("write sample");
        path
    }

    #[test]
    fn validate_rejects_unread_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = touch_sample(tmp.path(), "unread.txt");

        let tracker = FileFreshnessTracker::new();
        let err = tracker
            .validate(&path)
            .expect_err("must reject unread file");
        assert!(err.contains("has not been read"), "{err}");
    }

    #[test]
    fn validate_accepts_file_read_in_session() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = touch_sample(tmp.path(), "read.txt");

        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&path);
        assert!(tracker.validate(&path).is_ok());
    }

    #[test]
    fn validate_rejects_externally_modified_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = touch_sample(tmp.path(), "stale.txt");

        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&path);

        // External edit: change length so the fingerprint differs even with
        // coarse mtime granularity.
        std::fs::write(&path, "changed contents\n").expect("external write");

        let err = tracker.validate(&path).expect_err("must reject stale file");
        assert!(err.contains("changed on disk"), "{err}");
    }

    #[test]
    fn chained_edits_after_write_do_not_require_re_read() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = touch_sample(tmp.path(), "chain.txt");

        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&path);
        tracker.record_write(&path);
        assert!(tracker.validate(&path).is_ok());
    }

    #[test]
    fn validate_allows_missing_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let tracker = FileFreshnessTracker::new();
        assert!(
            tracker
                .validate(&tmp.path().join("does-not-exist.txt"))
                .is_ok()
        );
    }

    #[test]
    fn apply_patch_paths_cover_field_changes_and_diff_headers() {
        let input = json!({
            "path": "explicit.txt",
            "changes": [
                {"path": "replaced.txt", "content": "x"}
            ],
            "patch": "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@\n"
        });
        let paths = apply_patch_target_paths(&input);
        assert_eq!(
            paths,
            vec![
                "explicit.txt".to_owned(),
                "replaced.txt".to_owned(),
                "src/lib.rs".to_owned()
            ]
        );
    }

    #[test]
    fn apply_patch_paths_deduplicate_repeated_headers() {
        let input = json!({
            "patch": "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n"
        });
        assert_eq!(apply_patch_target_paths(&input), vec!["a.txt".to_owned()]);
    }

    #[test]
    fn detect_changed_lists_only_drifted_files() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let stable = touch_sample(tmp.path(), "stable.txt");
        let drifted = touch_sample(tmp.path(), "drifted.txt");

        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&stable);
        tracker.record_read(&drifted);

        // External edit changes length, so the fingerprint differs.
        std::fs::write(&drifted, "rewritten contents\n").expect("external write");

        let mut changed = tracker.detect_changed();
        changed.sort();
        assert_eq!(changed, vec![drifted], "stable file must not be listed");
    }

    #[test]
    fn stale_read_hint_caps_the_list_at_five() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let tracker = FileFreshnessTracker::new();
        for i in 0..7 {
            let path = touch_sample(tmp.path(), &format!("f{i}.txt"));
            tracker.record_read(&path);
            std::fs::write(&path, "longer contents that change the length\n")
                .expect("external write");
        }
        let hint = stale_read_hint(&tracker).expect("hint");
        assert!(hint.contains("f0.txt"));
        assert!(hint.contains("f4.txt"));
        assert!(!hint.contains("f5.txt"), "only the first five are named");
        assert!(hint.contains("and 2 more"));
    }

    /// Minimal `ToolSpec` stand-in for exercising the wrapper's roles.
    struct StubTool {
        name: &'static str,
    }

    #[async_trait]
    impl ToolSpec for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({})
        }
        fn output_schema(&self) -> serde_json::Value {
            json!({})
        }
        fn capabilities(&self) -> Vec<crate::tools::spec::ToolCapability> {
            Vec::new()
        }
        async fn execute(&self, _input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success("command output".to_string()))
        }
    }

    #[tokio::test]
    async fn shell_probe_appends_stale_hint_after_success() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let drifted = touch_sample(tmp.path(), "gen.txt");
        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&drifted);
        std::fs::write(&drifted, "formatted output\n").expect("external write");

        let wrapped = wrap_if_freshness_eligible(
            Arc::new(StubTool {
                name: "exec_shell",
            }),
            tracker,
        );
        let context = ToolContext::new(tmp.path().to_path_buf());
        let result = wrapped
            .execute(json!({"command": "cargo fmt"}), &context)
            .await
            .expect("execute");
        assert!(result.success);
        assert!(
            result.content.contains("[Files changed on disk since last read"),
            "hint missing: {}",
            result.content
        );
        assert!(result.content.contains("gen.txt"));
    }

    #[tokio::test]
    async fn shell_probe_is_silent_when_nothing_drifted() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let stable = touch_sample(tmp.path(), "ok.txt");
        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&stable);

        let wrapped = wrap_if_freshness_eligible(
            Arc::new(StubTool {
                name: "exec_shell",
            }),
            tracker,
        );
        let context = ToolContext::new(tmp.path().to_path_buf());
        let result = wrapped
            .execute(json!({"command": "ls"}), &context)
            .await
            .expect("execute");
        assert_eq!(result.content, "command output", "no hint expected");
    }
}
