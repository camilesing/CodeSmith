//! `search_sessions` tool — cross-session content search (round-3 TODO-43).
//!
//! Model-visible, workspace-scoped lazy search over the saved session
//! transcripts (`<sessions>/*.json`, the same directory `FileSessionStore`
//! writes). Each call enumerates candidate files (newest first, capped),
//! parses the ones within the per-file size cap, keeps sessions whose
//! `metadata.workspace` matches the invoking session's workspace, and
//! BM25-scores every message (the ranking core, tokenizer, and excerpt
//! window are shared with `recall_archive`).
//!
//! Known limitations (deliberate, round-3 plan §2.4): no persistent index
//! and no background refresh — a call is O(sessions × messages) over local
//! disk, acceptable while session counts stay bounded (prove the value
//! first; an FTS read model is the follow-up shape). Scope is the workspace
//! key only — the `parent_session_id` lineage chain is NOT traversed (a
//! forked session saved under another workspace stays invisible). Files past
//! the scan cap (oldest dropped) and oversized files are skipped and
//! counted, never fatal. Only the shared REPL sessions dir is covered; the
//! RLM/background session stores are separate surfaces. The current
//! session's own file participates as last saved (turn-boundary autosave).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use codesmith_agent_runtime::session_store::SavedSession;
use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};

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

enum CappedRead {
    Parsed(SavedSession),
    /// Larger than the cap (fstat or the take() bound tripped).
    Oversize,
    /// Vanished, unreadable, non-UTF-8, or invalid JSON.
    Corrupt,
}

/// Read at most `MAX_SESSION_FILE_BYTES + 1` bytes from `file` and parse,
/// so the cap holds even if the file grows between fstat and read.
fn read_capped(file: std::fs::File) -> CappedRead {
    use std::io::Read;
    let mut buf = Vec::new();
    if file
        .take(MAX_SESSION_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .is_err()
    {
        return CappedRead::Corrupt;
    }
    if buf.len() > MAX_SESSION_FILE_BYTES as usize {
        return CappedRead::Oversize;
    }
    match String::from_utf8(buf)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
    {
        Some(session) => CappedRead::Parsed(session),
        None => CappedRead::Corrupt,
    }
}

/// Enumerate, scope-filter, and load the scoreable corpus. Blocking — runs
/// under `spawn_blocking` (directory walks + file reads).
fn scan_saved_sessions(sessions_dir: &Path, workspace: &Path) -> (Vec<SessionDoc>, ScanStats) {
    let mut stats = ScanStats::default();
    let mut docs: Vec<SessionDoc> = Vec::new();

    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return (docs, stats);
    };
    // Candidates: *.json files with mtimes, newest first (the scan cap then
    // drops the OLDEST sessions, the least likely to be relevant).
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
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
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if candidates.len() > MAX_SESSIONS_SCANNED {
        stats.scan_truncated = true;
        candidates.truncate(MAX_SESSIONS_SCANNED);
    }

    let workspace_key = comparable(workspace);
    let mut retained_text_bytes = 0usize;
    'sessions: for (_mtime, path) in &candidates {
        // Cap the read atomically on the opened handle: a stat-then-read
        // race (the current session's file rewritten by turn-boundary
        // autosave mid-scan) could otherwise pull unbounded bytes into
        // memory past the cap before the JSON parse rejects them.
        let outcome = match std::fs::File::open(path) {
            Ok(file) => match file.metadata() {
                // Non-regular files (FIFO, device) report len 0 — never
                // read them unboundedly.
                Ok(meta) if !meta.is_file() => CappedRead::Corrupt,
                Ok(meta) if meta.len() > MAX_SESSION_FILE_BYTES => CappedRead::Oversize,
                Ok(_) => read_capped(file),
                Err(_) => CappedRead::Corrupt,
            },
            Err(_) => CappedRead::Corrupt,
        };
        let session = match outcome {
            CappedRead::Parsed(session) => session,
            CappedRead::Oversize => {
                stats.skipped_oversize += 1;
                continue;
            }
            CappedRead::Corrupt => {
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

#[cfg(test)]
mod tests {
    // `std::env::set_var` is `unsafe` in edition 2024; the HomeGuard below
    // mutates env vars only while holding the process-wide lock (the same
    // precedent as `recall_archive`'s test module).
    #![allow(unsafe_code)]
    use super::*;
    use codesmith_agent_runtime::models::{ContentBlock, Message};
    use codesmith_agent_runtime::session_store::{
        FileSessionStore, create_saved_session_with_id_and_mode,
    };
    use codesmith_agent_runtime::test_support::lock_test_env;
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
}
