//! Symbolic handle storage and bounded reads — data types.
//!
//! `var_handle` is the shared protocol that lets expensive environments
//! (RLM sessions, sub-agent transcripts, large artifacts) hand the parent a
//! small symbolic reference instead of copying the whole payload into the
//! parent transcript.
//!
//! This module hosts the portable data types ([`SharedHandleStore`],
//! [`HandleStore`], [`VarHandle`], …) that `spec::RuntimeToolServices` and
//! producer tools share across the crate boundary. The `HandleReadTool`
//! `ToolSpec` implementation and the projection helpers stay in
//! `codesmith-tool-impls`; they consume these types through its re-export.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::utils::sha256_hex;

/// Preview length (in chars) stored on every [`VarHandle`] so consumers can
/// show a snippet without touching the backing payload.
const REPR_PREVIEW_CHARS: usize = 160;

/// Entry cap for [`HandleStore`]. The payloads parked here are by design the
/// large ones (RLM transcripts, sub-agent artifacts) and a store can be held
/// for a whole REPL session, so unbounded inserts are a monotonic memory
/// leak. Eviction is FIFO by recency; an evicted handle resolves to a clear
/// not-found error (`handle_read`) rather than silently wrong data.
const MAX_HANDLE_ENTRIES: usize = 1024;

/// Total-payload cap for [`HandleStore`] (64 MiB), bounding memory even when
/// individual payloads are large. The just-inserted entry is never evicted by
/// its own insert, so the budget may overshoot by one payload.
const MAX_HANDLE_TOTAL_BYTES: usize = 64 * 1024 * 1024;

pub type SharedHandleStore = Arc<Mutex<HandleStore>>;

#[must_use]
pub fn new_shared_handle_store() -> SharedHandleStore {
    Arc::new(Mutex::new(HandleStore::default()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VarHandle {
    pub kind: String,
    pub session_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    /// Kind-dependent length — see [`HandleValue::length`] for the exact
    /// semantics per payload kind. It is NOT a byte size and NOT a bound on
    /// the serialized payload: a 1-element list wrapping a 10 MB string
    /// reports `1`.
    pub length: usize,
    pub repr_preview: String,
    pub sha256: String,
}

impl VarHandle {
    #[must_use]
    pub fn key(&self) -> HandleKey {
        HandleKey {
            session_id: self.session_id.clone(),
            name: self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandleKey {
    pub session_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct HandleRecord {
    pub handle: VarHandle,
    pub value: HandleValue,
    /// Cached `value.stable_bytes().len()` — the digest already forces the
    /// bytes through the hasher, so accounting the size for the eviction
    /// budget is free.
    pub approx_bytes: usize,
}

#[allow(dead_code)] // Producers land in later v0.8.33 slices; handle_read is first.
#[derive(Debug, Clone)]
pub enum HandleValue {
    Text(String),
    Json(Value),
}

#[allow(dead_code)] // Foundation methods used by upcoming RLM/agent session producers.
impl HandleValue {
    /// Kind-dependent length, serialized into [`VarHandle::length`]:
    ///
    /// - [`HandleValue::Text`]: char count (Unicode scalar values).
    /// - JSON arrays: element count.
    /// - JSON objects: entry count.
    /// - Other JSON scalars (string/bool/number/null): char count of the
    ///   serialized JSON form.
    ///
    /// The units are deliberately mixed per kind. This is NOT a byte size
    /// and NOT a bound on the serialized payload: a 1-element list wrapping
    /// a 10 MB string reports `1`. Use a handle's `count` projection
    /// (`handle_read`) for byte-accurate sizing.
    fn length(&self) -> usize {
        match self {
            Self::Text(text) => text.chars().count(),
            Self::Json(Value::Array(items)) => items.len(),
            Self::Json(Value::Object(map)) => map.len(),
            Self::Json(value) => value.to_string().chars().count(),
        }
    }

    fn type_name(&self) -> String {
        match self {
            Self::Text(_) => "str".to_string(),
            Self::Json(Value::Array(_)) => "list".to_string(),
            Self::Json(Value::Object(_)) => "dict".to_string(),
            Self::Json(Value::String(_)) => "str".to_string(),
            Self::Json(Value::Bool(_)) => "bool".to_string(),
            Self::Json(Value::Number(_)) => "number".to_string(),
            Self::Json(Value::Null) => "null".to_string(),
        }
    }

    fn stable_bytes(&self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.as_bytes().to_vec(),
            Self::Json(value) => {
                serde_json::to_vec(value).expect("serde_json::Value always serializes to JSON")
            }
        }
    }

    fn repr_preview(&self) -> String {
        match self {
            Self::Text(text) => truncate_chars(text, REPR_PREVIEW_CHARS),
            Self::Json(value) => truncate_chars(&value.to_string(), REPR_PREVIEW_CHARS),
        }
    }
}

#[derive(Debug, Default)]
pub struct HandleStore {
    records: HashMap<HandleKey, HandleRecord>,
    /// Insertion/recency order driving FIFO eviction; re-inserting a key
    /// refreshes its position.
    order: VecDeque<HandleKey>,
    total_bytes: usize,
}

#[allow(dead_code)] // Insertors are for producer tools; this PR wires the reader first.
impl HandleStore {
    #[must_use]
    pub fn insert_text(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> VarHandle {
        self.insert(session_id, name, HandleValue::Text(text.into()))
    }

    #[must_use]
    pub fn insert_json(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        value: Value,
    ) -> VarHandle {
        self.insert(session_id, name, HandleValue::Json(value))
    }

    /// Resolve a handle to its stored payload.
    ///
    /// Returns an owned clone of the record so callers holding a
    /// [`SharedHandleStore`] (a tokio `Mutex`) can drop the guard right
    /// after the lookup instead of holding the global lock across the
    /// full-payload clone/projection.
    ///
    /// Last-writer-wins on same-key re-insert: the lookup keys on
    /// `(session_id, name)` only, so a handle issued before a re-insert of
    /// the same key resolves to the *new* payload, whose digest, length and
    /// type no longer match the passed handle. Consumers that must detect
    /// the swap verify the digest themselves (`handle_read` rejects a
    /// `sha256` mismatch).
    #[must_use]
    pub fn get(&self, handle: &VarHandle) -> Option<HandleRecord> {
        self.records.get(&handle.key()).cloned()
    }

    /// Drop every handle owned by `session_id`, returning how many were
    /// removed. Lets the runtime reclaim the large payloads (RLM transcripts,
    /// sub-agent artifacts) a finished session parked in the store.
    pub fn remove_session(&mut self, session_id: &str) -> usize {
        let doomed: Vec<HandleKey> = self
            .records
            .iter()
            .filter(|(key, _)| key.session_id == session_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &doomed {
            if let Some(record) = self.records.remove(key) {
                self.total_bytes = self.total_bytes.saturating_sub(record.approx_bytes);
            }
        }
        self.order.retain(|key| key.session_id != session_id);
        doomed.len()
    }

    fn insert(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        value: HandleValue,
    ) -> VarHandle {
        let session_id = session_id.into();
        let name = name.into();
        let stable_bytes = value.stable_bytes();
        let handle = VarHandle {
            kind: "var_handle".to_string(),
            session_id: session_id.clone(),
            name: name.clone(),
            type_name: value.type_name(),
            length: value.length(),
            repr_preview: value.repr_preview(),
            sha256: sha256_hex(&stable_bytes),
        };
        let approx_bytes = stable_bytes.len();
        let key = HandleKey { session_id, name };
        if let Some(prev) = self.records.insert(
            key.clone(),
            HandleRecord {
                handle: handle.clone(),
                value,
                approx_bytes,
            },
        ) {
            // Same-key re-insert replaces the payload: retire the old
            // accounting and refresh the key's recency.
            self.total_bytes = self.total_bytes.saturating_sub(prev.approx_bytes);
            self.order.retain(|k| k != &key);
        }
        self.order.push_back(key.clone());
        self.total_bytes += approx_bytes;
        self.evict_over_budget(&key);
        handle
    }

    /// FIFO-evict oldest entries until the store is back under both budgets.
    /// The key just inserted is never evicted by its own insert — a caller
    /// must always be able to read back what it just parked.
    fn evict_over_budget(&mut self, keep: &HandleKey) {
        while self.records.len() > MAX_HANDLE_ENTRIES || self.total_bytes > MAX_HANDLE_TOTAL_BYTES {
            let Some(oldest) = self.order.front().cloned() else {
                break;
            };
            if &oldest == keep {
                break;
            }
            self.order.pop_front();
            if let Some(record) = self.records.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(record.approx_bytes);
            }
        }
    }
}

/// Truncate `text` to at most `max_chars` Unicode scalar values.
///
/// Shared by [`HandleValue::repr_preview`] and the projection
/// helpers in `codesmith-tool-impls`.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_oldest_beyond_entry_budget() {
        let mut store = HandleStore::default();
        let first = store.insert_text("s", "h0", "first");
        for idx in 1..=MAX_HANDLE_ENTRIES {
            let _ = store.insert_text("s", format!("h{idx}"), "payload");
        }
        // `first` (oldest) was evicted; the newest insert is readable.
        assert!(store.get(&first).is_none());
        assert_eq!(store.records.len(), MAX_HANDLE_ENTRIES);
        let newest = VarHandle {
            kind: "var_handle".to_string(),
            session_id: "s".to_string(),
            name: format!("h{MAX_HANDLE_ENTRIES}"),
            type_name: "str".to_string(),
            length: 7,
            repr_preview: "payload".to_string(),
            sha256: sha256_hex(b"payload"),
        };
        assert!(store.get(&newest).is_some());
    }

    #[test]
    fn reinsert_refreshes_recency_and_accounting() {
        let mut store = HandleStore::default();
        let old = store.insert_text("s", "a", "x".repeat(50));
        let _ = store.insert_text("s", "b", "y".repeat(50));
        // Re-insert "a": it is now the newest key and its old bytes are
        // retired from the budget.
        let renewed = store.insert_text("s", "a", "z".repeat(80));
        assert_eq!(store.total_bytes, 50 + 80);
        assert_eq!(
            store.order.iter().filter(|k| k.name == "a").count(),
            1,
            "re-insert must not duplicate the recency entry"
        );
        // Same-key lookup is by key, not handle: both the stale and the
        // renewed handle resolve to the NEW payload (documented
        // last-writer-wins semantics).
        assert_eq!(
            store.get(&old).map(|record| record.handle.sha256),
            Some(renewed.sha256.clone())
        );
        assert!(store.get(&renewed).is_some());
    }

    #[test]
    fn remove_session_clears_records_order_and_bytes() {
        let mut store = HandleStore::default();
        let _ = store.insert_text("s1", "a", "hello");
        let _ = store.insert_text("s2", "b", "world");
        assert_eq!(store.remove_session("s1"), 1);
        assert_eq!(store.records.len(), 1);
        assert!(store.order.iter().all(|k| k.session_id != "s1"));
        assert_eq!(store.total_bytes, b"world".len());
        assert_eq!(store.remove_session("s1"), 0);
    }

    #[test]
    fn byte_budget_bounds_total_payload() {
        let mut store = HandleStore::default();
        let chunk = "x".repeat(MAX_HANDLE_TOTAL_BYTES / 4 + 1);
        for idx in 0..8 {
            let _ = store.insert_text("s", format!("big{idx}"), chunk.clone());
        }
        assert!(store.total_bytes <= MAX_HANDLE_TOTAL_BYTES + chunk.len());
        assert!(
            store.records.len() < 8,
            "byte budget must have evicted entries"
        );
    }
}
