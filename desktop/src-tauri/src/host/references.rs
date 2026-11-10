//! `fileReferences/list` — the vendored `client-ui-reference` plugin's `@`
//! file source over the session workspace.
//!
//! Upstream's host owns a cancellable workspace index behind the same Remote;
//! here the bridge walks the thread's workspace directly (the root comes from
//! the runtime thread record, like `host::files`). The wire value is the
//! upstream `FileReferenceCandidate[]` (`{path, kind}` with `kind` =
//! `file` | `directory`): selected values stay ordinary `@path` prompt text,
//! which CodeSmith's REPL already understands.
//!
//! Query semantics mirror `file-reference-local`:
//! * an empty query lists the workspace root;
//! * a query containing `/` lists that directory (the fragment after the last
//!   slash filters it);
//! * a bare fragment searches the whole workspace with the upstream scoring
//!   (exact > name prefix > name contains > path contains > subsequence),
//!   directories first on ties, at most 20 rows.
//!
//! Hidden entries are skipped unless the query itself asks for a dot path;
//! the version-control/dependency/build directories upstream excludes are
//! excluded here too (`.git`, `node_modules`, `target`, …). Symlinks are
//! skipped rather than followed.
//!
//! Known limitations: `sessionReferenceResolver/candidates` answers `[]` —
//! CodeSmith has no cross-session context-injection feature, so the `@` menu
//! lists files only. The index is a bounded one-shot walk (50k entries),
//! cached per workspace and invalidated when a turn settles; it does not watch
//! the filesystem.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use super::files;
use super::{HostState, rpc_error, rpc_ok};

/// Largest candidate list one query serves (upstream default).
const MAX_RESULTS: usize = 20;
/// Largest index one workspace keeps (upstream default).
const MAX_ENTRIES: usize = 50_000;

/// Directory basenames never traversed or offered (upstream's default list).
const EXCLUDED_DIRECTORIES: [&str; 15] = [
    ".git",
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    "target",
    ".next",
    ".nuxt",
    ".turbo",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
];

/// One completion candidate, in the upstream wire shape.
#[derive(Clone, Debug, Serialize)]
struct Candidate {
    path: String,
    kind: &'static str,
}

/// A discovery failure: the wire code plus its message.
#[derive(Debug)]
struct RefError {
    code: &'static str,
    message: String,
}

impl RefError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<files::FileError> for RefError {
    fn from(error: files::FileError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

/// The per-workspace index cache. Invalidation is explicit (a settled turn
/// may have written files); there is no TTL, so an idle `@` menu does not
/// re-walk the tree.
#[derive(Default)]
pub struct ReferenceIndex {
    entries: Mutex<HashMap<PathBuf, Arc<Vec<Candidate>>>>,
}

impl ReferenceIndex {
    /// Drop every cached workspace index (a turn settled).
    pub fn invalidate(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }

    fn get(&self, root: &Path) -> Option<Arc<Vec<Candidate>>> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(root).cloned())
    }

    fn put(&self, root: &Path, candidates: Arc<Vec<Candidate>>) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(root.to_path_buf(), candidates);
        }
    }
}

/// The session a `fileReferences/list` call addresses. The generated remote
/// declares the session as an `agent`-scope lookup, so its wire name is
/// `agentId` (the client suppresses a failed lookup to an empty menu, which
/// made a sessionId-only reader silently useless); accept the direct names
/// too, for hand-written callers and older wire shapes.
fn reference_session_id(request: &Value) -> Option<&str> {
    ["sessionId", "agentId", "session_id"]
        .iter()
        .find_map(|key| request.get(*key).and_then(Value::as_str))
}

/// `fileReferences/list` — the `@` menu's file source.
pub async fn list(host: &HostState, args: &Value) -> Value {
    let request = args.get("request").unwrap_or(args);
    let Some(session_id) = reference_session_id(request) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "fileReferences/list without sessionId",
        );
    };
    let query = request
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let root = match files::workspace_root(host, session_id).await {
        Ok(root) => root,
        Err(error) => return rpc_error(error.code, error.message),
    };
    let root_path = PathBuf::from(&root);
    let cached = host.reference_index.get(&root_path);
    let scan_root = root_path.clone();
    let scan_query = query.clone();
    let result =
        tokio::task::spawn_blocking(move || discover(&scan_root, &scan_query, cached)).await;
    match result {
        Ok(Ok((candidates, built))) => {
            if let Some(entries) = built {
                host.reference_index.put(&root_path, entries);
            }
            rpc_ok(json!(candidates))
        }
        Ok(Err(error)) => rpc_error(error.code, error.message),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("fileReferences/list join failed: {err}"),
        ),
    }
}

/// One query's answer: the ranked rows plus the index this call built (if
/// any), so the caller can cache it outside the blocking task.
type Discovery = Result<(Vec<Candidate>, Option<Arc<Vec<Candidate>>>), RefError>;

/// One query: the directory listing when the path form asks for one, else the
/// cached workspace index. Returns the cached index when this call built one.
fn discover(root: &Path, query: &str, cached: Option<Arc<Vec<Candidate>>>) -> Discovery {
    if query.is_empty() || query.contains('/') {
        return Ok((list_directory(root, query)?, None));
    }
    if let Some(entries) = cached {
        return Ok((rank(entries.iter(), query), None));
    }
    let entries = Arc::new(scan_workspace(root)?);
    let ranked = rank(entries.iter(), query);
    Ok((ranked, Some(entries)))
}

/// List one directory (the root when `display_directory` is empty), filtering
/// the fragment after the last slash.
fn list_directory(root: &Path, query: &str) -> Result<Vec<Candidate>, RefError> {
    let (display_directory, fragment) = match query.rfind('/') {
        Some(slash) => (&query[..=slash], &query[slash + 1..]),
        None => ("", query),
    };
    if display_directory
        .split('/')
        .any(|segment| EXCLUDED_DIRECTORIES.contains(&segment))
    {
        return Ok(Vec::new());
    }
    let absolute = files::resolve_target(root, display_directory)?;
    let entries = read_directory(&absolute)?;
    let mut candidates = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && !fragment.starts_with('.') {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        let path = format!("{display_directory}{name}");
        if file_type.is_dir() {
            if EXCLUDED_DIRECTORIES.contains(&name.as_str()) {
                continue;
            }
            candidates.push(Candidate {
                path,
                kind: "directory",
            });
        } else if file_type.is_file() {
            candidates.push(Candidate { path, kind: "file" });
        }
    }
    Ok(rank_listing(candidates.iter(), fragment))
}

/// Walk the workspace into the bounded index. A root read failure fails the
/// query; an unreadable subdirectory contributes nothing, like upstream.
fn scan_workspace(root: &Path) -> Result<Vec<Candidate>, RefError> {
    let mut indexed = Vec::new();
    let mut directories: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut cursor = 0;
    while cursor < directories.len() && indexed.len() < MAX_ENTRIES {
        let (absolute, relative) = directories[cursor].clone();
        let is_root = cursor == 0;
        cursor += 1;
        let entries = match read_directory(&absolute) {
            Ok(entries) => entries,
            Err(error) if is_root => return Err(error),
            Err(_) => continue,
        };
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if EXCLUDED_DIRECTORIES.contains(&name.as_str()) {
                    continue;
                }
                indexed.push(Candidate {
                    path: path.clone(),
                    kind: "directory",
                });
                directories.push((absolute.join(&name), path));
            } else if file_type.is_file() {
                indexed.push(Candidate { path, kind: "file" });
            }
            if indexed.len() >= MAX_ENTRIES {
                break;
            }
        }
    }
    Ok(indexed)
}

/// Sorted directory entries by file name (upstream's deterministic order).
fn read_directory(path: &Path) -> Result<Vec<std::fs::DirEntry>, RefError> {
    let read = std::fs::read_dir(path).map_err(|err| {
        RefError::new(
            "workspace-file/not-found",
            format!("{}: {err}", path.display()),
        )
    })?;
    let mut entries: Vec<std::fs::DirEntry> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// Upstream's ranking: score descending, directories first on ties, shorter
/// paths next, then lexicographic. Bare global queries skip hidden segments
/// unless the query itself names a dot path.
fn rank<'a>(candidates: impl Iterator<Item = &'a Candidate>, query: &str) -> Vec<Candidate> {
    rank_filtered(candidates, query, true)
}

/// A directory listing that the query explicitly named: hidden parents are
/// allowed (the user typed the `.config/` prefix).
fn rank_listing<'a>(
    candidates: impl Iterator<Item = &'a Candidate>,
    query: &str,
) -> Vec<Candidate> {
    rank_filtered(candidates, query, false)
}

fn rank_filtered<'a>(
    candidates: impl Iterator<Item = &'a Candidate>,
    query: &str,
    global_visibility: bool,
) -> Vec<Candidate> {
    let mut ranked: Vec<(i64, &Candidate)> = candidates
        .filter(|candidate| !global_visibility || visible_for_global_query(&candidate.path, query))
        .filter_map(|candidate| score(candidate, query).map(|score| (score, candidate)))
        .collect();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| kind_rank(left).cmp(&kind_rank(right)))
            .then_with(|| {
                if query.is_empty() {
                    std::cmp::Ordering::Equal
                } else {
                    left.path.len().cmp(&right.path.len())
                }
            })
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

/// Directories stay visible for a bare query only when the query opts into
/// dot paths; a dot path may match hidden entries.
fn visible_for_global_query(path: &str, query: &str) -> bool {
    if query.starts_with('.') || query.contains("/.") {
        return true;
    }
    !path.split('/').any(|segment| segment.starts_with('.'))
}

fn score(candidate: &Candidate, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let path = candidate.path.to_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    let needle = query.to_lowercase();
    let directory_bonus = if candidate.kind == "directory" { 25 } else { 0 };
    if name == needle {
        return Some(1_000 + directory_bonus);
    }
    if name.starts_with(&needle) {
        return Some(900 + directory_bonus);
    }
    if name.contains(&needle) {
        return Some(700 + directory_bonus);
    }
    if path.contains(&needle) {
        return Some(500 + directory_bonus);
    }
    subsequence_score(&path, &needle).map(|score| 300 + score + directory_bonus)
}

/// The upstream gap penalty: every skipped byte costs one point, floored at 0.
fn subsequence_score(target: &str, query: &str) -> Option<i64> {
    let mut from = 0;
    let mut gap = 0i64;
    for character in query.chars() {
        let found = target[from..].find(character)?;
        gap += found as i64;
        from += found + character.len_utf8();
    }
    Some((100 - gap).max(0))
}

fn kind_rank(candidate: &Candidate) -> u8 {
    if candidate.kind == "directory" { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_reads_the_remotes_agent_scope_wire_name() {
        // The generated remote (sessionFileReferences.list) scopes on the
        // agent and sends `{agentId, query}`; a sessionId-only reader
        // answered a wire error the client turned into an empty menu.
        assert_eq!(
            reference_session_id(&json!({ "agentId": "thr_1", "query": "" })),
            Some("thr_1")
        );
        assert_eq!(
            reference_session_id(&json!({ "sessionId": "thr_2" })),
            Some("thr_2")
        );
        assert_eq!(
            reference_session_id(&json!({ "session_id": "thr_3" })),
            Some("thr_3")
        );
        assert_eq!(reference_session_id(&json!({ "query": "" })), None);
    }

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cds-refs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/nested")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("src/nested/deep.rs"), "//").unwrap();
        std::fs::write(dir.join("node_modules/pkg/index.js"), "//").unwrap();
        std::fs::write(dir.join("README.md"), "#").unwrap();
        std::fs::write(dir.join(".hidden"), "x").unwrap();
        std::fs::create_dir_all(dir.join(".config")).unwrap();
        std::fs::write(dir.join(".config/settings.json"), "{}").unwrap();
        dir
    }

    #[test]
    fn root_listing_skips_hidden_and_excluded_directories() {
        let root = temp_workspace("root");
        let listed = list_directory(&root, "").expect("root listing");
        let paths: Vec<&str> = listed.iter().map(|entry| entry.path.as_str()).collect();
        assert!(paths.contains(&"src"), "{paths:?}");
        assert!(paths.contains(&"README.md"), "{paths:?}");
        assert!(!paths.contains(&"node_modules"), "excluded directory");
        assert!(!paths.contains(&".git"), "excluded directory");
        assert!(!paths.contains(&".hidden"), "hidden entry");
        // Directories sort before files on an empty fragment.
        assert_eq!(listed[0].kind, "directory");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_fragment_filters_and_dot_fragment_reveals_hidden() {
        let root = temp_workspace("fragment");
        let listed = list_directory(&root, "src/").expect("src listing");
        let paths: Vec<&str> = listed.iter().map(|entry| entry.path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"), "{paths:?}");
        assert!(paths.contains(&"src/nested"), "{paths:?}");
        let hidden = list_directory(&root, ".h").expect("hidden listing");
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].path, ".hidden");
        // A query that names a hidden directory lists it; the global hidden
        // filter must not swallow the explicit path.
        let config = list_directory(&root, ".config/").expect("dot directory");
        assert_eq!(config[0].path, ".config/settings.json");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn global_query_ranks_exact_before_prefix_and_skips_excluded() {
        let root = temp_workspace("global");
        let index = scan_workspace(&root).expect("scan");
        let ranked = rank(index.iter(), "main");
        assert_eq!(ranked[0].path, "src/main.rs");
        let ranked = rank(index.iter(), "deep");
        assert_eq!(ranked[0].path, "src/nested/deep.rs");
        // node_modules never enters the index; hidden entries are filtered
        // out for a bare query.
        assert!(rank(index.iter(), "index").is_empty());
        assert!(rank(index.iter(), "hidden").is_empty());
        let ranked = rank(index.iter(), ".hidden");
        assert_eq!(ranked[0].path, ".hidden");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn query_forms_match_upstream_semantics() {
        let root = temp_workspace("forms");
        // A bare fragment on an uncached call builds the index.
        let (candidates, built) = discover(&root, "deep", None).expect("discover");
        assert!(built.is_some(), "a fresh bare query builds the index");
        assert_eq!(candidates[0].path, "src/nested/deep.rs");
        // A cached call reuses it.
        let entries = built.expect("index");
        let (candidates, built) = discover(&root, "main", Some(entries)).expect("cached");
        assert!(built.is_none());
        assert_eq!(candidates[0].path, "src/main.rs");
        // A path query lists live and never builds the index.
        let (candidates, built) = discover(&root, "src/", None).expect("listing");
        assert!(built.is_none());
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.path == "src/main.rs")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn outside_workspace_paths_are_refused() {
        let root = temp_workspace("outside");
        let error = list_directory(&root, "../").expect_err("escape must fail");
        assert_eq!(error.code, "workspace-file/outside-workspace");
        let _ = std::fs::remove_dir_all(&root);
    }
}
