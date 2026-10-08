//! Persistent activation/enablement state for script Mods (§F script mod
//! layer).
//!
//! Backs `/mods activate|disable|enable` + the `manage_mods` tool. Mirrors
//! `ExtensionStateStore` (`crates/tui/src/extension_state.rs`) — same
//! atomic-write, malformed→default, BTreeSet-for-determinism strategy —
//! with one divergence: a malformed file is set aside as
//! `mods_state.toml.bak` (then `.bak.1`, `.bak.2`, … — first free slot)
//! before the default fallback, so the next persist cannot destroy the
//! original and a later corruption never overwrites an earlier recovery
//! copy (the sibling stores do not do this).
//!
//! Storage shape (TOML at `~/.codesmith/mods_state.toml`):
//!
//! ```toml
//! disabled = ["mod-id-1"]
//! activated = ["mod-id-2"]
//!
//! [activation_hashes]
//! mod-id-2 = "<sha256-of-entry-file-at-activation>"
//! ```
//!
//! Semantics:
//! - `activated` — the user has consented to this mod id (first-activation
//!   approval, plan §五). Same-id reloads — watcher or manual — need no
//!   further approval, EXCEPT when the entry file's content changed since
//!   the recorded `activation_hashes` entry: activation consent is bound to
//!   the content the user approved, so a changed mod (git pull, an edited
//!   file) goes back to pending and asks for a fresh `/mods activate`.
//!   Mods activated before the hash existed load once and get their hash
//!   backfilled at that load.
//! - `disabled` — explicitly turned off (implies activated; activation is
//!   retained so re-enabling needs no fresh consent).
//!
//! Known limitation: only the manifest's entry script is hashed — companion
//! files the entry reads at runtime are not part of the consent receipt.
//!
//! Default state when the file does not exist: empty lists (nothing
//! activated — the first-activation gate). A corrupt file is logged, set
//! aside as `mods_state.toml.bak` (numbered on repeat corruption), and
//! treated as the default, so upgrades never silently activate mods and
//! the original survives the next persist for manual recovery.

use std::collections::{BTreeMap, BTreeSet};
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
    activation_hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OnDiskState {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    activated: Vec<String>,
    #[serde(default)]
    activation_hashes: BTreeMap<String, String>,
}

/// Outcome of comparing a mod's current entry-file hash against the one
/// recorded at activation (see [`ModStateStore::activation_hash_state`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationHashState {
    /// Matches the recorded hash — the recorded consent covers this content.
    Matches,
    /// No hash recorded yet (activated before hashes existed): loads, and
    /// the caller backfills via [`ModStateStore::record_activation_hash`].
    NoRecord,
    /// Content changed since activation — consent does not cover it; the
    /// mod goes back to pending for a fresh `/mods activate`.
    Changed,
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
                activation_hashes: BTreeMap::new(),
            });
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read mod state at {}", path.display()))?;
        let parsed: OnDiskState = match toml::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                // Set the unreadable original aside (best-effort) before
                // falling back: the store keeps `path`, so the next
                // mutation's whole-file persist would otherwise destroy the
                // only hand-recoverable copy. Numbered slots — `fs::rename`
                // replaces an existing destination, so a fixed `.bak` name
                // would destroy the earlier recovery copy on a SECOND
                // corruption event.
                let mut backup = path.with_extension("toml.bak");
                let mut n = 1;
                while backup.exists() {
                    backup = path.with_extension(format!("toml.bak.{n}"));
                    n += 1;
                }
                tracing::warn!(
                    "mods_state.toml at {} is malformed ({}); treating nothing as activated \
                     (original set aside as {})",
                    path.display(),
                    err,
                    backup.display()
                );
                let _ = fs::rename(&path, backup);
                OnDiskState::default()
            }
        };

        Ok(Self {
            path: Some(path),
            disabled: parsed.disabled.into_iter().collect(),
            activated: parsed.activated.into_iter().collect(),
            activation_hashes: parsed.activation_hashes,
        })
    }

    /// First-activation gate: a mod loads only when its id is activated AND
    /// not disabled.
    pub fn should_load(&self, mod_id: &str) -> bool {
        self.activated.contains(mod_id) && !self.disabled.contains(mod_id)
    }

    /// Re-read this store's file from disk. For read-modify-write cycles
    /// that run under `mod_ops::mod_state_lock` but started from a
    /// lock-free snapshot (the reload backfill path): persisting the stale
    /// snapshot would clobber a concurrent writer, so the mutation re-loads
    /// first. A path-less (in-memory) store re-loads as empty — the
    /// backfill no-ops rather than write anywhere unexpected.
    pub fn reload(&self) -> Result<Self> {
        match self.path.as_ref() {
            Some(path) => Self::load_from(path.clone()),
            None => Ok(Self::default()),
        }
    }

    pub fn is_activated(&self, mod_id: &str) -> bool {
        self.activated.contains(mod_id)
    }

    pub fn is_disabled(&self, mod_id: &str) -> bool {
        self.disabled.contains(mod_id)
    }

    /// Compare the mod's current entry-file hash with the recorded one.
    /// Unactivated mods and hash-less legacy activations both load (see
    /// [`ActivationHashState`]).
    #[must_use]
    pub fn activation_hash_state(&self, mod_id: &str, actual: &str) -> ActivationHashState {
        match self.activation_hashes.get(mod_id) {
            Some(recorded) if recorded == actual => ActivationHashState::Matches,
            Some(_) => ActivationHashState::Changed,
            None => ActivationHashState::NoRecord,
        }
    }

    /// Record (or refresh) an activation hash — set by `activate`, and as a
    /// backfill on the first load of a hash-less legacy activation. Persists
    /// only when the value actually changes.
    pub fn record_activation_hash(&mut self, mod_id: &str, entry_hash: &str) -> Result<()> {
        if self.activation_hashes.get(mod_id) == Some(&entry_hash.to_string()) {
            return Ok(());
        }
        self.activation_hashes
            .insert(mod_id.to_string(), entry_hash.to_string());
        self.persist()
    }

    /// Record first-activation consent (bound to the entry-file content
    /// hash) and clear any disable.
    pub fn activate(&mut self, mod_id: &str, entry_hash: &str) -> Result<()> {
        let a_changed = self.activated.insert(mod_id.to_string());
        let d_changed = self.disabled.remove(mod_id);
        let h_changed = self
            .activation_hashes
            .insert(mod_id.to_string(), entry_hash.to_string())
            .is_some_and(|old| old != entry_hash);
        if !a_changed && !d_changed && !h_changed {
            return Ok(());
        }
        self.persist()
    }

    /// Drop the activation record entirely (mod removal).
    pub fn deactivate(&mut self, mod_id: &str) -> Result<()> {
        let a_changed = self.activated.remove(mod_id);
        let d_changed = self.disabled.remove(mod_id);
        let h_changed = self.activation_hashes.remove(mod_id).is_some();
        if !a_changed && !d_changed && !h_changed {
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
            activation_hashes: self.activation_hashes.clone(),
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
        store.activate("m1", "hash-m1").unwrap();
        assert!(store.should_load("m1"));
        assert!(!store.should_load("m2"));
    }

    #[test]
    fn activate_persists_across_reload() {
        let (dir, mut store) = fresh();
        store.activate("m1", "hash-m1").unwrap();
        let reloaded = ModStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(reloaded.should_load("m1"));
    }

    #[test]
    fn disable_gates_load_but_keeps_activation() {
        let (_dir, mut store) = fresh();
        store.activate("m1", "hash-m1").unwrap();
        store.set_enabled("m1", false).unwrap();
        assert!(!store.should_load("m1"));
        assert!(store.is_activated("m1"), "disable keeps consent");
        store.set_enabled("m1", true).unwrap();
        assert!(store.should_load("m1"), "re-enable needs no fresh consent");
    }

    #[test]
    fn deactivate_drops_everything() {
        let (_dir, mut store) = fresh();
        store.activate("m1", "hash-m1").unwrap();
        store.set_enabled("m1", false).unwrap();
        store.deactivate("m1").unwrap();
        assert!(!store.is_activated("m1"));
        assert!(!store.is_disabled("m1"));
    }

    #[test]
    fn redundant_activate_is_noop() {
        let (dir, mut store) = fresh();
        store.activate("m1", "hash-m1").unwrap();
        let before = fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        store.activate("m1", "hash-m1").unwrap();
        let after = fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert_eq!(before, after, "no-op activate must not rewrite the file");
    }

    #[test]
    fn malformed_file_falls_back_to_default_and_keeps_a_backup() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        fs::write(&path, b"this is not toml = { broken").unwrap();
        let store = ModStateStore::load_from(path.clone()).unwrap();
        assert!(!store.should_load("anything"));
        // The corrupt original is set aside, not destroyed by the fallback
        // store's next whole-file persist.
        let bak = dir.path().join("mods_state.toml.bak");
        assert_eq!(
            fs::read_to_string(&bak).unwrap(),
            "this is not toml = { broken"
        );
        // And the fresh file persists cleanly over the fallback state.
        let mut store = ModStateStore::load_from(path).unwrap();
        store.activate("m", "h").unwrap();
        assert!(
            fs::read_to_string(dir.path().join(STATE_FILE_NAME))
                .unwrap()
                .contains("m")
        );
    }

    #[test]
    fn repeated_corruption_keeps_every_backup() {
        // `fs::rename` replaces an existing destination on Unix — a second
        // corruption event must not overwrite the first hand-recovery copy.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        fs::write(&path, b"first corruption = { broken").unwrap();
        let _ = ModStateStore::load_from(path.clone()).unwrap();
        fs::write(&path, b"second corruption = { broken").unwrap();
        let _ = ModStateStore::load_from(path.clone()).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("mods_state.toml.bak")).unwrap(),
            "first corruption = { broken"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("mods_state.toml.bak.1")).unwrap(),
            "second corruption = { broken"
        );
    }

    #[test]
    fn lists_are_deterministic_order() {
        let (_dir, mut store) = fresh();
        store.activate("zeta", "hash-zeta").unwrap();
        store.activate("alpha", "hash-alpha").unwrap();
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
        store.activate("m1", "hash-m1").unwrap();
        store.set_enabled("m1", false).unwrap();
        let reloaded = ModStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(!reloaded.should_load("m1"));
        assert!(reloaded.is_activated("m1"));
    }

    #[test]
    fn activated_serializes_as_toml_array() {
        let (dir, mut store) = fresh();
        store.activate("m1", "hash-m1").unwrap();
        let raw = fs::read_to_string(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(raw.contains("activated"), "raw: {raw}");
        assert!(raw.contains("m1"));
    }
}
