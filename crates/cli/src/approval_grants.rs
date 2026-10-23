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
            removed: BTreeMap::new(),
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
        had
    }

    /// Write the store back, union-merging with what is on disk minus
    /// anything [`Self::revoke`] removed this session — so a second
    /// codesmith running in the same workspace cannot lose its grants to
    /// this process's snapshot.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut doc = load_doc(path);
        for (key, grants) in &self.projects {
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
            if self.projects.contains_key(key) {
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
        codesmith_agent_runtime::utils::write_atomic(path, encoded.as_bytes())
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
            "approvals.toml is corrupt; quarantined a copy and starting with no persistent grants"
        );
        // Quarantine before defaulting: the next save would otherwise
        // overwrite the corrupt file and destroy the only copy of the
        // user's grant history.
        codesmith_agent_runtime::utils::preserve_corrupt_state_file(path);
        GrantsDoc::default()
    })
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
    fn load_corrupt_file_degrades_to_empty() {
        let path = temp_path("corrupt.toml");
        std::fs::write(&path, "not [ valid toml {{{{").expect("write");
        let grants = ApprovalGrants::load(&path);
        assert_eq!(grants, ApprovalGrants::default());
    }

    impl ApprovalGrants {}
}
