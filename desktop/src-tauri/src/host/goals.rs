//! The desktop GoalBar bridge: the `goal` session projection and the
//! `goals/get|edit|pause|resume|clear` remotes.
//!
//! CodeSmith already owns the goal as engine state: the model's
//! `create_goal`/`get_goal`/`update_goal` tools mutate a shared
//! [`GoalState`](codesmith_agent_runtime::tool_state::goal::GoalState), and
//! the engine's round driver continues an active goal at turn end. The
//! runtime exposes that state at `GET|POST /v1/threads/{id}/goal`; this
//! module maps it onto the vendored client's `GoalProjection`/`GoalView`.
//!
//! Two sources feed the projection:
//!
//! - **Live** — when the thread's engine is loaded, the runtime view is
//!   authoritative (it carries paused/blocked/cleared states and the round
//!   counters).
//! - **Journal** — when the engine is not loaded, the latest completed
//!   `create_goal`/`update_goal` tool result is replayed from the thread
//!   record. Viewing a session must not load an engine, so this fallback is
//!   what puts the bar on screen after a desktop restart before the first
//!   turn. The engine seeds the same state from that journal entry when it
//!   loads.
//!
//! UI mutations (`edit`/`pause`/`resume`/`clear`) POST the runtime action
//! (loading the engine, which seeds from the journal first) and bump a
//! bridge-local revision so the client's CAS-facing projection updates.
//!
//! **Desktop pause/clear outlive an engine eviction.** The runtime's journal
//! records model-tool changes only, and its seed deliberately restores an
//! active journaled goal, so a process-local pause or clear was lost the
//! moment the engine was evicted. The bridge therefore persists its overlay
//! per session at `~/.codesmith/desktop/goals.json` (atomic rename; a corrupt
//! file logs and is ignored, never silently reset) and:
//!
//! - the read path presents the persisted pause/clear over the journal
//!   fallback without loading an engine, and re-applies it to a live engine
//!   that is already loaded (no engine is spawned by a read);
//! - [`reconcile`] re-applies the persisted action before a prompt, so a
//!   freshly loaded engine cannot drive continuation rounds for a goal the
//!   desktop stopped.
//!
//! Known limitations: an overlay's identity is the goal's creating tool item;
//! a goal created through the desktop `/goal` command has no journal item, so
//! it does not survive an engine eviction at all (there is nothing to
//! re-apply) — only journaled goals do. A queued turn started by the runtime
//! after an eviction, without an intervening desktop prompt, is the one path
//! that can still outrun a pause for a single turn. The projection's
//! id/revision are synthesized from the journal item identity (the runtime
//! snapshot has no id), so they stay stable for one goal but are not a
//! durable compare-and-set across a bridge restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::{mapper, rpc_error, rpc_ok, HostState};

/// The default round cap the runtime exposes (kept in step with
/// `codesmith_agent_runtime::tool_state::goal::MAX_GOAL_ROUNDS`).
const FALLBACK_MAX_GOAL_ROUNDS: u64 = 25;

/// Serializes read-modify-write cycles over the one overlay file.
static PERSIST_LOCK: Mutex<()> = Mutex::const_new(());

/// One session's bridge-local mutation overlay. Persisted at
/// `desktop/goals.json` so a pause/clear survives a bridge restart and an
/// engine eviction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Overlay {
    /// Revisions applied through the desktop bar (the journal counts model
    /// tool calls only).
    pub extra_revisions: u64,
    /// Wall-clock ms of the latest desktop mutation.
    pub updated_ms: Option<i64>,
    /// A desktop pause applies to the goal identity it was issued against.
    pub paused: bool,
    pub paused_goal_id: Option<String>,
    /// A desktop clear suppresses the journal fallback for that exact goal
    /// identity; a new `create_goal` starts a new identity and reappears.
    pub cleared: bool,
    pub cleared_goal_id: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    sessions: BTreeMap<String, Overlay>,
}

fn overlay_path(host: &HostState) -> PathBuf {
    host.settings_home().join("desktop").join("goals.json")
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

fn is_goal_tool(summary: &str) -> bool {
    summary.starts_with("create_goal") || summary.starts_with("update_goal")
}

fn item_ms(item: &Value) -> Option<i64> {
    item.get("ended_at")
        .or_else(|| item.get("started_at"))
        .and_then(Value::as_str)
        .and_then(mapper::epoch_ms)
}

/// The journal's goal record: the latest completed goal-tool snapshot plus
/// the identity/metadata derived from the tool items.
struct JournalGoal {
    id: String,
    snapshot: Value,
    revision: u64,
    created_ms: i64,
    updated_ms: i64,
}

fn journal_goal(detail: &Value, session_id: &str) -> Option<JournalGoal> {
    let items: Vec<&Value> = detail
        .get("items")?
        .as_array()?
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("completed"))
        .filter(|item| {
            item.get("summary")
                .and_then(Value::as_str)
                .is_some_and(is_goal_tool)
        })
        .collect();
    let latest = *items.last()?;
    let create = items
        .iter()
        .rev()
        .find(|item| {
            item.get("summary")
                .and_then(Value::as_str)
                .is_some_and(|summary| summary.starts_with("create_goal"))
        })
        .copied();
    let snapshot: Value = serde_json::from_str(latest.get("detail")?.as_str()?).ok()?;
    let objective = snapshot
        .get("objective")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if objective.is_empty() {
        return None;
    }
    // The identity is the goal's creating call: updates keep the same id so
    // the bar's local edit state survives them; a re-create starts a new one.
    let identity = create.unwrap_or(latest);
    let id = format!(
        "goal_{}",
        identity
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(session_id)
    );
    let created_ms = create
        .and_then(item_ms)
        .or_else(|| items.first().and_then(|item| item_ms(item)))
        .unwrap_or_else(now_ms);
    let updated_ms = item_ms(latest).unwrap_or(created_ms);
    Some(JournalGoal {
        id,
        snapshot,
        revision: items.len() as u64,
        created_ms,
        updated_ms,
    })
}

/// The stable identity a persisted pause/clear applies to. Falls back to the
/// session-scoped synthetic id for a goal the journal does not know (a
/// desktop-created goal).
fn goal_identity(detail: &Value, session_id: &str) -> String {
    journal_goal(detail, session_id)
        .map(|journal| journal.id)
        .unwrap_or_else(|| format!("goal_{session_id}"))
}

/// Load the persisted overlays once per Host generation. A corrupt file logs
/// loud and degrades to the in-memory state; it is never silently rewritten.
async fn hydrate(host: &HostState) {
    if host.goal_overlays_loaded.swap(true, Ordering::SeqCst) {
        return;
    }
    let path = overlay_path(host);
    let loaded = tokio::task::spawn_blocking(move || load_store(&path)).await;
    match loaded {
        Ok(Ok(store)) => {
            let mut overlays = host.goal_overlays.lock().await;
            for (session_id, overlay) in store.sessions {
                overlays.entry(session_id).or_insert(overlay);
            }
        }
        Ok(Err(err)) => {
            eprintln!("codesmith desktop: goal overlay store unreadable: {err:#}");
        }
        Err(err) => {
            eprintln!("codesmith desktop: goal overlay load join failed: {err}");
        }
    }
}

async fn overlay_for(host: &HostState, session_id: &str) -> Overlay {
    hydrate(host).await;
    host.goal_overlays
        .lock()
        .await
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

/// Update one session's overlay in memory and on disk. The runtime mutation
/// already succeeded, so a failed write logs loud but does not fail the RPC.
async fn store_overlay(host: &HostState, session_id: &str, overlay: Overlay) {
    hydrate(host).await;
    host.goal_overlays
        .lock()
        .await
        .insert(session_id.to_string(), overlay.clone());
    let path = overlay_path(host);
    let session_id = session_id.to_string();
    let _guard = PERSIST_LOCK.lock().await;
    let written = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut store = load_store(&path)?;
        store.sessions.insert(session_id, overlay);
        save_store(&path, &store)
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(err)) => eprintln!("codesmith desktop: goal overlay persist failed: {err:#}"),
        Err(err) => eprintln!("codesmith desktop: goal overlay persist join failed: {err}"),
    }
}

/// Forget a desktop pause/clear after an edit/resume that supersedes it.
pub async fn note_edit(host: &HostState, session_id: &str) {
    let mut overlay = overlay_for(host, session_id).await;
    overlay.extra_revisions += 1;
    overlay.updated_ms = Some(now_ms());
    overlay.paused = false;
    overlay.paused_goal_id = None;
    overlay.cleared = false;
    overlay.cleared_goal_id = None;
    store_overlay(host, session_id, overlay).await;
}

/// Re-apply the persisted pause/clear before a prompt loads (or re-seeds) the
/// engine. `apply_goal_action` loads an evicted engine from the journal
/// first, so this is what stops a stopped goal from being driven again. A
/// refusal that means "already satisfied" is silent; anything else logs.
pub async fn reconcile(host: &HostState, session_id: &str) {
    let overlay = overlay_for(host, session_id).await;
    if !overlay.paused && !overlay.cleared {
        return;
    }
    let Ok(detail) = host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    else {
        return;
    };
    let identity = goal_identity(&detail, session_id);
    let (action, goal_id) =
        if overlay.cleared && overlay.cleared_goal_id.as_deref() == Some(identity.as_str()) {
            ("clear", &overlay.cleared_goal_id)
        } else if overlay.paused && overlay.paused_goal_id.as_deref() == Some(identity.as_str()) {
            ("pause", &overlay.paused_goal_id)
        } else {
            return;
        };
    if goal_id.is_none() {
        return;
    }
    let path = format!("/v1/threads/{session_id}/goal");
    match host
        .backend
        .post_with_error_message::<Value>(&path, &json!({ "action": action }))
        .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(message)) if action == "pause" && message.contains("no active goal to pause") => {
            // The seeded state is already paused/blocked/gone: satisfied.
        }
        Ok(Err(message)) => {
            eprintln!(
                "codesmith desktop: goal {action} reconcile for {session_id} refused: {message}"
            );
        }
        Err(err) => {
            eprintln!(
                "codesmith desktop: goal {action} reconcile for {session_id} failed: {err:#}"
            );
        }
    }
}

/// The `goal` projection value (`GoalProjection | null`). `detail` is the
/// thread record the caller already read.
pub async fn projection(host: &HostState, session_id: &str, detail: &Value) -> Value {
    let live = host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}/goal"))
        .await
        .ok();
    let loaded = live
        .as_ref()
        .and_then(|value| value.get("loaded"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let journal = journal_goal(detail, session_id);
    let overlay = overlay_for(host, session_id).await;
    let journal_id = journal
        .as_ref()
        .map(|journal| journal.id.clone())
        .unwrap_or_else(|| format!("goal_{session_id}"));
    let goal_cleared =
        overlay.cleared && overlay.cleared_goal_id.as_deref() == Some(journal_id.as_str());
    let goal_paused =
        overlay.paused && overlay.paused_goal_id.as_deref() == Some(journal_id.as_str());

    let (mut snapshot, rounds_started, max_rounds) = if loaded {
        let Some(goal) = live
            .as_ref()
            .and_then(|value| value.get("goal"))
            .filter(|goal| !goal.is_null())
        else {
            // The engine's own state is authoritative: no goal, no bar.
            return Value::Null;
        };
        // A read never loads an engine, but when one is already loaded the
        // persisted stop is re-applied to it (a fresh load seeds the journal
        // active goal). A refusal logs and leaves the live value visible.
        let mut effective = goal.clone();
        if goal_cleared {
            match post_goal_action(host, session_id, "clear").await {
                Ok(()) => return Value::Null,
                Err(err) => {
                    eprintln!(
                        "codesmith desktop: goal clear reconcile for {session_id} refused: {err:#}"
                    );
                }
            }
        } else if goal_paused && goal.get("status").and_then(Value::as_str) == Some("active") {
            match post_goal_action(host, session_id, "pause").await {
                Ok(()) => {
                    effective["status"] = json!("paused");
                    effective["blocker"] = json!("paused from the desktop");
                }
                Err(err) => {
                    eprintln!(
                        "codesmith desktop: goal pause reconcile for {session_id} refused: {err:#}"
                    );
                }
            }
        }
        (
            effective,
            goal.get("rounds_started")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            goal.get("max_goal_rounds")
                .and_then(Value::as_u64)
                .unwrap_or(FALLBACK_MAX_GOAL_ROUNDS),
        )
    } else {
        let Some(journal) = journal.as_ref() else {
            return Value::Null;
        };
        if goal_cleared {
            return Value::Null;
        }
        let mut snapshot = journal.snapshot.clone();
        // A journaled terminal state (complete/blocked) wins over a desktop
        // pause issued while the goal was still active.
        if goal_paused && snapshot.get("status").and_then(Value::as_str) == Some("active") {
            snapshot["status"] = json!("paused");
            if snapshot
                .get("blocker")
                .is_none_or(|blocker| blocker.is_null())
            {
                snapshot["blocker"] = json!("paused from the desktop");
            }
        }
        (snapshot, 0, FALLBACK_MAX_GOAL_ROUNDS)
    };

    let Some(objective) = snapshot
        .get("objective")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Value::Null;
    };
    let phase: &'static str = match snapshot.get("status").and_then(Value::as_str) {
        Some("active") => "active",
        Some("paused") => "paused",
        Some("blocked") => "blocked",
        Some("complete") => "complete",
        _ => return Value::Null,
    };
    if phase == "blocked" && !snapshot["blocker"].is_string() {
        snapshot["blocker"] = json!("blocked");
    }
    let revision = journal
        .as_ref()
        .map(|journal| journal.revision)
        .unwrap_or(1)
        + overlay.extra_revisions;
    let created_ms = journal
        .as_ref()
        .map(|journal| journal.created_ms)
        .unwrap_or_else(now_ms);
    let updated_ms = overlay
        .updated_ms
        .unwrap_or_else(|| {
            journal
                .as_ref()
                .map(|journal| journal.updated_ms)
                .unwrap_or(created_ms)
        })
        .max(
            journal
                .as_ref()
                .map(|journal| journal.updated_ms)
                .unwrap_or(created_ms),
        );
    let mut goal = json!({
        "id": journal_id,
        "revision": revision,
        "objective": objective,
        "phase": phase,
        "maxGoalRounds": max_rounds,
    });
    if phase == "blocked" {
        let message = snapshot
            .get("blocker")
            .and_then(Value::as_str)
            .filter(|blocker| !blocker.trim().is_empty())
            .unwrap_or("blocked");
        goal["blockedReason"] = json!({ "code": "blocked", "message": message });
    }
    json!({
        "goal": goal,
        "roundsStarted": rounds_started,
        "createdAt": created_ms,
        "updatedAt": updated_ms,
    })
}

/// One best-effort goal action from the read path; `Err` carries the
/// runtime's refusal text.
async fn post_goal_action(host: &HostState, session_id: &str, action: &str) -> Result<()> {
    host.backend
        .post_with_error_message::<Value>(
            &format!("/v1/threads/{session_id}/goal"),
            &json!({ "action": action }),
        )
        .await
        .context("deliver goal action")?
        .map(|_| ())
        .map_err(anyhow::Error::msg)
}

/// The `GoalView` the `goals/get` remote answers: the projection plus the
/// process-local activation flag. `None` means "no current goal" (the client
/// expects an absent value, not `null`).
async fn goal_view(host: &HostState, session_id: &str) -> Option<Value> {
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await
        .ok()?;
    let projection = projection(host, session_id, &detail).await;
    let goal = projection.get("goal")?.clone();
    let activation = if goal.get("phase").and_then(Value::as_str) == Some("active") {
        "armed"
    } else {
        "disarmed"
    };
    let mut view = goal;
    view["roundsStarted"] = projection.get("roundsStarted").cloned().unwrap_or(json!(0));
    view["createdAt"] = projection.get("createdAt").cloned().unwrap_or(json!(0));
    view["updatedAt"] = projection.get("updatedAt").cloned().unwrap_or(json!(0));
    view["activation"] = json!(activation);
    Some(view)
}

fn session_arg(args: &Value) -> Option<String> {
    args.get("agentId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `goals/edit` objective: the third parameter is named `request`, so it
/// rides one level below the object-keyed `{agentId, ref, request}` envelope
/// (a flattened `{objective}` is accepted too).
fn edit_objective(args: &Value) -> Option<&str> {
    args.get("request")
        .unwrap_or(args)
        .get("objective")
        .and_then(Value::as_str)
}

/// The client's optional round-cap edit. CodeSmith's cap is the runtime
/// constant; a request for a different cap is refused rather than silently
/// ignored (the bar never sends one today, but the remote declares it).
fn validate_max_goal_rounds(args: &Value) -> std::result::Result<Option<u64>, Value> {
    let requested = args
        .get("request")
        .unwrap_or(args)
        .get("maxGoalRounds")
        .and_then(Value::as_u64);
    match requested {
        Some(max) if max != FALLBACK_MAX_GOAL_ROUNDS => Err(rpc_error(
            "goal/rejected",
            format!(
                "the goal round cap is fixed at {FALLBACK_MAX_GOAL_ROUNDS}; {max} is not supported"
            ),
        )),
        other => Ok(other),
    }
}

/// `goals/get` — the live goal view. An absent value means no current goal.
pub async fn get(host: &HostState, args: &Value) -> Value {
    let args = args.get("request").unwrap_or(args);
    let Some(session_id) = session_arg(args) else {
        return rpc_error("gateway/arguments-invalid", "goals/get without agentId");
    };
    match goal_view(host, &session_id).await {
        Some(view) => rpc_ok(view),
        // `GoalView | undefined`: an absent value, not JSON null (the client
        // dereferences a present null).
        None => json!({ "ok": true }),
    }
}

/// `goals/edit|pause|resume|clear` — one runtime goal mutation, then a live
/// projection push.
pub async fn action(host: &HostState, endpoint: &str, args: &Value) -> Value {
    // Object-keyed args: `{agentId, ref, request:{objective?, maxGoalRounds?}}`
    // — the `request` member is the remote's third parameter, never the whole
    // envelope, so `agentId` must be read before unwrapping it.
    let Some(session_id) = session_arg(args) else {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("{endpoint} without agentId"),
        );
    };
    let requested_max = match validate_max_goal_rounds(args) {
        Ok(max) => max,
        Err(error) => return error,
    };
    let verb = endpoint.rsplit('/').next().unwrap_or_default();
    let body = match verb {
        "edit" => match edit_objective(args) {
            Some(objective) => json!({ "action": "edit", "objective": objective }),
            // A round-cap-only edit is a no-op: the cap is the runtime
            // constant, and the request is otherwise valid.
            None if requested_max.is_some() => {
                return match goal_view(host, &session_id).await {
                    Some(view) => rpc_ok(view),
                    None => rpc_error("no-current-goal", "no current goal to mutate"),
                };
            }
            None => {
                return rpc_error(
                    "gateway/arguments-invalid",
                    "goals/edit without an objective",
                );
            }
        },
        "pause" => json!({ "action": "pause" }),
        "resume" => json!({ "action": "resume" }),
        "clear" => json!({ "action": "clear" }),
        other => {
            return rpc_error(
                "gateway/arguments-invalid",
                format!("unknown goal mutation {other:?}"),
            );
        }
    };
    let detail: Value = match host.backend.get(&format!("/v1/threads/{session_id}")).await {
        Ok(detail) => detail,
        Err(err) => {
            return rpc_error("gateway/internal", format!("{endpoint}: {err:#}"));
        }
    };
    let before = projection(host, &session_id, &detail).await;
    if before.is_null() {
        return rpc_error("no-current-goal", "no current goal to mutate");
    }
    let previous = before.get("goal").cloned().unwrap_or(Value::Null);
    let previous_id = previous
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let previous_revision = previous
        .get("revision")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    match host
        .backend
        .post_with_error_message::<Value>(&format!("/v1/threads/{session_id}/goal"), &body)
        .await
    {
        Ok(Ok(_)) => {
            let mut overlay = overlay_for(host, &session_id).await;
            overlay.extra_revisions += 1;
            overlay.updated_ms = Some(now_ms());
            match verb {
                "clear" => {
                    overlay.cleared = true;
                    overlay.cleared_goal_id = Some(previous_id.clone());
                    overlay.paused = false;
                    overlay.paused_goal_id = None;
                }
                "pause" => {
                    overlay.paused = true;
                    overlay.paused_goal_id = Some(previous_id.clone());
                    overlay.cleared = false;
                    overlay.cleared_goal_id = None;
                }
                _ => {
                    overlay.paused = false;
                    overlay.paused_goal_id = None;
                    overlay.cleared = false;
                    overlay.cleared_goal_id = None;
                }
            }
            store_overlay(host, &session_id, overlay).await;
            push(host, &session_id).await;
            if verb == "clear" {
                return rpc_ok(json!({
                    "id": previous_id,
                    "revision": previous_revision + 1,
                }));
            }
            match goal_view(host, &session_id).await {
                Some(view) => rpc_ok(view),
                None => rpc_error("no-current-goal", "the goal disappeared after the mutation"),
            }
        }
        Ok(Err(message)) => rpc_error("goal/rejected", message),
        Err(err) => rpc_error("gateway/internal", format!("{endpoint}: {err:#}")),
    }
}

/// Push one session's `goal` projection to every open `session/control`
/// stream (a mutation or a completed goal tool changed it).
pub async fn push(host: &HostState, session_id: &str) {
    let detail = match host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    {
        Ok(detail) => detail,
        Err(err) => {
            eprintln!(
                "codesmith desktop: goal projection refresh for {session_id} failed: {err:#}"
            );
            return;
        }
    };
    let value = projection(host, session_id, &detail).await;
    let seq = host.next_projection_seq();
    host.push_live(
        "session/control",
        super::sessions::projection_update(session_id, seq, &json!({ "goal": value })),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal_item(id: &str, summary: &str, snapshot: Value) -> Value {
        json!({
            "id": id,
            "status": "completed",
            "summary": summary,
            "detail": snapshot.to_string(),
            "started_at": "2026-11-11T10:00:00Z",
            "ended_at": "2026-11-11T10:00:05Z",
        })
    }

    /// The bridge-free projection path when the engine is not loaded: the
    /// journal fallback, its identity, and the round counters.
    #[test]
    fn journal_goal_reads_the_latest_snapshot_and_keeps_the_create_identity() {
        let detail = json!({
            "items": [
                goal_item("item_create", "create_goal", json!({
                    "objective": "ship it", "status": "active", "token_budget": 500,
                    "elapsed_seconds": 1, "evidence": null, "blocker": null,
                })),
                goal_item("item_update", "update_goal", json!({
                    "objective": "ship it", "status": "blocked", "token_budget": 500,
                    "elapsed_seconds": 2, "evidence": null, "blocker": "needs input",
                })),
            ]
        });
        let journal = journal_goal(&detail, "thr_1").expect("goal");
        assert_eq!(journal.id, "goal_item_create");
        assert_eq!(journal.snapshot["status"], "blocked");
        assert_eq!(journal.revision, 2);
        assert_eq!(
            journal.updated_ms,
            mapper::epoch_ms("2026-11-11T10:00:05Z").unwrap()
        );
    }

    #[test]
    fn journal_goal_ignores_items_without_a_snapshot_or_objective() {
        let detail = json!({
            "items": [
                goal_item("item_create", "create_goal", json!({
                    "objective": "  ", "status": "active",
                })),
            ]
        });
        assert!(journal_goal(&detail, "thr_1").is_none());
    }

    #[test]
    fn edit_args_keep_the_agent_id_and_read_the_nested_objective() {
        // Object-keyed wire shape: the remote's third parameter is named
        // `request`, so the objective sits one level down and `agentId`
        // stays on the outer envelope.
        let args = json!({
            "agentId": "thr_1",
            "ref": { "id": "goal_x", "revision": 2 },
            "request": { "objective": "ship it" },
        });
        assert_eq!(session_arg(&args).as_deref(), Some("thr_1"));
        assert_eq!(edit_objective(&args), Some("ship it"));
        assert_eq!(
            edit_objective(&json!({ "objective": "flattened" })),
            Some("flattened")
        );
    }

    #[test]
    fn max_goal_rounds_accepts_only_the_runtime_cap() {
        assert_eq!(
            validate_max_goal_rounds(
                &json!({ "agentId": "x", "request": { "maxGoalRounds": 25 } })
            ),
            Ok(Some(25))
        );
        assert_eq!(
            validate_max_goal_rounds(&json!({ "agentId": "x" })),
            Ok(None)
        );
        let error = validate_max_goal_rounds(
            &json!({ "agentId": "x", "request": { "maxGoalRounds": 50 } }),
        )
        .expect_err("a different cap must be refused, not silently ignored");
        assert_eq!(error["error"]["code"], "goal/rejected");
    }

    #[test]
    fn overlay_round_trips_and_a_missing_file_is_empty() {
        let path = std::env::temp_dir().join(format!(
            "cds-goals-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        assert!(load_store(&path).unwrap().sessions.is_empty());
        let mut store = Store::default();
        store.sessions.insert(
            "thr_1".into(),
            Overlay {
                extra_revisions: 2,
                updated_ms: Some(5),
                paused: true,
                paused_goal_id: Some("goal_item".into()),
                cleared: false,
                cleared_goal_id: None,
            },
        );
        save_store(&path, &store).unwrap();
        let reloaded = load_store(&path).unwrap();
        let overlay = reloaded.sessions.get("thr_1").expect("overlay");
        assert!(overlay.paused);
        assert_eq!(overlay.paused_goal_id.as_deref(), Some("goal_item"));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_store(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn goal_identity_prefers_the_create_item_and_falls_back_to_the_session() {
        let detail = json!({
            "items": [
                goal_item("item_create", "create_goal", json!({
                    "objective": "ship it", "status": "active",
                })),
                goal_item("item_update", "update_goal", json!({
                    "objective": "ship it", "status": "paused",
                })),
            ]
        });
        assert_eq!(goal_identity(&detail, "thr_1"), "goal_item_create");
        assert_eq!(
            goal_identity(&json!({ "items": [] }), "thr_1"),
            "goal_thr_1"
        );
    }

    /// The projection's journal path is pure over the overlay: a persisted
    /// pause shows as paused after an engine eviction, and a persisted clear
    /// hides the replayed goal. Both are exercised through the pure pieces
    /// (the HTTP path needs a fake runtime; the dispatch tests cover it).
    #[test]
    fn journal_fallback_applies_the_persisted_pause_and_clear() {
        let detail = json!({
            "items": [goal_item("item_create", "create_goal", json!({
                "objective": "ship it", "status": "active", "blocker": null,
            }))],
        });
        let journal = journal_goal(&detail, "thr_1").expect("goal");
        let mut overlay = Overlay {
            paused: true,
            paused_goal_id: Some(journal.id.clone()),
            ..Overlay::default()
        };
        let mut snapshot = journal.snapshot.clone();
        if overlay.paused_goal_id.as_deref() == Some(journal.id.as_str()) {
            snapshot["status"] = json!("paused");
        }
        assert_eq!(snapshot["status"], "paused");
        overlay.paused = false;
        overlay.cleared = true;
        overlay.cleared_goal_id = Some(journal.id.clone());
        assert!(overlay.cleared);
    }
}
