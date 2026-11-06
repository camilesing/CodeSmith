//! `session/follow`, `session/page`, `session/prompt`, `session/cancel`
//! translated onto `codesmith serve --http`.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::HostState;
use super::mapper::{WireSeq, map_envelope, replay_events};

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
        "assistantStream": { "revision": 0, "activeAttempt": null },
    });
    tx.send(serde_json::to_string(&snapshot)?).await.ok();

    // Live tail: subscribe from the backend's own latest seq.
    let since = detail.get("latest_seq").and_then(Value::as_u64).unwrap_or(0);
    let mut lines = host
        .backend
        .get_sse_lines(&format!("/v1/threads/{session_id}/events?since_seq={since}"))
        .await?;
    let mut wire = WireSeq::resume_after(last_seq);
    use futures_util::StreamExt;
    let mut buffer = String::new();
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
                for event in map_envelope(&mut wire, &envelope) {
                    let frame = json!({ "type": "event", "event": event });
                    if tx.send(serde_json::to_string(&frame)?).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
    Ok(())
}

/// `session/page` — history window before a seq, from the replayed journal.
pub async fn page(host: &HostState, request: &Value) -> Result<Value> {
    let session_id = address_session_id(request)?;
    let before_seq = request
        .get("beforeSeq")
        .or_else(|| request.get("request").and_then(|r| r.get("beforeSeq")))
        .and_then(Value::as_u64);
    let detail: Value = host.backend.get(&format!("/v1/threads/{session_id}")).await?;
    let events = replay_events(&detail);
    let selected: Vec<Value> = match before_seq {
        Some(before) => events.into_iter().filter(|e| e["seq"].as_u64().unwrap_or(0) < before).collect(),
        None => events,
    };
    Ok(json!({
        "records": selected.into_iter().map(|event| json!({ "type": "event", "event": event })).collect::<Vec<_>>(),
        "hasMore": false,
        "cursor": 0,
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
                .find(|turn| turn.get("status").and_then(Value::as_str) == Some("running"))
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
