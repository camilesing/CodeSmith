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

/// Drift-scan stat of a tracked path: its current fingerprint, a deletion,
/// or an unreadable stat. Transient stat failures (permissions,
/// EINTR-like) are [`DriftProbe::Unknown`] so they can be skipped — only a
/// deletion is drift worth surfacing.
enum DriftProbe {
    Present(FileState),
    Deleted,
    Unknown,
}

fn drift_probe(path: &Path) -> DriftProbe {
    match std::fs::metadata(path) {
        Ok(meta) => match meta.modified() {
            Ok(mtime) => DriftProbe::Present(FileState {
                mtime,
                len: meta.len(),
            }),
            Err(_) => DriftProbe::Unknown,
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => DriftProbe::Deleted,
        Err(_) => DriftProbe::Unknown,
    }
}

/// Per-engine map of workspace paths to their last-known on-disk state.
/// Cheap to clone; all clones share one state map.
#[derive(Debug, Clone, Default)]
pub struct FileFreshnessTracker {
    states: Arc<Mutex<HashMap<PathBuf, FileState>>>,
    /// On-disk state each path was surfaced with in its last stale-read
    /// hint (`None` = it was already deleted). Suppresses re-announcing
    /// the same drift on every subsequent shell poll; a path that drifts
    /// *again* (state differs from the hinted snapshot) is re-announced.
    hinted: Arc<Mutex<HashMap<PathBuf, Option<FileState>>>>,
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
        // A fresh read re-arms hinting: drift from this new state must be
        // announced even if it matches an older hinted state.
        self.hinted.lock().expect("freshness hint lock").remove(path);
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

    /// Drift scan shared by [`Self::detect_changed`] and
    /// [`Self::detect_unhinted_changed`]: every tracked path whose current
    /// on-disk state differs from the last-known state, paired with that
    /// current state (`None` = deleted since the last read). The entries
    /// are snapshotted under the lock and probed outside it — mirroring
    /// `validate`/`record_read`, so a slow stat never stalls concurrent
    /// recorders on parallel tool executions.
    fn scan_drift(&self) -> Vec<(PathBuf, Option<FileState>)> {
        let snapshot: Vec<(PathBuf, FileState)> = self
            .states
            .lock()
            .expect("freshness map lock")
            .iter()
            .map(|(path, known)| (path.clone(), *known))
            .collect();
        snapshot
            .into_iter()
            .filter_map(|(path, known)| {
                let current = match drift_probe(&path) {
                    DriftProbe::Present(state) => Some(state),
                    // Deleted since the last read is drift — arguably the
                    // most important case (a generator removed the file):
                    // surface it so the model rediscovers it.
                    DriftProbe::Deleted => None,
                    // Transient stat failure — skip, not drift.
                    DriftProbe::Unknown => return None,
                };
                (Some(known) != current).then_some((path, current))
            })
            .collect()
    }

    /// Paths whose on-disk fingerprint no longer matches the last known
    /// state — externally modified (or deleted) since their last
    /// read/write (P2-5 `staleReadFileStateHint`). The model's picture of
    /// those files has expired; callers surface the list so the model re-reads
    /// *before* its next edit gets rejected by [`Self::validate`].
    pub fn detect_changed(&self) -> Vec<PathBuf> {
        self.scan_drift().into_iter().map(|(path, _)| path).collect()
    }

    /// [`Self::detect_changed`] minus drift already announced: every path
    /// remembers the on-disk state it was hinted with, so a shell poll
    /// re-reporting the same drift stays silent while a path that drifts
    /// again is re-announced. Marks everything it returns as hinted —
    /// callers should surface the returned list exactly once.
    pub fn detect_unhinted_changed(&self) -> Vec<PathBuf> {
        let drifted = self.scan_drift();
        // No syscalls under this lock — the scan already ran outside it.
        let mut hinted = self.hinted.lock().expect("freshness hint lock");
        drifted
            .into_iter()
            .filter_map(|(path, current)| {
                if hinted.get(&path) == Some(&current) {
                    None // already announced at this exact on-disk state
                } else {
                    hinted.insert(path.clone(), current);
                    Some(path)
                }
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
    /// After a successful shell call that reports a *finished* task, re-stat
    /// every tracked file and append a stale-read hint naming the ones that
    /// changed (P2-5 `staleReadFileStateHint`): the model's picture of
    /// those files just expired, and re-reading beats getting rejected at
    /// the next edit. Never records paths — a shell input has no
    /// legitimate `path` field.
    ShellProbe,
}

/// Shell-family tool names that get the [`FreshnessRole::ShellProbe`]
/// post-success drift probe. Kept in lockstep with the registered shell
/// family (`ToolRegistryBuilder::with_shell_tools` /
/// `with_runtime_task_shell_tools`); a tui-side test pins the two together
/// so a rename or new variant can't silently no-op the probe.
pub const SHELL_PROBE_TOOLS: &[&str] = &[
    "exec_shell",
    "exec_shell_wait",
    "exec_shell_interact",
    "exec_shell_cancel",
    "exec_wait",
    "exec_interact",
    "task_shell_start",
    "task_shell_wait",
];

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
        name if SHELL_PROBE_TOOLS.contains(&name) => FreshnessRole::ShellProbe,
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
        // Shell inputs carry no tracked `path` — extracting (and resolving)
        // one would let a hallucinated field steer the wrapper, so the
        // probe role skips the work entirely.
        let resolved: Vec<PathBuf> = if self.role == FreshnessRole::ShellProbe {
            Vec::new()
        } else {
            target_paths(self.inner.name(), &input)
                .iter()
                .filter_map(|p| context.resolve_path(p).ok())
                .collect()
        };

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
        if !result.success {
            return Ok(result);
        }
        // Only Read/Write roles record: a hallucinated `path` in a shell
        // input must never mark a file known-fresh — that would defeat the
        // read-before-edit gate this module exists to enforce.
        for path in &resolved {
            match self.role {
                FreshnessRole::Read => self.tracker.record_read(path),
                FreshnessRole::Write => self.tracker.record_write(path),
                FreshnessRole::ShellProbe => {}
            }
        }
        // P2-5 staleReadFileStateHint: a successful shell call may have
        // rewritten tracked files (sed -i, generators, formatters). Naming
        // the stale ones now — instead of waiting for the next edit's
        // rejection — turns a passive gate into an active hint, saving a
        // failed tool round. Skipped while the task is still running: the
        // re-probe would race the command's writes — the completing poll
        // (status != "Running") does the probe instead.
        if self.role == FreshnessRole::ShellProbe && !background_task_still_running(&result) {
            // O(tracked-files) stat scan — off the async worker, mirroring
            // the `knowledge/prefetch.rs` blocking-offload convention.
            let tracker = self.tracker.clone();
            let changed = tokio::task::spawn_blocking(move || tracker.detect_unhinted_changed())
                .await
                .unwrap_or_default();
            if let Some(hint) = stale_read_hint(&changed) {
                let mut result = result;
                result.content.push_str(&hint);
                return Ok(result);
            }
        }
        Ok(result)
    }
}

/// Whether `result` reports a shell task that is still running (background
/// start, unfinished poll): its writes have not landed yet, so a drift
/// probe now would miss exactly the bulk-rewriting commands the hint
/// exists for.
fn background_task_still_running(result: &ToolResult) -> bool {
    result
        .metadata
        .as_ref()
        .and_then(|m| m.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status == "Running")
}

/// Render the stale-read hint for a shell result: the changed paths
/// (bounded to five, then an "and N more" tail), byte-stable formatting.
fn stale_read_hint(changed: &[PathBuf]) -> Option<String> {
    if changed.is_empty() {
        return None;
    }
    let mut changed = changed.to_vec();
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
    fn detect_changed_reports_deleted_tracked_files() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = touch_sample(tmp.path(), "removed.txt");

        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&path);
        std::fs::remove_file(&path).expect("external removal");

        assert_eq!(
            tracker.detect_changed(),
            vec![path],
            "a deleted tracked file is drift the model must rediscover"
        );
    }

    #[test]
    fn same_drift_is_hinted_once_until_it_drifts_again() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let drifted = touch_sample(tmp.path(), "poll.txt");

        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&drifted);

        std::fs::write(&drifted, "v2 contents\n").expect("external write");
        assert_eq!(
            tracker.detect_unhinted_changed(),
            vec![drifted.clone()],
            "first drift is announced"
        );
        assert!(
            tracker.detect_unhinted_changed().is_empty(),
            "an identical re-poll must not re-announce the same drift"
        );

        std::fs::write(&drifted, "v3 even longer contents\n").expect("external write");
        assert_eq!(
            tracker.detect_unhinted_changed(),
            vec![drifted.clone()],
            "drift past the hinted state is announced again"
        );

        // A re-read re-arms hinting for the next drift from the new state.
        tracker.record_read(&drifted);
        std::fs::write(&drifted, "v4 yet another longer contents\n").expect("external write");
        assert_eq!(
            tracker.detect_unhinted_changed(),
            vec![drifted],
            "post-re-read drift is announced"
        );
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
        let hint = stale_read_hint(&tracker.detect_unhinted_changed()).expect("hint");
        assert!(hint.contains("f0.txt"));
        assert!(hint.contains("f4.txt"));
        assert!(!hint.contains("f5.txt"), "only the first five are named");
        assert!(hint.contains("and 2 more"));
    }

    /// Minimal `ToolSpec` stand-in for exercising the wrapper's roles.
    struct StubTool {
        name: &'static str,
        metadata: Option<Value>,
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
            let mut result = ToolResult::success("command output".to_string());
            result.metadata = self.metadata.clone();
            Ok(result)
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
                metadata: None,
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
                metadata: None,
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

    #[tokio::test]
    async fn shell_probe_skips_running_background_task() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let drifted = touch_sample(tmp.path(), "gen.txt");
        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&drifted);
        std::fs::write(&drifted, "formatted output\n").expect("external write");

        // task_shell_start / background exec return immediately with
        // status "Running" while the command keeps writing — the probe
        // must wait for the completing poll instead of racing the writes.
        let wrapped = wrap_if_freshness_eligible(
            Arc::new(StubTool {
                name: "task_shell_start",
                metadata: Some(json!({"status": "Running", "task_id": "t-1"})),
            }),
            tracker,
        );
        let context = ToolContext::new(tmp.path().to_path_buf());
        let result = wrapped
            .execute(json!({"command": "cargo fmt"}), &context)
            .await
            .expect("execute");
        assert_eq!(
            result.content, "command output",
            "no hint while the background task is still running"
        );
    }

    #[tokio::test]
    async fn shell_probe_hints_on_completing_poll_and_not_twice() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let drifted = touch_sample(tmp.path(), "gen.txt");
        let tracker = FileFreshnessTracker::new();
        tracker.record_read(&drifted);
        std::fs::write(&drifted, "formatted output\n").expect("external write");

        let context = ToolContext::new(tmp.path().to_path_buf());
        for (tool, expect_hint) in [
            ("task_shell_wait", true),
            ("task_shell_wait", false),
            ("exec_shell_wait", false),
        ] {
            let wrapped = wrap_if_freshness_eligible(
                Arc::new(StubTool {
                    name: tool,
                    metadata: Some(json!({"status": "Completed", "exit_code": 0})),
                }),
                tracker.clone(),
            );
            let result = wrapped
                .execute(json!({"task_id": "t-1"}), &context)
                .await
                .expect("execute");
            assert_eq!(
                result.content.contains("[Files changed on disk since last read"),
                expect_hint,
                "{tool}: hint presence mismatch ({})",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn shell_probe_does_not_record_hallucinated_path() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = touch_sample(tmp.path(), "hallucinated.txt");
        let tracker = FileFreshnessTracker::new();

        // exec_shell's schema doesn't strip unknown fields, so a
        // hallucinated `path` rides along in the input — it must NOT mark
        // the file known-fresh (that would defeat read-before-edit).
        let wrapped = wrap_if_freshness_eligible(
            Arc::new(StubTool {
                name: "exec_shell",
                metadata: None,
            }),
            tracker.clone(),
        );
        let context = ToolContext::new(tmp.path().to_path_buf());
        let result = wrapped
            .execute(json!({"command": "ls", "path": "hallucinated.txt"}), &context)
            .await
            .expect("execute");
        assert!(result.success);
        assert_eq!(result.content, "command output", "nothing drifted");

        let err = tracker
            .validate(&path)
            .expect_err("hallucinated path must not be recorded as read");
        assert!(err.contains("has not been read"), "{err}");
    }
}
