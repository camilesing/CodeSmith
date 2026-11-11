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
//! Known limitations: pause/resume/edit/clear are process-local (the journal
//! records model-tool changes only); an engine eviction restores the latest
//! journaled active/completed goal, not a desktop pause or clear. The
//! projection's id/revision are synthesized from the journal item identity
//! (the runtime snapshot has no id), so they stay stable for one goal but
//! are not a durable compare-and-set across a bridge restart.

use serde_json::{json, Value};

use super::{mapper, rpc_error, rpc_ok, HostState};

/// The default round cap the runtime exposes (kept in step with
/// `codesmith_agent_runtime::tool_state::goal::MAX_GOAL_ROUNDS`).
const FALLBACK_MAX_GOAL_ROUNDS: u64 = 25;

/// One session's bridge-local mutation overlay.
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    /// Revisions applied through the desktop bar (the journal counts model
    /// tool calls only).
    pub extra_revisions: u64,
    /// Wall-clock ms of the latest desktop mutation.
    pub updated_ms: Option<i64>,
    /// A desktop clear suppresses the journal fallback for that exact goal
    /// identity; a new `create_goal` starts a new identity and reappears.
    pub cleared: bool,
    pub cleared_goal_id: Option<String>,
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
    let overlay = host
        .goal_overlays
        .lock()
        .await
        .get(session_id)
        .cloned()
        .unwrap_or_default();
    let journal_id = journal
        .as_ref()
        .map(|journal| journal.id.clone())
        .unwrap_or_else(|| format!("goal_{session_id}"));

    let (snapshot, rounds_started, max_rounds) = if loaded {
        let Some(goal) = live
            .as_ref()
            .and_then(|value| value.get("goal"))
            .filter(|goal| !goal.is_null())
        else {
            // The engine's own state is authoritative: no goal, no bar.
            return Value::Null;
        };
        (
            goal.clone(),
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
        if overlay.cleared && overlay.cleared_goal_id.as_deref() == Some(journal_id.as_str()) {
            return Value::Null;
        }
        (journal.snapshot.clone(), 0, FALLBACK_MAX_GOAL_ROUNDS)
    };

    let Some(objective) = snapshot.get("objective").and_then(Value::as_str) else {
        return Value::Null;
    };
    let phase = match snapshot.get("status").and_then(Value::as_str) {
        Some("active") => "active",
        Some("paused") => "paused",
        Some("blocked") => "blocked",
        Some("complete") => "complete",
        _ => return Value::Null,
    };
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
    // Object-keyed args: `{agentId, ref, request:{objective?}}` — the
    // `request` member is the remote's third parameter, never the whole
    // envelope, so `agentId` must be read before unwrapping it.
    let Some(session_id) = session_arg(args) else {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("{endpoint} without agentId"),
        );
    };
    let verb = endpoint.rsplit('/').next().unwrap_or_default();
    let body = match verb {
        "edit" => {
            let Some(objective) = edit_objective(args) else {
                return rpc_error(
                    "gateway/arguments-invalid",
                    "goals/edit without an objective",
                );
            };
            json!({ "action": "edit", "objective": objective })
        }
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
            {
                let mut overlays = host.goal_overlays.lock().await;
                let overlay = overlays.entry(session_id.clone()).or_default();
                overlay.extra_revisions += 1;
                overlay.updated_ms = Some(now_ms());
                if verb == "clear" {
                    overlay.cleared = true;
                    overlay.cleared_goal_id = Some(previous_id.clone());
                } else {
                    overlay.cleared = false;
                    overlay.cleared_goal_id = None;
                }
            }
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
}
