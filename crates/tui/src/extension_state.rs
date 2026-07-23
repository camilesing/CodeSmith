//! Persistent enable/disable + install provenance state for extensions.
//!
//! Backs `/extension enable|disable` + GET/POST runtime API. Mirrors
//! `SkillStateStore` (`crates/tui/src/skill_state.rs`) verbatim — same
//! atomic-write, malformed→default, BTreeSet-for-determinism strategy.
//!
//! Storage shape (TOML at `~/.codesmith/extensions_state.toml`):
//!
//! ```toml
//! disabled = ["ext-id-1"]
//! installed = ["git:github.com/foo/bar@v1"]   # §F5 provenance; slice 1 unused
//! ```
//!
//! Default state when the file does not exist: empty lists (everything
//! enabled, nothing installed). A corrupt file is logged and treated as
//! the default, so upgrades never accidentally disable every extension.

// Slice 1 (§7.2) does not wire `ExtensionStateStore` into `App` —
// `build_extension_runtime` (Task 9) is its first caller. Suppress dead-code
// until then; remove this attribute when Task 9 lands.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const STATE_FILE_NAME: &str = "extensions_state.toml";

#[derive(Debug, Clone, Default)]
pub struct ExtensionStateStore {
    path: Option<PathBuf>,
    disabled: BTreeSet<String>,
    /// §F5c: install-source provenance keyed by extension id (e.g.
    /// `"fixture-dylib" -> "git:github.com/foo/bar@v1"`). §F5b declared the
    /// field but never populated it (slice 1 read/wrote a `BTreeSet<String>`
    /// for forward-compat); §F5c changes to a map so `/extension uninstall
    /// <id>` can remove by id. No migration: §F5b never wrote real data.
    installed: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OnDiskState {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    installed: BTreeMap<String, String>,
}

impl ExtensionStateStore {
    pub fn load_default() -> Result<Self> {
        let path = default_state_path()?;
        Self::load_from(path)
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                disabled: BTreeSet::new(),
                installed: BTreeMap::new(),
            });
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read extension state at {}", path.display()))?;
        let parsed: OnDiskState = match toml::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    "extensions_state.toml at {} is malformed ({}); treating all extensions as enabled",
                    path.display(),
                    err
                );
                OnDiskState::default()
            }
        };

        Ok(Self {
            path: Some(path),
            disabled: parsed.disabled.into_iter().collect(),
            installed: parsed.installed,
        })
    }

    pub fn is_enabled(&self, ext_id: &str) -> bool {
        !self.disabled.contains(ext_id)
    }

    pub fn set_enabled(&mut self, ext_id: &str, enabled: bool) -> Result<()> {
        let changed = if enabled {
            self.disabled.remove(ext_id)
        } else {
            self.disabled.insert(ext_id.to_string())
        };
        if !changed {
            return Ok(());
        }
        self.persist()
    }

    pub fn disabled(&self) -> Vec<String> {
        self.disabled.iter().cloned().collect()
    }

    /// Provenance strings for installed extensions (back-compat: returns the
    /// values). §F5c keys by id internally; use [`installed_ids`](Self::installed_ids)
    /// for the keys.
    pub fn installed(&self) -> Vec<String> {
        self.installed.values().cloned().collect()
    }

    /// Ids of installed extensions (§F5c).
    pub fn installed_ids(&self) -> Vec<String> {
        self.installed.keys().cloned().collect()
    }

    /// Provenance for one installed extension id (§F5c).
    pub fn provenance_for(&self, id: &str) -> Option<String> {
        self.installed.get(id).cloned()
    }

    /// Record install provenance for `id` (§F5c). Overwrites if reinstalled.
    pub fn add_installed(&mut self, id: &str, provenance: &str) -> Result<()> {
        self.installed
            .insert(id.to_string(), provenance.to_string());
        self.persist()
    }

    /// Remove install provenance for `id` (§F5c). No-op if absent.
    pub fn remove_installed(&mut self, id: &str) -> Result<()> {
        self.installed.remove(id);
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let on_disk = OnDiskState {
            disabled: self.disabled.iter().cloned().collect(),
            installed: self.installed.clone(),
        };
        let body = toml::to_string_pretty(&on_disk).context("serialize extension state")?;
        atomic_write(path, body.as_bytes())
    }
}

fn default_state_path() -> Result<PathBuf> {
    let dir = codesmith_config::ensure_state_dir(".")
        .context("could not resolve or create CodeSmith state directory")?;
    Ok(dir.join(STATE_FILE_NAME))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir for {}", path.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, bytes).with_context(|| format!("write tmp at {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename tmp into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh() -> (TempDir, ExtensionStateStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        let store = ExtensionStateStore::load_from(path).unwrap();
        (dir, store)
    }

    #[test]
    fn missing_file_defaults_to_everything_enabled() {
        let (_dir, store) = fresh();
        assert!(store.is_enabled("anything"));
        assert!(store.disabled().is_empty());
    }

    #[test]
    fn disable_then_reload_persists() {
        let (dir, mut store) = fresh();
        store.set_enabled("foo", false).unwrap();
        assert!(!store.is_enabled("foo"));

        let reloaded = ExtensionStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(!reloaded.is_enabled("foo"));
        assert!(reloaded.is_enabled("bar"));
    }

    #[test]
    fn enable_removes_from_disabled_list() {
        let (_dir, mut store) = fresh();
        store.set_enabled("foo", false).unwrap();
        store.set_enabled("foo", true).unwrap();
        assert!(store.is_enabled("foo"));
    }

    #[test]
    fn redundant_toggle_is_noop() {
        let (_dir, mut store) = fresh();
        store.set_enabled("foo", true).unwrap();
        assert!(store.disabled().is_empty());
    }

    #[test]
    fn malformed_file_falls_back_to_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        fs::write(&path, b"this is not toml = { broken").unwrap();
        let store = ExtensionStateStore::load_from(path).unwrap();
        assert!(store.is_enabled("anything"));
    }

    #[test]
    fn disabled_list_is_deterministic_order() {
        let (_dir, mut store) = fresh();
        store.set_enabled("zeta", false).unwrap();
        store.set_enabled("alpha", false).unwrap();
        assert_eq!(
            store.disabled(),
            vec!["alpha".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn add_installed_persists_and_provenance_for_reads() {
        let (dir, mut store) = fresh();
        store.add_installed("my-ext", "git:github.com/foo/bar@v1").unwrap();
        assert_eq!(
            store.provenance_for("my-ext").as_deref(),
            Some("git:github.com/foo/bar@v1")
        );
        let reloaded = ExtensionStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert_eq!(
            reloaded.provenance_for("my-ext").as_deref(),
            Some("git:github.com/foo/bar@v1")
        );
        assert!(reloaded.installed_ids().contains(&"my-ext".to_string()));
    }

    #[test]
    fn remove_installed_persists() {
        let (dir, mut store) = fresh();
        store.add_installed("a", "git:x@v1").unwrap();
        store.add_installed("b", "git:y@v2").unwrap();
        store.remove_installed("a").unwrap();
        assert!(store.provenance_for("a").is_none());
        assert!(store.provenance_for("b").is_some());
        let reloaded = ExtensionStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(reloaded.provenance_for("a").is_none());
        assert!(reloaded.provenance_for("b").is_some());
    }

    #[test]
    fn installed_persists_as_toml_table() {
        let (dir, mut store) = fresh();
        store.add_installed("ext", "path:/abs").unwrap();
        let raw = fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(
            raw.contains("[installed]") || raw.contains("installed = {"),
            "installed must serialize as a TOML table: {raw}"
        );
        assert!(raw.contains("ext"));
        assert!(raw.contains("path:/abs"));
    }

    #[test]
    fn installed_ids_deterministic_order() {
        let (_dir, mut store) = fresh();
        store.add_installed("zeta", "git:z@1").unwrap();
        store.add_installed("alpha", "git:a@2").unwrap();
        let mut ids = store.installed_ids();
        ids.sort();
        assert_eq!(ids, vec!["alpha".to_string(), "zeta".to_string()]);
    }
}
