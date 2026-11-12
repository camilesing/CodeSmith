//! Per-message and session feedback, stored in a desktop-local sidecar.
//!
//! CodeSmith's runtime journal has no feedback event type and the CLI's
//! `/feedback` ledger is transcript-index based. The vendored feedback
//! surface (`client-ui-message-feedback`) nevertheless expects the host's
//! `messageFeedback/list|put|delete` compare-and-set contract and the
//! session-level `sessionFeedback/record`. This module backs both from one
//! atomic JSON document at `~/.codesmith/desktop/feedback.json`:
//!
//! ```text
//! { "sessions": { "<sessionId>": {
//!     "messages": { "<messageId>": { rating, note?, category?, version, … } },
//!     "records":  [ { text?, category?, at } ]
//! } } }
//! ```
//!
//! The sidecar is desktop-local, like the Workspace registry: it survives a
//! bridge restart (the UI's list read seeds its controls) and is never sent
//! to the runtime or model context. Version tokens are UUIDs and every
//! mutation is compare-and-set, so two connected clients cannot silently
//! overwrite each other's rating; only the acting client updates live (the
//! follow stream carries no synthesized feedback events).
//!
//! Known limitations: no live cross-client feedback events (a reload re-reads
//! the sidecar); a message's feedback is validated against the runtime
//! journal at write time, but deleting the session log leaves orphan rows in
//! the sidecar.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{rpc_error, rpc_ok, HostState};

/// The fixed category vocabulary the vendored dialog offers.
const CATEGORIES: [&str; 7] = [
    "other",
    "task-result",
    "instruction-following",
    "product-interaction",
    "service-stability",
    "resource-cost",
    "security-privacy-permission",
];

/// The vendored host's note cap (bytes of UTF-8) — upstream's web bundle
/// configures `maxNoteBytes: 8192`.
const MAX_NOTE_BYTES: usize = 8192;

/// Serializes read-modify-write cycles over the one sidecar file.
static FEEDBACK_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    sessions: BTreeMap<String, SessionFeedback>,
}

#[derive(Default, Serialize, Deserialize)]
struct SessionFeedback {
    #[serde(default)]
    messages: BTreeMap<String, MessageFeedbackItem>,
    #[serde(default)]
    records: Vec<SessionFeedbackRecord>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageFeedbackItem {
    message_id: String,
    rating: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    version: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFeedbackRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    at: i64,
}

fn store_path(host: &HostState) -> PathBuf {
    host.settings_home().join("desktop").join("feedback.json")
}

fn load_store(path: &Path) -> Result<Store> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn save_store(path: &Path, store: &Store) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(store)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn request_arg(args: &Value) -> &Value {
    args.get("request").unwrap_or(args)
}

fn rejected(error: Value) -> Value {
    rpc_ok(json!({ "ok": false, "error": error }))
}

fn accepted(value: Value) -> Value {
    rpc_ok(json!({ "ok": true, "value": value }))
}

fn session_not_found(session_id: &str) -> Value {
    rejected(json!({ "code": "session-not-found", "sessionId": session_id }))
}

fn target_not_found(session_id: &str, message_id: &str) -> Value {
    rejected(json!({
        "code": "target-not-found",
        "sessionId": session_id,
        "messageId": message_id,
    }))
}

fn version_conflict(current: Option<&MessageFeedbackItem>) -> Value {
    rejected(json!({
        "code": "version-conflict",
        "current": current.cloned().map_or(Value::Null, |item| json!(item)),
    }))
}

/// One runtime thread record, or `None` when the thread does not exist.
async fn thread_detail(host: &HostState, session_id: &str) -> Result<Option<Value>> {
    host.backend
        .get_optional(&format!("/v1/threads/{session_id}"))
        .await
}

/// A finalized assistant message is the only feedback target the vendored
/// controls address; a tool/user/status item id is refused.
fn is_assistant_message(detail: &Value, message_id: &str) -> bool {
    detail
        .get("items")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("id").and_then(Value::as_str) == Some(message_id)
                    && item.get("kind").and_then(Value::as_str) == Some("agent_message")
                    && item.get("status").and_then(Value::as_str) == Some("completed")
            })
        })
}

fn valid_category(category: Option<&str>) -> bool {
    category.is_none_or(|category| CATEGORIES.contains(&category))
}

/// `messageFeedback/list` — every current item for one persisted session.
pub async fn message_list(host: &HostState, args: &Value) -> Value {
    let request = request_arg(args);
    let Some(session_id) = request.get("sessionId").and_then(Value::as_str) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "messageFeedback/list without sessionId",
        );
    };
    match thread_detail(host, session_id).await {
        Ok(None) => return session_not_found(session_id),
        Ok(Some(_)) => {}
        Err(err) => {
            return rpc_error(
                "gateway/internal",
                format!("messageFeedback/list session check: {err:#}"),
            )
        }
    }
    let session_id = session_id.to_string();
    let path = store_path(host);
    let _guard = FEEDBACK_LOCK.lock().await;
    let loaded = tokio::task::spawn_blocking(move || {
        let store = load_store(&path)?;
        Ok::<_, anyhow::Error>(
            store
                .sessions
                .get(&session_id)
                .map(|session| session.messages.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default(),
        )
    })
    .await;
    match loaded {
        Ok(Ok(items)) => accepted(json!({ "items": items })),
        Ok(Err(err)) => rpc_error("gateway/internal", format!("messageFeedback/list: {err:#}")),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("messageFeedback/list join: {err}"),
        ),
    }
}

/// `messageFeedback/put` — create or replace with compare-and-set.
pub async fn message_put(host: &HostState, args: &Value) -> Value {
    let request = request_arg(args);
    let (Some(session_id), Some(message_id), Some(rating)) = (
        request.get("sessionId").and_then(Value::as_str),
        request.get("messageId").and_then(Value::as_str),
        request.get("rating").and_then(Value::as_str),
    ) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "messageFeedback/put without sessionId, messageId, or rating",
        );
    };
    if !matches!(rating, "positive" | "negative") {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("messageFeedback/put rating {rating:?}"),
        );
    }
    let category = request.get("category").and_then(Value::as_str);
    if !valid_category(category) {
        return rpc_error(
            "gateway/arguments-invalid",
            format!(
                "messageFeedback/put category {category:?} is not one of {}",
                CATEGORIES.join("|")
            ),
        );
    }
    let note = match request.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(note)) => {
            if note.trim().is_empty() {
                return rejected(json!({ "code": "note-blank" }));
            }
            let actual = note.len();
            if actual > MAX_NOTE_BYTES {
                return rejected(json!({
                    "code": "note-too-large",
                    "maxBytes": MAX_NOTE_BYTES,
                    "actualBytes": actual,
                }));
            }
            Some(note.clone())
        }
        Some(_) => {
            return rpc_error(
                "gateway/arguments-invalid",
                "messageFeedback/put note must be a string",
            )
        }
    };
    let category = category.map(str::to_string);
    let observed = match request.get("ifVersion") {
        Some(Value::Null) => None,
        Some(Value::String(version)) => Some(version.clone()),
        _ => {
            return rpc_error(
                "gateway/arguments-invalid",
                "messageFeedback/put without ifVersion (string or null)",
            )
        }
    };
    match thread_detail(host, session_id).await {
        Ok(None) => return session_not_found(session_id),
        Ok(Some(detail)) if !is_assistant_message(&detail, message_id) => {
            return target_not_found(session_id, message_id)
        }
        Ok(Some(_)) => {}
        Err(err) => {
            return rpc_error(
                "gateway/internal",
                format!("messageFeedback/put session check: {err:#}"),
            )
        }
    }

    let path = store_path(host);
    let session_id = session_id.to_string();
    let message_id = message_id.to_string();
    let rating = rating.to_string();
    let _guard = FEEDBACK_LOCK.lock().await;
    let written = tokio::task::spawn_blocking(move || {
        let mut store = load_store(&path)?;
        let session = store.sessions.entry(session_id.clone()).or_default();
        let current = session.messages.get(&message_id).cloned();
        if observed.as_deref() != current.as_ref().map(|item| item.version.as_str()) {
            return Ok::<Value, anyhow::Error>(version_conflict(current.as_ref()));
        }
        if let Some(existing) = current.as_ref() {
            if existing.rating == rating && existing.note == note && existing.category == category {
                return Ok::<Value, anyhow::Error>(accepted(json!(existing)));
            }
        }
        let now = now_ms();
        let item = MessageFeedbackItem {
            message_id: message_id.clone(),
            rating,
            note,
            category,
            version: uuid::Uuid::new_v4().to_string(),
            created_at: current.as_ref().map_or(now, |item| item.created_at),
            updated_at: now.max(current.as_ref().map_or(now, |item| item.updated_at)),
        };
        session.messages.insert(message_id, item.clone());
        save_store(&path, &store)?;
        Ok::<Value, anyhow::Error>(accepted(json!(item)))
    })
    .await;
    match written {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => rpc_error("gateway/internal", format!("messageFeedback/put: {err:#}")),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("messageFeedback/put join: {err}"),
        ),
    }
}

/// `messageFeedback/delete` — idempotent retraction with compare-and-set.
pub async fn message_delete(host: &HostState, args: &Value) -> Value {
    let request = request_arg(args);
    let (Some(session_id), Some(message_id), Some(if_version)) = (
        request.get("sessionId").and_then(Value::as_str),
        request.get("messageId").and_then(Value::as_str),
        request.get("ifVersion").and_then(Value::as_str),
    ) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "messageFeedback/delete without sessionId, messageId, or ifVersion",
        );
    };
    match thread_detail(host, session_id).await {
        Ok(None) => return session_not_found(session_id),
        Ok(Some(_)) => {}
        Err(err) => {
            return rpc_error(
                "gateway/internal",
                format!("messageFeedback/delete session check: {err:#}"),
            )
        }
    }
    let path = store_path(host);
    let session_id = session_id.to_string();
    let message_id = message_id.to_string();
    let if_version = if_version.to_string();
    let _guard = FEEDBACK_LOCK.lock().await;
    let written = tokio::task::spawn_blocking(move || {
        let mut store = load_store(&path)?;
        let Some(session) = store.sessions.get_mut(&session_id) else {
            return Ok::<_, anyhow::Error>(accepted(json!({ "absent": true })));
        };
        let Some(current) = session.messages.get(&message_id) else {
            return Ok(accepted(json!({ "absent": true })));
        };
        if current.version != if_version {
            return Ok(version_conflict(Some(current)));
        }
        session.messages.remove(&message_id);
        save_store(&path, &store)?;
        Ok(accepted(json!({ "absent": true })))
    })
    .await;
    match written {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => rpc_error(
            "gateway/internal",
            format!("messageFeedback/delete: {err:#}"),
        ),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("messageFeedback/delete join: {err}"),
        ),
    }
}

enum RecordFailure {
    MissingSession,
    Internal(anyhow::Error),
}

/// Append one session-level record to the sidecar. Shared by the RPC and the
/// `/feedback <text>` command.
async fn append_session_record(
    host: &HostState,
    session_id: &str,
    text: Option<String>,
    category: Option<String>,
) -> Result<(), RecordFailure> {
    match thread_detail(host, session_id).await {
        Ok(None) => return Err(RecordFailure::MissingSession),
        Ok(Some(_)) => {}
        Err(err) => return Err(RecordFailure::Internal(err)),
    }
    let path = store_path(host);
    let session_id = session_id.to_string();
    let _guard = FEEDBACK_LOCK.lock().await;
    let written = tokio::task::spawn_blocking(move || {
        let mut store = load_store(&path)?;
        store
            .sessions
            .entry(session_id)
            .or_default()
            .records
            .push(SessionFeedbackRecord {
                text,
                category,
                at: now_ms(),
            });
        save_store(&path, &store)
    })
    .await;
    match written {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(RecordFailure::Internal(err)),
        Err(err) => Err(RecordFailure::Internal(anyhow::anyhow!(
            "feedback record join: {err}"
        ))),
    }
}

/// `sessionFeedback/record` — the dialog's session-level remark.
pub async fn session_record(host: &HostState, args: &Value) -> Value {
    let request = request_arg(args);
    let Some(session_id) = request.get("sessionId").and_then(Value::as_str) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "sessionFeedback/record without sessionId",
        );
    };
    let category = request.get("category").and_then(Value::as_str);
    if !valid_category(category) {
        return rpc_error(
            "gateway/arguments-invalid",
            format!(
                "sessionFeedback/record category {category:?} is not one of {}",
                CATEGORIES.join("|")
            ),
        );
    }
    let text = match request.get("text") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Some(_) => {
            return rpc_error(
                "gateway/arguments-invalid",
                "sessionFeedback/record text must be a string",
            )
        }
    };
    match append_session_record(host, session_id, text, category.map(str::to_string)).await {
        Ok(()) => accepted(json!({ "recorded": true })),
        Err(RecordFailure::MissingSession) => session_not_found(session_id),
        Err(RecordFailure::Internal(err)) => rpc_error(
            "gateway/internal",
            format!("sessionFeedback/record: {err:#}"),
        ),
    }
}

/// Record a `/feedback <text>` remark from the command catalog. The session
/// is already known live by the time the command executes; a vanished
/// session still settles as the command's error text.
pub async fn record_text(host: &HostState, session_id: &str, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Feedback text is required. Usage: /feedback <text>".into());
    }
    match append_session_record(host, session_id, Some(text.to_string()), None).await {
        Ok(()) => Ok(()),
        Err(RecordFailure::MissingSession) => Err(format!("session {session_id} not found")),
        Err(RecordFailure::Internal(err)) => Err(format!("{err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cds-feedback-{tag}-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn store_round_trips_and_a_missing_file_is_empty() {
        let path = temp_path("roundtrip");
        assert!(load_store(&path).unwrap().sessions.is_empty());
        let mut store = Store::default();
        store.sessions.entry("thr_1".into()).or_default();
        save_store(&path, &store).unwrap();
        assert!(load_store(&path).unwrap().sessions.contains_key("thr_1"));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_store(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn category_and_note_vocabularies_match_the_client() {
        assert!(valid_category(None));
        for category in CATEGORIES {
            assert!(valid_category(Some(category)));
        }
        assert!(!valid_category(Some("bogus")));
        assert_eq!(MAX_NOTE_BYTES, 8192);
    }
}
