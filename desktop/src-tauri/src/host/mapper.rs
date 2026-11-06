//! CodeSmith runtime envelopes → client session wire events.
//!
//! One wire event: `{type, seq, time, data, surfaceOp?}` — `surfaceOp`
//! is required on `user/message`, `assistant/message`, `system/message`,
//! `developer/message` and `tool/result` (assertSessionWireEvent rejects
//! any other key on the envelope).
//!
//! Known limitations: `item.delta` envelopes are not projected live in the
//! first cut — the transcript settles on `assistant/message` when the item
//! completes (assistant-stream frames arrive with M2.5); tool items lose
//! their specific tool name in the durable record, so `tool/call` recovers
//! it from the item summary prefix when possible.

use serde_json::{Value, json};

/// Allocates the monotone per-session wire seq.
#[derive(Default)]
pub struct WireSeq {
    next: u64,
}

impl WireSeq {
    pub fn resume_after(last: u64) -> Self {
        Self { next: last }
    }

    pub fn event(&mut self, kind: &str, data: Value, surface_append: bool) -> Value {
        self.next += 1;
        let mut event = json!({
            "type": kind,
            "seq": self.next,
            "time": chrono::Utc::now().timestamp_millis(),
            "data": data,
        });
        if surface_append {
            event["surfaceOp"] = json!("append");
        }
        event
    }
}

fn content_text(detail: Option<&str>) -> String {
    detail.unwrap_or_default().to_string()
}

/// Replay a `ThreadDetail` (GET /v1/threads/{id}) into wire events.
/// Returns the events in order; turns still running get no `turn/end`.
pub fn replay_events(detail: &Value) -> Vec<Value> {
    let mut seq = WireSeq::default();
    let mut events = Vec::new();
    let turns = detail.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
    let items = detail.get("items").and_then(Value::as_array).cloned().unwrap_or_default();

    let turn_end_reason = |turn: &Value| -> Option<&'static str> {
        match turn.get("status").and_then(Value::as_str) {
            Some("completed") => Some("completed"),
            Some("interrupted") => Some("interrupted"),
            Some("error") | Some("failed") => Some("error"),
            _ => None, // running / pending: no end yet
        }
    };

    for turn in &turns {
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        events.push(seq.event("turn/start", json!({ "turn": turn_id }), false));
        let mut turn_items: Vec<&Value> = items
            .iter()
            .filter(|item| item.get("turn_id").and_then(Value::as_str) == Some(turn_id.as_str()))
            .collect();
        turn_items.sort_by_key(|item| {
            item.get("started_at").and_then(Value::as_str).unwrap_or_default().to_string()
        });
        for item in turn_items {
            map_item_to_events(&mut seq, item, &turn_id, &mut events);
        }
        if let Some(reason) = turn_end_reason(turn) {
            events.push(seq.event(
                "turn/end",
                json!({ "turn": turn_id, "reason": { "kind": reason } }),
                false,
            ));
        }
    }
    events
}

/// One durable item → its wire events (appended to `events`).
fn map_item_to_events(seq: &mut WireSeq, item: &Value, turn_id: &str, events: &mut Vec<Value>) {
    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    let detail = item.get("detail").and_then(Value::as_str);
    let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
    let status = item.get("status").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "user_message" => events.push(seq.event(
            "user/message",
            json!({
                "message": {
                    "id": item_id,
                    "role": "user",
                    "content": [{ "type": "text", "text": content_text(detail) }],
                    "source": { "kind": "user" },
                }
            }),
            true,
        )),
        "agent_message" => events.push(seq.event(
            "assistant/message",
            json!({
                "turn": turn_id,
                "message": {
                    "id": item_id,
                    "role": "assistant",
                    "content": [{ "type": "text", "text": content_text(detail) }],
                }
            }),
            true,
        )),
        "agent_reasoning" => events.push(seq.event(
            "assistant/message",
            json!({
                "turn": turn_id,
                "message": {
                    "id": item_id,
                    "role": "assistant",
                    "content": [{ "type": "reasoning", "text": content_text(detail) }],
                }
            }),
            true,
        )),
        "tool_call" | "command_execution" | "file_change" => {
            // Durable tool items: the start carried {tool:{id,name,input}} on
            // the wire, but the stored item keeps only summary/detail. The
            // summary is "<name>: <output>" after completion and
            // "<name> started" while in progress.
            let summary = item.get("summary").and_then(Value::as_str).unwrap_or_default();
            let name = summary.split(':').next().unwrap_or("tool").trim().to_string();
            events.push(seq.event(
                "tool/call",
                json!({ "turn": turn_id, "callId": item_id, "name": name, "arguments": {} }),
                false,
            ));
            if status != "in_progress" {
                events.push(seq.event(
                    "tool/result",
                    json!({
                        "turn": turn_id,
                        "message": {
                            "toolCallId": item_id,
                            "isError": status == "failed",
                            "content": [{ "type": "text", "text": content_text(detail) }],
                        }
                    }),
                    true,
                ));
            }
        }
        "context_compaction" => events.push(seq.event(
            "system/message",
            json!({
                "message": { "id": item_id, "content": [{ "type": "text", "text": content_text(detail) }] }
            }),
            true,
        )),
        // Error/status items become visible system messages — a failed turn
        // must not vanish from the transcript.
        "error" | "status" => events.push(seq.event(
            "system/message",
            json!({
                "message": { "id": item_id, "content": [{ "type": "text", "text": content_text(detail) }] }
            }),
            true,
        )),
        _ => {}
    }
}

/// Map one live SSE envelope into zero or more wire events.
pub fn map_envelope(seq: &mut WireSeq, envelope: &Value) -> Vec<Value> {
    let mut events = Vec::new();
    let event_name = envelope.get("event").and_then(Value::as_str).unwrap_or_default();
    let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
    let turn_id = envelope.get("turn_id").and_then(Value::as_str).unwrap_or_default().to_string();
    match event_name {
        "turn.started" => events.push(seq.event("turn/start", json!({ "turn": turn_id }), false)),
        "turn.completed" | "turn.failed" => {
            let status = payload
                .get("turn")
                .and_then(|t| t.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let reason = match status {
                "interrupted" => "interrupted",
                "error" | "failed" => "error",
                _ => "completed",
            };
            events.push(seq.event(
                "turn/end",
                json!({ "turn": turn_id, "reason": { "kind": reason } }),
                false,
            ));
        }
        "item.started" | "item.completed" | "item.failed" => {
            if let Some(item) = payload.get("item") {
                // Live tool starts carry the real tool name/input payload.
                let mut item_with_tool = item.clone();
                if event_name == "item.started" {
                    if let Some(tool) = payload.get("tool") {
                        item_with_tool["summary"] = json!(format!(
                            "{} started",
                            tool.get("name").and_then(Value::as_str).unwrap_or("tool")
                        ));
                        // Keep the input for arguments on the live path.
                        item_with_tool["metadata"] = json!({
                            "tool_name": tool.get("name").cloned().unwrap_or(Value::Null),
                            "tool_input": tool.get("input").cloned().unwrap_or(Value::Null),
                        });
                    }
                }
                // Route through the durable-item mapper for consistency;
                // in-progress items yield call-only, completed yield both.
                let mut item_events = Vec::new();
                map_item_to_events(&mut *seq, &item_with_tool, &turn_id, &mut item_events);
                events.append(&mut item_events);
            }
        }
        // item.delta / thread.* / turn.steered / turn.interrupt_requested:
        // the transcript settles from the completed items; deltas arrive
        // with the assistant-stream bridge (M2.5).
        _ => {}
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &str, status: &str, detail: &str) -> Value {
        json!({
            "id": format!("item_{kind}"),
            "turn_id": "turn_1",
            "kind": kind,
            "status": status,
            "detail": detail,
            "summary": if kind.starts_with("tool") || kind == "command_execution" || kind == "file_change" {
                format!("bash: {detail}")
            } else { detail.to_string() },
            "started_at": "2026-11-06T10:00:00Z",
        })
    }

    #[test]
    fn replay_maps_kinds_with_surface_ops_and_monotone_seq() {
        let detail = json!({
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [
                item("user_message", "completed", "hello"),
                item("agent_reasoning", "completed", "thinking"),
                item("agent_message", "completed", "hi there"),
                item("command_execution", "completed", "ok"),
            ],
        });
        let events = replay_events(&detail);
        let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            vec![
                "turn/start", "user/message", "assistant/message", "assistant/message",
                "tool/call", "tool/result", "turn/end"
            ]
        );
        let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        let mut sorted = seqs.clone();
        sorted.sort();
        assert_eq!(seqs, sorted, "seq must be monotone");
        for kind in ["user/message", "assistant/message", "tool/result"] {
            assert!(
                events.iter().any(|e| e["type"] == kind && e["surfaceOp"] == "append"),
                "{kind} must carry surfaceOp append"
            );
        }
        assert_eq!(events[1]["data"]["message"]["content"][0]["text"], "hello");
        assert_eq!(
            events[2]["data"]["message"]["content"][0]["type"], "reasoning",
            "agent_reasoning settles as a reasoning content block"
        );
        assert_eq!(events[4]["data"]["name"], "bash");
    }

    #[test]
    fn running_turn_gets_no_end_and_live_envelopes_map() {
        let detail = json!({
            "turns": [{ "id": "turn_9", "status": "running" }],
            "items": [item("agent_message", "in_progress", "")],
        });
        let events = replay_events(&detail);
        assert!(!events.iter().any(|e| e["type"] == "turn/end"));

        let mut seq = WireSeq::resume_after(2);
        let live = map_envelope(
            &mut seq,
            &json!({ "event": "turn.started", "turn_id": "turn_9", "payload": {} }),
        );
        assert_eq!(live[0]["type"], "turn/start");
        let live = map_envelope(
            &mut seq,
            &json!({
                "event": "turn.completed", "turn_id": "turn_9",
                "payload": { "turn": { "status": "interrupted" } }
            }),
        );
        assert_eq!(live[0]["data"]["reason"]["kind"], "interrupted");
        assert_eq!(live[0]["seq"], 4, "seq continues across calls");
    }
}
