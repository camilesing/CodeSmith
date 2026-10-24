//! Per-workspace trust list of external paths the agent may read/write
//! without triggering a `PathEscape` error (#29).
//!
//! Storage: `~/.codesmith/workspace-trust.json`. The file is a JSON object
//! mapping each workspace's canonical path to a sorted list of canonical
//! paths the user has explicitly trusted from that workspace. Trust granted
//! in workspace A does not apply when running from workspace B.
//!
//! Two writers: the REPL commands `/trust add` / `/trust remove`, and the
//! path-access prompt a file tool raises when it targets an untrusted path
//! (an `[a]` answer stores the path the user just allowed; `[y]` grants it for
//! the turn only, in the engine's live list, and leaves this file untouched).
//!
//! Threat model: this is a deliberate user opt-in to a path the workspace
//! sandbox would otherwise refuse. The only access the trust list grants is
//! through CodeSmith's own file tools (`read_file`, `write_file`, etc.) —
//! it does not loosen the OS sandbox profile (Seatbelt/Landlock) used for
//! shell commands. Sandbox-profile expansion is tracked separately so a
//! shell tool can opt into the same paths in a future release.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use codesmith_agent_runtime::utils::write_atomic;

const TRUST_FILE_NAME: &str = "workspace-trust.json";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct TrustFile {
    /// Map workspace canonical path → sorted unique trusted paths. `PathBuf`
    /// on both sides so the in-memory pipeline (compare, retain, push) never
    /// routes a real path through `to_string_lossy`. Known limitation: the
    /// on-disk form is JSON, so non-UTF-8 path bytes are still mangled at
    /// serialization time — the type change only stops adding a SECOND lossy
    /// conversion before the file is even reached.
    #[serde(default)]
    workspaces: BTreeMap<PathBuf, Vec<PathBuf>>,
}

/// In-memory trust list for a single workspace, snapshotted at load time.
/// Tools consult this snapshot to decide whether an out-of-workspace path
/// is permitted; the engine refreshes it after `/trust` mutations.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceTrust {
    paths: Vec<PathBuf>,
}

impl WorkspaceTrust {
    #[must_use]
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self { paths: Vec::new() }
    }

    /// Load the trusted-paths snapshot for `workspace` from disk. Missing or
    /// malformed files yield an empty list rather than an error so a corrupt
    /// trust file never wedges the frontend; the next mutation rewrites it.
    #[must_use]
    pub fn load_for(workspace: &Path) -> Self {
        match trust_file_path() {
            Some(path) => Self::load_from_file(workspace, &path),
            None => Self::empty(),
        }
    }

    fn load_from_file(workspace: &Path, file_path: &Path) -> Self {
        let key = workspace_key(workspace);
        let file = read_trust_file_at(file_path).unwrap_or_default();
        let paths = file
            .workspaces
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        Self { paths }
    }

    /// Return the trusted paths in canonical form.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Whether the candidate is trusted: the candidate (after canonical
    /// normalization) starts with one of the trusted prefixes. Directory
    /// trust grants access to anything under the directory.
    ///
    /// Both the canonicalized and the raw candidate form are matched, and the
    /// trusted prefix is re-canonicalized at check time: a grant recorded for
    /// a not-yet-existing path keeps its verbatim form (macOS
    /// `/var/folders/...` vs. canonical `/private/var/folders/...`), so a
    /// single-form comparison silently never matches once the path exists.
    ///
    /// Known limitation (TOCTOU): the check and the file tool's actual open
    /// are separate steps; a symlink swapped in between can redirect access
    /// after the check returned true. The engine's sandbox remains the
    /// enforcement boundary for shell commands.
    #[must_use]
    #[allow(dead_code)]
    pub fn permits(&self, candidate: &Path) -> bool {
        let forms = [candidate.canonicalize().ok(), Some(candidate.to_path_buf())];
        self.paths.iter().any(|trusted| {
            let trusted = trusted.canonicalize().unwrap_or_else(|_| trusted.clone());
            forms.iter().flatten().any(|c| c.starts_with(&trusted))
        })
    }
}

/// Add `path` to `workspace`'s trust list and persist. Returns the canonical
/// trusted path that was actually stored, so callers can echo it back to the
/// user.
pub fn add(workspace: &Path, path: &Path) -> Result<PathBuf> {
    let trust_path = trust_file_path()
        .context("home directory not available; cannot persist workspace trust list")?;
    add_at(workspace, path, &trust_path)
}

fn add_at(workspace: &Path, path: &Path, trust_path: &Path) -> Result<PathBuf> {
    let canonical = canonicalize_or_keep(path);
    let key = workspace_key(workspace);
    // The whole read-modify-write runs under the shared fd-lock helper
    // (sibling `workspace-trust.json.lock`): two CLI processes mutating
    // different workspaces' entries share one file, and without the lock
    // both read the old state and the loser's write silently drops the
    // winner's entry.
    codesmith_agent_runtime::settings_file::with_settings_lock(trust_path, || {
        let mut file = read_trust_file_at(trust_path)?;
        let entry = file.workspaces.entry(key.clone()).or_default();
        if !entry.iter().any(|p| p == &canonical) {
            entry.push(canonical.clone());
            entry.sort();
            entry.dedup();
        }
        write_trust_file_at(&file, trust_path)?;
        Ok(canonical.clone())
    })
}

/// Remove `path` from `workspace`'s trust list. Returns true when an entry
/// was actually removed.
pub fn remove(workspace: &Path, path: &Path) -> Result<bool> {
    let Some(trust_path) = trust_file_path() else {
        return Ok(false);
    };
    remove_at(workspace, path, &trust_path)
}

fn remove_at(workspace: &Path, path: &Path, trust_path: &Path) -> Result<bool> {
    let canonical = canonicalize_or_keep(path);
    let key = workspace_key(workspace);
    // Same fd-lock rationale as `add_at`: hold the exclusive guard across
    // read-modify-write so a concurrent mutation cannot be lost.
    codesmith_agent_runtime::settings_file::with_settings_lock(trust_path, || {
        let mut file = read_trust_file_at(trust_path)?;
        let removed = match file.workspaces.get_mut(&key) {
            Some(entry) => {
                let len_before = entry.len();
                entry.retain(|p| p != &canonical);
                let changed = entry.len() != len_before;
                if entry.is_empty() {
                    file.workspaces.remove(&key);
                }
                changed
            }
            None => false,
        };
        if removed {
            write_trust_file_at(&file, trust_path)?;
        }
        Ok(removed)
    })
}

fn workspace_key(workspace: &Path) -> PathBuf {
    canonicalize_or_keep(workspace)
}

fn canonicalize_or_keep(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn trust_file_path() -> Option<PathBuf> {
    codesmith_config::ensure_state_dir(".")
        .ok()
        .map(|dir| dir.join(TRUST_FILE_NAME))
}

fn read_trust_file_at(path: &Path) -> Result<TrustFile> {
    if !path.exists() {
        return Ok(TrustFile::default());
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    match serde_json::from_str(&raw) {
        Ok(file) => Ok(file),
        Err(err) => {
            // Quarantine the unparsable file before the caller's
            // default-and-rewrite destroys it for good.
            tracing::warn!(
                "trust file {} failed to parse ({err}); quarantined a copy and continuing empty",
                path.display()
            );
            codesmith_agent_runtime::utils::preserve_corrupt_state_file(path);
            Ok(TrustFile::default())
        }
    }
}

fn write_trust_file_at(file: &TrustFile, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(file).context("serialize trust file")?;
    write_atomic(path, json.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Set up an isolated fake `~/.codesmith/workspace-trust.json` location.
    /// Returns the tmpdir (kept alive for the test) plus the explicit trust
    /// file path passed to the `*_at` helpers — avoids touching `$HOME` so
    /// tests run safely in parallel.
    fn isolated_trust_path() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let trust_path = tmp.path().join(".codesmith").join("workspace-trust.json");
        (tmp, trust_path)
    }

    #[test]
    fn empty_trust_for_unknown_workspace() {
        let (tmp, trust_path) = isolated_trust_path();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let trust = WorkspaceTrust::load_from_file(&workspace, &trust_path);
        assert!(trust.paths().is_empty());
        assert!(!trust.permits(Path::new("/anywhere")));
    }

    #[test]
    fn add_persists_and_load_returns_path() {
        let (tmp, trust_path) = isolated_trust_path();
        let workspace = tmp.path().join("ws");
        let other = tmp.path().join("data/notes");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        let stored = add_at(&workspace, &other, &trust_path).expect("add");
        // On macOS, /var/folders is a symlink to /private/var/folders so the
        // canonical form may live under that prefix. Compare using
        // canonicalize on both ends.
        let canonical_other = other.canonicalize().unwrap_or(other.clone());
        assert_eq!(stored, canonical_other);

        let trust = WorkspaceTrust::load_from_file(&workspace, &trust_path);
        assert_eq!(trust.paths().len(), 1);
        // Create the file so canonicalize resolves through any symlinks; the
        // stored trust path uses the canonical form.
        let inner = other.join("file.md");
        std::fs::write(&inner, "x").unwrap();
        assert!(trust.permits(&inner));
        assert!(!trust.permits(Path::new("/etc/passwd")));
    }

    #[test]
    fn add_is_idempotent() {
        let (tmp, trust_path) = isolated_trust_path();
        let workspace = tmp.path().join("ws");
        let other = tmp.path().join("data/notes");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        let _ = add_at(&workspace, &other, &trust_path).unwrap();
        let _ = add_at(&workspace, &other, &trust_path).unwrap();
        let trust = WorkspaceTrust::load_from_file(&workspace, &trust_path);
        assert_eq!(trust.paths().len(), 1);
    }

    #[test]
    fn trust_is_workspace_scoped() {
        let (tmp, trust_path) = isolated_trust_path();
        let ws_a = tmp.path().join("ws-a");
        let ws_b = tmp.path().join("ws-b");
        let other = tmp.path().join("data/notes");
        std::fs::create_dir_all(&ws_a).unwrap();
        std::fs::create_dir_all(&ws_b).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        add_at(&ws_a, &other, &trust_path).unwrap();
        assert_eq!(
            WorkspaceTrust::load_from_file(&ws_a, &trust_path)
                .paths()
                .len(),
            1
        );
        assert_eq!(
            WorkspaceTrust::load_from_file(&ws_b, &trust_path)
                .paths()
                .len(),
            0
        );
    }

    #[test]
    fn remove_deletes_path() {
        let (tmp, trust_path) = isolated_trust_path();
        let workspace = tmp.path().join("ws");
        let other = tmp.path().join("data/notes");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        add_at(&workspace, &other, &trust_path).unwrap();
        let removed = remove_at(&workspace, &other, &trust_path).unwrap();
        assert!(removed);

        let trust = WorkspaceTrust::load_from_file(&workspace, &trust_path);
        assert!(trust.paths().is_empty());
    }

    /// A grant recorded for a path that did not exist yet keeps the verbatim
    /// (non-canonical) form; once the path exists and the candidate
    /// canonicalizes (macOS `/var` → `/private/var`), the check must still
    /// match instead of silently never applying the user's opt-in.
    #[test]
    fn permits_matches_verbatim_grant_after_path_exists() {
        let (tmp, _trust_path) = isolated_trust_path();
        let other = tmp.path().join("data/notes");
        std::fs::create_dir_all(&other).unwrap();
        let inner = other.join("file.md");
        std::fs::write(&inner, "x").unwrap();

        // Store the raw, possibly non-canonical form directly (as
        // canonicalize_or_keep would for a not-yet-existing path).
        let trust = WorkspaceTrust {
            paths: vec![other.clone()],
        };
        assert!(
            trust.permits(&inner),
            "raw-form grant must match the canonicalized candidate"
        );
        assert!(
            trust.permits(&inner.canonicalize().unwrap_or(inner.clone())),
            "grant must also match itself in canonical form"
        );
        assert!(!trust.permits(Path::new("/etc/passwd")));
    }
}
