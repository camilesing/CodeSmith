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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// In-memory mirror of `approvals.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalGrants {
    projects: BTreeMap<String, BTreeSet<String>>,
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
    Some(
        codesmith_agent_runtime::workspace_trust::default_config_path()?
            .parent()
            .map(Path::to_path_buf)?
            .join("approvals.toml"),
    )
}

impl ApprovalGrants {
    /// Load the store from `path`. Missing or corrupt file → empty store
    /// (never fatal — see the module doc).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        Self {
            projects: load_doc(path)
                .projects
                .into_iter()
                .map(|(key, project)| (key, project.grants))
                .collect(),
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

    /// Record a grant. Returns `true` when it was new (the caller may skip
    /// the save when `false`).
    pub fn insert(&mut self, workspace_key: &str, grouping_key: &str) -> bool {
        self.projects
            .entry(workspace_key.to_string())
            .or_default()
            .insert(grouping_key.to_string())
    }

    /// Remove a grant. Returns `true` when something was removed. Reserved
    /// for a future `/approvals` management command — not yet wired into
    /// the UI.
    #[allow(dead_code)]
    pub fn remove(&mut self, workspace_key: &str, grouping_key: &str) -> bool {
        let removed = self
            .projects
            .get_mut(workspace_key)
            .is_some_and(|grants| grants.remove(grouping_key));
        // Drop emptied project tables so the file doesn't accumulate
        // `[projects."..."]` husks.
        self.projects.retain(|_, grants| !grants.is_empty());
        removed
    }

    /// The persisted grants for one workspace (sorted; for tests and a
    /// future management command).
    #[allow(dead_code)]
    #[must_use]
    pub fn grants_for(&self, workspace_key: &str) -> Vec<String> {
        self.projects
            .get(workspace_key)
            .map(|grants| grants.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Serialize to `path` atomically, merged with the on-disk state.
    ///
    /// * **Atomic** — via `write_atomic` (temp file + fsync + rename in
    ///   the same directory): a crash mid-write can't leave a torn file,
    ///   which the next `load` would silently degrade to an empty store.
    /// * **Merged** — the store is loaded once at startup, so another
    ///   concurrently running CodeSmith session may have persisted its
    ///   own grants since; union with the on-disk sets before
    ///   serializing so the last writer doesn't erase them.
    /// * **Truthful timestamps** — projects whose merged grant set is
    ///   unchanged from disk keep their on-disk `updated_at`; only
    ///   actually-modified projects are stamped `now`.
    ///
    /// The file keeps owner-only permissions (0o600 on Unix), same
    /// posture as config.toml writes.
    pub fn save(&self, path: &Path) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let on_disk = load_doc(path);
        let mut merged: BTreeMap<String, BTreeSet<String>> = self.projects.clone();
        for (key, project) in &on_disk.projects {
            merged
                .entry(key.clone())
                .or_default()
                .extend(project.grants.iter().cloned());
        }
        let doc = GrantsDoc {
            projects: merged
                .iter()
                .map(|(key, grants)| {
                    let updated_at = match on_disk.projects.get(key) {
                        Some(on_disk_project) if &on_disk_project.grants == grants => {
                            on_disk_project.updated_at.clone()
                        }
                        _ => Some(now.clone()),
                    };
                    (
                        key.clone(),
                        GrantsProject {
                            grants: grants.clone(),
                            updated_at,
                        },
                    )
                })
                .collect(),
        };
        let serialized =
            toml::to_string_pretty(&doc).context("failed to serialize approvals.toml")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        crate::utils::write_atomic(path, serialized.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        // write_atomic's temp file is created 0o600 on Unix; re-assert for
        // pre-existing targets and mode-at-open edge cases (filesystems
        // that reject chmod keep the written contents).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

/// Read the raw on-disk document. Missing or corrupt file → empty doc,
/// never fatal (same degradation as [`ApprovalGrants::load`]).
fn load_doc(path: &Path) -> GrantsDoc {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return GrantsDoc::default(),
    };
    toml::from_str::<GrantsDoc>(&raw).unwrap_or_else(|err| {
        tracing::warn!(
            target: "codesmith::approvals",
            path = %path.display(),
            error = %err,
            "approvals.toml is corrupt; starting with no persistent grants"
        );
        GrantsDoc::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        // into_path() hands ownership back (no auto-cleanup); these tests
        // are short-lived and the directories live under TMPDIR.
        tempfile::tempdir().expect("tempdir").into_path().join(name)
    }

    #[test]
    fn load_missing_file_yields_empty_store() {
        let grants = ApprovalGrants::load(Path::new("/nonexistent/approvals.toml"));
        assert!(!grants.is_granted("/w/a", "shell:cargo build"));
    }

    #[test]
    fn load_corrupt_file_degrades_to_empty() {
        let path = temp_path("corrupt.toml");
        std::fs::write(&path, "not [ valid toml {{{{").expect("write");
        let grants = ApprovalGrants::load(&path);
        assert_eq!(grants, ApprovalGrants::default());
    }

    #[test]
    fn save_then_load_round_trips_grants() {
        let path = temp_path("roundtrip.toml");
        let mut grants = ApprovalGrants::default();
        grants.insert("/w/project-a", "shell:cargo build");
        grants.insert("/w/project-a", "shell:cargo test");
        grants.insert("/w/project-b", "shell:git status");
        grants.save(&path).expect("save");

        let reloaded = ApprovalGrants::load(&path);
        assert_eq!(reloaded.grants_for("/w/project-a").len(), 2);
        assert!(reloaded.is_granted("/w/project-a", "shell:cargo build"));
        assert!(reloaded.is_granted("/w/project-b", "shell:git status"));
    }

    #[test]
    fn grants_never_leak_across_workspaces() {
        let mut grants = ApprovalGrants::default();
        grants.insert("/w/project-a", "shell:cargo build");
        assert!(!grants.is_granted("/w/project-b", "shell:cargo build"));
    }

    #[test]
    fn save_merges_grants_persisted_by_a_concurrent_session() {
        let path = temp_path("merge.toml");
        // This session loaded an empty store at startup…
        let mut ours = ApprovalGrants::default();
        ours.insert("/w/project-a", "shell:cargo build");
        // …then another session in a different project saved its own
        // grants to the shared file.
        let mut theirs = ApprovalGrants::default();
        theirs.insert("/w/project-a", "shell:cargo test");
        theirs.insert("/w/project-b", "shell:git status");
        theirs.save(&path).expect("their save");

        // Our save must union with theirs, not last-writer-wins-erase.
        ours.save(&path).expect("merged save");
        let reloaded = ApprovalGrants::load(&path);
        assert!(reloaded.is_granted("/w/project-a", "shell:cargo build"));
        assert!(reloaded.is_granted("/w/project-a", "shell:cargo test"));
        assert!(reloaded.is_granted("/w/project-b", "shell:git status"));
    }

    #[test]
    fn save_preserves_untouched_projects_updated_at() {
        let path = temp_path("timestamps.toml");
        let mut other = ApprovalGrants::default();
        other.insert("/w/other", "shell:git status");
        other.save(&path).expect("save");
        let before: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        let original = before["projects"]["/w/other"]["updated_at"]
            .as_str()
            .expect("updated_at present")
            .to_string();

        // A different session saves an unrelated project — /w/other's
        // timestamp must not be rewritten.
        let mut ours = ApprovalGrants::default();
        ours.insert("/w/ours", "shell:cargo build");
        ours.save(&path).expect("save");

        let after: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        let kept = after["projects"]["/w/other"]["updated_at"]
            .as_str()
            .expect("other project still has updated_at");
        assert_eq!(
            kept, original,
            "untouched project's updated_at must survive"
        );
        // The project this save actually touched did get a fresh stamp.
        assert!(
            after["projects"]["/w/ours"]["updated_at"]
                .as_str()
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_leaves_owner_only_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("mode.toml");
        let mut grants = ApprovalGrants::default();
        grants.insert("/w/a", "shell:cargo build");
        grants.save(&path).expect("save");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "approvals.toml must stay owner-only");
    }

    #[test]
    fn insert_is_idempotent_and_remove_drops_emptied_tables() {
        let mut grants = ApprovalGrants::default();
        assert!(grants.insert("/w/a", "shell:cargo build"));
        assert!(
            !grants.insert("/w/a", "shell:cargo build"),
            "second insert is not new"
        );
        assert!(grants.remove("/w/a", "shell:cargo build"));
        assert_eq!(grants.grants_for("/w/a"), Vec::<String>::new());
        // The emptied project table is gone entirely.
        assert!(!grants.save_roundtrip_contains_table("/w/a"));
    }

    impl ApprovalGrants {
        fn save_roundtrip_contains_table(&self, key: &str) -> bool {
            let path = temp_path("husk.toml");
            self.save(&path).expect("save");
            let raw = std::fs::read_to_string(&path).expect("read");
            raw.contains(&format!("[projects.{key:?}]"))
        }
    }
}
