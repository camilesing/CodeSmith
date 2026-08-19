//! Per-workspace SQLite store for the index.
//!
//! Location: `~/.codesmith/index/<ws-hash>/index.db` where `<ws-hash>` is
//! the first 16 hex chars of SHA-256 over the canonical workspace path.
//! The index is derived data: a schema-version mismatch drops and rebuilds
//! every table instead of migrating, and freshness is tracked by
//! mtime+size only (no content hashing).

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::types::{
    FileEntry, FileQuery, Language, Location, Occurrence, OccurrenceRole, Symbol, SymbolKind,
    SymbolQuery, glob_match,
};

/// Current store schema version. Bump on any breaking shape change; older
/// databases are dropped and rebuilt on open.
pub const SCHEMA_VERSION: i64 = 1;

const CREATE_SCHEMA_SQL: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE files(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    mtime_ms INTEGER NOT NULL,
    size INTEGER NOT NULL,
    language TEXT,
    symbol_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE symbols(
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    container TEXT,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    signature TEXT
);
CREATE INDEX idx_symbols_name ON symbols(name);
CREATE INDEX idx_symbols_kind ON symbols(kind);
CREATE TABLE occurrences(
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    line INTEGER NOT NULL
);
CREATE INDEX idx_occurrences_name ON occurrences(name);
";

/// Stable on-disk directory key for a workspace root.
#[must_use]
pub fn workspace_hash(workspace_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let canonical =
        fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..16].to_string()
}

/// A stored file row: surrogate id plus the inventory entry.
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: i64,
    pub entry: FileEntry,
}

/// SQLite-backed index store. One instance per workspace; safe to share
/// across threads (the connection sits behind a mutex — every operation is
/// short, and refresh work happens outside on a blocking thread before
/// writing).
pub struct IndexStore {
    conn: Mutex<Connection>,
}

impl IndexStore {
    /// Open (or create) the index database at an explicit `db_path`,
    /// rebuilding from scratch when the stored schema version differs from
    /// [`SCHEMA_VERSION`].
    pub fn open(workspace_root: &Path, db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating index dir {}", db_path.display()))?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("opening index db {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema(workspace_root)?;
        Ok(store)
    }

    /// Open the default on-disk location for `workspace_root`:
    /// `~/.codesmith/index/<workspace_hash>/index.db`.
    pub fn open_default(workspace_root: &Path) -> Result<Self> {
        let home = dirs::home_dir().context("cannot resolve home directory for index storage")?;
        let db_path = home
            .join(".codesmith")
            .join("index")
            .join(workspace_hash(workspace_root))
            .join("index.db");
        Self::open(workspace_root, &db_path)
    }

    fn init_schema(&self, workspace_root: &Path) -> Result<()> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let has_meta: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|v| v != 0)
            .context("checking index schema state")?;
        let stored: Option<i64> = if has_meta {
            conn.query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("reading index schema version")?
            .and_then(|v| v.parse().ok())
        } else {
            None
        };
        match stored {
            Some(v) if v == SCHEMA_VERSION => {}
            Some(v) => {
                tracing::info!(
                    stored = v,
                    current = SCHEMA_VERSION,
                    "index schema changed; rebuilding"
                );
                conn.execute_batch(
                    "DROP TABLE IF EXISTS occurrences;
                     DROP TABLE IF EXISTS symbols;
                     DROP TABLE IF EXISTS files;
                     DROP TABLE IF EXISTS meta;",
                )?;
                Self::create_schema(&conn, workspace_root)?;
            }
            None => Self::create_schema(&conn, workspace_root)?,
        }
        Ok(())
    }

    fn create_schema(conn: &Connection, workspace_root: &Path) -> Result<()> {
        conn.execute_batch(CREATE_SCHEMA_SQL)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('workspace_path', ?1)",
            params![workspace_root.to_string_lossy().to_string()],
        )?;
        Ok(())
    }

    /// Record which backend produced the current symbol data.
    pub fn set_backend(&self, backend_id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('backend', ?1)",
            params![backend_id],
        )?;
        Ok(())
    }

    /// Transactionally replace one file's inventory row and its extraction
    /// output (symbols + occurrences). Called for every new or dirty file.
    pub fn replace_file(
        &self,
        entry: &FileEntry,
        extraction: &crate::backend::Extraction,
    ) -> Result<()> {
        let mut conn = self.conn.lock().expect("index store mutex poisoned");
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM files WHERE path = ?1", params![entry.path])?;
        tx.execute(
            "INSERT INTO files(path, mtime_ms, size, language, symbol_count)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.path,
                entry.mtime_ms,
                entry.size,
                entry.language.map(|l| l.as_str().to_string()),
                extraction.symbols.len() as i64,
            ],
        )?;
        let file_id = tx.last_insert_rowid();
        for symbol in &extraction.symbols {
            tx.execute(
                "INSERT INTO symbols(file_id, name, kind, container, line, col, end_line, end_col, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    file_id,
                    symbol.name,
                    symbol.kind.as_str(),
                    symbol.container,
                    symbol.location.line,
                    symbol.location.col,
                    symbol.location.end_line,
                    symbol.location.end_col,
                    symbol.signature,
                ],
            )?;
        }
        for occurrence in &extraction.occurrences {
            tx.execute(
                "INSERT INTO occurrences(file_id, name, role, line)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    file_id,
                    occurrence.name,
                    match occurrence.role {
                        OccurrenceRole::Definition => "definition",
                        OccurrenceRole::Reference => "reference",
                    },
                    occurrence.line,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Snapshot of every inventory row (used by the freshness diff).
    pub fn all_files(&self) -> Result<Vec<FileRecord>> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let mut stmt = conn.prepare("SELECT id, path, mtime_ms, size, language FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok(FileRecord {
                id: row.get(0)?,
                entry: file_entry_from_row(row)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Delete inventory rows whose path is not in `live`; cascades to
    /// symbols and occurrences. Returns how many files were purged.
    pub fn delete_missing(&self, live: &HashSet<String>) -> Result<usize> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let mut stmt = conn.prepare("SELECT id, path FROM files")?;
        let stale: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(_, path)| !live.contains(path))
            .collect();
        drop(stmt);
        let mut purged = 0;
        for (id, _) in &stale {
            conn.execute("DELETE FROM files WHERE id = ?1", params![id])?;
            purged += 1;
        }
        Ok(purged)
    }

    /// Case-insensitive substring search over symbol definitions with
    /// exact → prefix → substring ranking. Kind filtering happens in SQL;
    /// the file glob and ranking happen here.
    pub fn search_symbols(&self, query: &SymbolQuery) -> Result<Vec<Symbol>> {
        if query.query.is_empty() {
            bail!("symbol search query must not be empty");
        }
        let needle = query.query.to_lowercase();
        let fetch_cap = query.limit.saturating_mul(4).clamp(query.limit, 500);
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let base_sql = "SELECT s.name, s.kind, s.container, f.path, s.line, s.col, s.end_line, s.end_col, s.signature
                        FROM symbols s JOIN files f ON f.id = s.file_id
                        WHERE instr(lower(s.name), ?1) > 0";
        let sql = match query.kind {
            Some(_kind) => format!("{base_sql} AND s.kind = ?2 LIMIT ?3"),
            None => format!("{base_sql} LIMIT ?2"),
        };
        let mut stmt = conn.prepare(&sql)?;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Symbol> {
            Ok(Symbol {
                name: row.get(0)?,
                kind: kind_from_str(&row.get::<_, String>(1)?).unwrap_or(SymbolKind::Function),
                container: row.get(2)?,
                path: row.get(3)?,
                location: Location {
                    line: row.get(4)?,
                    col: row.get(5)?,
                    end_line: row.get(6)?,
                    end_col: row.get(7)?,
                },
                signature: row.get(8)?,
            })
        };
        let mut candidates = match query.kind {
            Some(kind) => stmt
                .query_map(params![needle, kind.as_str(), fetch_cap], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![needle, fetch_cap], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        };
        drop(stmt);
        let glob = query.file_glob.as_deref();
        if let Some(g) = glob {
            candidates.retain(|s| glob_match(g, &s.path));
        }
        let needle_ci = needle;
        candidates.sort_by(|a, b| {
            symbol_rank(&a.name, &needle_ci)
                .cmp(&symbol_rank(&b.name, &needle_ci))
                .then_with(|| a.name.len().cmp(&b.name.len()))
                .then_with(|| a.path.cmp(&b.path))
        });
        candidates.truncate(query.limit);
        Ok(candidates)
    }

    /// Definitions whose name case-insensitively equals `name`.
    pub fn find_definition(&self, name: &str) -> Result<Vec<Symbol>> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT s.name, s.kind, s.container, f.path, s.line, s.col, s.end_line, s.end_col, s.signature
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE lower(s.name) = lower(?1)
             ORDER BY f.path, s.line",
        )?;
        let symbols = stmt
            .query_map(params![name], |row| {
                Ok(Symbol {
                    name: row.get(0)?,
                    kind: kind_from_str(&row.get::<_, String>(1)?).unwrap_or(SymbolKind::Function),
                    container: row.get(2)?,
                    path: row.get(3)?,
                    location: Location {
                        line: row.get(4)?,
                        col: row.get(5)?,
                        end_line: row.get(6)?,
                        end_col: row.get(7)?,
                    },
                    signature: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(symbols)
    }

    /// Lexical occurrences (definitions + references) of `name`, ordered by
    /// path then line.
    pub fn find_occurrences(&self, name: &str) -> Result<Vec<Occurrence>> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT o.name, o.role, f.path, o.line
             FROM occurrences o JOIN files f ON f.id = o.file_id
             WHERE lower(o.name) = lower(?1)
             ORDER BY f.path, o.line",
        )?;
        let occurrences = stmt
            .query_map(params![name], |row| {
                Ok(Occurrence {
                    name: row.get(0)?,
                    role: if row.get::<_, String>(1)?.as_str() == "definition" {
                        OccurrenceRole::Definition
                    } else {
                        OccurrenceRole::Reference
                    },
                    path: row.get(2)?,
                    line: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(occurrences)
    }

    /// File inventory listing with glob / extension filters applied while
    /// rows stream out of SQLite (stops at `limit`).
    pub fn list_files(&self, query: &FileQuery) -> Result<Vec<FileEntry>> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let mut stmt =
            conn.prepare("SELECT path, mtime_ms, size, language FROM files ORDER BY path")?;
        let mut out = Vec::new();
        let mut rows = stmt.query_map([], file_entry_from_row)?;
        for row in rows.by_ref() {
            let entry = row?;
            if let Some(ext) = query.extension.as_deref() {
                let matches_ext = Path::new(&entry.path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case(ext));
                if !matches_ext {
                    continue;
                }
            }
            if let Some(glob) = query.glob.as_deref()
                && !glob_match(glob, &entry.path)
            {
                continue;
            }
            out.push(entry);
            if out.len() >= query.limit {
                break;
            }
        }
        Ok(out)
    }

    /// Row counts for [`IndexStats`].
    pub fn counts(&self) -> Result<(u64, u64)> {
        let conn = self.conn.lock().expect("index store mutex poisoned");
        let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let symbols: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok((files.unsigned_abs(), symbols.unsigned_abs()))
    }
}

fn file_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileEntry> {
    Ok(FileEntry {
        path: row.get("path")?,
        mtime_ms: row.get("mtime_ms")?,
        size: row.get::<_, i64>("size")?.unsigned_abs(),
        language: row
            .get::<_, Option<String>>("language")?
            .and_then(|l| language_from_str(&l)),
    })
}

fn language_from_str(value: &str) -> Option<Language> {
    Language::all()
        .iter()
        .copied()
        .find(|l| l.as_str() == value)
}

fn kind_from_str(value: &str) -> Option<SymbolKind> {
    SymbolKind::all()
        .iter()
        .copied()
        .find(|k| k.as_str() == value)
}

/// Rank a candidate name against the (lowercased) needle: exact match
/// ranks best, then prefix, then plain substring.
fn symbol_rank(name: &str, needle: &str) -> u8 {
    let lower = name.to_lowercase();
    if lower == needle {
        0
    } else if lower.starts_with(needle) {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Extraction;
    use crate::types::{Occurrence, OccurrenceRole};

    fn store_in(tmp: &tempfile::TempDir) -> IndexStore {
        IndexStore::open(tmp.path(), &tmp.path().join("index.db")).expect("open store")
    }

    fn extraction_fixture() -> Extraction {
        Extraction {
            symbols: vec![
                Symbol {
                    name: "ToolRegistry".into(),
                    kind: SymbolKind::Struct,
                    container: None,
                    path: "src/registry.rs".into(),
                    location: Location {
                        line: 10,
                        col: 1,
                        end_line: 40,
                        end_col: 2,
                    },
                    signature: Some("struct ToolRegistry".into()),
                },
                Symbol {
                    name: "build".into(),
                    kind: SymbolKind::Method,
                    container: Some("ToolRegistry".into()),
                    path: "src/registry.rs".into(),
                    location: Location {
                        line: 20,
                        col: 5,
                        end_line: 30,
                        end_col: 6,
                    },
                    signature: Some("fn build(&self) -> Result<()>".into()),
                },
                Symbol {
                    name: "RegistryBuilder".into(),
                    kind: SymbolKind::Struct,
                    container: None,
                    path: "src/other.rs".into(),
                    location: Location {
                        line: 5,
                        col: 1,
                        end_line: 15,
                        end_col: 2,
                    },
                    signature: None,
                },
            ],
            occurrences: vec![
                Occurrence {
                    name: "ToolRegistry".into(),
                    role: OccurrenceRole::Definition,
                    path: "src/registry.rs".into(),
                    line: 10,
                },
                Occurrence {
                    name: "ToolRegistry".into(),
                    role: OccurrenceRole::Reference,
                    path: "src/main.rs".into(),
                    line: 3,
                },
            ],
        }
    }

    fn file_entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            mtime_ms: 1_000,
            size: 42,
            language: Some(Language::Rust),
        }
    }

    #[test]
    fn open_creates_schema_and_meta() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("nested").join("index.db");
        let store = IndexStore::open(tmp.path(), &db_path).expect("open");
        let conn = store.conn.lock().unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .expect("schema_version row");
        assert_eq!(version, "1");
    }

    #[test]
    fn replace_file_roundtrip_and_replacement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = store_in(&tmp);
        store
            .replace_file(&file_entry("src/registry.rs"), &extraction_fixture())
            .expect("insert");
        store
            .replace_file(&file_entry("src/other.rs"), &extraction_fixture())
            .expect("insert second file");

        let files = store.all_files().expect("all_files");
        assert_eq!(files.len(), 2);

        // Replacing the same path must not duplicate symbols.
        store
            .replace_file(&file_entry("src/registry.rs"), &extraction_fixture())
            .expect("replace");
        let (files, symbols) = store.counts().expect("counts");
        // both files share the fixture's symbol set (3 symbols each)
        assert_eq!((files, symbols), (2, 6));

        let occurrences = store.find_occurrences("toolregistry").expect("occ");
        assert_eq!(occurrences.len(), 4, "definition + reference per file copy");
        assert!(
            occurrences
                .iter()
                .any(|o| o.role == OccurrenceRole::Reference)
        );
    }

    #[test]
    fn search_symbols_ranks_exact_prefix_substring() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = store_in(&tmp);
        store
            .replace_file(&file_entry("src/a.rs"), &extraction_fixture())
            .expect("insert");
        let hits = store
            .search_symbols(&SymbolQuery {
                query: "registry".into(),
                kind: None,
                file_glob: None,
                limit: 10,
            })
            .expect("search");
        assert_eq!(hits.len(), 2, "ToolRegistry + RegistryBuilder");
        assert_eq!(
            hits[0].name, "RegistryBuilder",
            "prefix match beats mid-name substring"
        );
        assert_eq!(hits[1].name, "ToolRegistry");

        let exact = store
            .search_symbols(&SymbolQuery {
                query: "toolregistry".into(),
                kind: None,
                file_glob: None,
                limit: 10,
            })
            .expect("search exact");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].name, "ToolRegistry");

        let kind_hits = store
            .search_symbols(&SymbolQuery {
                query: "registry".into(),
                kind: Some(SymbolKind::Method),
                file_glob: None,
                limit: 10,
            })
            .expect("search kind");
        assert!(kind_hits.is_empty(), "no method contains 'registry'");

        store
            .replace_file(&file_entry("src/other.rs"), &extraction_fixture())
            .expect("insert second file");
        let glob_hits = store
            .search_symbols(&SymbolQuery {
                query: "registry".into(),
                kind: None,
                file_glob: Some("src/other.rs".into()),
                limit: 10,
            })
            .expect("search glob");
        assert_eq!(
            glob_hits.len(),
            2,
            "both fixture symbols filed under src/other.rs"
        );
        assert!(glob_hits.iter().all(|s| s.path == "src/other.rs"));
    }

    #[test]
    fn find_definition_is_case_insensitive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = store_in(&tmp);
        store
            .replace_file(&file_entry("src/a.rs"), &extraction_fixture())
            .expect("insert");
        let defs = store.find_definition("BUILD").expect("defs");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "build");
        assert_eq!(defs[0].container.as_deref(), Some("ToolRegistry"));
    }

    #[test]
    fn delete_missing_purges_and_cascades() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = store_in(&tmp);
        store
            .replace_file(&file_entry("src/a.rs"), &extraction_fixture())
            .expect("insert");
        store
            .replace_file(&file_entry("src/gone.rs"), &extraction_fixture())
            .expect("insert");
        let purged = store
            .delete_missing(&HashSet::from(["src/a.rs".to_string()]))
            .expect("purge");
        assert_eq!(purged, 1);
        let files = store.all_files().expect("files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].entry.path, "src/a.rs");
        // symbols of the purged file cascaded away
        let defs = store.find_definition("RegistryBuilder").expect("defs");
        assert_eq!(defs.len(), 1, "only the surviving file's copy remains");
    }

    #[test]
    fn list_files_filters_extension_glob_and_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = store_in(&tmp);
        store
            .replace_file(&file_entry("src/a.rs"), &Extraction::default())
            .expect("insert");
        store
            .replace_file(&file_entry("src/b.py"), &Extraction::default())
            .expect("insert");
        store
            .replace_file(&file_entry("src/c.rs"), &Extraction::default())
            .expect("insert");

        let rs = store
            .list_files(&FileQuery {
                extension: Some("rs".into()),
                limit: 10,
                ..Default::default()
            })
            .expect("list");
        assert_eq!(rs.len(), 2);
        assert!(rs.iter().all(|f| f.path.ends_with(".rs")));

        let globbed = store
            .list_files(&FileQuery {
                glob: Some("src/b.*".into()),
                limit: 10,
                ..Default::default()
            })
            .expect("list");
        assert_eq!(globbed.len(), 1);
        assert_eq!(globbed[0].path, "src/b.py");

        let limited = store
            .list_files(&FileQuery {
                limit: 2,
                ..Default::default()
            })
            .expect("list");
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn schema_version_mismatch_rebuilds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("index.db");
        {
            let store = IndexStore::open(tmp.path(), &db_path).expect("open");
            store
                .replace_file(&file_entry("src/a.rs"), &extraction_fixture())
                .expect("insert");
        }
        {
            let conn = Connection::open(&db_path).expect("reopen raw");
            conn.execute(
                "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
                [],
            )
            .expect("bump version");
        }
        let store = IndexStore::open(tmp.path(), &db_path).expect("reopen");
        let (files, symbols) = store.counts().expect("counts");
        assert_eq!(
            (files, symbols),
            (0, 0),
            "mismatched version dropped derived data"
        );
        let version: String = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .expect("version")
        };
        assert_eq!(version, "1");
    }

    #[test]
    fn workspace_hash_is_stable_and_path_sensitive() {
        let a = workspace_hash(Path::new("/repos/alpha"));
        let b = workspace_hash(Path::new("/repos/beta"));
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
        assert_eq!(a, workspace_hash(Path::new("/repos/alpha")));
    }
}
