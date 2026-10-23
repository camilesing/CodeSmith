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
    /// Backing file; `None` for an [`ModKvStore::ephemeral`] store (never
    /// persisted). `map` stays `None` until first access (lazy load).
    path: Option<PathBuf>,
    /// `None` until first access (lazy load); `Some` thereafter.
    map: Option<BTreeMap<String, serde_json::Value>>,
    /// Set when the backing file existed but could not be read: `persist`
    /// refuses to overwrite on-disk state with a map built from a read
    /// that never succeeded (a transient EACCES/NFS blip would otherwise
    /// atomically rename a near-empty map over the real file — permanent
    /// key loss).
    load_failed: bool,
}

impl std::fmt::Debug for ModKvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModKvStore")
            .field(
                "path",
                &self
                    .0
                    .lock()
                    .expect("kv lock poisoned")
                    .path
                    .as_deref()
                    .map(std::path::Path::display),
            )
            .finish()
    }
}

impl ModKvStore {
    /// Create a store backed by `path` (e.g.
    /// `~/.codesmith/mods-state/project-commit-guard.json`). The file is not
    /// read until the first `get`/`set`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self(Arc::new(Mutex::new(KvInner {
            path: Some(path),
            map: None,
            load_failed: false,
        })))
    }

    /// Create an in-memory-only store: reads see an empty map and writes are
    /// never persisted. For callers with no owning state directory (tests,
    /// path-less stores) — never persist mod data into a shared temp dir.
    #[must_use]
    pub fn ephemeral() -> Self {
        Self(Arc::new(Mutex::new(KvInner {
            path: None,
            map: None,
            load_failed: false,
        })))
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
            tracing::warn!("mod kv persist failed ({}): {e}", inner.path_display());
        }
    }

    /// Drop a key + persist (used by mod removal to clean state).
    pub fn remove(&self, key: &str) {
        let mut inner = self.0.lock().expect("kv lock poisoned");
        inner.ensure_loaded();
        if let Some(map) = inner.map.as_mut() {
            map.remove(key);
        }
        if let Err(e) = inner.persist() {
            tracing::warn!("mod kv persist failed ({}): {e}", inner.path_display());
        }
    }
}

impl KvInner {
    fn path_display(&self) -> std::borrow::Cow<'_, str> {
        self.path
            .as_deref()
            .map(|p| std::borrow::Cow::Owned(p.display().to_string()))
            .unwrap_or_else(|| std::borrow::Cow::Borrowed("(ephemeral)"))
    }

    fn ensure_loaded(&mut self) {
        if self.map.is_some() {
            return;
        }
        let Some(path) = self.path.as_deref() else {
            // Ephemeral store: nothing on disk, start empty.
            self.map = Some(BTreeMap::new());
            return;
        };
        self.map = Some(match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                tracing::warn!(
                    "mod kv file {} malformed ({e}); treating as empty",
                    path.display()
                );
                BTreeMap::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                // An unreadable-but-present file is NOT the same as an empty
                // store. Reads see empty so the mod keeps working, but the
                // failure is recorded: `persist` must refuse to overwrite
                // on-disk state with a map built from a read that never
                // succeeded.
                tracing::warn!(
                    "mod kv file {} unreadable ({e}); reads return empty and                      writes are refused until reload",
                    path.display()
                );
                self.load_failed = true;
                BTreeMap::new()
            }
        });
    }

    fn persist(&self) -> std::io::Result<()> {
        if self.load_failed {
            return Err(std::io::Error::other(
                "kv file was unreadable; refusing to overwrite on-disk state",
            ));
        }
        let Some(map) = &self.map else {
            return Ok(());
        };
        let Some(path) = self.path.as_deref() else {
            return Ok(()); // ephemeral: nothing to persist
        };
        let body = serde_json::to_string_pretty(map)
            .map_err(|e| std::io::Error::other(format!("serialize kv: {e}")))?;
        atomic_write(path, body.as_bytes())
    }
}

/// Write bytes to a sibling temp file then rename over `path` (atomic on
/// both Unix `rename` and Windows `MoveFileEx` semantics of `fs::rename`
/// best-effort).
///
/// The temp name carries the pid: a global mod's KV file is shared across
/// concurrent sessions (two REPL sessions, REPL + CLI), and a fixed sibling
/// name would have one process rename the other's half-written content (or
/// fail on ENOENT). The file is fsynced before the rename so a crash never
/// leaves an empty file behind the (atomic) rename point.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::other(format!("kv path {} has no parent", path.display()))
    })?;
    std::fs::create_dir_all(parent)?;
    // pid alone does not disambiguate two stores in the same process
    // sharing a backing path (a reloaded global mod): the sequence number
    // keeps concurrent temp files distinct.
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
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
    fn unreadable_file_refuses_destructive_persist() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("global-x.json");
            std::fs::write(&path, r#"{"keep":1}"#).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Root (some CI) reads through mode 0 — the guard is
            // unobservable there, so skip rather than fail.
            if std::fs::read_to_string(&path).is_ok() {
                return;
            }
            let kv = ModKvStore::new(path.clone());
            kv.set("new", json!(2));
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let on_disk =
                std::fs::read_to_string(&path).expect("original file still present");
            assert_eq!(on_disk, r#"{"keep":1}"#);
        }
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

    #[test]
    fn ephemeral_store_round_trips_in_memory_only() {
        let kv = ModKvStore::ephemeral();
        kv.set("k", json!("v"));
        kv.remove("nope");
        assert_eq!(kv.get("k"), Some(json!("v")));
        // A distinct ephemeral store shares nothing (no backing file).
        assert_eq!(ModKvStore::ephemeral().get("k"), None);
    }
}
