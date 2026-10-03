//! Per-mod persistent KV store (`mod_state_get` / `mod_state_set`).
//!
//! One JSON file per mod instance — `<kv-dir>/<scope>-<id>.json` — lazy-loaded
//! on first access, `Mutex`-guarded, persisted via write-temp-then-rename
//! (mirrors `extension_state.rs`'s atomic-write strategy). Isolation is by
//! construction: each `RhaiMod` bakes its own `ModKvStore` into its Rhai
//! `Engine`'s natives, so one mod physically cannot name another's file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Per-mod KV store. Cheap to clone (Arc inside) — clones share one file.
#[derive(Clone)]
pub struct ModKvStore(Arc<Mutex<KvInner>>);

struct KvInner {
    path: PathBuf,
    /// `None` until first access (lazy load); `Some` thereafter.
    map: Option<BTreeMap<String, serde_json::Value>>,
}

impl std::fmt::Debug for ModKvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModKvStore")
            .field("path", &self.0.lock().expect("kv lock poisoned").path)
            .finish()
    }
}

impl ModKvStore {
    /// Create a store backed by `path` (e.g.
    /// `~/.codesmith/mods-state/project-commit-guard.json`). The file is not
    /// read until the first `get`/`set`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self(Arc::new(Mutex::new(KvInner { path, map: None })))
    }

    /// Read a key. `None` when absent or when the backing file is malformed
    /// (malformed → treated as empty, same forgiving posture as
    /// `extension_state.rs`).
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        let mut inner = self.0.lock().expect("kv lock poisoned");
        inner.ensure_loaded();
        inner.map.as_ref().and_then(|m| m.get(key)).cloned()
    }

    /// Write a key + persist atomically.
    pub fn set(&self, key: &str, value: serde_json::Value) {
        let mut inner = self.0.lock().expect("kv lock poisoned");
        inner.ensure_loaded();
        if let Some(map) = inner.map.as_mut() {
            map.insert(key.to_string(), value);
        }
        if let Err(e) = inner.persist() {
            tracing::warn!("mod kv persist failed ({}): {e}", inner.path.display());
        }
    }

    /// Drop a key + persist (used by mod removal to clean state).
    pub fn remove(&self, key: &str) {
        let mut inner = self.0.lock().expect("kv lock poisoned");
        inner.ensure_loaded();
        if let Some(map) = inner.map.as_mut() {
            map.remove(key);
        }
        let _ = inner.persist();
    }
}

impl KvInner {
    fn ensure_loaded(&mut self) {
        if self.map.is_some() {
            return;
        }
        self.map = Some(match std::fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                tracing::warn!(
                    "mod kv file {} malformed ({}); treating as empty",
                    self.path.display(),
                    e
                );
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        });
    }

    fn persist(&self) -> std::io::Result<()> {
        let Some(map) = &self.map else {
            return Ok(());
        };
        let body = serde_json::to_string_pretty(map)
            .map_err(|e| std::io::Error::other(format!("serialize kv: {e}")))?;
        atomic_write(&self.path, body.as_bytes())
    }
}

/// Write bytes to a sibling temp file then rename over `path` (atomic on
/// both Unix `rename` and Windows `MoveFileEx` semantics of `fs::rename`
/// best-effort).
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::other(format!("kv path {} has no parent", path.display()))
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn missing_file_defaults_to_empty() {
        let dir = TempDir::new().unwrap();
        let kv = ModKvStore::new(dir.path().join("global-x.json"));
        assert!(kv.get("anything").is_none());
    }

    #[test]
    fn set_then_reload_persists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("global-x.json");
        let kv = ModKvStore::new(path.clone());
        kv.set("ci", json!("green"));
        assert_eq!(kv.get("ci"), Some(json!("green")));

        let reloaded = ModKvStore::new(path);
        assert_eq!(reloaded.get("ci"), Some(json!("green")));
        assert!(reloaded.get("other").is_none());
    }

    #[test]
    fn set_overwrites_previous_value() {
        let dir = TempDir::new().unwrap();
        let kv = ModKvStore::new(dir.path().join("project-y.json"));
        kv.set("count", json!(1));
        kv.set("count", json!(2));
        assert_eq!(kv.get("count"), Some(json!(2)));
    }

    #[test]
    fn malformed_file_falls_back_to_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("global-x.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let kv = ModKvStore::new(path);
        assert!(kv.get("k").is_none());
        // And a subsequent set recovers the file.
        kv.set("k", json!("v"));
        assert_eq!(kv.get("k"), Some(json!("v")));
    }

    #[test]
    fn remove_drops_key_and_persists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("global-x.json");
        let kv = ModKvStore::new(path.clone());
        kv.set("a", json!(1));
        kv.set("b", json!(2));
        kv.remove("a");
        assert!(kv.get("a").is_none());
        let reloaded = ModKvStore::new(path);
        assert!(reloaded.get("a").is_none());
        assert_eq!(reloaded.get("b"), Some(json!(2)));
    }

    #[test]
    fn stores_structured_values() {
        let dir = TempDir::new().unwrap();
        let kv = ModKvStore::new(dir.path().join("global-x.json"));
        kv.set("cfg", json!({"depth": 3, "names": ["a", "b"], "on": true}));
        assert_eq!(
            kv.get("cfg"),
            Some(json!({"depth": 3, "names": ["a", "b"], "on": true}))
        );
    }

    #[test]
    fn clones_share_one_backing_file() {
        let dir = TempDir::new().unwrap();
        let kv = ModKvStore::new(dir.path().join("global-x.json"));
        let clone = kv.clone();
        clone.set("k", json!("v"));
        assert_eq!(kv.get("k"), Some(json!("v")));
    }
}
