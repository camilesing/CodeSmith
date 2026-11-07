//! `session/follow`, `session/page`, `session/prompt`, `session/cancel`,
//! `session/rename|fork|search`, archive toggles, the skills catalog and the
//! control projections — translated onto `codesmith serve --http`.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::mapper::{replay_events, Frame, SessionMapper};
use super::HostState;

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

/// Snapshot + live frames for `session/follow`. Yields the snapshot first;
/// live envelopes arrive as `{type:'event'}` frames on the returned channel.
pub async fn follow(
    host: &HostState,
    request: &Value,
    tx: &tokio::sync::mpsc::Sender<String>,
) -> Result<()> {
    let session_id = address_session_id(request)?;
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    let events = replay_events(&detail);
    let last_seq = events.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0);
    let title = detail
        .pointer("/thread/title")
        .and_then(Value::as_str)
        .unwrap_or("New Session");
    // The seat's model chip reads the session's modelSelection projection; a
    // thread without an explicit model resolves to the catalog default so a
    // fresh session never waits on a projection nobody will push.
    let values = projection_values_with_model(host, &detail).await;
    let snapshot = json!({
        "type": "snapshot",
        "header": { "sessionId": session_id, "title": title },
        "cursor": last_seq,
        "records": events.into_iter().map(|event| json!({ "type": "event", "event": event })).collect::<Vec<_>>(),
        "hasMore": false,
        "projections": { "asOfSeq": last_seq, "values": values },
        // Revision 0 = no assistant-stream frames yet; the live tail's frames
        // start at 1 (strict +1 per frame, enforced by the client).
        "assistantStream": { "revision": 0 },
    });
    tx.send(serde_json::to_string(&snapshot)?).await.ok();

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
    use futures_util::StreamExt;
    let mut buffer = String::new();
    let send = |frame: &Frame| -> Result<String> {
        serde_json::to_string(&frame.to_wire()).context("serialize follow frame")
    };
    while let Some(chunk) = lines.next().await {
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
                match envelope
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "approval.required" => {
                        if let Some((event_id, frame)) =
                            super::events::approval_waterfall(&session_id, &envelope["payload"])
                        {
                            host.push_event_frame(frame).await;
                            host.pending_inputs.lock().await.insert(
                                event_id.clone(),
                                super::events::PendingInput {
                                    session_id: session_id.clone(),
                                    input_id: event_id.clone(),
                                    kind: super::events::InputKind::Approval,
                                },
                            );
                        }
                    }
                    "user_input.required" => {
                        if let Some((input_id, frame)) =
                            super::events::questions_waterfall(&session_id, &envelope["payload"])
                        {
                            host.pending_inputs.lock().await.insert(
                                input_id.clone(),
                                super::events::PendingInput {
                                    session_id: session_id.clone(),
                                    input_id: input_id.clone(),
                                    kind: super::events::InputKind::Questions,
                                },
                            );
                            host.push_event_frame(frame).await;
                        }
                    }
                    "approval.decided" | "approval.timeout" => {
                        if let Some(id) = envelope["payload"]
                            .get("id")
                            .or_else(|| envelope["payload"].get("approval_id"))
                            .and_then(Value::as_str)
                        {
                            host.cancel_pending(id).await;
                        }
                    }
                    "turn.completed" | "turn.failed" => {
                        host.cancel_pending_for_session(&session_id).await;
                        // Refresh the control projections (context pressure,
                        // model selection) for this session now that its turn
                        // settled with fresh usage.
                        push_control_projection(&host, &session_id).await;
                    }
                    _ => {}
                }
                let mut frames = mapper.map_envelope(&envelope);
                // A user message that settles a submitted prompt carries the
                // prompt's rpcId, so the client retires its optimistic copy
                // instead of rendering the message twice.
                let is_user_echo = envelope.get("event").and_then(Value::as_str)
                    == Some("item.started")
                    && envelope
                        .pointer("/payload/item/kind")
                        .and_then(Value::as_str)
                        == Some("user_message");
                if is_user_echo {
                    let next_rpc = host
                        .pending_rpc_ids
                        .lock()
                        .await
                        .get_mut(&session_id)
                        .and_then(|queue| (!queue.is_empty()).then(|| queue.remove(0)));
                    if let Some(rpc) = next_rpc {
                        if let Some(Frame::Event(event)) = frames.first_mut() {
                            if event["type"] == "user/message" {
                                event["data"]["source"]["rpcId"] = json!(rpc);
                            }
                        }
                    }
                }
                for frame in frames {
                    let wire = send(&frame)?;
                    if tx.send(wire).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
    // The SSE tail ended (backend dropped the stream): close any attempt
    // still streaming so the client does not wait for a settlement.
    for frame in mapper.abandon_open_attempts() {
        let wire = send(&frame)?;
        if tx.send(wire).await.is_err() {
            break;
        }
    }
    Ok(())
}

/// `session/page` — one history window from the replayed journal.
/// Contract (client journal-stream): the page's last record seq must equal
/// the requested `throughSeq` (inclusive cut; omitted = log end), records
/// stay under `beforeSeq` (exclusive), `hasMore` reports earlier history.
pub async fn page(host: &HostState, request: &Value) -> Result<Value> {
    let session_id = address_session_id(request)?;
    let through_seq = request.get("throughSeq").and_then(Value::as_i64);
    let before_seq = request.get("beforeSeq").and_then(Value::as_u64);
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await?;
    let events = replay_events(&detail);
    let last = events.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0);
    let cut = through_seq.map(|t| t.max(0) as u64).unwrap_or(last);
    let selected: Vec<Value> = events
        .into_iter()
        .filter(|e| {
            let seq = e["seq"].as_u64().unwrap_or(0);
            seq <= cut && before_seq.is_none_or(|before| seq < before)
        })
        .collect();
    let has_more = selected
        .first()
        .and_then(|e| e["seq"].as_u64())
        .is_some_and(|first| first > 1);
    Ok(json!({
        "records": selected.into_iter().map(|event| json!({ "type": "event", "event": event })).collect::<Vec<_>>(),
        "hasMore": has_more,
    }))
}

#[derive(serde::Deserialize)]
struct PromptPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

fn prompt_text(content: &[PromptPart]) -> Result<String> {
    let text: Vec<&str> = content
        .iter()
        .filter(|p| p.kind == "text")
        .map(|p| p.text.as_str())
        .collect();
    if text.is_empty() {
        bail!("prompt without a text part");
    }
    Ok(text.join("\n"))
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

/// `session/prompt` — queue starts a turn; steer rides the active turn.
/// The request's `rpcId` is remembered so the follow loop can stamp the
/// settling `user/message` echo (the client's optimistic-copy dedupe key).
pub async fn prompt(host: &HostState, request: &Value) -> Result<Value> {
    let inner = request.get("request").unwrap_or(request);
    let session_id = inner
        .get("sessionId")
        .or_else(|| inner.get("session_id"))
        .and_then(Value::as_str)
        .context("prompt without sessionId")?
        .to_string();
    let mode = inner.get("mode").and_then(Value::as_str).unwrap_or("queue");
    let rpc_id = inner
        .get("rpcId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let content: Vec<PromptPart> = serde_json::from_value(
        inner
            .get("content")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    )?;
    let text = prompt_text(&content)?;

    if mode == "steer" {
        match active_turn_id(host, &session_id).await? {
            Some(turn_id) => {
                let _: Value = host
                    .backend
                    .post(
                        &format!("/v1/threads/{session_id}/turns/{turn_id}/steer"),
                        &json!({ "prompt": text }),
                    )
                    .await?;
                if let Some(rpc) = rpc_id {
                    host.pending_rpc_ids
                        .lock()
                        .await
                        .entry(session_id)
                        .or_default()
                        .push(rpc);
                }
            }
            None => bail!("steer with no running turn"),
        }
        return Ok(json!({ "accepted": true }));
    }
    let _: Value = host
        .backend
        .post(
            &format!("/v1/threads/{session_id}/turns"),
            &json!({ "prompt": text }),
        )
        .await?;
    if let Some(rpc) = rpc_id {
        host.pending_rpc_ids
            .lock()
            .await
            .entry(session_id)
            .or_default()
            .push(rpc);
    }
    Ok(json!({ "accepted": true }))
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

/// `session/selectModel` — record the session's model for later turns.
/// Known limitation: the runtime thread takes one model id (no provider
/// axis), so the catalog's provider field is echoed back but not forwarded.
pub async fn select_model(host: &HostState, request: &Value) -> Result<Value> {
    let session_id = request
        .get("sessionId")
        .or_else(|| request.get("session_id"))
        .and_then(Value::as_str)
        .context("selectModel without sessionId")?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .context("selectModel without model")?;
    let provider = request
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
/// projection at the returned seq, so the echo is the thread's latest seq.
pub async fn rename(host: &HostState, request: &Value) -> Result<Value> {
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
    let detail: Value = host
        .backend
        .get(&format!("/v1/threads/{session_id}"))
        .await
        .context("re-read renamed thread")?;
    let seq = detail
        .get("latest_seq")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(json!({ "title": title, "seq": seq }))
}

/// `session/fork` — POST the fork route; the child thread record names the
/// new session.
pub async fn fork(host: &HostState, request: &Value) -> Result<Value> {
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
    // The fork lands in the same implicit workspace: fold it into every open
    // workspace follow stream, the same way session/create publishes.
    let view = super::workspace_view_live(host).await;
    host.push_live(
        "workspace/follow",
        json!({ "type": "upsert", "workspace": view }),
    )
    .await;
    Ok(json!({ "sessionId": child_id }))
}

/// `session/search` — the runtime summary search matches thread content; the
/// preview snippet rides as the hit's snippet.
pub async fn search(host: &HostState, request: &Value) -> Result<Value> {
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
    // Fold the new list into every open workspace follow stream.
    let view = super::workspace_view_live(host).await;
    host.push_live(
        "workspace/follow",
        json!({
            "type": "baseline",
            "value": { "items": [view], "archivedSessionIds": archived_ids, "pinnedSessionIds": [] }
        }),
    )
    .await;
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

/// The catalog's default selection, when the runtime advertises models.
async fn catalog_default_model(host: &HostState) -> Option<(String, String)> {
    catalog_selection(host, None).await
}

/// The (provider, model) pair for a model id — the runtime's own provider
/// naming, so projection values match the catalog groups verbatim. A `None`
/// model id resolves to the catalog default.
async fn catalog_selection(host: &HostState, model_id: Option<&str>) -> Option<(String, String)> {
    #[derive(serde::Deserialize)]
    struct ModelEntry {
        id: String,
        provider: String,
        default: bool,
    }
    let models: Vec<ModelEntry> = host.backend.get("/v1/models").await.ok()?;
    let chosen = match model_id {
        Some(id) => models.iter().find(|model| model.id == id),
        None => models.iter().find(|model| model.default),
    }
    .or_else(|| models.first())?;
    Some((chosen.provider.clone(), chosen.id.clone()))
}

/// One session's projection values the bridge can compute from the runtime:
/// context pressure (the latest turn's provider-reported prompt size) and the
/// model selection. Everything else stays absent per the projection contract.
fn projection_values(detail: &Value) -> Value {
    // The latest completed turn's usage is the newest provider sample.
    let pressure = detail
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
        });
    let model = detail.pointer("/thread/model").and_then(Value::as_str);
    let mut values = json!({});
    let obj = values.as_object_mut().unwrap();
    if let Some(tokens) = pressure {
        obj.insert(
            "contextPressure".into(),
            json!({ "pressureTokens": tokens, "projectedTokens": tokens }),
        );
    }
    values
}

/// `projection_values` with the model selection resolved against the catalog:
/// the thread record carries only the model id, and the provider comes from
/// the runtime's own model list so the value matches the catalog groups
/// verbatim (the directory rejects a selection whose provider has no group).
async fn projection_values_with_model(host: &HostState, detail: &Value) -> Value {
    let mut values = projection_values(detail);
    if values.get("modelSelection").is_some() {
        return values;
    }
    let model = detail.pointer("/thread/model").and_then(Value::as_str);
    if let Some((provider, model)) = catalog_selection(host, model).await {
        if let Some(obj) = values.as_object_mut() {
            // ModelSelectionProjection: `next` feeds the next model step;
            // `lastUsed` is unknown from the thread record alone.
            obj.insert(
                "modelSelection".into(),
                json!({ "lastUsed": null, "next": { "provider": provider, "model": model } }),
            );
        }
    }
    values
}

/// `session/control` baseline: projections for the most recently used
/// sessions (bounded — one thread read each; deeper history stays absent
/// until its session settles a turn, which pushes a live projection frame).
pub async fn control_baseline(host: &HostState) -> Result<Vec<Value>> {
    #[derive(serde::Deserialize)]
    struct Summary {
        id: String,
        #[serde(default)]
        model: Option<String>,
    }
    let summaries: Vec<Summary> = host
        .backend
        .get("/v1/threads/summary?limit=20")
        .await
        .unwrap_or_default();
    // Every listed session gets a modelSelection (thread model, else the
    // catalog default) so a fresh session's model seat never waits on a
    // projection nobody would push; context pressure still needs the detail.
    let fallback = catalog_default_model(host).await;
    let mut projections = serde_json::Map::new();
    for summary in summaries {
        let mut values = json!({});
        let selection = match summary.model.as_deref().filter(|m| !m.is_empty()) {
            Some(model) => catalog_selection(host, Some(model))
                .await
                .or(fallback.clone()),
            None => fallback.clone(),
        };
        if let Some((provider, model)) = selection {
            values["modelSelection"] =
                json!({ "lastUsed": null, "next": { "provider": provider, "model": model } });
        }
        if let Ok(detail) = host
            .backend
            .get::<Value>(&format!("/v1/threads/{}", summary.id))
            .await
        {
            let seq = detail
                .get("latest_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if let (Some(target), Some(source)) = (
                values.as_object_mut(),
                projection_values(&detail).as_object(),
            ) {
                for (key, item) in source {
                    target.insert(key.clone(), item.clone());
                }
            }
            projections.insert(
                summary.id.clone(),
                json!({ "asOfSeq": seq, "values": values }),
            );
        } else if values.as_object().is_some_and(|o| !o.is_empty()) {
            projections.insert(summary.id, json!({ "asOfSeq": 0, "values": values }));
        }
    }
    Ok(vec![
        json!({ "type": "baseline", "value": { "projections": projections } }),
    ])
}

/// Push one session's refreshed projections to every open `session/control`
/// stream (the follow loop calls this when a turn settles).
pub async fn push_control_projection(host: &HostState, session_id: &str) {
    let Ok(detail) = host
        .backend
        .get::<Value>(&format!("/v1/threads/{session_id}"))
        .await
    else {
        return;
    };
    let seq = detail
        .get("latest_seq")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let changes = projection_values_with_model(host, &detail).await;
    if changes.as_object().is_some_and(|o| !o.is_empty()) {
        host.push_live(
            "session/control",
            json!({ "type": "projection", "sessionId": session_id, "seq": seq, "changes": changes }),
        )
        .await;
    }
}
