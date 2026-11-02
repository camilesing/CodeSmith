//! `search_sessions` tool — cross-session content search (round-3 TODO-43;
//! persistent index P1-5).
//!
//! Model-visible, workspace-scoped search over the saved session
//! transcripts (`<sessions>/*.json`, the same directory `FileSessionStore`
//! writes). The primary path is a **persistent per-workspace FTS5 index**
//! (`codesmith_index::session_index`, `~/.codesmith/index/<ws-hash>/
//! sessions.db`): each query lazily refreshes it — sessions whose
//! *generation* (journal file mtime+size when one exists, snapshot
//! mtime+size otherwise) is unchanged are not re-read — then queries it.
//! The scan path survives verbatim as the explicit fallback when the index
//! cannot open; the payload's `via` field says which one ran
//! (`"index"` / `"scan"`).
//!
//! Reconciliation (P1-5): every indexed row carries the generation it was
//! indexed under, and every hit is re-checked against the live session
//! file's generation before it is returned — a session rewritten by
//! compaction or edit rollback invalidates its old hits instead of silently
//! serving content the current transcript no longer holds.
//!
//! Known limitations (deliberate): refresh is lazy and caller-driven (no
//! background writer); sessions beyond the refresh cap keep their indexed
//! rows, guarded by the hit-time generation check. FTS5's bm25 ranks close
//! to but not identically with the scan path's hand-rolled BM25, and CJK
//! text still collapses to one long token. Scope is the workspace key only
//! — the `parent_session_id` lineage chain is NOT traversed. Only the
//! shared REPL sessions dir is covered; RLM/background session stores are
//! separate surfaces. The current session's own file participates as last
//! saved (turn-boundary autosave).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};
use codesmith_index::session_index::{SessionIndex, SessionProjection, fts_query_from_terms};
use codesmith_session::store::SavedSession;

use super::recall_archive::{
    CONTEXT_WINDOW_CHARS, best_window, bm25_rank, message_text, round_score, tokenize,
};

/// Same result envelope as `recall_archive` — the sibling search surface.
const DEFAULT_MAX_RESULTS: usize = 3;
const HARD_MAX_RESULTS: usize = 10;
/// Scan cap: at most this many session files are read per call, newest
/// first — the bound that keeps a call off an unbounded home directory.
/// `FileSessionStore` already retains only ~50 sessions, so in practice
/// this is the second line for non-store writers (imports, older layouts).
const MAX_SESSIONS_SCANNED: usize = 200;
/// Per-session read cap: files larger than this are skipped (and counted).
/// A saved session is pretty-printed JSON; healthy sessions are far below.
const MAX_SESSION_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Aggregate cap on retained message text: sessions are read newest-first,
/// so once this much text is in memory the remaining (older) sessions
/// cannot beat what is already collected. Without it, 200 × 10 MiB of
/// oversized-but-legal files would materialize ~2 GiB in one call.
const MAX_TOTAL_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub struct SessionSearchTool;

/// One scoreable message with its session provenance.
struct SessionDoc {
    session_id: String,
    title: String,
    updated_at: chrono::DateTime<chrono::Utc>,
    message_index: usize,
    role: String,
    text: String,
    tokens: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SessionHit {
    session_id: String,
    title: String,
    updated_at: chrono::DateTime<chrono::Utc>,
    message_index: usize,
    role: String,
    score: f64,
    /// Short window around the best match, with `…` markers when truncated.
    excerpt: String,
}

/// Bookkeeping surfaced in every result so skips are visible, not silent.
#[derive(Default, Serialize)]
struct ScanStats {
    sessions_parsed: usize,
    sessions_in_workspace: usize,
    skipped_other_workspace: usize,
    skipped_oversize: usize,
    skipped_corrupt: usize,
    scan_truncated: bool,
}

#[async_trait]
impl ToolSpec for SessionSearchTool {
    fn name(&self) -> &'static str {
        "search_sessions"
    }

    fn description(&self) -> &'static str {
        "Search past session transcripts in this workspace by content (BM25 over \
         messages). Use to recall decisions or context from earlier conversations; \
         returns matching excerpts with session id and title."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query. Tokenized and BM25-scored against saved session messages."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum hits to return (default 3, hard-capped at 10)."
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = required_str(&input, "query")?.trim().to_string();
        if query.is_empty() {
            return Err(ToolError::invalid_input("query cannot be empty"));
        }
        let max_results = match input.get("max_results") {
            None | Some(Value::Null) => DEFAULT_MAX_RESULTS,
            Some(v) => match v.as_u64() {
                Some(n) => usize::try_from(n)
                    .unwrap_or(DEFAULT_MAX_RESULTS)
                    .clamp(1, HARD_MAX_RESULTS),
                None => {
                    return Err(ToolError::invalid_input(
                        "max_results must be a positive integer",
                    ));
                }
            },
        };

        let workspace = context.workspace.clone();
        let scan = tokio::task::spawn_blocking(move || -> Result<ToolResult, ToolError> {
            let sessions_dir = match sessions_root() {
                Ok(dir) => dir,
                Err(err) => {
                    return Err(ToolError::execution_failed(format!(
                        "Failed to resolve sessions directory: {err}"
                    )));
                }
            };
            // P1-5: the persistent per-workspace FTS index is the primary
            // path; `None` falls back to the scan path (payload says which
            // ran via `via`).
            if let Some(result) = index_search(&sessions_dir, &workspace, &query, max_results) {
                return result;
            }
            let (docs, stats) = scan_saved_sessions(&sessions_dir, &workspace);
            let query_tokens = tokenize(&query);
            if query_tokens.is_empty() {
                return Err(ToolError::invalid_input(
                    "query has no scoring tokens after tokenization",
                ));
            }

            let token_sets: Vec<&[String]> = docs.iter().map(|d| d.tokens.as_slice()).collect();
            let hits: Vec<SessionHit> = bm25_rank(&token_sets, &query_tokens, max_results)
                .into_iter()
                .map(|(idx, score)| {
                    let doc = &docs[idx];
                    SessionHit {
                        session_id: doc.session_id.clone(),
                        title: doc.title.clone(),
                        updated_at: doc.updated_at,
                        role: doc.role.clone(),
                        message_index: doc.message_index,
                        score: round_score(score),
                        excerpt: best_window(&doc.text, &query_tokens, CONTEXT_WINDOW_CHARS),
                    }
                })
                .collect();

            let payload = json!({
                "query": query,
                "workspace": workspace.display().to_string(),
                "via": "scan",
                "sessions_parsed": stats.sessions_parsed,
                "sessions_in_workspace": stats.sessions_in_workspace,
                "skipped_other_workspace": stats.skipped_other_workspace,
                "skipped_oversize": stats.skipped_oversize,
                "skipped_corrupt": stats.skipped_corrupt,
                "scan_truncated": stats.scan_truncated,
                "hits": hits,
            });
            Ok(ToolResult::success(payload.to_string()))
        });

        scan.await.map_err(|err| {
            ToolError::execution_failed(format!("session scan task failed: {err}"))
        })?
    }
}

/// The saved-session root: the resolved CodeSmith state dir (prefers
/// `~/.codesmith/sessions`), mirroring `recall_archive`'s resolution.
fn sessions_root() -> Result<PathBuf, std::io::Error> {
    match codesmith_config::resolve_state_dir("sessions") {
        Ok(dir) => Ok(dir),
        Err(_) => {
            let home = dirs::home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not resolve home directory for sessions root",
                )
            })?;
            Ok(home.join(".codesmith").join("sessions"))
        }
    }
}

/// Canonicalize for comparison, falling back to the raw path when the
/// target no longer exists (a session's recorded workspace may have been
/// moved/deleted since it was saved — that session must stay comparable,
/// not silently become scope-mismatched garbage).
fn comparable(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        // `canonicalize` yields verbatim `\\?\C:\...` paths on Windows
        // while the not-found fallback keeps the unprefixed spelling;
        // stripping the prefix keeps both branches comparable.
        const VERBATIM_PREFIX: &str = "\\\\?\\";
        let text = canonical.to_string_lossy();
        if let Some(stripped) = text.strip_prefix(VERBATIM_PREFIX) {
            return PathBuf::from(stripped);
        }
    }
    canonical
}

/// Read at most `MAX_SESSION_FILE_BYTES + 1` bytes from `file` and parse,
/// so the cap holds even if the file grows between fstat and read.
fn read_capped(file: std::fs::File) -> ReadOutcome {
    use std::io::Read;
    let mut buf = Vec::new();
    if file
        .take(MAX_SESSION_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .is_err()
    {
        return ReadOutcome::Corrupt;
    }
    if buf.len() > MAX_SESSION_FILE_BYTES as usize {
        return ReadOutcome::Oversize;
    }
    match String::from_utf8(buf)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
    {
        Some(session) => ReadOutcome::Parsed(Box::new(session)),
        None => ReadOutcome::Corrupt,
    }
}

/// Enumerate, scope-filter, and load the scoreable corpus. Blocking — runs
/// under `spawn_blocking` (directory walks + file reads).
fn scan_saved_sessions(sessions_dir: &Path, workspace: &Path) -> (Vec<SessionDoc>, ScanStats) {
    let mut stats = ScanStats::default();
    let mut docs: Vec<SessionDoc> = Vec::new();

    // Candidates: *.json files with mtimes, newest first (the scan cap then
    // drops the OLDEST sessions, the least likely to be relevant).
    let candidates = enumerate_candidates(sessions_dir);
    if candidates.truncated {
        stats.scan_truncated = true;
    }

    let workspace_key = comparable(workspace);
    let mut retained_text_bytes = 0usize;
    'sessions: for (_mtime, path) in &candidates.files {
        let session = match read_session_file(path) {
            ReadOutcome::Parsed(session) => session,
            ReadOutcome::Oversize => {
                stats.skipped_oversize += 1;
                continue;
            }
            ReadOutcome::Corrupt => {
                stats.skipped_corrupt += 1;
                continue;
            }
        };
        stats.sessions_parsed += 1;
        if comparable(&session.metadata.workspace) != workspace_key {
            stats.skipped_other_workspace += 1;
            continue;
        }
        stats.sessions_in_workspace += 1;
        for (idx, message) in session.messages.iter().enumerate() {
            let role = message.role.clone();
            let text = message_text(message);
            if text.trim().is_empty() {
                continue;
            }
            let tokens = tokenize(&text);
            if tokens.is_empty() {
                continue;
            }
            retained_text_bytes += text.len();
            docs.push(SessionDoc {
                session_id: session.metadata.id.clone(),
                title: session.metadata.title.clone(),
                updated_at: session.metadata.updated_at,
                message_index: idx,
                role,
                text,
                tokens,
            });
            if retained_text_bytes >= MAX_TOTAL_TEXT_BYTES {
                // Sessions are scanned newest-first; older material cannot
                // outrank what is already collected.
                stats.scan_truncated = true;
                break 'sessions;
            }
        }
    }
    (docs, stats)
}

/// The generation of one session's durable content: the journal file's
/// (mtime_ms, size) when the session keeps a journal (the journal moves on
/// every append and rebuild, so it is the finer-grained boundary), else the
/// snapshot file's. `None` when neither file states — the session is gone.
fn generation_of(session_id: &str, sessions_dir: &Path) -> Option<(i64, i64)> {
    let journal = sessions_dir.join(format!("{session_id}.journal.jsonl"));
    let snapshot = sessions_dir.join(format!("{session_id}.json"));
    let file = if journal.is_file() { journal } else { snapshot };
    let meta = std::fs::metadata(file).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some((mtime_ms, i64::try_from(meta.len()).ok()?))
}

/// The persistent-index path (P1-5): lazily refresh the per-workspace FTS
/// index, query it, and reconcile every hit against the live session files.
/// `None` means "index unusable" — the caller falls back to the scan path.
fn index_search(
    sessions_dir: &Path,
    workspace: &Path,
    query: &str,
    max_results: usize,
) -> Option<Result<ToolResult, ToolError>> {
    let mut index = SessionIndex::open_default(workspace).ok()?;

    // Refresh, newest-first over the same candidate set the scan path uses.
    let candidates = enumerate_candidates(sessions_dir);
    let mut stats = ScanStats::default();
    if candidates.truncated {
        stats.scan_truncated = true;
    }
    let workspace_key = comparable(workspace);
    let mut live_ids: Vec<String> = Vec::new();
    for (_mtime, path) in &candidates.files {
        let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        live_ids.push(session_id.to_string());
        // The incremental win: a session whose generation is unchanged is
        // not re-read at all — stat only.
        let Some(generation) = generation_of(session_id, sessions_dir) else {
            continue;
        };
        if index.tracked_generation(session_id).ok().flatten() == Some(generation) {
            // Unchanged: counted as an in-workspace session this call, but
            // not re-read — the incremental win.
            stats.sessions_in_workspace += 1;
            continue;
        }
        let session = match read_session_file(path) {
            ReadOutcome::Parsed(session) => session,
            ReadOutcome::Oversize => {
                stats.skipped_oversize += 1;
                continue;
            }
            ReadOutcome::Corrupt => {
                stats.skipped_corrupt += 1;
                continue;
            }
        };
        stats.sessions_parsed += 1;
        if comparable(&session.metadata.workspace) != workspace_key {
            stats.skipped_other_workspace += 1;
            continue;
        }
        stats.sessions_in_workspace += 1;
        let messages = project_messages(&session);
        let projection = SessionProjection {
            session_id: session.metadata.id.clone(),
            workspace_key: workspace_key.display().to_string(),
            title: session.metadata.title.clone(),
            updated_at: session.metadata.updated_at.to_rfc3339(),
            generation,
            messages,
        };
        index.upsert_session(&projection).ok()?;
    }
    let live_refs: Vec<&str> = live_ids.iter().map(String::as_str).collect();
    let _pruned = index.prune_missing(&live_refs).unwrap_or(0);

    let fts_query = fts_query_from_terms(query);
    if fts_query.is_empty() {
        // Same error contract as the scan path for a tokenless query.
        return Some(Err(ToolError::invalid_input(
            "query has no scoring tokens after tokenization",
        )));
    }

    // Reconcile: a hit survives only when the session's live generation
    // still matches the one it was indexed under — a compaction or rollback
    // rewrite invalidates it rather than serving dropped content.
    let raw = index.search(&fts_query, max_results * 3).ok()?;
    let mut hits: Vec<Value> = Vec::new();
    let mut stale_dropped = 0usize;
    for hit in raw {
        if hits.len() >= max_results {
            break;
        }
        let live = generation_of(&hit.session_id, sessions_dir);
        if live != Some(hit.generation) {
            stale_dropped += 1;
            continue;
        }
        hits.push(json!({
            "session_id": hit.session_id,
            "title": hit.title,
            "updated_at": hit.updated_at,
            "message_index": hit.message_index,
            "role": hit.role,
            "score": hit.score,
            "excerpt": hit.excerpt,
        }));
    }

    let payload = json!({
        "query": query,
        "workspace": workspace.display().to_string(),
        "via": "index",
        "sessions_parsed": stats.sessions_parsed,
        "sessions_in_workspace": stats.sessions_in_workspace,
        "skipped_other_workspace": stats.skipped_other_workspace,
        "skipped_oversize": stats.skipped_oversize,
        "skipped_corrupt": stats.skipped_corrupt,
        "scan_truncated": stats.scan_truncated,
        "stale_hits_dropped": stale_dropped,
        "hits": hits,
    });
    Some(Ok(ToolResult::success(payload.to_string())))
}

/// Candidate session files, newest first, capped at
/// [`MAX_SESSIONS_SCANNED`] — shared by the index refresh and the scan
/// fallback so both examine the same corpus.
struct Candidates {
    files: Vec<(std::time::SystemTime, PathBuf)>,
    truncated: bool,
}

fn enumerate_candidates(sessions_dir: &Path) -> Candidates {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return Candidates {
            files: Vec::new(),
            truncated: false,
        };
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                return None;
            }
            let mtime = entry.metadata().ok()?.modified().ok()?;
            Some((mtime, path))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let truncated = files.len() > MAX_SESSIONS_SCANNED;
    if truncated {
        files.truncate(MAX_SESSIONS_SCANNED);
    }
    Candidates { files, truncated }
}

enum ReadOutcome {
    // `Parsed` is boxed: `SavedSession` is ~550 bytes and the other
    // variants carry none, so an unboxed variant bloated every outcome on
    // the stack.
    Parsed(Box<SavedSession>),
    /// Larger than the cap (fstat or the take() bound tripped).
    Oversize,
    /// Vanished, unreadable, non-UTF-8, or invalid JSON.
    Corrupt,
}

/// Size-capped open+read+parse of one session file (the same discipline the
/// scan path applies: cap on the opened handle, non-regular files never
/// read unboundedly).
fn read_session_file(path: &Path) -> ReadOutcome {
    match std::fs::File::open(path) {
        Ok(file) => match file.metadata() {
            Ok(meta) if !meta.is_file() => ReadOutcome::Corrupt,
            Ok(meta) if meta.len() > MAX_SESSION_FILE_BYTES => ReadOutcome::Oversize,
            Ok(_) => read_capped(file),
            Err(_) => ReadOutcome::Corrupt,
        },
        Err(_) => ReadOutcome::Corrupt,
    }
}

/// Project a saved session into indexable `(role, text)` messages with the
/// same `message_text` extraction the scan path scores with.
fn project_messages(session: &SavedSession) -> Vec<(String, String)> {
    session
        .messages
        .iter()
        .map(|message| (message.role.clone(), message_text(message)))
        .filter(|(_, text)| !text.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    // `std::env::set_var` is `unsafe` in edition 2024; the HomeGuard below
    // mutates env vars only while holding the process-wide lock (the same
    // precedent as `recall_archive`'s test module).
    #![allow(unsafe_code)]
    use super::*;
    use codesmith_agent_runtime::models::{ContentBlock, Message};
    use codesmith_agent_runtime::test_support::lock_test_env;
    use codesmith_session::store::{FileSessionStore, create_saved_session_with_id_and_mode};
    use tempfile::TempDir;

    // Same isolation pattern as `recall_archive`'s tests: point the config
    // resolver's home at a tempdir for the test's lifetime, serialized by
    // the process-wide env lock.
    struct HomeGuard {
        tmp: TempDir,
        original_codesmith_home: Option<String>,
        original_home: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl HomeGuard {
        fn new() -> Self {
            let lock = lock_test_env();
            let tmp = TempDir::new().expect("tempdir");
            let original_codesmith_home = std::env::var("CODESMITH_HOME").ok();
            let original_home = std::env::var("HOME").ok();
            // SAFETY: serialized by the process-wide lock; only this thread
            // mutates the env vars for the duration of the guard.
            unsafe {
                std::env::set_var("CODESMITH_HOME", tmp.path().join(".codesmith"));
                std::env::set_var("HOME", tmp.path());
            }
            Self {
                tmp,
                original_codesmith_home,
                original_home,
                _lock: lock,
            }
        }
        fn sessions_dir(&self) -> PathBuf {
            self.tmp.path().join(".codesmith").join("sessions")
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: still holding the env lock.
            unsafe {
                match self.original_codesmith_home.take() {
                    Some(v) => std::env::set_var("CODESMITH_HOME", v),
                    None => std::env::remove_var("CODESMITH_HOME"),
                }
                match self.original_home.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    fn text_message(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    /// Save a session with the given id, workspace, and messages into the
    /// guarded sessions dir.
    fn save_session(home: &HomeGuard, id: &str, workspace: &Path, messages: &[(&str, &str)]) {
        let store = FileSessionStore::new(home.sessions_dir()).expect("store");
        let msgs: Vec<Message> = messages
            .iter()
            .map(|(role, text)| text_message(role, text))
            .collect();
        let session = create_saved_session_with_id_and_mode(
            id.to_string(),
            &msgs,
            "test-model",
            workspace,
            0,
            None,
            None,
        );
        store.save_session(&session).expect("save session");
    }

    fn payload_of(result: &ToolResult) -> serde_json::Value {
        serde_json::from_str(&result.content).expect("tool payload is JSON")
    }

    fn ctx_for(workspace: &Path) -> ToolContext {
        ToolContext::new(workspace.to_path_buf())
    }

    #[tokio::test]
    async fn finds_content_across_sessions_in_workspace_only() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        let other_ws = TempDir::new().expect("other ws");
        save_session(
            &home,
            "sess-alpha",
            ws.path(),
            &[
                ("user", "how did we fix the flaky cache test?"),
                ("assistant", "we pinned the clock and dropped shared state"),
            ],
        );
        save_session(
            &home,
            "sess-beta",
            ws.path(),
            &[("user", "unrelated chatter about the weather")],
        );
        save_session(
            &home,
            "sess-other-ws",
            other_ws.path(),
            &[("user", "flaky cache test discussion in another workspace")],
        );

        let tool = SessionSearchTool;
        let result = tool
            .execute(
                json!({"query": "flaky cache test", "max_results": 5}),
                &ctx_for(ws.path()),
            )
            .await
            .expect("search runs");
        let payload = payload_of(&result);

        let hits = payload["hits"].as_array().expect("hits array");
        assert!(!hits.is_empty(), "same-workspace hit must be found");
        assert!(
            hits.iter().all(|h| h["session_id"]
                .as_str()
                .is_some_and(|id| id != "sess-other-ws")),
            "other-workspace sessions must be filtered out: {payload}"
        );
        assert!(
            hits[0]["session_id"] == "sess-alpha",
            "the matching session ranks first: {payload}"
        );
        assert!(
            hits[0]["excerpt"]
                .as_str()
                .is_some_and(|s| s.contains("flaky")),
            "excerpt centers on the match: {payload}"
        );
        assert_eq!(payload["skipped_other_workspace"], 1, "{payload}");
        assert_eq!(payload["sessions_in_workspace"], 2, "{payload}");
    }

    #[tokio::test]
    async fn empty_hits_report_zeroes() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        save_session(
            &home,
            "sess-quiet",
            ws.path(),
            &[("user", "boring content")],
        );

        let tool = SessionSearchTool;
        let result = tool
            .execute(json!({"query": "quantum unicorn"}), &ctx_for(ws.path()))
            .await
            .expect("search runs");
        let payload = payload_of(&result);
        assert_eq!(
            payload["hits"].as_array().map(Vec::len),
            Some(0),
            "no match, no hits: {payload}"
        );
        assert_eq!(payload["sessions_in_workspace"], 1, "{payload}");
    }

    #[tokio::test]
    async fn oversize_files_are_skipped_and_counted() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        save_session(
            &home,
            "sess-small",
            ws.path(),
            &[("user", "needle found here")],
        );
        // An oversized .json candidate: content is irrelevant — the size
        // check happens before any parse.
        let oversized = home.sessions_dir().join("oversize.json");
        std::fs::write(
            &oversized,
            vec![b' '; usize::try_from(MAX_SESSION_FILE_BYTES + 1).expect("usize")],
        )
        .expect("write oversized");

        let tool = SessionSearchTool;
        let result = tool
            .execute(json!({"query": "needle"}), &ctx_for(ws.path()))
            .await
            .expect("search runs");
        let payload = payload_of(&result);
        assert_eq!(payload["skipped_oversize"], 1, "{payload}");
        assert_eq!(payload["sessions_parsed"], 1, "{payload}");
        assert!(
            !payload["hits"].as_array().expect("hits").is_empty(),
            "healthy session still searchable: {payload}"
        );
    }

    #[tokio::test]
    async fn scan_cap_truncates_and_reports() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        // Write raw session files directly: `FileSessionStore`'s own
        // retention (MAX_SESSIONS = 50) would evict store-saved sessions
        // long before this scan cap engaged.
        let dir = home.sessions_dir();
        std::fs::create_dir_all(&dir).expect("sessions dir");
        for i in 0..=MAX_SESSIONS_SCANNED {
            let msgs = vec![text_message("user", &format!("filler session number {i}"))];
            let session = create_saved_session_with_id_and_mode(
                format!("raw-{i:04}"),
                &msgs,
                "test-model",
                ws.path(),
                0,
                None,
                None,
            );
            std::fs::write(
                dir.join(format!("raw-{i:04}.json")),
                serde_json::to_string(&session).expect("serialize"),
            )
            .expect("write session file");
        }

        let tool = SessionSearchTool;
        let result = tool
            .execute(json!({"query": "filler session"}), &ctx_for(ws.path()))
            .await
            .expect("search runs");
        let payload = payload_of(&result);
        assert_eq!(payload["scan_truncated"], true, "{payload}");
        assert_eq!(
            payload["sessions_parsed"], MAX_SESSIONS_SCANNED as u64,
            "the cap bounds reads: {payload}"
        );
    }

    #[tokio::test]
    async fn rejects_empty_query_and_caps_max_results() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        let ctx = ctx_for(ws.path());
        let tool = SessionSearchTool;
        let err = tool
            .execute(json!({"query": "   "}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));

        save_session(&home, "sess-cap", ws.path(), &[("user", "zebra content")]);
        let result = tool
            .execute(json!({"query": "zebra", "max_results": 999}), &ctx)
            .await
            .expect("search runs");
        let payload = payload_of(&result);
        assert!(
            payload["hits"].as_array().expect("hits").len() <= HARD_MAX_RESULTS,
            "{payload}"
        );
    }

    /// The workspace comparison survives a symlinked path (macOS `/var` →
    /// `/private/var`): a session saved via the canonical path is found
    /// when queried via the symlinked path, and vice versa.
    #[tokio::test]
    async fn workspace_match_ignores_symlink_spelling() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        save_session(
            &home,
            "sess-link",
            ws.path(),
            &[("user", "symlink probe content")],
        );

        // `canonicalize` resolves the tempdir symlink chain on macOS.
        let canonical = std::fs::canonicalize(ws.path()).expect("canonical");
        if canonical == ws.path() {
            // No symlink in play on this platform — the comparison already
            // agrees by equality; nothing extra to pin.
            return;
        }
        let tool = SessionSearchTool;
        let result = tool
            .execute(json!({"query": "symlink probe"}), &ctx_for(&canonical))
            .await
            .expect("search runs");
        let payload = payload_of(&result);
        assert_eq!(payload["sessions_in_workspace"], 1, "{payload}");
        assert!(!payload["hits"].as_array().expect("hits").is_empty());
    }

    /// P1-5: the persistent index is the primary path — the payload says
    /// `via: "index"`, and a second query re-reads nothing (the generation
    /// check skips unchanged sessions).
    #[tokio::test]
    async fn index_path_serves_queries_and_skips_unchanged_sessions() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        save_session(
            &home,
            "sess-idx",
            ws.path(),
            &[("user", "how did we fix the flaky cache test")],
        );

        let tool = SessionSearchTool;
        let first = tool
            .execute(json!({"query": "flaky cache"}), &ctx_for(ws.path()))
            .await
            .expect("first search");
        let payload = payload_of(&first);
        assert_eq!(payload["via"], "index", "{payload}");
        assert_eq!(
            payload["sessions_parsed"], 1,
            "first run indexes: {payload}"
        );
        let hits = payload["hits"].as_array().expect("hits");
        assert_eq!(hits[0]["session_id"], "sess-idx", "{payload}");

        let second = tool
            .execute(json!({"query": "flaky cache"}), &ctx_for(ws.path()))
            .await
            .expect("second search");
        let payload = payload_of(&second);
        assert_eq!(payload["via"], "index", "{payload}");
        assert_eq!(
            payload["sessions_parsed"], 0,
            "unchanged generation ⇒ no re-read: {payload}"
        );
        assert_eq!(payload["sessions_in_workspace"], 1, "{payload}");
        assert!(!payload["hits"].as_array().expect("hits").is_empty());
    }

    /// P1-5 reconciliation: a session rewritten by compaction (same id,
    /// fewer messages, new mtime) must not serve the dropped content — the
    /// stale hit is invalidated and the refresh re-projects the session.
    #[tokio::test]
    async fn rewritten_session_invalidates_stale_hits() {
        let home = HomeGuard::new();
        let ws = TempDir::new().expect("ws");
        save_session(
            &home,
            "sess-compact",
            ws.path(),
            &[("user", "needle: the flaky cache test discussion")],
        );
        let tool = SessionSearchTool;
        let first = tool
            .execute(json!({"query": "flaky cache"}), &ctx_for(ws.path()))
            .await
            .expect("first search");
        assert!(!payload_of(&first)["hits"].as_array().unwrap().is_empty());

        // Compaction rewrite: new mtime (size may coincidentally match),
        // the old message is gone, new content present.
        std::fs::remove_file(home.sessions_dir().join("sess-compact.json"))
            .expect("remove old snapshot");
        save_session(
            &home,
            "sess-compact",
            ws.path(),
            &[("assistant", "summary: we discussed the weather instead")],
        );

        let second = tool
            .execute(json!({"query": "flaky cache"}), &ctx_for(ws.path()))
            .await
            .expect("second search");
        let payload = payload_of(&second);
        assert_eq!(payload["via"], "index", "{payload}");
        assert!(
            payload["hits"].as_array().unwrap().is_empty(),
            "dropped content must not be served: {payload}"
        );

        let third = tool
            .execute(json!({"query": "weather"}), &ctx_for(ws.path()))
            .await
            .expect("third search");
        assert!(
            !payload_of(&third)["hits"].as_array().unwrap().is_empty(),
            "the rewritten session is re-projected and searchable"
        );
    }
}
