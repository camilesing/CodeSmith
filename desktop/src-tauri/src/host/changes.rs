//! `/api/changes.summary` · `/api/changes.diff` · `/api/changes.open` ·
//! `/api/present.host` · `/api/present.open` — the vendored changes-review
//! tab and changed-files card, served from the thread's durable tool items.
//!
//! Upstream's host snapshots the git tree around each turn; CodeSmith's
//! runtime instead makes every file tool (`write_file`, `edit_file`,
//! `apply_patch`) return a unified diff of what it did, and the item records
//! it durably as its result (`detail`), with the call's arguments in
//! `metadata.arguments`. These routes aggregate one turn's settled
//! `file_change` items into the summary the card lists and re-serve each
//! file's diff as the comparison the review tab renders — the same evidence
//! the transcript's tool cards show, folded per file.
//!
//! Coordinates: the client addresses a summary by the seq of its
//! `workspace/changes` journal event (the mapper emits one per turn that
//! settled a file change); the route resolves seq → turn through a replay of
//! the thread record.
//!
//! Known limitations: a turn's repeated edits to one path fold into one row
//! whose comparison is the LAST call's (the runtime keeps per-call diffs,
//! not a turn-start/end pair — the aggregate counts still sum every call); a
//! `file_change` item whose result carries no parsable diff (a foreign HTTP
//! client's call, or a record from before arguments were kept) lists without
//! counts and its comparison serves empty hunks; binary files carry no
//! marker in the runtime record, so they parse as text.

use std::path::Path;

use serde_json::{json, Value};

use super::HostState;

/// Bytes a file's diff may serve before the comparison degrades (upstream's
/// `maxFileBytes` default).
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

/// One parsed unified-diff hunk (the client's `WorkspaceDiffHunk`).
#[derive(Debug, Clone, PartialEq)]
struct Hunk {
    old_start: u64,
    old_lines: u64,
    new_start: u64,
    new_lines: u64,
    lines: Vec<String>,
}

/// The diff of one file-change call: hunks plus its +/- counts.
#[derive(Debug, Clone, Default)]
struct FileDiff {
    hunks: Vec<Hunk>,
    added: u64,
    deleted: u64,
}

/// One changed file as the summary lists it (and the diff re-serves).
#[derive(Debug, Clone)]
struct ChangedFile {
    /// The call's authored path (absolute or workspace-relative).
    path: String,
    /// Display form: relative to the thread workspace when it lies inside.
    display: String,
    diff: FileDiff,
}

/// Parse the first unified-diff region of a tool result body: `@@` hunk
/// headers with their bodies. Returns `None` when no hunk header appears
/// (the body is a refusal, a notice, or a foreign call's prose).
pub fn parse_unified_diff(text: &str) -> Option<FileDiff> {
    let mut diff = FileDiff::default();
    let mut current: Option<Hunk> = None;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("@@") {
            let header = header.split("@@").next().unwrap_or_default();
            let mut numbers = header.split_whitespace().filter_map(|token| {
                token
                    .strip_prefix('-')
                    .or_else(|| token.strip_prefix('+'))
                    .map(|pair| {
                        let mut parts = pair.split(',');
                        let start: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                        let count: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                        (start.max(1), count)
                    })
            });
            let (old_start, old_lines) = numbers.next().unwrap_or((1, 0));
            let (new_start, new_lines) = numbers.next().unwrap_or((1, 0));
            if let Some(hunk) = current.take() {
                diff.hunks.push(hunk);
            }
            current = Some(Hunk {
                old_start,
                old_lines,
                new_start,
                new_lines,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(hunk) = current.as_mut() {
            // A diff body line is prefixed ' ', '+', '-', or '\'; the tool's
            // trailing summary line after the last hunk ends the region.
            if !matches!(
                line.chars().next(),
                Some(' ') | Some('+') | Some('-') | Some('\\')
            ) {
                break;
            }
            if line.starts_with('+') {
                diff.added += 1;
            } else if line.starts_with('-') {
                diff.deleted += 1;
            }
            hunk.lines.push(line.to_string());
        }
    }
    if let Some(hunk) = current.take() {
        diff.hunks.push(hunk);
    }
    (!diff.hunks.is_empty()).then_some(diff)
}

/// A workspace-relative, slash-separated display path.
fn display_path(workspace: &str, path: &str) -> String {
    let relative = Path::new(path)
        .strip_prefix(workspace)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    relative.replace('\\', "/")
}

/// One turn's changed files, folded per path (a repeated edit keeps the last
/// call's hunks; the counts still sum every call).
fn turn_changes(detail: &Value, turn_id: &str, workspace: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    for item in detail
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if item.get("turn_id").and_then(Value::as_str) != Some(turn_id)
            || item.get("kind").and_then(Value::as_str) != Some("file_change")
            || item.get("status").and_then(Value::as_str) == Some("in_progress")
        {
            continue;
        }
        let path = item
            .pointer("/metadata/arguments/path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if path.is_empty() {
            continue;
        }
        // A call without a parsable diff still lists the path (a foreign
        // client's edit), with zero counts and empty hunks.
        let diff = item
            .get("detail")
            .and_then(Value::as_str)
            .and_then(parse_unified_diff)
            .unwrap_or_default();
        // The fold keys on the workspace-relative display path, so one file
        // addressed in two spellings (absolute and relative) is one row; the
        // row keeps the first call's authored `path`.
        let display = if Path::new(&path).is_absolute() {
            display_path(workspace, &path)
        } else {
            path.replace('\\', "/")
        };
        match files.iter_mut().find(|file| file.display == display) {
            Some(existing) => {
                existing.diff.added += diff.added;
                existing.diff.deleted += diff.deleted;
                if !diff.hunks.is_empty() {
                    existing.diff.hunks = diff.hunks;
                }
            }
            None => files.push(ChangedFile {
                path,
                display,
                diff,
            }),
        }
    }
    files.sort_by(|a, b| a.display.cmp(&b.display));
    files
}

/// The summary a `workspace/changes` event at `seq` announces, as the
/// client's `ChangesSummary`.
async fn changes_summary(host: &HostState, session_id: &str, seq: i64) -> Option<Value> {
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await
        .ok()?;
    let turn_no = super::mapper::replay_events(&detail)
        .into_iter()
        .find(|event| {
            event["type"] == "workspace/changes" && event["seq"].as_i64() == Some(seq)
        })
        .and_then(|event| event["data"]["turn"].as_u64())?;
    let turn = detail
        .get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| turns.get(turn_no.checked_sub(1)? as usize))?;
    let workspace = detail
        .pointer("/thread/workspace")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let files = turn_changes(&detail, turn.get("id")?.as_str()?, workspace);
    let (added, deleted): (u64, u64) =
        files.iter().fold((0, 0), |(a, d), file| {
            (a + file.diff.added, d + file.diff.deleted)
        });
    Some(json!({
        "turn": turn_no,
        "cwd": workspace,
        "files": files.iter().map(|file| json!({
            "path": file.path,
            "display": file.display,
            "added": file.diff.added,
            "deleted": file.diff.deleted,
        })).collect::<Vec<_>>(),
        "total": files.len(),
        "added": added,
        "deleted": deleted,
    }))
}

/// The comparison for one summary file, as the client's `WorkspaceFileDiff`.
async fn changes_diff(
    host: &HostState,
    session_id: &str,
    seq: i64,
    index: usize,
) -> Option<Value> {
    let summary = changes_summary(host, session_id, seq).await?;
    let file = summary
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| files.get(index))?;
    let path = file.get("path")?.as_str()?.to_string();
    let display = file.get("display")?.as_str()?.to_string();
    // Re-fold the turn to recover this file's hunks.
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await
        .ok()?;
    let turn_no = summary.get("turn")?.as_u64()?;
    let turn = detail
        .get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| turns.get(turn_no.checked_sub(1)? as usize))?;
    let workspace = detail
        .pointer("/thread/workspace")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let changed = turn_changes(&detail, turn.get("id")?.as_str()?, workspace)
        .into_iter()
        .find(|changed| changed.path == path)?;
    let hunks_json = |hunks: &[Hunk]| -> Vec<Value> {
        hunks
            .iter()
            .map(|hunk| {
                json!({
                    "oldStart": hunk.old_start,
                    "oldLines": hunk.old_lines,
                    "newStart": hunk.new_start,
                    "newLines": hunk.new_lines,
                    "lines": hunk.lines,
                })
            })
            .collect()
    };
    let hunks_bytes: usize = changed
        .diff
        .hunks
        .iter()
        .map(|hunk| hunk.lines.iter().map(|line| line.len() + 1).sum::<usize>())
        .sum();
    if hunks_bytes > MAX_DIFF_BYTES {
        return Some(json!({ "kind": "oversized", "path": path, "display": display }));
    }
    let before = changed
        .diff
        .hunks
        .iter()
        .any(|hunk| hunk.old_lines > 0 || hunk.lines.iter().any(|l| l.starts_with('-')));
    let after = changed
        .diff
        .hunks
        .iter()
        .any(|hunk| hunk.new_lines > 0 || hunk.lines.iter().any(|l| l.starts_with('+')));
    Some(json!({
        "kind": "text",
        "path": path,
        "display": display,
        "before": before,
        "after": after,
        "hunks": hunks_json(&changed.diff.hunks),
        "coarse": false,
    }))
}

/// Open or reveal one changed file in the OS (`action` = `open` | `reveal`).
/// Blocking (process spawn); the axum handler runs it on `spawn_blocking`.
pub fn open_changed_file(action: &str, path: &str) -> Result<(), String> {
    let target = Path::new(path);
    if !target.is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    let status = match (action, std::env::consts::OS) {
        ("open", "macos") => std::process::Command::new("open").arg(target).status(),
        ("open", "windows") => std::process::Command::new("cmd")
            .args(["/C", "start", "", &target.display().to_string()])
            .status(),
        ("open", _) => std::process::Command::new("xdg-open").arg(target).status(),
        (_, "macos") => std::process::Command::new("open")
            .arg("-R")
            .arg(target)
            .status(),
        (_, "windows") => std::process::Command::new("explorer")
            .arg(format!("/select,{}", target.display()))
            .status(),
        (_, _) => {
            // Reveal on Linux: open the containing directory.
            let parent = target.parent().unwrap_or(target);
            std::process::Command::new("xdg-open").arg(parent).status()
        }
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("open {path} exited {status}")),
        Err(err) => Err(format!("open {path} failed: {err}")),
    }
}

/// The desktop metadata the vendored "Open in" surfaces read.
pub fn present_host() -> Value {
    let file_manager = match std::env::consts::OS {
        "macos" => "finder",
        "windows" => "explorer",
        _ => "directory",
    };
    json!({ "name": "CodeSmith Desktop", "available": true, "fileManager": file_manager })
}

/// The coordinates every changes route addresses a summary by.
pub struct ChangesQuery {
    pub session_id: String,
    pub seq: i64,
    pub index: usize,
}

/// The routes this module serves, wired in `serve_bridge`.
pub mod route {
    use super::*;

    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::Json;
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub(crate) struct ChangesParams {
        sessionId: Option<String>,
        seq: Option<i64>,
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        action: Option<String>,
    }

    fn coordinates(params: &ChangesParams) -> Option<ChangesQuery> {
        Some(ChangesQuery {
            session_id: params.sessionId.clone()?,
            seq: params.seq?,
            index: params.index.unwrap_or(0),
        })
    }

    /// `GET /api/changes.summary?sessionId&seq`
    pub async fn summary(
        State(host): State<super::super::SharedHost>,
        Query(params): Query<ChangesParams>,
    ) -> Response {
        let Some(query) = coordinates(&params) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        match changes_summary(&host, &query.session_id, query.seq).await {
            Some(summary) => Json(summary).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    /// `GET /api/changes.diff?sessionId&seq&index`
    pub async fn diff(
        State(host): State<super::super::SharedHost>,
        Query(params): Query<ChangesParams>,
    ) -> Response {
        let Some(query) = coordinates(&params) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        match changes_diff(&host, &query.session_id, query.seq, query.index).await {
            Some(diff) => Json(diff).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    /// `GET|POST /api/changes.open?sessionId&seq&index&action` — open or
    /// reveal one changed file; 422 is the client's `nativeUnavailable`.
    pub async fn open(
        State(host): State<super::super::SharedHost>,
        Query(params): Query<ChangesParams>,
    ) -> Response {
        let Some(query) = coordinates(&params) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let action = params.action.clone().unwrap_or_else(|| "open".into());
        if action != "open" && action != "reveal" {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let Some(summary) = changes_summary(&host, &query.session_id, query.seq).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Some(path) = summary
            .pointer("/files")
            .and_then(Value::as_array)
            .and_then(|files| files.get(query.index))
            .and_then(|file| file.get("path"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let opened =
            tokio::task::spawn_blocking(move || open_changed_file(&action, &path)).await;
        match opened {
            Ok(Ok(())) => StatusCode::OK.into_response(),
            Ok(Err(_failure)) => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            Err(err) => {
                eprintln!("codesmith desktop: changes.open join failed: {err}");
                StatusCode::UNPROCESSABLE_ENTITY.into_response()
            }
        }
    }

    /// `GET /api/present.host` — desktop metadata for the "Open in" menu.
    pub async fn present_host() -> Response {
        Json(super::present_host()).into_response()
    }

    /// `GET|POST /api/present.open` — the declared-file opener; CodeSmith has
    /// no declared-file registry, so the menu never offers it, and an
    /// explicit call answers the client's `nativeUnavailable`.
    pub async fn present_open() -> Response {
        StatusCode::UNPROCESSABLE_ENTITY.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_diffs_parse_into_hunks_and_counts() {
        let body = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n fn main() {\n-    let x = 1;\n+    let x = 2;\n+    let y = 3;\n }\nWrote 42 bytes to src/lib.rs";
        let diff = parse_unified_diff(body).expect("the diff parses");
        assert_eq!(diff.added, 2);
        assert_eq!(diff.deleted, 1);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].old_start, 1);
        assert_eq!(diff.hunks[0].old_lines, 3);
        assert_eq!(diff.hunks[0].new_start, 1);
        assert_eq!(diff.hunks[0].new_lines, 4);
        assert_eq!(diff.hunks[0].lines.len(), 5, "context plus changes");
        // The trailing tool summary line ends the region.
        assert!(!diff.hunks[0].lines.iter().any(|l| l.contains("Wrote")));
    }

    #[test]
    fn a_creation_diff_has_no_old_side_and_results_without_diffs_list_clean() {
        let body = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\nCreated new.txt (12 bytes)";
        let diff = parse_unified_diff(body).expect("the creation parses");
        assert_eq!(diff.added, 2);
        assert_eq!(diff.deleted, 0);
        assert_eq!(diff.hunks[0].old_start, 1);
        assert_eq!(diff.hunks[0].old_lines, 0);
        // A body with no hunk header is not a diff.
        assert!(parse_unified_diff("Wrote 3 bytes to a.txt\n(no changes)").is_none());
    }

    #[test]
    fn a_turn_folds_repeated_edits_to_one_path() {
        let detail = json!({
            "thread": { "workspace": "/w/proj" },
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [
                { "id": "i1", "turn_id": "turn_1", "kind": "file_change", "status": "completed",
                  "detail": "@@ -1 +1 @@\n-a\n+b\n",
                  "metadata": { "arguments": { "path": "/w/proj/src/a.rs" } } },
                { "id": "i2", "turn_id": "turn_1", "kind": "file_change", "status": "completed",
                  "detail": "@@ -5 +5 @@\n-c\n+d\n+e\n",
                  "metadata": { "arguments": { "path": "src/a.rs" } } },
                // A non-file item and an in-progress edit are ignored.
                { "id": "i3", "turn_id": "turn_1", "kind": "agent_message", "status": "completed" },
                { "id": "i4", "turn_id": "turn_1", "kind": "file_change", "status": "in_progress",
                  "metadata": { "arguments": { "path": "/w/proj/b.rs" } } },
            ],
        });
        let files = turn_changes(&detail, "turn_1", "/w/proj");
        assert_eq!(files.len(), 1, "one row for one path in two spellings");
        assert_eq!(files[0].display, "src/a.rs");
        assert_eq!(files[0].diff.added, 3, "counts sum both calls");
        assert_eq!(files[0].diff.deleted, 2);
        assert_eq!(files[0].diff.hunks.len(), 1, "the last call's hunks");
    }
}
