//! `session/follow`, `session/page`, `session/prompt`, `session/cancel`
//! translated onto `codesmith serve --http`.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::HostState;
use super::mapper::{Frame, SessionMapper, replay_events};

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
    let detail: Value = host.backend.get(&format!("/v1/threads/{session_id}")).await?;
    let events = replay_events(&detail);
    let last_seq = events.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0);
    let title = detail
        .pointer("/thread/title")
        .and_then(Value::as_str)
        .unwrap_or("New Session");
    let snapshot = json!({
        "type": "snapshot",
        "header": { "sessionId": session_id, "title": title },
        "cursor": last_seq,
        "records": events.into_iter().map(|event| json!({ "type": "event", "event": event })).collect::<Vec<_>>(),
        "hasMore": false,
        "projections": { "asOfSeq": last_seq, "values": {} },
        // Revision 0 = no assistant-stream frames yet; the live tail's frames
        // start at 1 (strict +1 per frame, enforced by the client).
        "assistantStream": { "revision": 0 },
    });
    tx.send(serde_json::to_string(&snapshot)?).await.ok();

    // Live tail: subscribe from the backend's own latest seq.
    let since = detail.get("latest_seq").and_then(Value::as_u64).unwrap_or(0);
    let mut lines = host
        .backend
        .get_sse_lines(&format!("/v1/threads/{session_id}/events?since_seq={since}"))
        .await?;
    let mut mapper = SessionMapper::resume_after(last_seq);
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
                match envelope.get("event").and_then(Value::as_str).unwrap_or_default() {
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
                    }
                    _ => {}
                }
                for frame in mapper.map_envelope(&envelope) {
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
    let detail: Value = host.backend.get(&format!("/v1/threads/{session_id}")).await?;
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
    let text: Vec<&str> = content.iter().filter(|p| p.kind == "text").map(|p| p.text.as_str()).collect();
    if text.is_empty() {
        bail!("prompt without a text part");
    }
    Ok(text.join("\n"))
}

async fn active_turn_id(host: &HostState, session_id: &str) -> Result<Option<String>> {
    let detail: Value = host.backend.get(&format!("/v1/threads/{session_id}")).await?;
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
pub async fn prompt(host: &HostState, request: &Value) -> Result<Value> {
    let inner = request.get("request").unwrap_or(request);
    let session_id = inner
        .get("sessionId")
        .or_else(|| inner.get("session_id"))
        .and_then(Value::as_str)
        .context("prompt without sessionId")?;
    let mode = inner.get("mode").and_then(Value::as_str).unwrap_or("queue");
    let content: Vec<PromptPart> =
        serde_json::from_value(inner.get("content").cloned().unwrap_or(Value::Array(Vec::new())))?;
    let text = prompt_text(&content)?;

    if mode == "steer" {
        match active_turn_id(host, session_id).await? {
            Some(turn_id) => {
                let _: Value = host
                    .backend
                    .post(
                        &format!("/v1/threads/{session_id}/turns/{turn_id}/steer"),
                        &json!({ "prompt": text }),
                    )
                    .await?;
            }
            None => bail!("steer with no running turn"),
        }
        return Ok(json!({ "accepted": true }));
    }
    let _: Value = host
        .backend
        .post(&format!("/v1/threads/{session_id}/turns"), &json!({ "prompt": text }))
        .await?;
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
            .post_empty(&format!("/v1/threads/{session_id}/turns/{turn_id}/interrupt"))
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
        .patch(&format!("/v1/threads/{session_id}"), &json!({ "model": model }))
        .await
        .context("patch thread model")?;
    Ok(json!({ "selected": { "provider": provider, "model": model } }))
}
