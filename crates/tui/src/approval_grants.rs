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
//! auto-approving. Save failures surface to the caller (the approval
//! decision still applies for this session; only the persistence is
//! lost).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
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
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(_) => return Self::default(),
        };
        match toml::from_str::<GrantsDoc>(&raw) {
            Ok(doc) => Self {
                projects: doc
                    .projects
                    .into_iter()
                    .map(|(key, project)| (key, project.grants))
                    .collect(),
            },
            Err(err) => {
                tracing::warn!(
                    target: "codesmith::approvals",
                    path = %path.display(),
                    error = %err,
                    "approvals.toml is corrupt; starting with no persistent grants"
                );
                Self::default()
            }
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
        self.projects
            .retain(|_, grants| !grants.is_empty());
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

    /// Serialize to `path` with owner-only permissions (same posture as
    /// config.toml writes).
    pub fn save(&self, path: &Path) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let doc = GrantsDoc {
            projects: self
                .projects
                .iter()
                .map(|(key, grants)| {
                    (
                        key.clone(),
                        GrantsProject {
                            grants: grants.clone(),
                            updated_at: Some(now.clone()),
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
        write_grants_file(path, &serialized)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

/// Owner-only file write, mirroring config's `write_config_file_secure`
/// posture (0o600 on Unix; host ACLs elsewhere).
fn write_grants_file(path: &Path, content: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        // Re-assert for pre-existing files / mode-at-open edge cases;
        // filesystems that reject chmod keep the written contents.
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        // into_path() hands ownership back (no auto-cleanup); these tests
        // are short-lived and the directories live under TMPDIR.
        tempfile::tempdir()
            .expect("tempdir")
            .into_path()
            .join(name)
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
    fn insert_is_idempotent_and_remove_drops_emptied_tables() {
        let mut grants = ApprovalGrants::default();
        assert!(grants.insert("/w/a", "shell:cargo build"));
        assert!(!grants.insert("/w/a", "shell:cargo build"), "second insert is not new");
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
