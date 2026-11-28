//! Cross-session persistent approval grants (P1-4).
//!
//! `ReviewDecision::AlwaysAllow` persists a grant into
//! `~/.codesmith/approvals.toml`, keyed by canonical workspace path:
//!
//! ```toml
//! [projects."/Users/camile/Work/Rust/CodeSmith"]
//! grants = ["shell:cargo build"]
//! updated_at = "2026-09-23T12:00:00Z"
//! ```
//!
//! The store lives in the user's home directory — never inside a
//! repository — so a cloned hostile workspace has no file it can write to
//! grant itself anything (the mirror article's "not one square inch of
//! grantable soil inside the repo"). Grants are arity-aware command
//! prefixes (the same `build_approval_grouping_key` the in-session
//! approve-for-session path uses), and lookups are scoped per workspace:
//! a grant recorded in project A never auto-approves anything in
//! project B.
//!
//! Load failures (missing file, corrupt TOML) degrade to an empty store —
//! a broken grants file must never block the session, it just stops
//! auto-approving. Saves are atomic (temp file + rename) and merged with
//! whatever is on disk, so a crash mid-write can't tear the file and two
//! concurrent sessions can't erase each other's grants. Save failures
//! surface to the caller (the approval decision still applies for this
//! session; only the persistence is lost).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// In-memory mirror of `approvals.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalGrants {
    projects: BTreeMap<String, BTreeSet<String>>,
    /// Grants removed this session, subtracted from the on-disk state in
    /// [`ApprovalGrants::save`] so the union merge can never resurrect a
    /// grant the user explicitly removed.
    removed: BTreeMap<String, BTreeSet<String>>,
    /// Grants added this session: the only thing [`ApprovalGrants::save`]
    /// merges onto the on-disk state. Re-inserting the whole in-memory view
    /// wrote back a grant another open session had just revoked.
    added: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GrantsDoc {
    #[serde(default)]
    projects: BTreeMap<String, GrantsProject>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GrantsProject {
    #[serde(default)]
    grants: BTreeSet<String>,
    updated_at: Option<String>,
}

/// Resolve `~/.codesmith/approvals.toml`. `None` when the home directory
/// can't be determined (CI containers) — callers treat that as "persistent
/// grants unavailable".
#[must_use]
pub fn approvals_toml_path() -> Option<PathBuf> {
    let parent = codesmith_agent_runtime::workspace_trust::default_config_path()?
        .parent()
        .map(Path::to_path_buf)?;
    // A relative/empty parent (e.g. CODESMITH_CONFIG_PATH="config.toml",
    // whose parent is "") resolves approvals.toml against the CWD — which
    // can be inside a repository, violating "not one square inch of
    // grantable soil inside the repo". Degrade to "persistent grants
    // unavailable" instead of writing into the workspace.
    if parent.as_os_str().is_empty() || !parent.is_absolute() {
        return None;
    }
    Some(parent.join("approvals.toml"))
}

impl ApprovalGrants {
    /// Load the store from `path`. Missing or corrupt file → empty store
    /// (never fatal — see the module doc).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let doc = load_doc(path).unwrap_or_else(|err| {
            // Only NotFound is legitimately empty (mapped inside load_doc).
            // Every other error (PermissionDenied, IsADirectory, transient
            // EIO) degrades the in-memory view, but save() refuses to
            // overwrite the unreadable file, so the on-disk history is
            // never destroyed by this session.
            tracing::warn!(
                target: "codesmith::approvals",
                path = %path.display(),
                error = %err,
                "approvals.toml is unreadable; starting with no persistent grants \
                 (saves are refused until the read error is fixed)"
            );
            GrantsDoc::default()
        });
        Self {
            projects: doc
                .projects
                .into_iter()
                .map(|(key, project)| (key, project.grants))
                .collect(),
            removed: BTreeMap::new(),
            added: BTreeMap::new(),
        }
    }

    /// Whether `workspace_key` has a persisted grant for `grouping_key`.
    /// The workspace key is the canonical workspace path — grants never
    /// leak across projects.
    #[must_use]
    pub fn is_granted(&self, workspace_key: &str, grouping_key: &str) -> bool {
        self.projects
            .get(workspace_key)
            .is_some_and(|grants| grants.contains(grouping_key))
    }

    /// future management command).
    #[allow(dead_code)]
    #[must_use]
    pub fn grants_for(&self, workspace_key: &str) -> Vec<String> {
        self.projects
            .get(workspace_key)
            .map(|grants| grants.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Remember a grant for `workspace_key`. Takes effect for the running
    /// session immediately (the approval path reads the caller's in-memory
    /// copy) and for later sessions once [`Self::save`] runs.
    pub fn grant(&mut self, workspace_key: &str, grouping_key: &str) {
        self.projects
            .entry(workspace_key.to_string())
            .or_default()
            .insert(grouping_key.to_string());
        self.added
            .entry(workspace_key.to_string())
            .or_default()
            .insert(grouping_key.to_string());
        // A re-granted key must not stay in the removed set, or the union
        // merge in `save` would drop it again.
        if let Some(removed) = self.removed.get_mut(workspace_key) {
            removed.remove(grouping_key);
        }
    }

    /// Forget a grant, recording the removal so [`Self::save`] cannot
    /// resurrect it from disk.
    pub fn revoke(&mut self, workspace_key: &str, grouping_key: &str) -> bool {
        let had = self
            .projects
            .get_mut(workspace_key)
            .is_some_and(|grants| grants.remove(grouping_key));
        self.removed
            .entry(workspace_key.to_string())
            .or_default()
            .insert(grouping_key.to_string());
        if let Some(added) = self.added.get_mut(workspace_key) {
            added.remove(grouping_key);
        }
        had
    }

    /// Write the store back: this session's additions merged onto what is
    /// on disk, minus anything [`Self::revoke`] removed this session — so a
    /// second codesmith running in the same workspace cannot lose its
    /// grants to this process's snapshot, and a grant that session revoked
    /// is not written back from this one's stale view. The delta is cleared
    /// only after a successful write.
    ///
    /// The whole load-merge-write cycle runs under the runtime's shared
    /// fd-lock helper (sibling `approvals.toml.lock`): without it, two
    /// sessions whose loads interleaved still clobbered each other's
    /// freshly added grants last-writer-wins, because the union merge only
    /// helps when both processes read before either writes. Callers are
    /// sync REPL handlers on an async dispatch chain (pre-existing
    /// posture); the lock waits at most 10 s under contention.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        codesmith_agent_runtime::settings_file::with_settings_lock(
            path,
            || -> anyhow::Result<()> {
                // Refuse to overwrite an unreadable file: merging onto an
                // empty view would rewrite it with only this session's
                // grants and destroy every other workspace's history.
                let mut doc = load_doc(path)?;
                for (key, grants) in &self.added {
                    let project = doc.projects.entry(key.clone()).or_default();
                    for grant in grants {
                        project.grants.insert(grant.clone());
                    }
                }
                for (key, removed) in &self.removed {
                    if let Some(project) = doc.projects.get_mut(key) {
                        for grant in removed {
                            project.grants.remove(grant);
                        }
                    }
                }
                doc.projects.retain(|_, project| !project.grants.is_empty());
                for (key, project) in doc.projects.iter_mut() {
                    if self.added.contains_key(key) || self.removed.contains_key(key) {
                        project.updated_at = Some(chrono::Utc::now().to_rfc3339());
                    }
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let encoded = toml::to_string_pretty(&doc)
                    .map_err(|err| std::io::Error::other(format!("encode grants: {err}")))?;
                // The module doc promises atomic saves; a plain `fs::write` torn by
                // a crash produced exactly the corrupt file the loader then
                // silently defaults away.
                Ok(codesmith_agent_runtime::utils::write_atomic(
                    path,
                    encoded.as_bytes(),
                )?)
            },
        )
        .map_err(|err| std::io::Error::other(format!("grants save under lock failed: {err:#}")))?;
        // The delta is on disk now. A failed save leaves it, so the next
        // one retries it.
        self.added.clear();
        self.removed.clear();
        Ok(())
    }
}

/// Read the raw on-disk document. Missing or corrupt file → empty doc,
/// never fatal for [`ApprovalGrants::load`]; any OTHER read error is
/// returned so [`ApprovalGrants::save`] can refuse to overwrite an
/// unreadable-but-intact file instead of destroying every other
/// workspace's grant history.
fn load_doc(path: &Path) -> std::io::Result<GrantsDoc> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GrantsDoc::default());
        }
        Err(err) => {
            // Only NotFound is legitimately empty. Every other error
            // (PermissionDenied, IsADirectory, transient EIO) used to
            // silently degrade to an empty store — a later save would then
            // overwrite the unreadable-but-intact file with only this
            // session's grants. Surface it loudly; the save side's lock
            // and atomic write keep the torn-overwrite window small.
            return Err(err);
        }
    };
    Ok(toml::from_str::<GrantsDoc>(&raw).unwrap_or_else(|err| {
        tracing::warn!(
            target: "codesmith::approvals",
            path = %path.display(),
            error = %err,
            "approvals.toml is corrupt; quarantined a copy and starting with no persistent grants"
        );
        // Quarantine before defaulting: the next save would otherwise
        // overwrite the corrupt file and destroy the only copy of the
        // user's grant history.
        codesmith_agent_runtime::utils::preserve_corrupt_state_file(path);
        GrantsDoc::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        // keep() hands ownership back (no auto-cleanup); these tests
        // are short-lived and the directories live under TMPDIR.
        tempfile::tempdir().expect("tempdir").keep().join(name)
    }

    #[test]
    fn load_missing_file_yields_empty_store() {
        let grants = ApprovalGrants::load(Path::new("/nonexistent/approvals.toml"));
        assert!(!grants.is_granted("/w/a", "shell:cargo build"));
    }

    #[test]
    #[cfg(unix)]
    fn save_refuses_to_overwrite_an_unreadable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.toml");
        std::fs::write(&path, "[projects]\n").unwrap();
        let original = std::fs::metadata(&path).unwrap().permissions();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200)).unwrap();
        let mut grants = ApprovalGrants::default();
        let res = grants.save(&path);
        std::fs::set_permissions(&path, original).unwrap();
        assert!(res.is_err(), "save must refuse when the file is unreadable");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "[projects]\n", "unreadable file must stay intact");
    }

    #[test]
    fn load_corrupt_file_degrades_to_empty() {
        let path = temp_path("corrupt.toml");
        std::fs::write(&path, "not [ valid toml {{{{").expect("write");
        let grants = ApprovalGrants::load(&path);
        assert_eq!(grants, ApprovalGrants::default());
    }

    /// Session A loaded the store with grant X, session B revoked X and
    /// saved, then A saved a new grant: A's save wrote its whole in-memory
    /// view back and resurrected the grant the user had removed.
    #[test]
    fn a_stale_session_saves_only_its_own_delta() {
        let path = temp_path("stale-delta.toml");
        let mut seed = ApprovalGrants::default();
        seed.grant("/w/a", "shell:cargo test");
        seed.save(&path).expect("seed saves");

        let mut a = ApprovalGrants::load(&path);
        let mut b = ApprovalGrants::load(&path);
        b.revoke("/w/a", "shell:cargo test");
        b.save(&path).expect("B saves the removal");
        a.grant("/w/a", "shell:npm test");
        a.save(&path).expect("A saves with a pre-removal snapshot");

        let reloaded = ApprovalGrants::load(&path);
        assert!(
            !reloaded.is_granted("/w/a", "shell:cargo test"),
            "a revoked grant came back"
        );
        assert!(reloaded.is_granted("/w/a", "shell:npm test"));
    }

    #[test]
    fn a_store_that_never_loaded_the_file_preserves_on_disk_grants() {
        // B never read the file: its save merges only its own delta onto
        // whatever is on disk at that moment, so A's earlier grant survives.
        // Sequential on purpose; the lock that keeps a concurrent saver out
        // of the merge is `a_save_waits_for_the_lock_another_writer_holds`.
        let path = temp_path("unloaded-store.toml");
        let mut a = ApprovalGrants::default();
        a.grant("/w/a", "shell:cargo build");
        a.save(&path).expect("A saves");

        let mut stale = ApprovalGrants::default();
        stale.grant("/w/b", "shell:npm test");
        stale.save(&path).expect("B saves with a pre-A snapshot");

        let reloaded = ApprovalGrants::load(&path);
        assert!(reloaded.is_granted("/w/a", "shell:cargo build"));
        assert!(reloaded.is_granted("/w/b", "shell:npm test"));
    }

    /// The load-merge-write in `save` is one critical section: while another
    /// writer holds the grants file's lock, a save waits for it. Without the
    /// lock the waiter merged onto the pre-holder state and its rename then
    /// dropped whatever the holder wrote.
    #[test]
    fn a_save_waits_for_the_lock_another_writer_holds() {
        let path = temp_path("held-lock.toml");
        let mut a = ApprovalGrants::default();
        a.grant("/w/a", "shell:cargo build");
        a.save(&path).expect("A saves");

        // Another writer holds the lock until this test lets go.
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let lock_path = path.clone();
        let holder = std::thread::spawn(move || {
            codesmith_agent_runtime::settings_file::with_settings_lock(&lock_path, || {
                held_tx.send(()).expect("the test is listening");
                release_rx.recv().expect("the test releases the holder");
                Ok(())
            })
            .expect("the holder takes the lock");
        });
        held_rx.recv().expect("the holder reports the lock is held");

        let mut b = ApprovalGrants::default();
        b.grant("/w/b", "shell:npm test");
        let saving = {
            let path = path.clone();
            std::thread::spawn(move || b.save(&path))
        };

        // The waiter polls the lock every 50 ms: several polls on, none of
        // B's grant has reached the file.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !saving.is_finished(),
            "a save must wait while another writer holds the lock"
        );
        assert!(
            !ApprovalGrants::load(&path).is_granted("/w/b", "shell:npm test"),
            "B's save wrote past the lock"
        );

        release_tx.send(()).expect("the holder is listening");
        holder.join().expect("the holder thread ends");
        saving
            .join()
            .expect("the saving thread ends")
            .expect("B's save completes once the lock is free");
        let reloaded = ApprovalGrants::load(&path);
        assert!(reloaded.is_granted("/w/a", "shell:cargo build"));
        assert!(reloaded.is_granted("/w/b", "shell:npm test"));
    }

    impl ApprovalGrants {}
}
