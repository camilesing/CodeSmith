//! Per-workspace persistent full-text index over saved session transcripts
//! (P1-5): the storage-discipline sibling of [`IndexStore`](crate::store).
//!
//! Location: `~/.codesmith/index/<ws-hash>/sessions.db` — the same
//! per-workspace layout, schema-version drop-and-rebuild rule, and WAL
//! pragmas as the symbol index's `index.db` next door. The index is derived
//! data: any shape change bumps [`SESSION_INDEX_SCHEMA_VERSION`] and the
//! tables are dropped, never migrated.
//!
//! Layering: this module is storage only. The caller (the `search_sessions`
//! tool) owns the filesystem truth — it reads session snapshots, extracts
//! message text with the same projection the scan path uses, and computes
//! each session's *generation* (journal file mtime+size when the session
//! keeps a journal, snapshot mtime+size otherwise). A generation is the
//! invalidation boundary: rows carry the generation they were indexed
//! under, and a hit whose session has since been rewritten (compaction,
//! edit rollback) is dropped at query time rather than silently returned —
//! the index never serves content the current transcript no longer holds.
//!
//! # Why not the `IndexBackendFactory` registry
//!
//! That seam is a single-file *extractor* seam (`extract(file, source) ->
//! symbols/occurrences`); a full-text session index is a store with its own
//! tables and query path, and forcing it through the extractor trait would
//! be obligation without reuse. What P1-5 reuses instead is the storage
//! spine this crate owns: per-workspace SQLite location scheme, ws-hash
//! keying, schema-version rebuild discipline, rusqlite — no second index
//! subsystem outside this crate.
//!
//! # Known limitations
//!
//! - Ranking is FTS5's `bm25` over the default unicode61 tokenizer — close
//!   cousin of the scan path's hand-rolled BM25, not the same numbers; and
//!   CJK text still collapses to one long token (same as the scan path).
//! - Refresh work is caller-driven (lazy, at query time); there is no
//!   background writer and no cross-process lock — two concurrent hosts on
//!   one workspace serialize on SQLite's `busy_timeout` at worst.
//! - Only sessions the caller projects are indexed (workspace-scoped by
//!   construction: one DB per workspace, refreshed with that workspace's
//!   sessions only).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::store::workspace_hash;

/// Current schema version of the session index. Bump on any breaking shape
/// change; older databases are dropped and rebuilt on open.
pub const SESSION_INDEX_SCHEMA_VERSION: i64 = 1;

/// The generation a session was indexed under: (mtime_ms, size) of the
/// journal file when one exists, else of the snapshot. Opaque to the store
/// — the caller computes and compares it.
pub type SessionGeneration = (i64, i64);

/// One session's projection handed in by the caller — plain data, so this
/// crate stays independent of the session store's types.
#[derive(Debug, Clone)]
pub struct SessionProjection {
    pub session_id: String,
    /// Canonicalized workspace key the caller scopes by (stored for
    /// diagnostics; the DB itself is already per-workspace).
    pub workspace_key: String,
    pub title: String,
    /// RFC 3339 timestamp text from the saved session.
    pub updated_at: String,
    pub generation: SessionGeneration,
    /// `(role, text)` per non-empty message, in transcript order.
    pub messages: Vec<(String, String)>,
}

/// One search hit, denormalized from the FTS row.
#[derive(Debug, Clone)]
pub struct SessionIndexHit {
    pub session_id: String,
    pub title: String,
    pub updated_at: String,
    pub message_index: i64,
    pub role: String,
    /// FTS5 bm25 score negated and rounded (higher = better, matching the
    /// scan path's convention).
    pub score: f64,
    /// FTS5 `snippet()` window around the best match.
    pub excerpt: String,
    /// The generation this row was indexed under — the caller reconciles it
    /// against the live session file before returning the hit.
    pub generation: SessionGeneration,
}

/// SQLite-backed session full-text index. One instance per workspace; not
/// `Sync` (a raw `Connection`) — create it on the blocking thread that uses
/// it, like the caller's scan path does.
pub struct SessionIndex {
    conn: Connection,
}

impl SessionIndex {
    /// Open (or create) the session index at an explicit `db_path`,
    /// rebuilding from scratch when the stored schema version differs from
    /// [`SESSION_INDEX_SCHEMA_VERSION`].
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating session index dir {}", db_path.display()))?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("opening session index db {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;",
        )?;
        let index = Self { conn };
        index.init_schema()?;
        Ok(index)
    }

    /// Open the default on-disk location for `workspace_root`:
    /// `~/.codesmith/index/<workspace_hash>/sessions.db`.
    pub fn open_default(workspace_root: &Path) -> Result<Self> {
        let home = codesmith_config::codesmith_home()
            .context("cannot resolve CodeSmith home for session index storage")?;
        let db_path = session_index_path(&home, workspace_root);
        Self::open(&db_path)
    }

    fn init_schema(&self) -> Result<()> {
        let stored: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse().ok());
        match stored {
            Some(v) if v == SESSION_INDEX_SCHEMA_VERSION => Ok(()),
            _ => {
                if stored.is_some() {
                    tracing::info!(
                        stored = ?stored,
                        current = SESSION_INDEX_SCHEMA_VERSION,
                        "session index schema changed; rebuilding"
                    );
                }
                self.conn.execute_batch(
                    "DROP TABLE IF EXISTS session_messages;
                     DROP TABLE IF EXISTS sessions;
                     DROP TABLE IF EXISTS meta;",
                )?;
                self.conn.execute_batch(CREATE_SESSION_SCHEMA_SQL)?;
                self.conn.execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
                    params![SESSION_INDEX_SCHEMA_VERSION.to_string()],
                )?;
                Ok(())
            }
        }
    }

    /// The generation a session is indexed under, or `None` when the
    /// session has no rows. The caller compares this against the live
    /// session file to decide whether a re-projection is needed.
    pub fn tracked_generation(&self, session_id: &str) -> Result<Option<SessionGeneration>> {
        let mut stmt = self
            .conn
            .prepare("SELECT gen_mtime_ms, gen_size FROM sessions WHERE session_id = ?1")?;
        let mut rows = stmt.query(params![session_id])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// Replace one session's rows wholesale (delete + insert in one
    /// transaction). A generation change is the only reason to call this —
    /// unchanged sessions are skipped by the caller via
    /// [`tracked_generation`](Self::tracked_generation).
    pub fn upsert_session(&mut self, projection: &SessionProjection) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![projection.session_id],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![projection.session_id],
        )?;
        tx.execute(
            "INSERT INTO sessions(session_id, workspace_key, title, updated_at, gen_mtime_ms, gen_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                projection.session_id,
                projection.workspace_key,
                projection.title,
                projection.updated_at,
                projection.generation.0,
                projection.generation.1,
            ],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO session_messages(text, session_id, message_index, role, title, updated_at, gen_mtime_ms, gen_size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (idx, (role, text)) in projection.messages.iter().enumerate() {
                stmt.execute(params![
                    text,
                    projection.session_id,
                    idx as i64,
                    role,
                    projection.title,
                    projection.updated_at,
                    projection.generation.0,
                    projection.generation.1,
                ])?;
            }
        }
        tx.commit().context("committing session upsert")
    }

    /// Delete every session's rows whose id is not in `live_ids` (the
    /// caller passes the ids it saw on disk this refresh). Returns how many
    /// sessions were pruned.
    pub fn prune_missing(&mut self, live_ids: &[&str]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut stmt = tx.prepare("SELECT session_id FROM sessions")?;
        let stale: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter(|id| !live_ids.contains(&id.as_str()))
            .collect();
        drop(stmt);
        for id in &stale {
            tx.execute(
                "DELETE FROM session_messages WHERE session_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM sessions WHERE session_id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(stale.len())
    }

    /// Number of indexed sessions (diagnostics).
    pub fn session_count(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .context("counting indexed sessions")
    }

    /// FTS5 search over the indexed messages, best bm25 first, at most
    /// `limit` hits. `fts_query` must already be FTS5-safe (see
    /// [`fts_query_from_terms`]); an empty one returns no hits.
    pub fn search(&self, fts_query: &str, limit: usize) -> Result<Vec<SessionIndexHit>> {
        if fts_query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT session_id, message_index, role, title, updated_at,
                    gen_mtime_ms, gen_size, bm25(session_messages) AS rank,
                    snippet(session_messages, 0, '…', '…', ' ', 12) AS excerpt
             FROM session_messages
             WHERE session_messages MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let hits = stmt
            .query_map(params![fts_query, limit as i64], |row| {
                let rank: f64 = row.get(7)?;
                Ok(SessionIndexHit {
                    session_id: row.get(0)?,
                    message_index: row.get(1)?,
                    role: row.get(2)?,
                    title: row.get(3)?,
                    updated_at: row.get(4)?,
                    generation: (row.get(5)?, row.get(6)?),
                    // FTS5 bm25: lower (more negative) = better; negate so
                    // higher = better, matching the scan path's convention.
                    score: (-rank * 10_000.0).round() / 10_000.0,
                    excerpt: row.get(8)?,
                })
            })?
            .filter_map(|hit| hit.ok())
            .collect();
        Ok(hits)
    }
}

/// The session-index db path under an explicit CodeSmith home.
#[must_use]
pub fn session_index_path(codesmith_home: &Path, workspace_root: &Path) -> PathBuf {
    codesmith_home
        .join("index")
        .join(workspace_hash(workspace_root))
        .join("sessions.db")
}

const CREATE_SESSION_SCHEMA_SQL: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE sessions(
    session_id TEXT PRIMARY KEY,
    workspace_key TEXT NOT NULL,
    title TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    gen_mtime_ms INTEGER NOT NULL,
    gen_size INTEGER NOT NULL
);
CREATE VIRTUAL TABLE session_messages USING fts5(
    text,
    session_id UNINDEXED,
    message_index UNINDEXED,
    role UNINDEXED,
    title UNINDEXED,
    updated_at UNINDEXED,
    gen_mtime_ms UNINDEXED,
    gen_size UNINDEXED
);
";

/// Build an FTS5-safe query from free-text terms: each alphanumeric run of
/// ≥ 2 chars becomes a quoted token, joined with implicit AND — mirroring
/// the scan path's tokenizer (lowercase split on non-alphanumerics) closely
/// enough that the same query finds the same families of terms. Empty input
/// yields an empty (no-match) query.
#[must_use]
pub fn fts_query_from_terms(query: &str) -> String {
    let lowered = query.to_lowercase();
    let mut terms: Vec<&str> = Vec::new();
    for term in lowered.split(|c: char| !c.is_alphanumeric()) {
        if term.chars().count() >= 2 && !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms
        .iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn projection(
        id: &str,
        generation: SessionGeneration,
        messages: &[(&str, &str)],
    ) -> SessionProjection {
        SessionProjection {
            session_id: id.to_string(),
            workspace_key: "/ws".to_string(),
            title: format!("title {id}"),
            updated_at: "2026-11-03T00:00:00Z".to_string(),
            generation,
            messages: messages
                .iter()
                .map(|(role, text)| ((*role).to_string(), (*text).to_string()))
                .collect(),
        }
    }

    fn index() -> (tempfile::TempDir, SessionIndex) {
        let dir = tempdir().expect("tempdir");
        let idx = SessionIndex::open(&dir.path().join("sessions.db")).expect("open");
        (dir, idx)
    }

    #[test]
    fn upsert_then_search_ranks_and_excerpts() {
        let (_dir, mut idx) = index();
        idx.upsert_session(&projection(
            "s1",
            (1, 10),
            &[
                ("user", "how did we fix the flaky cache test"),
                ("assistant", "we pinned the clock"),
                ("assistant", "unrelated closing remark about zebras"),
            ],
        ))
        .expect("upsert");

        let hits = idx
            .search(&fts_query_from_terms("flaky cache"), 10)
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert_eq!(hits[0].message_index, 0);
        assert_eq!(hits[0].role, "user");
        // Negated bm25: positive and higher = better (a 1-of-3 match has
        // positive IDF).
        assert!(hits[0].score > 0.0, "score: {}", hits[0].score);
        assert!(hits[0].excerpt.contains("flaky"), "{}", hits[0].excerpt);
        assert_eq!(hits[0].generation, (1, 10));

        // No match → no hits; empty FTS query → no hits (not an error).
        assert!(
            idx.search(&fts_query_from_terms("quantum unicorn"), 10)
                .unwrap()
                .is_empty()
        );
        assert!(idx.search("", 10).unwrap().is_empty());
    }

    #[test]
    fn re_upsert_with_a_new_generation_replaces_old_content() {
        let (_dir, mut idx) = index();
        idx.upsert_session(&projection(
            "s1",
            (1, 10),
            &[("user", "needle: the flaky cache test discussion")],
        ))
        .expect("first upsert");

        // Compaction rewrote the session: the old message is gone, the
        // generation moved. The stale content must not remain searchable.
        idx.upsert_session(&projection(
            "s1",
            (2, 20),
            &[("assistant", "summary: we discussed the weather")],
        ))
        .expect("second upsert");

        assert!(
            idx.search(&fts_query_from_terms("flaky cache"), 10)
                .unwrap()
                .is_empty(),
            "content dropped by the rewrite must not be served"
        );
        let hits = idx.search(&fts_query_from_terms("weather"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].generation, (2, 20));
        assert_eq!(idx.tracked_generation("s1").unwrap(), Some((2, 20)));
    }

    #[test]
    fn prune_missing_removes_vanished_sessions_only() {
        let (_dir, mut idx) = index();
        idx.upsert_session(&projection("keep", (1, 1), &[("user", "keep me needle")]))
            .expect("upsert keep");
        idx.upsert_session(&projection("gone", (1, 1), &[("user", "vanishing needle")]))
            .expect("upsert gone");

        let pruned = idx.prune_missing(&["keep"]).expect("prune");
        assert_eq!(pruned, 1);
        assert_eq!(idx.session_count().unwrap(), 1);
        let hits = idx.search(&fts_query_from_terms("needle"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "keep");
    }

    #[test]
    fn schema_version_mismatch_rebuilds_instead_of_failing() {
        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("sessions.db");
        {
            let mut idx = SessionIndex::open(&db).expect("open");
            idx.upsert_session(&projection("s1", (1, 1), &[("user", "stale needle")]))
                .expect("upsert");
        }
        // Corrupt the stored version: reopen must drop and rebuild.
        {
            let conn = Connection::open(&db).expect("reopen raw");
            conn.execute(
                "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
                [],
            )
            .expect("bump stored version");
        }
        let idx = SessionIndex::open(&db).expect("reopen rebuilds");
        assert_eq!(idx.session_count().unwrap(), 0, "rows were dropped");
        assert!(
            idx.search(&fts_query_from_terms("needle"), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fts_query_terms_are_quoted_deduped_and_dedropped() {
        assert_eq!(
            fts_query_from_terms("the flaky-cache THE test"),
            "\"the\" \"flaky\" \"cache\" \"test\""
        );
        assert_eq!(fts_query_from_terms("a I 2"), "");
        assert_eq!(
            fts_query_from_terms("we're \"quoted\""),
            "\"we\" \"re\" \"quoted\""
        );
    }
}
