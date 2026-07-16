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
//! `ToolSpec` implementation and the projection helpers stay in the TUI
//! (they depend on `spec.rs` which has not yet migrated); they consume these
//! types through the TUI shim re-export.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::utils::sha256_hex;

/// Preview length (in chars) stored on every [`VarHandle`] so consumers can
/// show a snippet without touching the backing payload.
const REPR_PREVIEW_CHARS: usize = 160;

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
}

#[allow(dead_code)] // Producers land in later v0.8.33 slices; handle_read is first.
#[derive(Debug, Clone)]
pub enum HandleValue {
    Text(String),
    Json(Value),
}

#[allow(dead_code)] // Foundation methods used by upcoming RLM/agent session producers.
impl HandleValue {
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
            Self::Json(value) => serde_json::to_vec(value).unwrap_or_default(),
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

    #[must_use]
    pub fn get(&self, handle: &VarHandle) -> Option<&HandleRecord> {
        self.records.get(&handle.key())
    }

    fn insert(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        value: HandleValue,
    ) -> VarHandle {
        let session_id = session_id.into();
        let name = name.into();
        let handle = VarHandle {
            kind: "var_handle".to_string(),
            session_id: session_id.clone(),
            name: name.clone(),
            type_name: value.type_name(),
            length: value.length(),
            repr_preview: value.repr_preview(),
            sha256: sha256_hex(&value.stable_bytes()),
        };
        let key = HandleKey { session_id, name };
        self.records.insert(
            key,
            HandleRecord {
                handle: handle.clone(),
                value,
            },
        );
        handle
    }
}

/// Truncate `text` to at most `max_chars` Unicode scalar values.
///
/// Shared by [`HandleValue::repr_preview`] and the TUI-side projection
/// helpers (re-exported through the TUI shim).
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx == max_chars {
            break;
        }
        out.push(ch);
    }
    out
}
