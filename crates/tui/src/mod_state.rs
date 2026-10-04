//! Persistent activation/enablement state for script Mods (§F script mod
//! layer).
//!
//! Backs `/mods activate|disable|enable` + the `manage_mods` tool. Mirrors
//! `ExtensionStateStore` (`crates/tui/src/extension_state.rs`) verbatim —
//! same atomic-write, malformed→default, BTreeSet-for-determinism strategy.
//!
//! Storage shape (TOML at `~/.codesmith/mods_state.toml`):
//!
//! ```toml
//! disabled = ["mod-id-1"]
//! activated = ["mod-id-2"]
//! ```
//!
//! Semantics:
//! - `activated` — the user has consented to this mod id (first-activation
//!   approval, plan §五). Same-id reloads — watcher or manual — need no
//!   further approval.
//! - `disabled` — explicitly turned off (implies activated; activation is
//!   retained so re-enabling needs no fresh consent).
//!
//! Default state when the file does not exist: empty lists (nothing
//! activated — the first-activation gate). A corrupt file is logged and
//! treated as the default, so upgrades never silently activate mods.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const STATE_FILE_NAME: &str = "mods_state.toml";

#[derive(Debug, Clone, Default)]
pub struct ModStateStore {
    path: Option<PathBuf>,
    disabled: BTreeSet<String>,
    activated: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OnDiskState {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    activated: Vec<String>,
}

impl ModStateStore {
    pub fn load_default() -> Result<Self> {
        let path = default_state_path()?;
        Self::load_from(path)
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                disabled: BTreeSet::new(),
                activated: BTreeSet::new(),
            });
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read mod state at {}", path.display()))?;
        let parsed: OnDiskState = match toml::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    "mods_state.toml at {} is malformed ({}); treating nothing as activated",
                    path.display(),
                    err
                );
                OnDiskState::default()
            }
        };

        Ok(Self {
            path: Some(path),
            disabled: parsed.disabled.into_iter().collect(),
            activated: parsed.activated.into_iter().collect(),
        })
    }

    /// First-activation gate: a mod loads only when its id is activated AND
    /// not disabled.
    pub fn should_load(&self, mod_id: &str) -> bool {
        self.activated.contains(mod_id) && !self.disabled.contains(mod_id)
    }

    pub fn is_activated(&self, mod_id: &str) -> bool {
        self.activated.contains(mod_id)
    }

    pub fn is_disabled(&self, mod_id: &str) -> bool {
        self.disabled.contains(mod_id)
    }

    /// Record first-activation consent (and clear any disable).
    pub fn activate(&mut self, mod_id: &str) -> Result<()> {
        let a_changed = self.activated.insert(mod_id.to_string());
        let d_changed = self.disabled.remove(mod_id);
        if !a_changed && !d_changed {
            return Ok(());
        }
        self.persist()
    }

    /// Drop the activation record entirely (mod removal).
    pub fn deactivate(&mut self, mod_id: &str) -> Result<()> {
        let a_changed = self.activated.remove(mod_id);
        let d_changed = self.disabled.remove(mod_id);
        if !a_changed && !d_changed {
            return Ok(());
        }
        self.persist()
    }

    pub fn set_enabled(&mut self, mod_id: &str, enabled: bool) -> Result<()> {
        let changed = if enabled {
            self.disabled.remove(mod_id)
        } else {
            self.disabled.insert(mod_id.to_string())
        };
        if !changed {
            return Ok(());
        }
        self.persist()
    }

    pub fn disabled(&self) -> Vec<String> {
        self.disabled.iter().cloned().collect()
    }

    pub fn activated(&self) -> Vec<String> {
        self.activated.iter().cloned().collect()
    }

    /// Directory holding per-mod KV files (`<state-dir>/mods-state/`). Mods
    /// loaded through `populate_extension_runtime` get
    /// `<kv_dir>/<scope>-<id>.json`; `None` only for in-memory stores (tests).
    /// Derived from this store's own path so tempdir-backed stores (tests)
    /// keep KV writes hermetic.
    pub fn kv_dir(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|d| d.join("mods-state"))
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let on_disk = OnDiskState {
            disabled: self.disabled.iter().cloned().collect(),
            activated: self.activated.iter().cloned().collect(),
        };
        let body = toml::to_string_pretty(&on_disk).context("serialize mod state")?;
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
    crate::utils::write_atomic(path, bytes).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh() -> (TempDir, ModStateStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        let store = ModStateStore::load_from(path).unwrap();
        (dir, store)
    }

    #[test]
    fn missing_file_defaults_to_nothing_activated() {
        let (_dir, store) = fresh();
        assert!(!store.should_load("anything"));
        assert!(store.activated().is_empty());
        assert!(store.disabled().is_empty());
    }

    #[test]
    fn activate_then_should_load() {
        let (_dir, mut store) = fresh();
        store.activate("m1").unwrap();
        assert!(store.should_load("m1"));
        assert!(!store.should_load("m2"));
    }

    #[test]
    fn activate_persists_across_reload() {
        let (dir, mut store) = fresh();
        store.activate("m1").unwrap();
        let reloaded = ModStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(reloaded.should_load("m1"));
    }

    #[test]
    fn disable_gates_load_but_keeps_activation() {
        let (_dir, mut store) = fresh();
        store.activate("m1").unwrap();
        store.set_enabled("m1", false).unwrap();
        assert!(!store.should_load("m1"));
        assert!(store.is_activated("m1"), "disable keeps consent");
        store.set_enabled("m1", true).unwrap();
        assert!(store.should_load("m1"), "re-enable needs no fresh consent");
    }

    #[test]
    fn deactivate_drops_everything() {
        let (_dir, mut store) = fresh();
        store.activate("m1").unwrap();
        store.set_enabled("m1", false).unwrap();
        store.deactivate("m1").unwrap();
        assert!(!store.is_activated("m1"));
        assert!(!store.is_disabled("m1"));
    }

    #[test]
    fn redundant_activate_is_noop() {
        let (dir, mut store) = fresh();
        store.activate("m1").unwrap();
        let before = fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        store.activate("m1").unwrap();
        let after = fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert_eq!(before, after, "no-op activate must not rewrite the file");
    }

    #[test]
    fn malformed_file_falls_back_to_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        fs::write(&path, b"this is not toml = { broken").unwrap();
        let store = ModStateStore::load_from(path).unwrap();
        assert!(!store.should_load("anything"));
    }

    #[test]
    fn lists_are_deterministic_order() {
        let (_dir, mut store) = fresh();
        store.activate("zeta").unwrap();
        store.activate("alpha").unwrap();
        store.set_enabled("beta", false).unwrap();
        assert_eq!(
            store.activated(),
            vec!["alpha".to_string(), "zeta".to_string()]
        );
        assert_eq!(store.disabled(), vec!["beta".to_string()]);
    }

    #[test]
    fn disable_then_reload_persists() {
        let (dir, mut store) = fresh();
        store.activate("m1").unwrap();
        store.set_enabled("m1", false).unwrap();
        let reloaded = ModStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(!reloaded.should_load("m1"));
        assert!(reloaded.is_activated("m1"));
    }

    #[test]
    fn activated_serializes_as_toml_array() {
        let (dir, mut store) = fresh();
        store.activate("m1").unwrap();
        let raw = fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(raw.contains("activated"), "raw: {raw}");
        assert!(raw.contains("m1"));
    }
}
