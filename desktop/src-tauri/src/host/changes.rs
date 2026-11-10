//! `/api/changes.summary` · `/api/changes.diff` · `/api/changes.open` ·
//! `/api/present.host` · `/api/present.open` — the vendored changes-review
//! tab and changed-files card, served from the thread's durable tool items.
//!
//! Upstream's host snapshots the git tree around each turn; CodeSmith's
//! runtime instead has every file tool report what it did, and the item
//! records that report durably: `write_file` and `edit_file` return a
//! unified diff as their result (`detail`), while `apply_patch` returns a
//! JSON summary and keeps the requested hunks in its `patch` argument
//! (recovered here, one region per file header). The call's arguments stay
//! in `metadata.arguments`. These routes aggregate one turn's completed
//! `file_change` items into the summary the card lists and re-serve each
//! file's diff as the comparison the review tab renders — the same evidence
//! the transcript's tool cards show, folded per file. A failed or denied
//! call changed nothing and is not listed.
//!
//! Coordinates: the client addresses a summary by the seq of its
//! `workspace/changes` journal event (the mapper emits one per turn that
//! settled a file change); the route resolves seq → turn through a replay of
//! the thread record.
//!
//! Known limitations: a turn's repeated edits to one path fold into one row
//! whose comparison is the LAST call's (the runtime keeps per-call diffs,
//! not a turn-start/end pair — the aggregate counts still sum every call);
//! `apply_patch`'s whole-file `changes` form lists its paths without a
//! comparison (the runtime keeps no pre-written content to diff against); a
//! `file_change` item whose result carries no parsable diff (a foreign HTTP
//! client's call, or a record from before arguments were kept) lists without
//! counts and its comparison serves empty hunks; binary files carry no
//! marker in the runtime record, so they parse as text.

use std::path::{Path, PathBuf};

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
fn parse_unified_diff(text: &str) -> Option<FileDiff> {
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
            // A "\ No newline at end of file" marker is not a hunk body line
            // (the client's hunk validator accepts only '+', '-', ' ');
            // skipping it keeps the served comparison decodable.
            if line.starts_with('\\') {
                continue;
            }
            // A diff body line is prefixed ' ', '+', or '-'; the tool's
            // trailing summary line after the last hunk ends the region.
            if !matches!(line.chars().next(), Some(' ') | Some('+') | Some('-')) {
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

/// One file path from a `---`/`+++` patch header, normalized the way
/// `apply_patch`'s own parser normalizes it: drop a tab-separated timestamp,
/// diff(1)'s quoting, and the `a/`/`b/` prefix. `/dev/null` names no side.
fn patch_header_path(raw: &str) -> Option<String> {
    let raw = raw.split('\t').next().unwrap_or(raw).trim();
    let raw = raw.trim_matches('"');
    if raw.is_empty() || raw == "/dev/null" || raw == "dev/null" {
        return None;
    }
    Some(
        raw.strip_prefix("a/")
            .or_else(|| raw.strip_prefix("b/"))
            .unwrap_or(raw)
            .to_string(),
    )
}

/// Per-file hunks of a multi-file `patch` argument, keyed by the path the
/// tool itself resolved. A path-override call passes one hunk stream instead
/// and never reaches this splitter.
fn patch_file_diffs(patch: &str) -> Vec<(String, FileDiff)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut old_path: Option<String> = None;
    for line in patch.lines() {
        if let Some(raw) = line.strip_prefix("--- ") {
            old_path = patch_header_path(raw);
        } else if let Some(raw) = line.strip_prefix("+++ ") {
            // A deletion's `+++ /dev/null` names the old side.
            if let Some(path) = patch_header_path(raw).or_else(|| old_path.clone()) {
                sections.push((path, String::new()));
            }
        } else if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections
        .into_iter()
        .filter_map(|(path, body)| parse_unified_diff(&body).map(|diff| (path, diff)))
        .collect()
}

/// The (path, diff) entries one completed file-change item reports.
///
/// `write_file`/`edit_file` return the diff as their result; `apply_patch`
/// returns a JSON summary and keeps the hunks in its argument. Its whole-file
/// `changes` form names paths with no comparison — there is no pre-written
/// content to diff against. A call without a parsable diff still lists its
/// authored path (a foreign client's edit) with zero counts and no hunks.
fn item_entries(item: &Value) -> Vec<(String, FileDiff)> {
    let arguments = item
        .pointer("/metadata/arguments")
        .cloned()
        .unwrap_or(Value::Null);
    let authored = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty());
    let result_diff = item
        .get("detail")
        .and_then(Value::as_str)
        .and_then(parse_unified_diff);
    if let Some(diff) = result_diff {
        return authored
            .map(|path| vec![(path.to_string(), diff)])
            .unwrap_or_default();
    }
    if let Some(patch) = arguments.get("patch").and_then(Value::as_str) {
        // With `path`, the tool applies the whole hunk stream to that one
        // file (any headers are advisory); without it, per-header regions.
        if let Some(path) = authored {
            return vec![(
                path.to_string(),
                parse_unified_diff(patch).unwrap_or_default(),
            )];
        }
        return patch_file_diffs(patch);
    }
    if let Some(changes) = arguments.get("changes").and_then(Value::as_array) {
        return changes
            .iter()
            .filter_map(|change| {
                change
                    .get("path")
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(|path| (path.to_string(), FileDiff::default()))
            })
            .collect();
    }
    authored
        .map(|path| (path.to_string(), FileDiff::default()))
        .into_iter()
        .collect()
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
        // Only a completed call changed a file: a failed or denied one left
        // the workspace untouched and must not be listed.
        if item.get("turn_id").and_then(Value::as_str) != Some(turn_id)
            || item.get("kind").and_then(Value::as_str) != Some("file_change")
            || item.get("status").and_then(Value::as_str) != Some("completed")
        {
            continue;
        }
        for (path, diff) in item_entries(item) {
            // The fold keys on the workspace-relative display path, so one
            // file addressed in two spellings (absolute and relative) is one
            // row; the row keeps the first call's authored `path`.
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
    }
    files.sort_by(|a, b| a.display.cmp(&b.display));
    files
}

/// The thread's workspace: the root listed `path` values are relative to.
fn workspace_of(detail: &Value) -> &str {
    detail
        .pointer("/thread/workspace")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The thread record plus the turn one `workspace/changes` seq announced.
/// The route's coordinates are a replay cursor, so seq → turn resolves through
/// the same replay the client consumed.
async fn resolve_turn(host: &HostState, session_id: &str, seq: i64) -> Option<(Value, u64, Value)> {
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await
        .ok()?;
    let turn_no = super::mapper::replay_events(&detail)
        .into_iter()
        .find(|event| event["type"] == "workspace/changes" && event["seq"].as_i64() == Some(seq))
        .and_then(|event| event["data"]["turn"].as_u64())?;
    let turn = detail
        .get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| turns.get(turn_no.checked_sub(1)? as usize))?
        .clone();
    Some((detail, turn_no, turn))
}

/// The wire summary for one replayed turn, as the client's `ChangesSummary`
/// (`{turn, files, total, added, deleted}` — upstream keeps the working
/// directory and snapshot ids on the Host).
fn summary_for_turn(detail: &Value, turn_no: u64, turn: &Value) -> Value {
    let files = turn_changes(
        detail,
        turn.get("id").and_then(Value::as_str).unwrap_or_default(),
        workspace_of(detail),
    );
    let (added, deleted): (u64, u64) = files.iter().fold((0, 0), |(a, d), file| {
        (a + file.diff.added, d + file.diff.deleted)
    });
    json!({
        "turn": turn_no,
        "files": files.iter().map(|file| json!({
            "path": file.path,
            "display": file.display,
            "added": file.diff.added,
            "deleted": file.diff.deleted,
        })).collect::<Vec<_>>(),
        "total": files.len(),
        "added": added,
        "deleted": deleted,
    })
}

/// The comparison for one listed file, as the client's `WorkspaceFileDiff`.
fn diff_for_changed(changed: &ChangedFile) -> Value {
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
        return json!({
            "kind": "oversized",
            "path": changed.path,
            "display": changed.display,
        });
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
    json!({
        "kind": "text",
        "path": changed.path,
        "display": changed.display,
        "before": before,
        "after": after,
        "hunks": hunks_json(&changed.diff.hunks),
        "coarse": false,
    })
}

/// The summary a `workspace/changes` event at `seq` announces.
async fn changes_summary(host: &HostState, session_id: &str, seq: i64) -> Option<Value> {
    let (detail, turn_no, turn) = resolve_turn(host, session_id, seq).await?;
    Some(summary_for_turn(&detail, turn_no, &turn))
}

/// The comparison for one summary file.
async fn changes_diff(host: &HostState, session_id: &str, seq: i64, index: usize) -> Option<Value> {
    let (detail, _turn_no, turn) = resolve_turn(host, session_id, seq).await?;
    // The summary's files are this same fold, in display order.
    let changed = turn_changes(&detail, turn.get("id")?.as_str()?, workspace_of(&detail))
        .into_iter()
        .nth(index)?;
    Some(diff_for_changed(&changed))
}

/// Where a listed file lives on this machine: an absolute listed path is used
/// as-is (upstream lists files outside the workspace too); a relative one
/// resolves against the session workspace. `None` is the client's
/// `nativeUnavailable` — the path cannot be mapped.
fn open_target(workspace: &str, path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    (!workspace.is_empty()).then(|| Path::new(workspace).join(path))
}

/// One changed-file open attempt: the launcher's outcome, with "the file is
/// gone" separated from "the opener failed" (upstream's 404 vs 500).
enum OpenOutcome {
    Opened,
    Missing,
    Failed(String),
}

/// Open or reveal one changed file in the OS (`action` = `open` | `reveal`).
/// Blocking (stat + process spawn); the axum handler runs it on
/// `spawn_blocking`.
pub fn open_changed_file(action: &str, target: &Path) -> Result<(), String> {
    if !target.is_absolute() {
        return Err(format!("{} is not an absolute path", target.display()));
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
        Ok(status) => Err(format!("open {} exited {status}", target.display())),
        Err(err) => Err(format!("open {} failed: {err}", target.display())),
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
    use axum::http::{header, HeaderValue, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::Json;
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub(crate) struct ChangesParams {
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
        seq: Option<i64>,
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        action: Option<String>,
    }

    fn coordinates(params: &ChangesParams) -> Option<ChangesQuery> {
        Some(ChangesQuery {
            session_id: params.session_id.clone()?,
            seq: params.seq?,
            index: params.index?,
        })
    }

    /// The summary and comparison responses must not be cached: a listed
    /// file's record can gain hunks only before its turn settles, but the
    /// client treats a cached 404 as permanent (`retryable: false`), so a
    /// pre-settlement miss would pin "missing" across the turn's end.
    fn no_store(response: Response) -> Response {
        let mut response = response;
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }

    /// `GET /api/changes.summary?sessionId&seq`
    pub async fn summary(
        State(host): State<super::super::SharedHost>,
        Query(params): Query<ChangesParams>,
    ) -> Response {
        let Some(session_id) = params.session_id.clone() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let Some(seq) = params.seq else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        no_store(match changes_summary(&host, &session_id, seq).await {
            Some(summary) => Json(summary).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        })
    }

    /// `GET /api/changes.diff?sessionId&seq&index`
    pub async fn diff(
        State(host): State<super::super::SharedHost>,
        Query(params): Query<ChangesParams>,
    ) -> Response {
        let Some(query) = coordinates(&params) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        no_store(
            match changes_diff(&host, &query.session_id, query.seq, query.index).await {
                Some(diff) => Json(diff).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            },
        )
    }

    /// `GET|POST /api/changes.open?sessionId&seq&index&action` — open or
    /// reveal one changed file. 404 means the listed file is gone, 422 the
    /// path cannot be mapped to this machine (the client's
    /// `nativeUnavailable`), 500 the launcher failed.
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
        let Some((detail, _turn_no, turn)) =
            resolve_turn(&host, &query.session_id, query.seq).await
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let workspace = workspace_of(&detail).to_string();
        let Some(changed) = turn.get("id").and_then(Value::as_str).and_then(|turn_id| {
            turn_changes(&detail, turn_id, &workspace)
                .into_iter()
                .nth(query.index)
        }) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Some(target) = open_target(&workspace, &changed.path) else {
            return no_store(StatusCode::UNPROCESSABLE_ENTITY.into_response());
        };
        let opened = tokio::task::spawn_blocking(move || {
            if !target.exists() {
                return OpenOutcome::Missing;
            }
            match open_changed_file(&action, &target) {
                Ok(()) => OpenOutcome::Opened,
                Err(err) => OpenOutcome::Failed(err),
            }
        })
        .await;
        no_store(match opened {
            Ok(OpenOutcome::Opened) => StatusCode::NO_CONTENT.into_response(),
            Ok(OpenOutcome::Missing) => StatusCode::NOT_FOUND.into_response(),
            Ok(OpenOutcome::Failed(err)) => {
                // The launcher's own text can carry the path; keep it in the
                // log, answer the client with the status only.
                eprintln!("codesmith desktop: changes.open failed: {err}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
            Err(err) => {
                eprintln!("codesmith desktop: changes.open join failed: {err}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        })
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

    #[test]
    fn apply_patch_comparisons_come_from_its_patch_argument() {
        // apply_patch's result is a JSON summary, not a diff: the requested
        // hunks ride the call's own argument, one region per file header.
        let patch = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+extra\n--- /dev/null\n+++ b/src/b.rs\n@@ -0,0 +1 @@\n+created\n";
        let detail = json!({
            "thread": { "workspace": "/w/proj" },
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [
                { "id": "i1", "turn_id": "turn_1", "kind": "file_change", "status": "completed",
                  "detail": "{\"success\":true,\"touched_files\":[\"src/a.rs\",\"src/b.rs\"]}",
                  "metadata": { "arguments": { "patch": patch } } },
            ],
        });
        let files = turn_changes(&detail, "turn_1", "/w/proj");
        assert_eq!(
            files
                .iter()
                .map(|file| file.display.clone())
                .collect::<Vec<_>>(),
            vec!["src/a.rs", "src/b.rs"]
        );
        let a = files
            .iter()
            .find(|file| file.display == "src/a.rs")
            .unwrap();
        assert_eq!((a.diff.added, a.diff.deleted), (2, 1));
        assert_eq!(a.diff.hunks.len(), 1);
        let b = files
            .iter()
            .find(|file| file.display == "src/b.rs")
            .unwrap();
        assert_eq!((b.diff.added, b.diff.deleted), (1, 0));
        // The comparison flags the creation: no old side.
        let diff = diff_for_changed(b);
        assert_eq!(diff["before"], false);
        assert_eq!(diff["after"], true);
        assert_eq!(diff["hunks"][0]["oldLines"], 0);
        // Every served hunk line stays decodable by the client.
        for hunk in diff["hunks"].as_array().unwrap() {
            for line in hunk["lines"].as_array().unwrap() {
                let line = line.as_str().unwrap();
                assert!(
                    line.starts_with('+') || line.starts_with('-') || line.starts_with(' '),
                    "{line:?}"
                );
            }
        }
    }

    #[test]
    fn apply_patch_whole_file_changes_list_paths_without_a_comparison() {
        let detail = json!({
            "thread": { "workspace": "/w/proj" },
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [
                { "id": "i1", "turn_id": "turn_1", "kind": "file_change", "status": "completed",
                  "detail": "{\"success\":true}",
                  "metadata": { "arguments": { "changes": [
                      { "path": "docs/a.md", "content": "x" },
                      { "path": "docs/b.md", "content": "y" }
                  ] } } },
            ],
        });
        let files = turn_changes(&detail, "turn_1", "/w/proj");
        assert_eq!(
            files
                .iter()
                .map(|file| file.display.clone())
                .collect::<Vec<_>>(),
            vec!["docs/a.md", "docs/b.md"]
        );
        assert!(files.iter().all(|file| file.diff.hunks.is_empty()));
        let summary = summary_for_turn(&detail, 1, &detail["turns"][0]);
        assert_eq!(summary["total"], 2);
        assert_eq!(summary["added"], 0);
        // Upstream keeps the working directory on the Host, not on the wire.
        assert!(!summary.as_object().unwrap().contains_key("cwd"));
    }

    #[test]
    fn open_targets_resolve_relative_paths_against_the_workspace() {
        // The summary lists workspace-relative paths (upstream's shape); the
        // OS opener needs an absolute one. A session without a workspace
        // cannot map a relative path — the client's nativeUnavailable.
        let workspace = std::env::temp_dir();
        let workspace_text = workspace.to_string_lossy().into_owned();
        assert_eq!(
            open_target(&workspace_text, "notes/a.txt"),
            Some(workspace.join("notes").join("a.txt"))
        );
        assert_eq!(open_target("", "notes/a.txt"), None);
        let absolute = workspace.join("elsewhere.txt");
        assert_eq!(
            open_target(&workspace_text, &absolute.to_string_lossy()),
            Some(absolute)
        );
    }

    #[test]
    fn failed_file_calls_are_not_changed_files() {
        let detail = json!({
            "thread": { "workspace": "/w/proj" },
            "turns": [{ "id": "turn_1", "status": "failed" }],
            "items": [
                { "id": "i1", "turn_id": "turn_1", "kind": "file_change", "status": "failed",
                  "detail": "permission denied",
                  "metadata": { "arguments": { "path": "src/a.rs" } } },
            ],
        });
        assert!(turn_changes(&detail, "turn_1", "/w/proj").is_empty());
        assert_eq!(
            summary_for_turn(&detail, 1, &detail["turns"][0])["total"],
            0
        );
    }

    #[test]
    fn summary_and_comparison_serve_one_fold() {
        let detail = json!({
            "thread": { "workspace": "/w/proj" },
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [
                { "id": "i1", "turn_id": "turn_1", "kind": "file_change", "status": "completed",
                  "detail": "@@ -0,0 +1,3 @@\n+alpha\n+beta\n+gamma\n",
                  "metadata": { "arguments": { "path": "src/a.rs" } } },
                { "id": "i2", "turn_id": "turn_1", "kind": "file_change", "status": "completed",
                  "detail": "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n",
                  "metadata": { "arguments": { "path": "src/a.rs" } } },
                { "id": "i3", "turn_id": "turn_1", "kind": "file_change", "status": "completed",
                  "detail": "@@ -0,0 +1,2 @@\n+one\n+two\n",
                  "metadata": { "arguments": { "path": "src/b.rs" } } },
            ],
        });
        let summary = summary_for_turn(&detail, 1, &detail["turns"][0]);
        assert_eq!(summary["turn"], 1);
        assert_eq!(summary["total"], 2);
        assert_eq!(summary["added"], 6, "counts sum every call");
        assert_eq!(summary["deleted"], 1);
        let files = turn_changes(&detail, "turn_1", "/w/proj");
        let diff = diff_for_changed(&files[0]);
        assert_eq!(diff["path"], "src/a.rs");
        assert_eq!(diff["display"], "src/a.rs");
        assert_eq!(diff["before"], true);
        assert_eq!(diff["after"], true);
        assert_eq!(
            diff["hunks"][0]["lines"],
            json!([" alpha", "-beta", "+BETA", " gamma"]),
            "the comparison is the last call's hunks"
        );
    }
}
