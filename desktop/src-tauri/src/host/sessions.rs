//! `session/follow`, `session/page`, `session/prompt`, `session/cancel`,
//! `session/updateQueue`, `session/rename|fork|search`, archive toggles, the
//! skills catalog and the control projections — translated onto
//! `codesmith serve --http`.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;

use super::mapper::{replay_events, SessionMapper};
use super::permissions;
use super::workspace::OpError;
use super::{HostState, SharedHost};

/// Baseline (turn, step) counters a live tail resumes from, derived from the
/// replayed journal: the replayed turn count, and the replayed step watermark
/// of the last turn (agent items each took one step).
fn resume_counters(detail: &Value) -> (u64, u64) {
    let turns = detail.get("turns").and_then(Value::as_array);
    let turn_no = turns.map_or(0, |turns| turns.len() as u64);
    let last_turn = turns.and_then(|turns| turns.last());
    let last_turn_id = last_turn
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let step = detail
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("turn_id").and_then(Value::as_str) == Some(last_turn_id.as_str())
                        && matches!(
                            item.get("kind").and_then(Value::as_str),
                            Some("agent_message") | Some("agent_reasoning")
                        )
                })
                .count() as u64
        })
        .unwrap_or(0);
    (turn_no, step)
}

fn address_session_id(request: &Value) -> Result<String> {
    let address = request
        .get("address")
        .or_else(|| request.get("request").and_then(|r| r.get("address")))
        .cloned()
        .unwrap_or(Value::Null);
    let id = address
        .get("sessionId")
        .or_else(|| address.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    id.with_context(|| "session address without sessionId".to_string())
}

/// The turn-lifecycle events that flip a client session's `running` flag.
/// `turn.completed` covers every terminal status (a normal stop, an
/// interrupt, a cancel) — the runtime emits no separate end event for those.
fn session_running_flip(event: &str) -> Option<bool> {
    match event {
        "turn.started" => Some(true),
        "turn.completed" | "turn.failed" => Some(false),
        _ => None,
    }
}

/// One `api-session/status` emit frame for the gateway's `$events` stream
/// (stream-protocol `{type:'emit', event, args}`; the client's session
/// controller listens with `(sessionId, running)`).
fn session_status_frame(session_id: &str, running: bool) -> Value {
    json!({
        "type": "emit",
        "event": "api-session/status",
        "args": [session_id, running],
    })
}

/// Repair a thread left in the inconsistent legacy pairing: `yolo` /
/// `coordinator` mode with `auto_approve` off. The bridge used to PATCH only
/// the mode when the permission default was full access, and the runtime's
/// prompt assembly asserts the pairing — prompting such a thread panics in
/// debug builds and describes approval behavior the runtime does not follow.
/// The repair runs when the session is first followed or prompted; a failed
/// PATCH is logged and the turn surfaces its own error.
async fn repair_auto_approve_pairing(host: &HostState, session_id: &str, detail: &Value) {
    let mode = detail
        .pointer("/thread/mode")
        .and_then(Value::as_str)
        .unwrap_or("agent");
    if !matches!(mode, "yolo" | "coordinator") {
        return;
    }
    if detail
        .pointer("/thread/auto_approve")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return;
    }
    match set_mode(host, session_id, mode).await {
        Ok(Ok(())) => {}
        Ok(Err((_status, message))) => {
            eprintln!("codesmith desktop: auto-approve repair refused for {session_id}: {message}")
        }
        Err(err) => {
            eprintln!("codesmith desktop: auto-approve repair failed for {session_id}: {err:#}")
        }
    }
}

/// [`repair_auto_approve_pairing`] for a session a prompt is about to drive:
/// a client may load history without opening a follow stream, so the prompt
/// path must not assume the follow-time repair already ran.
async fn repair_before_prompt(host: &HostState, session_id: &str) {
    let Ok(detail) = host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    else {
        return;
    };
    repair_auto_approve_pairing(host, session_id, &detail).await;
}

/// Snapshot + live frames for `session/follow`. Yields the snapshot first;
/// live envelopes arrive as `{type:'event'}` frames on the returned channel.
pub async fn follow(
    host: &SharedHost,
    request: &Value,
    tx: &tokio::sync::mpsc::Sender<Value>,
) -> Result<()> {
    let session_id = address_session_id(request)?;
    // Drawn before the read, so these values are at least as fresh as the seq.
    let projection_seq = host.next_projection_seq();
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    repair_auto_approve_pairing(host, &session_id, &detail).await;
    // The seat's model chip reads the session's modelSelection projection; a
    // thread without an explicit model resolves to the catalog default so a
    // fresh session never waits on a projection nobody will push.
    let models = runtime_models(host).await;
    // Settled assistant messages carry the seat's (provider, model) as their
    // source — the usage fold derives its routes from it. The user's own
    // pick wins (see `model_selection`).
    let thread_model = detail
        .pointer("/thread/model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty());
    let seat = model_selection(
        thread_model,
        host.model_choices.lock().await.get(&session_id),
        &models,
    );
    let events = match seat.as_ref() {
        Some((provider, model)) => super::mapper::replay_events_sourced(&detail, provider, model),
        None => replay_events(&detail),
    };
    // 0-based journal: an empty one opens at cursor -1 (mapper::WireSeq).
    let last_seq = events.last().and_then(|e| e["seq"].as_i64()).unwrap_or(-1);
    let title = detail
        .pointer("/thread/title")
        .and_then(Value::as_str)
        .unwrap_or("New Session");
    // SessionHeader's `cwd` is how a client knows the workspace to browse;
    // the desktop presents the thread's own workspace (which its
    // single-workspace list rows also carry).
    let cwd = detail
        .pointer("/thread/workspace")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty());
    let values = projection_values(host, &session_id, &detail, &models).await;
    // The live poll compares against the snapshot's own value, so the first
    // tick after a quiet follow never re-pushes an unchanged board.
    let mut last_team = values.get("agentTeam").cloned();
    let mut header = json!({ "sessionId": session_id, "title": title });
    if let Some(cwd) = cwd {
        header["cwd"] = json!(cwd);
    }
    let snapshot = json!({
        "type": "snapshot",
        "header": header,
        "cursor": last_seq,
        "records": events.into_iter().map(|event| json!({ "type": "event", "event": event })).collect::<Vec<_>>(),
        "hasMore": false,
        "projections": { "asOfSeq": projection_seq, "values": values },
        // Revision 0 = no assistant-stream frames yet; the live tail's frames
        // start at 1 (strict +1 per frame, enforced by the client).
        "assistantStream": { "revision": 0 },
    });
    if tx.send(snapshot).await.is_err() {
        return Ok(());
    }

    // The ticket board's live tail: the coordinator store persists before a
    // mutation returns, so a slow poll of its file keeps the board current
    // even when a report never wakes the coordinator. The ticker and the SSE
    // stream share this task; a tick that wins drops only the `next()`
    // future, which the stream owns cancel-safely.
    let ticket_workspace = cwd.map(std::path::PathBuf::from);
    let mut tickets_tick = tokio::time::interval(super::tickets::POLL_INTERVAL);
    tickets_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tickets_tick.tick().await;

    // Live tail: subscribe from the backend's own latest seq.
    let since = detail
        .get("latest_seq")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut lines = host
        .backend
        .get_sse_lines(&format!(
            "/v1/threads/{session_id}/events?since_seq={since}"
        ))
        .await?;
    let (turn_baseline, step_baseline) = resume_counters(&detail);
    let mut mapper = SessionMapper::resume_at(last_seq, turn_baseline, step_baseline);
    // A follow stream can connect after the turn's write_plan_file call; the
    // replayed snapshot seeds the body the live exit_plan_mode needs.
    mapper.seed_plan_content(super::mapper::latest_plan_content(&detail));
    if let Some((provider, model)) = seat.as_ref() {
        mapper.set_model_source(provider, model);
    }
    use futures_util::StreamExt;
    let mut buffer = String::new();
    loop {
        let chunk = tokio::select! {
            _ = tickets_tick.tick() => {
                let next = match ticket_workspace.as_deref() {
                    Some(workspace) => super::tickets::projection(workspace).await,
                    None => None,
                };
                if next != last_team {
                    last_team = next.clone();
                    let seq = host.next_projection_seq();
                    host.push_live(
                        "session/control",
                        projection_update(&session_id, seq, &json!({ "agentTeam": next })),
                    )
                    .await;
                }
                continue;
            }
            chunk = lines.next() => match chunk {
                Some(chunk) => chunk,
                None => break,
            },
        };
        let chunk = chunk?;
        buffer.push_str(&chunk);
        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer.drain(..=newline).collect();
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                let Ok(envelope) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                // Approvals and questions ride the $events waterfall, not the
                // follow stream; the runtime's own decision events and turn
                // ends cancel whatever is still pending.
                handle_waterfall_envelope(host, &session_id, &envelope).await;
                // `api-session/status`: the client session's `running` flag —
                // which decides a submission's placement (queued dock row vs
                // transcript copy) and the composer's busy look — flips only
                // on this Host event; the journal frames the follow stream
                // carries hold turn state, not session state.
                if let Some(running) = envelope
                    .get("event")
                    .and_then(Value::as_str)
                    .and_then(session_running_flip)
                {
                    host.push_event_frame(session_status_frame(&session_id, running))
                        .await;
                }
                match envelope.get("event").and_then(Value::as_str) {
                    // Refresh the control projections (context pressure,
                    // model selection, the queue) for this session now that
                    // its turn settled with fresh usage.
                    Some("turn.completed") | Some("turn.failed") => {
                        push_control_projection(host, &session_id).await;
                        // The turn may have written files: the `@` index is
                        // stale until the next walk.
                        host.reference_index.invalidate();
                    }
                    // A queue drain starts turns no prompt of this bridge
                    // armed the watcher for; re-arm (a no-op while armed).
                    Some("turn.started") => ensure_waterfall_watcher(host, &session_id).await,
                    // The todo dock updates as the model writes its list.
                    Some("item.started") => push_live_todos(host, &session_id, &envelope).await,
                    // The GoalBar follows the model's own goal-tool calls:
                    // create/update mutate the engine state the projection
                    // reads.
                    Some("item.completed") => {
                        if envelope
                            .pointer("/payload/item/summary")
                            .and_then(Value::as_str)
                            .is_some_and(|summary| {
                                summary.starts_with("create_goal")
                                    || summary.starts_with("update_goal")
                            })
                        {
                            super::goals::push(host, &session_id).await;
                        }
                    }
                    Some("queue.updated") => {
                        push_inbox_projection(host, &session_id, &envelope["payload"]["items"])
                            .await;
                    }
                    _ => {}
                }
                // A settled user message carries its own `source.rpcId` (the
                // mapper reads the item's metadata), so the client retires
                // its optimistic copy by identity. The model source follows
                // the freshest UI choice: a switch journals no runtime event.
                if let Some((provider, model)) = host.model_choices.lock().await.get(&session_id) {
                    mapper.set_model_source(provider, model);
                }
                for frame in mapper.map_envelope(&envelope) {
                    if tx.send(frame.to_wire()).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
    // The SSE tail ended (backend dropped the stream): close any attempt
    // still streaming so the client does not wait for a settlement.
    for frame in mapper.abandon_open_attempts() {
        if tx.send(frame.to_wire()).await.is_err() {
            break;
        }
    }
    Ok(())
}

/// `toolResults/watch` — the trajectory inspector's on-demand result lookup.
/// The wire `tool/result` event at `seq` is the value (the client renders it
/// as a JSON tree); a seq the replay lacks answers "no live value" and the
/// stream parks — ending it would read as a failure to the client's watcher.
/// Stateless over the thread record: works after a bridge restart with no
/// follow stream having run, at the cost of one thread read per watch.
pub async fn tool_results_watch(
    host: &HostState,
    request: &Value,
    tx: &tokio::sync::mpsc::Sender<Value>,
) -> Result<()> {
    // The remote's single `request` parameter: the gateway packs its args as
    // `{request: {…}}` (the shape every bridge reader unwraps).
    let request = request.get("request").unwrap_or(request);
    let session_id = request
        .get("sessionId")
        .or_else(|| request.get("session_id"))
        .and_then(Value::as_str)
        .context("toolResults/watch without sessionId")?;
    let seq = request
        .get("seq")
        .and_then(Value::as_i64)
        .context("toolResults/watch without seq")?;
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    let value = replay_events(&detail)
        .into_iter()
        .find(|event| event["type"] == "tool/result" && event["seq"].as_i64() == Some(seq))
        .map(|event| event["data"].clone());
    let frame = match value {
        Some(data) => json!({ "available": true, "value": data }),
        None => json!({ "available": false }),
    };
    if tx.send(frame).await.is_err() {
        return Ok(());
    }
    tx.closed().await;
    Ok(())
}

/// Register one incoming waterfall in the pending map; `Some(frame)` only on
/// the FIRST registration of its event id. The follow loop and the background
/// watcher both read the runtime's event tail, so the same
/// `approval.required` / `user_input.required` envelope arrives twice — a
/// second push would double-dispatch the client's waterfall chain and leave a
/// ghost approval card that outlives its decision (the deciding click
/// settles one dispatch; the other stays answerable).
fn fresh_waterfall(
    pendings: &mut std::collections::HashMap<String, super::events::PendingInput>,
    event_id: String,
    pending: super::events::PendingInput,
) -> Option<Value> {
    use std::collections::hash_map::Entry;
    match pendings.entry(event_id) {
        Entry::Vacant(slot) => {
            let frame = pending.frame.clone();
            slot.insert(pending);
            Some(frame)
        }
        Entry::Occupied(_) => None,
    }
}

/// Route one runtime envelope's waterfall lifecycle (shared by the follow
/// loops and the background watcher): register and push a new approval or
/// question (first delivery only, see [`fresh_waterfall`]), cancel one the
/// runtime settled, and drop whatever a finished turn leaves behind.
pub(super) async fn handle_waterfall_envelope(
    host: &HostState,
    session_id: &str,
    envelope: &Value,
) {
    let payload = &envelope["payload"];
    match envelope
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "approval.required" => {
            if let Some((event_id, frame)) = super::events::approval_waterfall(session_id, payload)
            {
                let pending = super::events::PendingInput {
                    session_id: session_id.to_string(),
                    input_id: event_id.clone(),
                    kind: super::events::InputKind::Approval,
                    frame: frame.clone(),
                };
                if let Some(frame) =
                    fresh_waterfall(&mut *host.pending_inputs.lock().await, event_id, pending)
                {
                    host.push_event_frame(frame).await;
                }
            }
        }
        "user_input.required" => {
            if let Some((input_id, frame)) = super::events::questions_waterfall(session_id, payload)
            {
                let pending = super::events::PendingInput {
                    session_id: session_id.to_string(),
                    input_id: input_id.clone(),
                    kind: super::events::InputKind::Questions,
                    frame: frame.clone(),
                };
                if let Some(frame) =
                    fresh_waterfall(&mut *host.pending_inputs.lock().await, input_id, pending)
                {
                    host.push_event_frame(frame).await;
                }
            }
        }
        "approval.decided" | "approval.timeout" => {
            if let Some(id) = payload
                .get("id")
                .or_else(|| payload.get("approval_id"))
                .and_then(Value::as_str)
            {
                host.cancel_pending(id).await;
            }
        }
        "turn.completed" | "turn.failed" => {
            host.cancel_pending_for_session(session_id).await;
            restore_plan_handoff(host, session_id).await;
        }
        _ => {}
    }
}

/// The plan-mode handoff an approved `exit_plan_mode` owes: back to the mode
/// the session came from, so the next turn carries that mode's toolset — the
/// REPL's `plan_restore_mode` rule, not a fixed `agent`. The approval is
/// delivered mid-turn; the runtime saves the turn's thread at settlement,
/// which overwrites a PATCH sent at approval time. Waiting for the turn-end
/// envelope is what makes the restore stick. The pending set makes this fire
/// once (the follow stream and the background watcher both deliver the same
/// turn-end event); a denial never enqueues it.
///
/// Known limitation: the stash is bridge-process state. A thread already in
/// `plan` when this process first sees it (another host switched it, or a
/// reloaded page re-approving a plan) has no stashed mode and falls back to
/// `agent`, which is the runtime's own default.
pub(super) async fn restore_plan_handoff(host: &HostState, session_id: &str) {
    if !host.plan_handoffs.lock().await.remove(session_id) {
        return;
    }
    let path = format!("/v1/threads/{session_id}");
    match host.backend.get::<Value>(&path).await {
        Ok(detail) => {
            let mode = detail
                .pointer("/thread/mode")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            let restore = host.plan_return_modes.lock().await.remove(session_id);
            if mode != "plan" {
                // Left plan by some other route (the plan chip, `/mode`):
                // the stash would arm a restore the session no longer owes.
                return;
            }
            let restore = restore.as_deref().unwrap_or("agent").to_string();
            match set_mode(host, session_id, &restore).await {
                Ok(Ok(())) => push_control_projection(host, session_id).await,
                Ok(Err((_status, message))) => {
                    eprintln!("codesmith desktop: plan handoff refused for {session_id}: {message}")
                }
                Err(err) => {
                    eprintln!("codesmith desktop: plan handoff failed for {session_id}: {err:#}")
                }
            }
        }
        Err(err) => {
            eprintln!("codesmith desktop: plan handoff read failed for {session_id}: {err:#}")
        }
    }
}

/// Write a thread's mode through the bridge, keeping the plan-return stash.
///
/// Every mode write the bridge performs goes through here: the `/mode` and
/// `/plan`/`/yolo` commands, the new-session permission default, the
/// auto-approve pairing repair, and the handoff itself. A direct PATCH
/// elsewhere would skip the stash, and the handoff would fall back to
/// `agent` — the very behaviour this function exists to avoid.
///
/// Entering `plan` records the mode the thread was in (`agent` when the read
/// fails, the runtime's default). Leaving plan, by any route, clears it: a
/// session that already left plan owes no handoff.
///
/// The outer error is the loopback transport; the inner one is the runtime's
/// verdict, which the command catalog quotes verbatim.
pub(super) async fn set_mode(
    host: &HostState,
    session_id: &str,
    mode: &str,
) -> Result<std::result::Result<(), (reqwest::StatusCode, String)>> {
    let previous = if mode == "plan" {
        current_mode(host, session_id).await
    } else {
        None
    };
    let path = format!("/v1/threads/{session_id}");
    let written = host
        .backend
        .request_with_status(
            reqwest::Method::PATCH,
            &path,
            Some(&super::commands::mode_patch(mode)),
        )
        .await?;
    if written.is_ok() {
        let mut stash = host.plan_return_modes.lock().await;
        if mode == "plan" {
            // A `plan` → `plan` write (or a failed previous-mode read) must
            // not stash `plan` itself; the handoff would then restore plan.
            if let Some(previous) = previous.filter(|previous| previous != "plan") {
                stash.insert(session_id.to_string(), previous);
            }
        } else {
            // Left plan by some other route: a stash would arm a restore the
            // session no longer owes.
            stash.remove(session_id);
        }
    }
    Ok(written.map(|_| ()))
}

/// The thread's current mode, or `None` when the read fails.
pub(super) async fn current_mode(host: &HostState, session_id: &str) -> Option<String> {
    read_mode(host, session_id).await.ok().flatten()
}

/// The same read with the transport error preserved, for callers that must
/// distinguish "not in plan" from "could not tell".
async fn read_mode(host: &HostState, session_id: &str) -> Result<Option<String>> {
    Ok(host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await?
        .pointer("/thread/mode")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

/// What `/plan off` settled on.
pub(super) enum PlanExit {
    /// The thread was in plan mode and now carries the mode it came from.
    Left,
    /// The thread was not in plan mode; nothing was written.
    NotActive,
}

/// Leave plan mode, restoring the permission mode the session came from
/// (the same stash an approved `exit_plan_mode` uses), or `agent` when the
/// bridge has no record. Refuses to write `agent` over a mode that was never
/// `plan`: `/plan off` is idempotent, not a hidden mode reset.
pub(super) async fn leave_plan(
    host: &HostState,
    session_id: &str,
) -> Result<std::result::Result<PlanExit, (reqwest::StatusCode, String)>> {
    if read_mode(host, session_id).await?.as_deref() != Some("plan") {
        return Ok(Ok(PlanExit::NotActive));
    }
    let restore = host
        .plan_return_modes
        .lock()
        .await
        .get(session_id)
        .cloned()
        .unwrap_or_else(|| "agent".to_string());
    // A stash that somehow names plan itself would re-enter the state we are
    // leaving; the runtime's default is the safe fallback.
    let restore = if restore == "plan" {
        "agent".to_string()
    } else {
        restore
    };
    Ok(set_mode(host, session_id, &restore)
        .await?
        .map(|()| PlanExit::Left))
}

/// Arm the session's background waterfall watcher. The runtime surfaces
/// `approval.required` / `user_input.required` only on the thread's event
/// stream; a follow stream that is absent when one fires (page reload mid
/// turn, a session no client follows) would leave the request invisible to
/// every `$events` client, and the runtime's own timeout decides. One
/// watcher per prompted session subscribes to the same tail and routes the
/// waterfall lifecycle on its own.
///
/// Known limitation: the check-then-spawn window can arm two watchers for
/// one session; pendings are keyed by runtime event id, cancels are
/// idempotent, and a waterfall pushes on first registration only (see
/// [`fresh_waterfall`]), so a duplicate reader is harmless. The watcher ends
/// with the runtime's SSE tail and re-arms on the next prompt.
pub(super) async fn ensure_waterfall_watcher(host: &SharedHost, session_id: &str) {
    if host
        .waterfall_watchers
        .lock()
        .await
        .contains_key(session_id)
    {
        return;
    }
    let watched = session_id.to_string();
    let task_host = Arc::clone(host);
    let task = tokio::spawn(async move {
        watch_session_waterfalls(&task_host, &watched).await;
        task_host.waterfall_watchers.lock().await.remove(&watched);
    });
    host.waterfall_watchers
        .lock()
        .await
        .insert(session_id.to_string(), task);
}

async fn watch_session_waterfalls(host: &HostState, session_id: &str) {
    let detail = match host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    {
        Ok(detail) => detail,
        Err(err) => {
            eprintln!("codesmith desktop: waterfall watcher for {session_id}: {err:#}");
            return;
        }
    };
    let since = detail
        .get("latest_seq")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut lines = match host
        .backend
        .get_sse_lines(&format!(
            "/v1/threads/{session_id}/events?since_seq={since}"
        ))
        .await
    {
        Ok(lines) => lines,
        Err(err) => {
            eprintln!("codesmith desktop: waterfall watcher for {session_id}: {err:#}");
            return;
        }
    };
    use futures_util::StreamExt;
    let mut buffer = String::new();
    while let Some(chunk) = lines.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => return,
        };
        buffer.push_str(&chunk);
        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer.drain(..=newline).collect();
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let Ok(envelope) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            handle_waterfall_envelope(host, session_id, &envelope).await;
        }
    }
}

/// `session/page` — one history window from the replayed journal.
/// Contract (client journal-stream): the page's last record seq must equal
/// the requested `throughSeq` (inclusive cut; omitted = log end), records
/// stay under `beforeSeq` (exclusive), `hasMore` reports earlier history.
pub async fn page(host: &HostState, request: &Value) -> Result<Value> {
    // A single object parameter arrives wrapped under `request` from the
    // generated client; unwrap it so the cut fields and the address read the
    // same object.
    let request = request.get("request").unwrap_or(request);
    let session_id = address_session_id(request)?;
    let through_seq = request.get("throughSeq").and_then(Value::as_i64);
    let before_seq = request.get("beforeSeq").and_then(Value::as_i64);
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    let events = replay_events(&detail);
    let last = events.last().and_then(|e| e["seq"].as_i64()).unwrap_or(-1);
    let cut = through_seq.unwrap_or(last);
    let selected: Vec<Value> = events
        .into_iter()
        .filter(|e| {
            let seq = e["seq"].as_i64().unwrap_or(0);
            seq <= cut && before_seq.is_none_or(|before| seq < before)
        })
        .collect();
    let has_more = selected
        .first()
        .and_then(|e| e["seq"].as_i64())
        .is_some_and(|first| first > 0);
    Ok(json!({
        "records": selected.into_iter().map(|event| json!({ "type": "event", "event": event })).collect::<Vec<_>>(),
        "hasMore": has_more,
    }))
}

/// `session/events` — the log-end window a resource provider reads before
/// paging backwards (`ui-plan`'s plan lookup fixes its cut here). Same record
/// shape as `session/page`, plus the cursor the resource carries into
/// `session/page` as `throughSeq`.
pub async fn events(host: &HostState, request: &Value) -> Result<Value> {
    let session_id = address_session_id(request)?;
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    let events = replay_events(&detail);
    let cursor = events
        .last()
        .and_then(|event| event["seq"].as_i64())
        .unwrap_or(-1);
    Ok(json!({
        "cursor": cursor,
        "records": events.into_iter().map(|event| json!({ "type": "event", "event": event })).collect::<Vec<_>>(),
        "hasMore": false,
    }))
}

#[derive(serde::Deserialize)]
struct PromptPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default, rename = "mediaType")]
    media_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "receiptId")]
    receipt_id: Option<String>,
}

/// The prompt's attachments as runtime turn attachments: `file` parts name
/// staged receipts (`host::uploads`), `image` parts carry inline base64
/// (an image draft that was never staged). Any other part kind is a wire
/// contract break and fails loudly.
async fn prompt_attachments(
    host: &HostState,
    session_id: &str,
    content: &[PromptPart],
) -> Result<Vec<Value>> {
    for part in content {
        if !matches!(part.kind.as_str(), "text" | "file" | "image") {
            bail!("unknown prompt part type {}", part.kind);
        }
    }
    let receipt_ids: Vec<String> = content
        .iter()
        .filter(|p| p.kind == "file")
        .map(|p| {
            p.receipt_id
                .clone()
                .context("file prompt part without receiptId")
        })
        .collect::<Result<_>>()?;
    let mut attachments =
        super::uploads::resolve_prompt_attachments(&host.uploads, session_id, &receipt_ids)
            .await
            .map_err(|err| anyhow!("{}", err.message))?;
    for part in content.iter().filter(|p| p.kind == "image") {
        attachments.push(
            super::uploads::inline_image_attachment(
                part.media_type.as_deref(),
                part.data.as_deref(),
                part.name.as_deref(),
            )
            .map_err(|err| anyhow!("{}", err.message))?,
        );
    }
    Ok(attachments)
}

/// The prompt's text parts joined; empty when the prompt carries none (an
/// attachment-only message — the runtime keeps the turn non-empty).
fn prompt_text(content: &[PromptPart]) -> String {
    content
        .iter()
        .filter(|p| p.kind == "text")
        .map(|p| p.text.as_str())
        .collect::<Vec<&str>>()
        .join("\n")
}

async fn active_turn_id(host: &HostState, session_id: &str) -> Result<Option<String>> {
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    Ok(detail
        .get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns
                .iter()
                .rev()
                .find(|turn| {
                    matches!(
                        turn.get("status").and_then(Value::as_str),
                        Some("in_progress") | Some("queued")
                    )
                })
                .and_then(|turn| turn.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }))
}

/// The prompt's client identity: the client sends `requestId` (its
/// `beginSubmission` receipt) and retires the optimistic composer copy only
/// when the settling `user/message` echo carries it back as `source.rpcId`.
/// `rpcId` is accepted for symmetry with the echo-side field name.
fn prompt_request_id(inner: &Value) -> Option<String> {
    ["requestId", "rpcId"]
        .iter()
        .find_map(|key| inner.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

/// `session/prompt` — queue parks the prompt behind a running turn or starts
/// it right away (the runtime's queue route decides, so there is no busy
/// check here to race); steer rides the active turn. The request's
/// `requestId` travels to the runtime as `rpc_id` and comes back on the
/// queued row and the settled user message (`source.rpcId`, the client's
/// optimistic-copy dedupe key), and every prompt arms the session's
/// waterfall watcher (approvals and questions must surface even when no
/// client follows the session).
pub async fn prompt(host: &SharedHost, request: &Value) -> Result<Value> {
    let inner = request.get("request").unwrap_or(request);
    let session_id = inner
        .get("sessionId")
        .or_else(|| inner.get("session_id"))
        .and_then(Value::as_str)
        .context("prompt without sessionId")?
        .to_string();
    let mode = inner.get("mode").and_then(Value::as_str).unwrap_or("queue");
    repair_before_prompt(host, &session_id).await;
    // A persisted desktop pause/clear must reach a freshly loaded engine
    // before this prompt's turn can let the goal round driver continue.
    super::goals::reconcile(host, &session_id).await;
    let rpc_id = prompt_request_id(inner);
    let content: Vec<PromptPart> = serde_json::from_value(
        inner
            .get("content")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    )?;
    let attachments = prompt_attachments(host, &session_id, &content).await?;
    let text = prompt_text(&content);
    if text.is_empty() && attachments.is_empty() {
        bail!("prompt without a text part");
    }

    let delivery = if mode == "steer" {
        Delivery::Steer
    } else {
        Delivery::Queue
    };
    deliver_prompt(
        host,
        &session_id,
        &text,
        &attachments,
        rpc_id.as_deref(),
        delivery,
    )
    .await?;
    // The runtime accepted the prompt (parked, started, or steered) and
    // holds its bytes now — retire the staged receipts (a refused POST
    // above leaves them retryable).
    let receipt_ids: Vec<String> = content
        .iter()
        .filter(|p| p.kind == "file")
        .filter_map(|p| p.receipt_id.clone())
        .collect();
    super::uploads::retire_receipts(&host.uploads, &receipt_ids).await;
    Ok(json!({ "accepted": true }))
}

/// How one prompt reaches the runtime: a steer rides the active turn, a
/// queue prompt parks on the thread, and `Auto` is the command plane's
/// upstream `agent.steer` behavior (steer a running turn, otherwise start
/// one with the same content).
#[derive(Clone, Copy)]
pub(super) enum Delivery {
    Queue,
    Steer,
    Auto,
}

/// Deliver one prompt (text plus already-resolved runtime attachments) to
/// the runtime and arm the session's waterfall watcher. The caller owns
/// staged-receipt retirement: a failed POST leaves them retryable.
pub(super) async fn deliver_prompt(
    host: &SharedHost,
    session_id: &str,
    text: &str,
    attachments: &[Value],
    rpc_id: Option<&str>,
    delivery: Delivery,
) -> Result<()> {
    let steer_target = match delivery {
        Delivery::Queue => None,
        Delivery::Steer | Delivery::Auto => active_turn_id(host, session_id).await?,
    };
    if matches!(delivery, Delivery::Steer) && steer_target.is_none() {
        bail!("steer with no running turn");
    }
    // Attachments ride only when present — a plain prompt keeps the body the
    // runtime has always taken.
    let mut body = json!({ "prompt": text, "rpc_id": rpc_id });
    if !attachments.is_empty() {
        body["attachments"] = json!(attachments);
    }
    match steer_target {
        Some(turn_id) => {
            // A steer rides the active turn through the runtime's steer route
            // — attachments included (admitted there through the same intake
            // a turn's opening message uses), upstream `agent.steer` parity.
            let path = format!("/v1/threads/{session_id}/turns/{turn_id}/steer");
            let _: Value = host.backend.post(&path, &body).await?;
        }
        None => {
            let _: Value = host
                .backend
                .post(&format!("/v1/threads/{session_id}/queue"), &body)
                .await?;
        }
    }
    ensure_waterfall_watcher(host, session_id).await;
    Ok(())
}

/// The client error a failed queue mutation surfaces as. A runtime 404 is
/// upstream's `session/queue-item-not-found` and a steer's 409 (no running
/// turn) its `session/steer-unavailable` — the queue dock converges on
/// both silently, as the item already left the queue or the turn ended.
/// A 400 is the caller's mistake (a blank edit, an attachment-carrying
/// steer) and stays loud.
fn queue_failure(kind: &str, status: u16, message: String) -> OpError {
    let code = match (status, kind) {
        (404, _) => "session/queue-item-not-found",
        (409, "steer") => "session/steer-unavailable",
        (400, _) => "gateway/bad-request",
        _ => "gateway/internal",
    };
    OpError {
        code,
        message: format!("session/updateQueue {kind}: {message}"),
    }
}

/// An `edit` action's replacement text. Upstream queue edits are text-only
/// (`QUEUE_EDIT_NON_TEXT`; its dock disables Edit on rows that carry
/// attachments) and must not be blank.
fn queue_edit_text(action: &Value) -> Result<String, OpError> {
    let blocks = action
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut parts = Vec::new();
    for block in &blocks {
        match (
            block.get("type").and_then(Value::as_str),
            block.get("text").and_then(Value::as_str),
        ) {
            (Some("text"), Some(text)) => parts.push(text),
            _ => {
                return Err(OpError {
                    code: "session/attachment-invalid",
                    message: "QUEUE_EDIT_NON_TEXT: queue edits accept text content only".into(),
                })
            }
        }
    }
    let text = parts.join("\n");
    if text.trim().is_empty() {
        return Err(OpError {
            code: "gateway/bad-request",
            message: "queue edits need non-empty text".into(),
        });
    }
    Ok(text)
}

/// The runtime queue route one `QueueAction` maps onto: `edit` is a text
/// replacement (PATCH), `remove` a DELETE, `steer` the item's steer action.
fn queue_route(
    session_id: &str,
    item_id: &str,
    action: &Value,
) -> Result<(reqwest::Method, String, Option<Value>), OpError> {
    let item_path = format!("/v1/threads/{session_id}/queue/{item_id}");
    match action.get("kind").and_then(Value::as_str) {
        Some("edit") => Ok((
            reqwest::Method::PATCH,
            item_path,
            Some(json!({ "prompt": queue_edit_text(action)? })),
        )),
        Some("remove") => Ok((reqwest::Method::DELETE, item_path, None)),
        Some("steer") => Ok((reqwest::Method::POST, format!("{item_path}/steer"), None)),
        other => Err(OpError {
            code: "gateway/arguments-invalid",
            message: format!("session/updateQueue: unknown action kind {other:?}"),
        }),
    }
}

/// `session/updateQueue` — one queue mutation (upstream `QueueAction`):
/// `edit`, `remove`, or `steer`, each a runtime queue route. The changed
/// queue reaches the dock through the runtime's `queue.updated` event (the
/// follow loop pushes it as the `inbox` projection), so this answers only
/// the receipt.
pub async fn update_queue(host: &HostState, request: &Value) -> Result<Value, OpError> {
    let inner = request.get("request").unwrap_or(request);
    let field = |key: &str| {
        inner
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| OpError {
                code: "gateway/arguments-invalid",
                message: format!("session/updateQueue without {key}"),
            })
    };
    let session_id = field("sessionId")?;
    let item_id = field("itemId")?;
    let action = inner.get("action").cloned().unwrap_or(Value::Null);
    let kind = action
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (method, path, body) = queue_route(&session_id, &item_id, &action)?;
    match host
        .backend
        .request_with_status(method, &path, body.as_ref())
        .await
    {
        Ok(Ok(_)) => Ok(json!({ "accepted": true })),
        Ok(Err((status, message))) => Err(queue_failure(&kind, status.as_u16(), message)),
        Err(err) => Err(OpError {
            code: "gateway/internal",
            message: format!("session/updateQueue: {err:#}"),
        }),
    }
}

/// `session/cancel` — interrupt the running turn.
pub async fn cancel(host: &HostState, request: &Value) -> Result<Value> {
    let inner = request.get("request").unwrap_or(request);
    let session_id = inner
        .get("sessionId")
        .or_else(|| inner.get("session_id"))
        .and_then(Value::as_str)
        .context("cancel without sessionId")?;
    if let Some(turn_id) = active_turn_id(host, session_id).await? {
        host.backend
            .post_empty(&format!(
                "/v1/threads/{session_id}/turns/{turn_id}/interrupt"
            ))
            .await?;
    }
    Ok(json!({ "accepted": true }))
}

/// `session/selectModel` — record the session's model for later turns, then
/// publish the switch: the seat renders the modelSelection projection, not
/// this reply, so without a frame it would fall back to the old model.
/// Known limitation: the runtime thread takes one model id (no provider
/// axis), so the catalog's provider is kept only in this process's
/// `model_choices` (display), never forwarded to the runtime.
pub async fn select_model(host: &HostState, request: &Value) -> Result<Value> {
    let inner = request.get("request").unwrap_or(request);
    let session_id = inner
        .get("sessionId")
        .or_else(|| inner.get("session_id"))
        .and_then(Value::as_str)
        .context("selectModel without sessionId")?
        .to_string();
    let model = inner
        .get("model")
        .and_then(Value::as_str)
        .context("selectModel without model")?
        .to_string();
    let provider = inner
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("codesmith")
        .to_string();
    let _: Value = host
        .backend
        .patch(
            &format!("/v1/threads/{session_id}"),
            &json!({ "model": model }),
        )
        .await
        .context("patch thread model")?;
    host.model_choices
        .lock()
        .await
        .insert(session_id.clone(), (provider.clone(), model.clone()));
    push_control_projection(host, &session_id).await;
    Ok(json!({ "selected": { "provider": provider, "model": model } }))
}

fn session_id_of(request: &Value, what: &str) -> Result<String> {
    request
        .get("sessionId")
        .or_else(|| request.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("{what} without sessionId"))
}

/// `session/rename` — PATCH the thread title. The client applies the title
/// projection at the returned seq, drawn after the write so no snapshot read
/// before it can outrank the new title.
pub async fn rename(host: &HostState, request: &Value) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let session_id = session_id_of(request, "session/rename")?;
    let title = request
        .get("title")
        .and_then(Value::as_str)
        .context("session/rename without title")?;
    let _: Value = host
        .backend
        .patch(
            &format!("/v1/threads/{session_id}"),
            &json!({ "title": title }),
        )
        .await
        .context("patch thread title")?;
    Ok(json!({ "title": title, "seq": host.next_projection_seq() }))
}

/// `session/fork` — POST the fork route; the child thread record names the
/// new session.
pub async fn fork(host: &HostState, request: &Value) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let session_id = session_id_of(request, "session/fork")?;
    let child: Value = host
        .backend
        .post(&format!("/v1/threads/{session_id}/fork"), &json!({}))
        .await
        .context("fork thread")?;
    let child_id = child
        .get("id")
        .and_then(Value::as_str)
        .context("fork response without id")?
        .to_string();
    // The fork lands in the same workspace: re-publish the complete follow
    // baseline, the same way session/create does.
    super::workspace::push_replace(host).await;
    Ok(json!({ "sessionId": child_id }))
}

/// `session/search` — the runtime summary search matches thread content; the
/// preview snippet rides as the hit's snippet.
pub async fn search(host: &HostState, request: &Value) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let query = request
        .get("query")
        .and_then(Value::as_str)
        .context("session/search without query")?;
    let encoded = urlencode(&format!("search={query}&limit=50"));
    let threads: Vec<Value> = host
        .backend
        .get(&format!("/v1/threads/summary?{encoded}"))
        .await
        .context("search threads")?;
    let items: Vec<Value> = threads
        .iter()
        .map(|t| {
            json!({
                "sessionId": t.get("id"),
                "snippet": t.get("preview").cloned().unwrap_or_default(),
            })
        })
        .collect();
    Ok(json!({ "items": items, "hasMore": false }))
}

/// Percent-encode one query component set (`a=b&c=d` form).
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'=' | b'&' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `workspace/archiveSession` / `workspace/unarchiveSession` — PATCH the
/// archived flag and answer with the complete archived set (the client keeps
/// one set, not deltas).
pub async fn set_archived(host: &HostState, request: &Value, archived: bool) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let session_id = session_id_of(request, "workspace/archiveSession")?;
    let _: Value = host
        .backend
        .patch(
            &format!("/v1/threads/{session_id}"),
            &json!({ "archived": archived }),
        )
        .await
        .context("patch thread archived")?;
    let archived_ids: Vec<String> = host
        .backend
        .get::<Vec<Value>>("/v1/threads/summary?limit=200&archived_only=true")
        .await
        .context("list archived threads")?
        .iter()
        .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    // Re-publish the complete baseline: membership, the archived set, and
    // the live pin set all move together (an empty pin set would read as
    // "no pins" and drop every client's pin display until its next reload).
    super::workspace::push_replace(host).await;
    Ok(json!({ "archivedSessionIds": archived_ids }))
}

/// `skills/list` — the runtime's user-invocable skills for the `/` menu.
pub async fn skills(_host: &HostState, _request: &Value) -> Result<Value> {
    #[derive(serde::Deserialize)]
    struct SkillEntry {
        name: String,
        description: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        when_to_use: Option<String>,
        #[serde(default)]
        user_invocable: bool,
    }
    #[derive(serde::Deserialize)]
    struct SkillsResponse {
        #[serde(default)]
        skills: Vec<SkillEntry>,
    }
    let response: SkillsResponse = _host
        .backend
        .get("/v1/skills")
        .await
        .context("list skills")?;
    let entries = response.skills;
    let skills: Vec<Value> = entries
        .into_iter()
        .filter(|skill| skill.user_invocable)
        .map(|skill| {
            json!({
                "name": skill.name,
                "description": skill.description,
                "path": skill.path.unwrap_or_default(),
                "whenToUse": skill.when_to_use.unwrap_or_default(),
                "modelInvocable": true,
            })
        })
        .collect();
    Ok(json!({ "skills": skills }))
}

/// One runtime model registry row (`GET /v1/models`).
#[derive(serde::Deserialize)]
struct RuntimeModel {
    id: String,
    provider: String,
    #[serde(default)]
    default: bool,
    /// Approximate context window in tokens (absent when the runtime can't
    /// estimate it) — feeds the context meter's capacity denominator.
    #[serde(default)]
    context_window: Option<u64>,
}

/// The runtime model registry, read once per projection batch. A failed read
/// leaves modelSelection absent; the seat's own catalog load reports the same
/// failure with a retry.
async fn runtime_models(host: &HostState) -> Vec<RuntimeModel> {
    match host.backend.get("/v1/models").await {
        Ok(models) => models,
        Err(err) => {
            eprintln!("codesmith desktop: model registry read failed: {err:#}");
            Vec::new()
        }
    }
}

/// The (provider, model) the seat shows for a thread's model id — runtime
/// provider naming, so the value matches the catalog groups verbatim. The
/// user's own pick wins while the thread still runs that id (one id can sit
/// under several providers); otherwise the first provider listing it. A model
/// the catalog lacks keeps its own id, so the seat reads it as not routable
/// rather than as some other model; a thread without a model takes the
/// catalog default.
fn model_selection(
    thread_model: Option<&str>,
    choice: Option<&(String, String)>,
    models: &[RuntimeModel],
) -> Option<(String, String)> {
    let default = models.iter().find(|m| m.default).or_else(|| models.first());
    let Some(id) = thread_model else {
        return default.map(|m| (m.provider.clone(), m.id.clone()));
    };
    if let Some((provider, model)) = choice.filter(|(_, model)| model == id) {
        return Some((provider.clone(), model.clone()));
    }
    let provider = models
        .iter()
        .find(|m| m.id == id)
        .or(default)
        .map_or_else(|| "codesmith".to_string(), |m| m.provider.clone());
    Some((provider, id.to_string()))
}

/// The title the runtime's own thread list derives: the user-set title, else
/// the latest turn's input summary; null while the thread has neither.
fn session_title(detail: &Value) -> Value {
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    text(detail.pointer("/thread/title"))
        .or_else(|| {
            text(
                detail
                    .get("turns")
                    .and_then(Value::as_array)
                    .and_then(|turns| turns.last())
                    .and_then(|turn| turn.get("input_summary")),
            )
        })
        .map_or(Value::Null, Value::String)
}

/// The latest completed turn's provider-reported prompt size.
fn context_pressure(detail: &Value) -> Option<u64> {
    detail
        .get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns
                .iter()
                .rev()
                .find_map(|turn| turn.get("usage").filter(|usage| !usage.is_null()))
        })
        .map(|usage| {
            let tokens = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
            tokens("input_tokens")
                + tokens("prompt_cache_hit_tokens")
                + tokens("prompt_cache_miss_tokens")
        })
}

/// Whole-log token accounting (the client's `tokenUsage` projection): the
/// three prompt-side billing buckets plus output, summed over every turn that
/// reported usage. CodeSmith's providers report no cache-write bucket, so it
/// stays 0 (the client renders that row only when non-zero).
fn token_usage_projection(detail: &Value) -> Option<Value> {
    let turns = detail.get("turns")?.as_array()?;
    let mut uncached = 0u64;
    let mut output = 0u64;
    let mut cache_read = 0u64;
    let mut any = false;
    for turn in turns {
        let Some(usage) = turn.get("usage").filter(|usage| !usage.is_null()) else {
            continue;
        };
        let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let input = count("input_tokens");
        let hit = count("prompt_cache_hit_tokens");
        let miss = usage
            .get("prompt_cache_miss_tokens")
            .and_then(Value::as_u64);
        uncached += miss.unwrap_or_else(|| input.saturating_sub(hit));
        cache_read += hit;
        output += count("output_tokens");
        any = true;
    }
    any.then(|| {
        json!({
            "uncachedInputTokens": uncached,
            "outputTokens": output,
            "cacheReadTokens": cache_read,
            "cacheWriteTokens": 0,
        })
    })
}

/// Whole-log activity stats (the client's `sessionStats` projection): turn and
/// step counts plus wall times folded from the durable records. TTFT is not
/// separable from durable records (they carry only item spans), so it reports
/// none; decode time uses the full agent-item spans of turns that reported
/// output tokens, which reads as a conservative (lower-bound) speed.
fn session_stats_projection(detail: &Value) -> Value {
    let turns = detail
        .get("turns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let items = detail
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let span_ms = |item: &Value| {
        let started = item
            .get("started_at")
            .and_then(Value::as_str)
            .and_then(super::mapper::epoch_ms);
        let ended = item
            .get("ended_at")
            .and_then(Value::as_str)
            .and_then(super::mapper::epoch_ms);
        started
            .zip(ended)
            .map(|(start, end)| (end - start).max(0) as u64)
    };
    let is_agent = |item: &Value| {
        matches!(
            item.get("kind").and_then(Value::as_str),
            Some("agent_message") | Some("agent_reasoning")
        )
    };
    let is_tool = |item: &Value| {
        matches!(
            item.get("kind").and_then(Value::as_str),
            Some("tool_call") | Some("command_execution") | Some("file_change")
        )
    };
    let settled = |item: &Value| item.get("status").and_then(Value::as_str) != Some("in_progress");

    let mut steps = 0u64;
    let mut llm_ms = 0u64;
    let mut tool_ms = 0u64;
    for item in &items {
        if !settled(item) {
            continue;
        }
        if is_agent(item) {
            steps += 1;
            llm_ms += span_ms(item).unwrap_or(0);
        } else if is_tool(item) {
            tool_ms += span_ms(item).unwrap_or(0);
        }
    }
    // Decode time: the agent-item spans of turns that billed output tokens.
    let mut decode_ms = 0u64;
    let mut decode_tokens = 0u64;
    for turn in &turns {
        let Some(usage) = turn.get("usage").filter(|usage| !usage.is_null()) else {
            continue;
        };
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or_default();
        decode_tokens += usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        decode_ms += items
            .iter()
            .filter(|item| {
                item.get("turn_id").and_then(Value::as_str) == Some(turn_id)
                    && is_agent(item)
                    && settled(item)
            })
            .filter_map(span_ms)
            .sum::<u64>();
    }
    json!({
        "turns": turns.len(),
        "steps": steps,
        "llmMs": llm_ms,
        "toolMs": tool_ms,
        "ttftMs": 0,
        "ttftSteps": 0,
        "decodeMs": decode_ms,
        "decodeTokens": decode_tokens,
    })
}

/// The thread's mode (`thread.mode`; absent reads as the runtime default).
fn thread_mode(detail: &Value) -> &str {
    detail
        .pointer("/thread/mode")
        .and_then(Value::as_str)
        .unwrap_or("agent")
}

/// Normalize one tool call's `todos` argument into the projection's item
/// shape; a non-array value is not a todo write.
fn normalize_todos(todos: &Value) -> Option<Value> {
    let array = todos.as_array()?;
    Some(Value::Array(
        array
            .iter()
            .filter_map(|todo| {
                let content = todo.get("content").and_then(Value::as_str)?;
                let status = todo
                    .get("status")
                    .and_then(Value::as_str)
                    .filter(|status| matches!(*status, "pending" | "in_progress" | "completed"))
                    .unwrap_or("pending");
                Some(json!({ "content": content, "status": status }))
            })
            .collect(),
    ))
}

/// The latest whole-list todo snapshot (`todo`/`checklist_write`), or null
/// before the first successful write. The runtime stores the call's input in
/// `metadata.arguments`, so the fold is a replay-time scan, not a second
/// state store.
fn todos_projection(detail: &Value) -> Value {
    let Some(items) = detail.get("items").and_then(Value::as_array) else {
        return Value::Null;
    };
    let mut latest = None;
    for item in items {
        if !matches!(
            item.get("kind").and_then(Value::as_str),
            Some("tool_call") | Some("command_execution") | Some("file_change")
        ) {
            continue;
        }
        // Only a settled, successful call is a write the model saw land.
        if item.get("status").and_then(Value::as_str) != Some("completed") {
            continue;
        }
        let name = super::mapper::tool_name_from_summary(item);
        let todos = match name.as_str() {
            // Whole-list writes carry the list in their own input.
            "todo" | "todo_write" | "checklist_write" => item
                .pointer("/metadata/arguments")
                .or_else(|| item.pointer("/metadata/input"))
                .and_then(|args| args.get("todos")),
            // Single-item mutations publish the resulting snapshot in their
            // result metadata; folding it keeps the dock correct when the
            // model uses the add/update aliases instead of a whole-list write.
            "todo_add" | "todo_update" | "checklist_add" | "checklist_update" => {
                item.pointer("/metadata/task_updates/checklist/items")
            }
            _ => continue,
        };
        let Some(todos) = todos else {
            continue;
        };
        latest = normalize_todos(todos);
    }
    latest.unwrap_or(Value::Null)
}

/// Push a live `todos` frame the moment the model writes its list — the
/// dock should appear mid-turn, not only when the turn settles. An
/// in-progress call's list is what the model is trying; the turn-end refresh
/// replaces it if the call fails.
async fn push_live_todos(host: &HostState, session_id: &str, envelope: &Value) {
    let Some(tool) = envelope.pointer("/payload/tool") else {
        return;
    };
    let Some(name) = tool.get("name").and_then(Value::as_str) else {
        return;
    };
    if !matches!(name, "todo" | "todo_write" | "checklist_write") {
        return;
    }
    let Some(todos) = tool.pointer("/input/todos").and_then(normalize_todos) else {
        return;
    };
    let seq = host.next_projection_seq();
    host.push_live(
        "session/control",
        projection_update(session_id, seq, &json!({ "todos": todos })),
    )
    .await;
}

/// The plan-mode collaboration state the composer chip reads (`active` only;
/// the bridge has no pending-command window to report).
fn plan_projection(mode: &str) -> Value {
    json!({ "active": mode == "plan", "pending": false })
}

/// The current permission selection over the mode ↔ preset mapping.
fn permissions_projection(mode: &str) -> Value {
    json!({ "currentValue": permissions::preset_of_mode(mode) })
}

/// One rail-card preview: collapsed whitespace, bounded on a char boundary.
fn bounded_preview(text: &str, limit: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= limit {
        return normalized;
    }
    let mut out: String = normalized.chars().take(limit).collect();
    out.push('…');
    out
}

/// Every started turn with its `turn/start` seq and bounded previews — the
/// turn navigator's data for history the client has not paged in.
fn turn_outline_projection(detail: &Value) -> Value {
    let events = replay_events(detail);
    let seqs: std::collections::HashMap<u64, i64> = events
        .iter()
        .filter(|event| event["type"] == "turn/start")
        .filter_map(|event| Some((event["data"]["turn"].as_u64()?, event["seq"].as_i64()?)))
        .collect();
    let turns = detail
        .get("turns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let items = detail
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut outline = Vec::new();
    for (index, turn) in turns.iter().enumerate() {
        let turn_no = (index as u64) + 1;
        let Some(seq) = seqs.get(&turn_no) else {
            continue;
        };
        let prompt = turn
            .get("input_summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or_default();
        let response = items
            .iter()
            .filter(|item| {
                item.get("turn_id").and_then(Value::as_str) == Some(turn_id)
                    && item.get("kind").and_then(Value::as_str) == Some("agent_message")
            })
            .filter_map(|item| item.get("detail").and_then(Value::as_str))
            .next_back()
            .unwrap_or_default();
        outline.push(json!({
            "turn": turn_no,
            "seq": seq,
            "prompt": bounded_preview(prompt, 200),
            "response": bounded_preview(response, 600),
        }));
    }
    Value::Array(outline)
}

/// One session's projection values the bridge can compute from the runtime:
/// title, context pressure (with the model's context window), model
/// selection, the queue (`inbox`), whole-log token usage and activity stats.
/// Every other key stays absent (capability absent, per the projection
/// contract). Each write is a whole baseline for these keys, so they are
/// always all computed together.
async fn projection_values(
    host: &HostState,
    session_id: &str,
    detail: &Value,
    models: &[RuntimeModel],
) -> Value {
    let mut values = serde_json::Map::new();
    values.insert("title".into(), session_title(detail));
    let mode = thread_mode(detail).to_string();
    // Composer docks/controls: the plan chip, the permission chip, the todo
    // dock, the turn navigator's history rail, and the workspace an `@` file
    // chip resolves against.
    values.insert("plan".into(), plan_projection(&mode));
    values.insert("permissions".into(), permissions_projection(&mode));
    values.insert("todos".into(), todos_projection(detail));
    // The GoalBar dock (host::goals): the live engine state when loaded,
    // else the durable goal-tool journal; `null` before a create/after a
    // clear. Reading this never loads an engine.
    values.insert(
        "goal".into(),
        super::goals::projection(host, session_id, detail).await,
    );
    values.insert("turnOutline".into(), turn_outline_projection(detail));
    let workspace = detail.pointer("/thread/workspace").and_then(Value::as_str);
    values.insert("workingDirectory".into(), json!(workspace));
    // The coordinator ticket board (host::tickets): a durable ticket file is
    // read without loading an engine, so the board survives an eviction.
    if let Some(workspace) = workspace.filter(|path| !path.is_empty()) {
        if let Some(team) = super::tickets::projection(std::path::Path::new(workspace)).await {
            values.insert("agentTeam".into(), team);
        }
    }
    let queue = detail
        .get("queue")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    values.insert("inbox".into(), super::mapper::inbox_projection(&queue));
    let thread_model = detail
        .pointer("/thread/model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty());
    if let Some(tokens) = context_pressure(detail) {
        // The capacity denominator: the thread model's (else the default
        // model's) declared context window; absent when the runtime can't
        // estimate one, which leaves the meter hidden rather than wrong.
        let window = thread_model
            .and_then(|id| models.iter().find(|m| m.id == id))
            .or_else(|| models.iter().find(|m| m.default))
            .or_else(|| models.first())
            .and_then(|m| m.context_window);
        let mut pressure = json!({ "pressureTokens": tokens, "projectedTokens": tokens });
        if let Some(window) = window {
            pressure["contextWindow"] = json!(window);
        }
        values.insert("contextPressure".into(), pressure);
    }
    if let Some(usage) = token_usage_projection(detail) {
        values.insert("tokenUsage".into(), usage);
    }
    values.insert("sessionStats".into(), session_stats_projection(detail));
    let choice = host.model_choices.lock().await.get(session_id).cloned();
    if let Some((provider, model)) = model_selection(thread_model, choice.as_ref(), models) {
        // ModelSelectionProjection: `next` feeds the next model step;
        // `lastUsed` is unknown from the thread record alone.
        values.insert(
            "modelSelection".into(),
            json!({ "lastUsed": null, "next": { "provider": provider, "model": model } }),
        );
    }
    Value::Object(values)
}

/// `session/projections` — one session's projection baseline outside a
/// follow stream. The vendored session controller reads it when restoring a
/// blank session and when a connection generation is re-established; the
/// reply is the same sequenced shape a follow snapshot carries, drawn from
/// the same `projection_values`, so a restored session never sees a different
/// value set than its stream would seed.
pub(super) async fn projections(host: &HostState, request: &Value) -> Result<Value> {
    let session_id = request
        .get("sessionId")
        .or_else(|| request.get("session_id"))
        .or_else(|| request.pointer("/request/sessionId"))
        .and_then(Value::as_str)
        .context("session/projections without sessionId")?;
    // Drawn before the read, so the values are at least as fresh as the seq.
    let seq = host.next_projection_seq();
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    let models = runtime_models(host).await;
    let values = projection_values(host, session_id, &detail, &models).await;
    Ok(json!({ "kind": "sequenced", "asOfSeq": seq, "values": values }))
}

/// `session/control` baseline: projections for the most recently used
/// sessions (bounded — one thread read each). Older sessions get theirs from
/// their follow snapshot, and every session from the frame its next settled
/// turn or model switch pushes.
pub async fn control_baseline(host: &HostState) -> Result<Vec<Value>> {
    #[derive(serde::Deserialize)]
    struct Summary {
        id: String,
    }
    let seq = host.next_projection_seq();
    let summaries: Vec<Summary> = match host.backend.get("/v1/threads/summary?limit=20").await {
        Ok(summaries) => summaries,
        Err(err) => {
            eprintln!("codesmith desktop: control baseline without sessions: {err:#}");
            Vec::new()
        }
    };
    let models = runtime_models(host).await;
    let mut projections = serde_json::Map::new();
    for summary in summaries {
        match host
            .backend
            .get::<Value>(&format!("/v1/threads/{}", summary.id))
            .await
        {
            Ok(detail) => {
                let mut values = projection_values(host, &summary.id, &detail, &models).await;
                // The sidebar baseline carries no goal: a followed session's
                // own snapshot does, and the control baseline would otherwise
                // read (and display) the goal of every listed session.
                if let Some(map) = values.as_object_mut() {
                    map.remove("goal");
                }
                projections.insert(summary.id, json!({ "asOfSeq": seq, "values": values }));
            }
            Err(err) => {
                eprintln!(
                    "codesmith desktop: control baseline skipped {}: {err:#}",
                    summary.id
                )
            }
        }
    }
    Ok(vec![
        json!({ "type": "baseline", "value": { "projections": projections } }),
    ])
}

/// Push one session's refreshed projections to every open `session/control`
/// stream (a settled turn or a model switch changed them).
pub async fn push_control_projection(host: &HostState, session_id: &str) {
    let seq = host.next_projection_seq();
    let detail = match host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    {
        Ok(detail) => detail,
        Err(err) => {
            eprintln!("codesmith desktop: projection refresh for {session_id} failed: {err:#}");
            return;
        }
    };
    let models = runtime_models(host).await;
    let values = projection_values(host, session_id, &detail, &models).await;
    host.push_live(
        "session/control",
        projection_update(session_id, seq, &values),
    )
    .await;
}

/// Push one session's queue rows (the `inbox` projection) to every open
/// `session/control` stream — the runtime's `queue.updated` carries the
/// whole queue, so the frame is a full value, never a delta.
async fn push_inbox_projection(host: &HostState, session_id: &str, items: &Value) {
    let queue = items.as_array().cloned().unwrap_or_default();
    let seq = host.next_projection_seq();
    let values = json!({ "inbox": super::mapper::inbox_projection(&queue) });
    host.push_live(
        "session/control",
        projection_update(session_id, seq, &values),
    )
    .await;
}

/// A `session/control` projection frame (SessionProjectionUpdate): one
/// `{key, val}` entry per carried key. The client iterates `changes`; an
/// object there throws inside its control stream and ends it for good.
pub(super) fn projection_update(session_id: &str, seq: u64, values: &Value) -> Value {
    let changes: Vec<Value> = values
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, val)| json!({ "key": key, "val": val }))
        .collect();
    json!({ "type": "projection", "sessionId": session_id, "seq": seq, "changes": changes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todos_fold_takes_the_latest_successful_whole_list() {
        let detail = json!({
            "items": [
                { "kind": "tool_call", "status": "completed", "summary": "checklist_write: ok",
                  "metadata": { "arguments": { "todos": [
                      { "content": "a", "status": "completed" }
                  ] } } },
                { "kind": "tool_call", "status": "completed", "summary": "todo_write: ok",
                  "metadata": { "arguments": { "todos": [
                      { "content": "b", "status": "in_progress" },
                      { "content": "c", "status": "bogus" }
                  ] } } },
                { "kind": "tool_call", "status": "failed", "summary": "todo: err",
                  "metadata": { "arguments": { "todos": [
                      { "content": "failed", "status": "pending" }
                  ] } } },
            ],
        });
        assert_eq!(
            todos_projection(&detail),
            json!([
                { "content": "b", "status": "in_progress" },
                // An unrecognized status reads as pending, the live tool's
                // own default.
                { "content": "c", "status": "pending" }
            ])
        );
        assert_eq!(todos_projection(&json!({ "items": [] })), Value::Null);
    }

    #[test]
    fn todos_fold_single_item_alias_snapshots() {
        // checklist_add/checklist_update publish the whole resulting list in
        // result metadata; the projection must prefer the latest snapshot
        // over an earlier whole-list write.
        let detail = json!({
            "items": [
                { "kind": "tool_call", "status": "completed", "summary": "todo_write: ok",
                  "metadata": { "arguments": { "todos": [
                      { "content": "a", "status": "pending" }
                  ] } } },
                { "kind": "tool_call", "status": "completed", "summary": "checklist_add: ok",
                  "metadata": { "task_updates": { "checklist": { "items": [
                      { "id": 0, "content": "a", "status": "in_progress" },
                      { "id": 1, "content": "b", "status": "pending" }
                  ] } } } },
                { "kind": "tool_call", "status": "completed", "summary": "todo_update: ok",
                  "metadata": { "task_updates": { "checklist": { "items": [
                      { "id": 0, "content": "a", "status": "completed" },
                      { "id": 1, "content": "b", "status": "in_progress" }
                  ] } } } },
            ],
        });
        assert_eq!(
            todos_projection(&detail),
            json!([
                { "content": "a", "status": "completed" },
                { "content": "b", "status": "in_progress" }
            ])
        );
    }

    #[test]
    fn plan_and_permissions_projections_follow_the_thread_mode() {
        assert_eq!(
            plan_projection("plan"),
            json!({ "active": true, "pending": false })
        );
        assert_eq!(
            plan_projection("agent"),
            json!({ "active": false, "pending": false })
        );
        assert_eq!(
            permissions_projection("yolo")["currentValue"],
            "danger-full-access"
        );
        assert_eq!(permissions_projection("plan")["currentValue"], "read-only");
        assert_eq!(
            permissions_projection("agent")["currentValue"],
            "workspace-write"
        );
        assert_eq!(
            permissions_projection("coordinator")["currentValue"],
            "custom"
        );
    }

    #[test]
    fn turn_outline_uses_replayed_turn_seqs() {
        let detail = json!({
            "turns": [
                { "id": "turn_1", "status": "completed", "input_summary": "first  task",
                  "created_at": "2026-11-06T10:00:00Z", "started_at": "2026-11-06T10:00:00Z",
                  "ended_at": "2026-11-06T10:00:09Z" },
                { "id": "turn_2", "status": "completed", "input_summary": "second",
                  "created_at": "2026-11-06T10:01:00Z", "started_at": "2026-11-06T10:01:00Z",
                  "ended_at": "2026-11-06T10:01:09Z" },
            ],
            "items": [
                { "id": "item_a", "turn_id": "turn_1", "kind": "agent_message",
                  "status": "completed", "detail": "first answer",
                  "started_at": "2026-11-06T10:00:01Z", "ended_at": "2026-11-06T10:00:04Z" },
                { "id": "item_b", "turn_id": "turn_2", "kind": "agent_message",
                  "status": "completed", "detail": "second answer",
                  "started_at": "2026-11-06T10:01:01Z", "ended_at": "2026-11-06T10:01:04Z" },
            ],
        });
        let outline = turn_outline_projection(&detail);
        let turns = outline.as_array().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["turn"], 1);
        assert_eq!(turns[0]["prompt"], "first task");
        assert_eq!(turns[0]["response"], "first answer");
        assert!(turns[0]["seq"].as_i64().unwrap() < turns[1]["seq"].as_i64().unwrap());
        assert_eq!(turns[1]["turn"], 2);
        assert_eq!(turns[1]["response"], "second answer");
    }

    #[test]
    fn previews_collapse_whitespace_and_bound_on_a_char_boundary() {
        assert_eq!(bounded_preview("  a\n\n b\tc ", 20), "a b c");
        let long = "计".repeat(40);
        let preview = bounded_preview(&long, 10);
        assert_eq!(preview.chars().count(), 11, "ten chars plus the ellipsis");
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn duplicate_waterfall_registration_pushes_only_the_first() {
        // The follow loop and the background watcher both deliver the same
        // approval.required; the second registration must not push again or
        // the client's waterfall chain double-dispatches (a ghost approval
        // card that outlives its decision).
        let frame = json!({
            "type": "waterfall", "event": "approval/request",
            "eventId": "appr_1", "agentId": "sess_1",
            "request": { "toolName": "write_file" },
        });
        let make = || super::super::events::PendingInput {
            session_id: "sess_1".into(),
            input_id: "appr_1".into(),
            kind: super::super::events::InputKind::Approval,
            frame: frame.clone(),
        };
        let mut pendings = std::collections::HashMap::new();
        let first = fresh_waterfall(&mut pendings, "appr_1".into(), make());
        let second = fresh_waterfall(&mut pendings, "appr_1".into(), make());
        assert!(first.is_some(), "the first registration pushes");
        assert!(
            second.is_none(),
            "a re-delivery of the same event must not push"
        );
        assert_eq!(pendings.len(), 1);
    }

    #[test]
    fn turn_lifecycle_flips_the_client_session_running_flag() {
        assert_eq!(session_running_flip("turn.started"), Some(true));
        assert_eq!(session_running_flip("turn.completed"), Some(false));
        assert_eq!(session_running_flip("turn.failed"), Some(false));
        assert_eq!(session_running_flip("item.delta"), None);
        assert_eq!(session_running_flip("queue.updated"), None);
        // The contract the client session controller listens on: args are
        // `(sessionId, running)` — the order is protocol, not taste.
        assert_eq!(
            session_status_frame("thr_1", true),
            json!({"type": "emit", "event": "api-session/status", "args": ["thr_1", true]})
        );
        assert_eq!(
            session_status_frame("thr_1", false),
            json!({"type": "emit", "event": "api-session/status", "args": ["thr_1", false]})
        );
    }

    #[test]
    fn queue_failures_map_onto_the_clients_converging_codes() {
        // The dock converges silently on these two (the item already left
        // the queue / the turn ended); a caller mistake stays loud.
        assert_eq!(
            queue_failure("remove", 404, "gone".into()).code,
            "session/queue-item-not-found"
        );
        assert_eq!(
            queue_failure("steer", 404, "gone".into()).code,
            "session/queue-item-not-found"
        );
        assert_eq!(
            queue_failure("steer", 409, "No active turn".into()).code,
            "session/steer-unavailable"
        );
        assert_eq!(
            queue_failure("steer", 400, "carries attachments".into()).code,
            "gateway/bad-request",
            "an attachment-carrying steer is refused loudly"
        );
        assert_eq!(
            queue_failure("edit", 409, "odd".into()).code,
            "gateway/internal",
            "only a steer has a legitimate conflict"
        );
    }

    #[test]
    fn queue_edits_are_text_only_and_non_blank() {
        let joined = queue_edit_text(&json!({ "kind": "edit", "content": [
            { "type": "text", "text": "first" }, { "type": "text", "text": "second" },
        ]}))
        .ok()
        .unwrap();
        assert_eq!(joined, "first\nsecond");

        let non_text = queue_edit_text(&json!({ "kind": "edit", "content": [
            { "type": "text", "text": "words" },
            { "type": "image", "attachment": { "attachmentId": "sha256:aa" } },
        ]}))
        .err()
        .unwrap();
        assert_eq!(non_text.code, "session/attachment-invalid");
        assert!(non_text.message.contains("QUEUE_EDIT_NON_TEXT"));

        let blank = queue_edit_text(&json!({ "kind": "edit", "content": [
            { "type": "text", "text": "   " },
        ]}))
        .err()
        .unwrap();
        assert_eq!(blank.code, "gateway/bad-request");
    }

    #[test]
    fn queue_actions_route_onto_the_runtime_queue() {
        let (method, path, body) = queue_route(
            "thr_1",
            "qitem_a",
            &json!({ "kind": "edit", "content": [{ "type": "text", "text": "new" }] }),
        )
        .ok()
        .unwrap();
        assert_eq!(method, reqwest::Method::PATCH);
        assert_eq!(path, "/v1/threads/thr_1/queue/qitem_a");
        assert_eq!(body, Some(json!({ "prompt": "new" })));

        let (method, path, body) = queue_route("thr_1", "qitem_a", &json!({ "kind": "remove" }))
            .ok()
            .unwrap();
        assert_eq!(method, reqwest::Method::DELETE);
        assert_eq!(path, "/v1/threads/thr_1/queue/qitem_a");
        assert_eq!(body, None);

        let (method, path, _) = queue_route("thr_1", "qitem_a", &json!({ "kind": "steer" }))
            .ok()
            .unwrap();
        assert_eq!(method, reqwest::Method::POST);
        assert_eq!(path, "/v1/threads/thr_1/queue/qitem_a/steer");

        let unknown = queue_route("thr_1", "qitem_a", &json!({ "kind": "reorder" }))
            .err()
            .unwrap();
        assert_eq!(unknown.code, "gateway/arguments-invalid");
    }

    fn model(id: &str, provider: &str, default: bool) -> RuntimeModel {
        RuntimeModel {
            id: id.into(),
            provider: provider.into(),
            default,
            context_window: None,
        }
    }

    #[test]
    fn prompt_request_id_reads_the_clients_request_id_first() {
        // The client's beginSubmission receipt rides as `requestId`; the echo
        // side of the same identity is `source.rpcId`. Missing both means the
        // client's optimistic copy must live until it re-reads the journal.
        assert_eq!(
            prompt_request_id(&json!({ "requestId": "req_1", "rpcId": "rpc_x" })),
            Some("req_1".to_string())
        );
        assert_eq!(
            prompt_request_id(&json!({ "rpcId": "rpc_x" })),
            Some("rpc_x".to_string())
        );
        assert_eq!(prompt_request_id(&json!({ "sessionId": "s" })), None);
    }

    #[test]
    fn projection_frames_list_changes_as_key_val_entries() {
        let frame = projection_update(
            "thr_1",
            7,
            &json!({ "title": "t", "contextPressure": { "pressureTokens": 3 } }),
        );
        assert_eq!(frame["type"], "projection");
        assert_eq!(frame["seq"], 7);
        let changes = frame["changes"].as_array().expect("changes is an array");
        assert_eq!(changes.len(), 2);
        assert!(changes.contains(&json!({ "key": "title", "val": "t" })));
    }

    #[test]
    fn model_selection_keeps_the_users_provider_and_never_invents_a_model() {
        let models = [
            model("model-pro", "alpha", true),
            model("model-pro", "beta", false),
            model("model-x", "beta", false),
        ];
        let pick = ("beta".to_string(), "model-pro".to_string());
        let pair = |p: &str, m: &str| Some((p.to_string(), m.to_string()));
        // The user's pick wins while the thread still runs that id...
        assert_eq!(
            model_selection(Some("model-pro"), Some(&pick), &models),
            pair("beta", "model-pro")
        );
        // ...and yields once the thread moved to another model.
        assert_eq!(
            model_selection(Some("model-x"), Some(&pick), &models),
            pair("beta", "model-x")
        );
        // Without a pick, a shared id resolves to its first provider.
        assert_eq!(
            model_selection(Some("model-pro"), None, &models),
            pair("alpha", "model-pro")
        );
        // An id the catalog lacks keeps its own id (not routable, not another model).
        assert_eq!(
            model_selection(Some("custom-x"), None, &models),
            pair("alpha", "custom-x")
        );
        // No thread model: the catalog default.
        assert_eq!(
            model_selection(None, None, &models),
            pair("alpha", "model-pro")
        );
    }

    #[test]
    fn session_title_prefers_the_user_title_then_the_latest_prompt() {
        assert_eq!(
            session_title(&json!({ "thread": {}, "turns": [] })),
            Value::Null
        );
        let turns = json!([{ "input_summary": "first" }, { "input_summary": "latest" }]);
        assert_eq!(
            session_title(&json!({ "thread": {}, "turns": turns })),
            json!("latest")
        );
        assert_eq!(
            session_title(&json!({ "thread": { "title": " Mine " }, "turns": turns })),
            json!("Mine")
        );
    }
}
