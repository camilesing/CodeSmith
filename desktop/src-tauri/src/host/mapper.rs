//! CodeSmith runtime envelopes → client session wire events + assistant stream.
//!
//! Wire event: `{type, seq, time, data, surfaceOp?}` — `surfaceOp` is required
//! on `user/message`, `assistant/message`, `system/message`, `developer/message`
//! and `tool/result` (the client's `assertSessionWireEvent` rejects any other
//! envelope key). Turns and steps are numeric ordinals (`turn/start {turn}`,
//! `step/start {turn, step}`), matching the upstream session vocabulary the
//! chat view groups nodes by (`${turn}:${step}`).
//!
//! Live `item.delta` envelopes fold into `assistant-stream` frames
//! (`{type:'assistant-stream', frame}`): one attempt per agent item, chunks
//! shaped `block-start` / `text-delta` / `reasoning-delta` / `block-end` with
//! block index 0 (each agent item is a single content block). The frame
//! `revision` increments by exactly 1 on every stream frame — the client's
//! follow transport enforces `revision === previous + 1` and drops the stream
//! otherwise. On item completion the durable `assistant/message` event is
//! emitted BEFORE the `end{committed}` frame: the client fold stages the
//! settlement while the attempt is open and only publishes it when the end
//! frame names its seq (an end frame with nothing staged forces a rebaseline).
//!
//! Known limitations: tool-call output deltas are not streamed (the tool node
//! settles from `tool/result`); durable tool items lose their tool name, so
//! `tool/call` recovers it from the item summary prefix on replay; an item
//! whose `item.started` was missed (follow opened mid-item) is adopted with a
//! synthesized `step/start` + stream start, so its block may miss early deltas.

use std::collections::HashMap;

use serde_json::{Value, json};

/// One follow-stream frame: a durable wire event or an assistant-stream frame.
#[derive(Debug, Clone)]
pub enum Frame {
    Event(Value),
    Stream(Value),
}

impl Frame {
    /// The follow-stream JSON body for this frame.
    pub fn to_wire(&self) -> Value {
        match self {
            Frame::Event(event) => json!({ "type": "event", "event": event }),
            Frame::Stream(frame) => json!({ "type": "assistant-stream", "frame": frame }),
        }
    }
}

/// Allocates the monotone per-session wire seq.
#[derive(Default)]
pub struct WireSeq {
    next: u64,
}

impl WireSeq {
    pub fn resume_after(last: u64) -> Self {
        Self { next: last }
    }

    pub fn last(&self) -> u64 {
        self.next
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

/// One live assistant attempt (one agent item being streamed).
struct Attempt {
    /// Dense per-attempt chunk counter; the client rebaselines on any gap.
    chunk_index: u64,
    /// Whether `block-start` was already announced for block 0.
    block_open: bool,
}

/// Live fold state for one session's follow stream.
#[derive(Default)]
pub struct SessionMapper {
    seq: WireSeq,
    /// Assistant-stream revision; +1 on every emitted stream frame.
    revision: u64,
    /// Numeric turn ordinal (1-based); the runtime turn id is not numeric.
    turn_no: u64,
    /// Current step ordinal within the turn (agent items increment it).
    step: u64,
    attempts: HashMap<String, Attempt>,
}

impl SessionMapper {
    pub fn resume_after(last_seq: u64) -> Self {
        Self {
            seq: WireSeq::resume_after(last_seq),
            ..Self::default()
        }
    }

    fn started_after_seq(&self) -> i64 {
        if self.seq.last() == 0 {
            -1
        } else {
            self.seq.last() as i64
        }
    }

    /// Emit the `start` frame and register the attempt for `item_id`.
    fn start_attempt(&mut self, item_id: &str) -> Option<Frame> {
        if self.attempts.contains_key(item_id) {
            return None;
        }
        let started_after_seq = self.started_after_seq();
        let turn = self.turn_no;
        let step = self.step;
        self.revision += 1;
        let frame = json!({
            "type": "start",
            "attemptId": item_id,
            "revision": self.revision,
            "startedAfterSeq": started_after_seq,
            "turn": turn,
            "step": step,
        });
        self.attempts.insert(
            item_id.to_string(),
            Attempt { chunk_index: 0, block_open: false },
        );
        Some(Frame::Stream(frame))
    }

    /// Emit one chunk frame for a registered attempt.
    fn chunk(&mut self, item_id: &str, chunk: Value) -> Option<Frame> {
        let revision = &mut self.revision;
        let attempt = self.attempts.get_mut(item_id)?;
        *revision += 1;
        let frame = json!({
            "type": "chunk",
            "attemptId": item_id,
            "revision": *revision,
            "index": attempt.chunk_index,
            "time": chrono::Utc::now().timestamp_millis(),
            "chunk": chunk,
        });
        attempt.chunk_index += 1;
        Some(Frame::Stream(frame))
    }

    /// Map one live SSE envelope into follow frames.
    pub fn map_envelope(&mut self, envelope: &Value) -> Vec<Frame> {
        let mut frames = Vec::new();
        let event_name = envelope.get("event").and_then(Value::as_str).unwrap_or_default();
        let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
        let item_id = envelope
            .get("item_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match event_name {
            "turn.started" => {
                self.turn_no += 1;
                self.step = 0;
                frames.push(Frame::Event(
                    self.seq.event("turn/start", json!({ "turn": self.turn_no }), false),
                ));
            }
            "turn.completed" | "turn.failed" => {
                let status = payload
                    .pointer("/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                let reason = match status {
                    "interrupted" => "interrupted",
                    "error" | "failed" => "error",
                    _ => "completed",
                };
                frames.push(Frame::Event(self.seq.event(
                    "turn/end",
                    json!({ "turn": self.turn_no, "reason": { "kind": reason } }),
                    false,
                )));
            }
            "item.started" => {
                let item = payload.get("item").cloned().unwrap_or(Value::Null);
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
                match kind {
                    "agent_message" | "agent_reasoning" => {
                        self.step += 1;
                        frames.push(Frame::Event(self.seq.event(
                            "step/start",
                            json!({ "turn": self.turn_no, "step": self.step }),
                            false,
                        )));
                        if let Some(frame) = self.start_attempt(&item_id) {
                            frames.push(frame);
                        }
                    }
                    "tool_call" | "command_execution" | "file_change" => {
                        let tool = payload.get("tool").cloned().unwrap_or(Value::Null);
                        let name = tool
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| tool_name_from_summary(&item));
                        let arguments = tool
                            .get("input")
                            .cloned()
                            .map(|input| input.to_string())
                            .unwrap_or_else(|| "{}".into());
                        frames.push(Frame::Event(self.seq.event(
                            "tool/call",
                            json!({
                                "turn": self.turn_no,
                                "step": self.step,
                                "callId": item_id,
                                "name": name,
                                "arguments": arguments,
                            }),
                            false,
                        )));
                    }
                    "user_message" => {
                        let detail = item.get("detail").and_then(Value::as_str);
                        frames.push(Frame::Event(self.seq.event(
                            "user/message",
                            // Flat UserMessage contract: the client's message
                            // node reads data.id and data.source.kind directly.
                            json!({
                                "id": item_id,
                                "role": "user",
                                "content": [{ "type": "text", "text": content_text(detail) }],
                                "source": { "kind": "user" },
                            }),
                            true,
                        )));
                    }
                    _ => {}
                }
            }
            "item.delta" => {
                let kind = payload.get("kind").and_then(Value::as_str).unwrap_or_default();
                let (block_type, chunk_type) = match kind {
                    "agent_message" => ("text", "text-delta"),
                    "agent_reasoning" => ("reasoning", "reasoning-delta"),
                    // Tool output deltas are not streamed; the tool node
                    // settles from its durable result.
                    _ => return frames,
                };
                let text = payload.get("delta").and_then(Value::as_str).unwrap_or_default();
                if !self.attempts.contains_key(&item_id) {
                    // Mid-item adoption: no step/start or stream start was
                    // seen. Synthesize both so the fold stays consistent.
                    self.step += 1;
                    frames.push(Frame::Event(self.seq.event(
                        "step/start",
                        json!({ "turn": self.turn_no, "step": self.step }),
                        false,
                    )));
                    if let Some(frame) = self.start_attempt(&item_id) {
                        frames.push(frame);
                    }
                }
                if let Some(attempt) = self.attempts.get_mut(&item_id) {
                    if !attempt.block_open {
                        attempt.block_open = true;
                        if let Some(frame) = self.chunk(
                            &item_id,
                            json!({ "type": "block-start", "index": 0, "blockType": block_type }),
                        ) {
                            frames.push(frame);
                        }
                    }
                }
                if let Some(frame) = self.chunk(
                    &item_id,
                    json!({ "type": chunk_type, "index": 0, "text": text }),
                ) {
                    frames.push(frame);
                }
            }
            "item.completed" | "item.failed" => {
                let item = payload.get("item").cloned().unwrap_or(Value::Null);
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
                let detail = item.get("detail").and_then(Value::as_str);
                let status = item.get("status").and_then(Value::as_str).unwrap_or_default();
                match kind {
                    "agent_message" | "agent_reasoning" => {
                        let block_type = if kind == "agent_message" { "text" } else { "reasoning" };
                        let text = content_text(detail);
                        if self.attempts.get(&item_id).is_some_and(|a| a.block_open) {
                            if let Some(frame) = self.chunk(
                                &item_id,
                                json!({ "type": "block-end", "index": 0, "block": { "type": block_type, "text": text } }),
                            ) {
                                frames.push(frame);
                            }
                        }
                        // The durable message event must precede the end frame
                        // (the client fold stages the settlement, then the end
                        // frame publishes it).
                        let message = self.seq.event(
                            "assistant/message",
                            json!({
                                "turn": self.turn_no,
                                "step": self.step,
                                "message": {
                                    "id": item_id,
                                    "role": "assistant",
                                    "content": [{ "type": block_type, "text": text }],
                                    // ModelMessageSource + empty stream records: the
                                    // usage fold reads source.provider/model and
                                    // stream.length on every settlement.
                                    "source": { "kind": "model", "provider": "codesmith", "model": "" },
                                },
                                "stream": []
                            }),
                            true,
                        );
                        let message_seq = message["seq"].as_u64().unwrap_or(0);
                        frames.push(Frame::Event(message));
                        if let Some(attempt) = self.attempts.remove(&item_id) {
                            self.revision += 1;
                            frames.push(Frame::Stream(json!({
                                "type": "end",
                                "attemptId": item_id,
                                "revision": self.revision,
                                "index": attempt.chunk_index,
                                "outcome": {
                                    "kind": "committed",
                                    "eventType": "assistant/message",
                                    "seq": message_seq,
                                },
                            })));
                        }
                        frames.push(Frame::Event(self.seq.event(
                            "step/end",
                            json!({ "turn": self.turn_no, "step": self.step }),
                            false,
                        )));
                    }
                    "tool_call" | "command_execution" | "file_change" => {
                        frames.push(Frame::Event(self.seq.event(
                            "tool/result",
                            json!({
                                "turn": self.turn_no,
                                "step": self.step,
                                // ToolResultMessage contract: source.callId is what the client's tool
// node matches the result to its call by.
"message": {
                                    "role": "tool",
                                    "source": { "kind": "tool", "callId": item_id },
                                    "toolCallId": item_id,
                                    "isError": status == "failed",
                                    "content": [{ "type": "text", "text": content_text(detail) }],
                                }
                            }),
                            true,
                        )));
                    }
                    // user_message settles at item.started (the durable
                    // item already carries its text); re-emitting here would
                    // duplicate the message.
                    "context_compaction" | "error" | "status" => {
                        frames.push(Frame::Event(self.seq.event(
                            "system/message",
                            json!({
                                "message": { "id": item_id, "content": [{ "type": "text", "text": content_text(detail) }] }
                            }),
                            true,
                        )));
                    }
                    _ => {}
                }
            }
            // thread.* / turn.steered / turn.interrupt_requested: the
            // transcript settles from the durable events above.
            _ => {}
        }
        frames
    }

    /// Close every still-open attempt as abandoned (SSE tail ended or
    /// reconnecting): the client retires the transient rows without a
    /// settlement.
    pub fn abandon_open_attempts(&mut self) -> Vec<Frame> {
        let mut frames = Vec::new();
        for (item_id, attempt) in std::mem::take(&mut self.attempts) {
            self.revision += 1;
            let _ = attempt;
            frames.push(Frame::Stream(json!({
                "type": "end",
                "attemptId": item_id,
                "revision": self.revision,
                "index": 0,
                "outcome": { "kind": "abandoned" },
            })));
        }
        frames
    }
}

/// Durable tool items keep only summary/detail; the summary is
/// "<name>: <output>" after completion and "<name> started" while running.
fn tool_name_from_summary(item: &Value) -> String {
    item.get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or("tool")
        .trim()
        .to_string()
}

/// Replay a `ThreadDetail` (GET /v1/threads/{id}) into wire events.
/// Returns the events in order; turns still running get no `turn/end`.
/// In-progress agent items are skipped (an unsettled assistant message has no
/// durable content; the live stream owns it while it runs).
pub fn replay_events(detail: &Value) -> Vec<Value> {
    let mut seq = WireSeq::default();
    let mut events = Vec::new();
    let turns = detail.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
    let items = detail.get("items").and_then(Value::as_array).cloned().unwrap_or_default();

    let turn_end_reason = |turn: &Value| -> Option<&'static str> {
        match turn.get("status").and_then(Value::as_str) {
            Some("completed") => Some("completed"),
            // Canceled and interrupted both read as an interrupted turn.
            Some("interrupted") | Some("canceled") => Some("interrupted"),
            Some("error") | Some("failed") => Some("error"),
            _ => None, // in_progress / queued: no end yet
        }
    };

    for (turn_index, turn) in turns.iter().enumerate() {
        let turn_no = (turn_index + 1) as u64;
        events.push(seq.event("turn/start", json!({ "turn": turn_no }), false));
        let turn_id = turn.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut turn_items: Vec<&Value> = items
            .iter()
            .filter(|item| item.get("turn_id").and_then(Value::as_str) == Some(turn_id.as_str()))
            .collect();
        turn_items.sort_by_key(|item| {
            item.get("started_at").and_then(Value::as_str).unwrap_or_default().to_string()
        });
        let mut step: u64 = 0;
        for item in turn_items {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let detail_text = item.get("detail").and_then(Value::as_str);
            let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
            let status = item.get("status").and_then(Value::as_str).unwrap_or_default();
            match kind {
                "user_message" => events.push(seq.event(
                    "user/message",
                    // Flat UserMessage contract (see the live arm above).
                    json!({
                        "id": item_id,
                        "role": "user",
                        "content": [{ "type": "text", "text": content_text(detail_text) }],
                        "source": { "kind": "user" },
                    }),
                    true,
                )),
                "agent_message" | "agent_reasoning" => {
                    if status == "in_progress" {
                        continue;
                    }
                    let block_type = if kind == "agent_message" { "text" } else { "reasoning" };
                    step += 1;
                    events.push(seq.event(
                        "step/start",
                        json!({ "turn": turn_no, "step": step }),
                        false,
                    ));
                    events.push(seq.event(
                        "assistant/message",
                        json!({
                            "turn": turn_no,
                            "step": step,
                            "message": {
                                "id": item_id,
                                "role": "assistant",
                                "content": [{ "type": block_type, "text": content_text(detail_text) }],
                                // ModelMessageSource + empty stream records
                                // (see the live arm).
                                "source": { "kind": "model", "provider": "codesmith", "model": "" },
                            },
                            "stream": []
                        }),
                        true,
                    ));
                    events.push(seq.event(
                        "step/end",
                        json!({ "turn": turn_no, "step": step }),
                        false,
                    ));
                }
                "tool_call" | "command_execution" | "file_change" => {
                    let name = tool_name_from_summary(item);
                    events.push(seq.event(
                        "tool/call",
                        json!({
                            "turn": turn_no,
                            "step": step,
                            "callId": item_id,
                            "name": name,
                            "arguments": "{}",
                        }),
                        false,
                    ));
                    if status != "in_progress" {
                        events.push(seq.event(
                            "tool/result",
                            json!({
                                "turn": turn_no,
                                "step": step,
                                // ToolResultMessage contract (see the live arm).
                                "message": {
                                    "role": "tool",
                                    "source": { "kind": "tool", "callId": item_id },
                                    "toolCallId": item_id,
                                    "isError": status == "failed",
                                    "content": [{ "type": "text", "text": content_text(detail_text) }],
                                }
                            }),
                            true,
                        ));
                    }
                }
                "context_compaction" | "error" | "status" => events.push(seq.event(
                    "system/message",
                    json!({
                        "message": { "id": item_id, "content": [{ "type": "text", "text": content_text(detail_text) }] }
                    }),
                    true,
                )),
                _ => {}
            }
        }
        if let Some(reason) = turn_end_reason(turn) {
            events.push(seq.event(
                "turn/end",
                json!({ "turn": turn_no, "reason": { "kind": reason } }),
                false,
            ));
        }
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
    fn replay_assigns_numeric_turns_and_steps() {
        let detail = json!({
            "turns": [
                { "id": "turn_1", "status": "completed" },
                { "id": "turn_2", "status": "completed" },
            ],
            "items": [
                item("user_message", "completed", "hello"),
                item("agent_reasoning", "completed", "thinking"),
                item("agent_message", "completed", "hi there"),
                item("command_execution", "completed", "ok"),
                item("agent_message", "in_progress", ""),
                { "id": "item_u2", "turn_id": "turn_2", "kind": "user_message",
                  "status": "completed", "detail": "again", "summary": "again",
                  "started_at": "2026-11-06T10:01:00Z" },
            ],
        });
        let events = replay_events(&detail);
        let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            vec![
                "turn/start", "user/message",
                "step/start", "assistant/message", "step/end",
                "step/start", "assistant/message", "step/end",
                "tool/call", "tool/result",
                // in-progress agent message skipped
                "turn/end",
                "turn/start", "user/message", "turn/end",
            ]
        );
        // Numeric turn ordinals, steps increment per agent item, tool items
        // ride the latest step.
        assert_eq!(events[0]["data"]["turn"], 1);
        // user/message rides the flat UserMessage contract — the client's
        // message node dereferences data.id and data.source.kind directly.
        assert_eq!(events[1]["data"]["id"], "item_user_message");
        assert_eq!(events[1]["data"]["source"]["kind"], "user");
        assert_eq!(events[1]["data"]["content"][0]["text"], "hello");
        assert_eq!(events[2]["data"]["step"], 1);
        assert_eq!(events[3]["data"]["step"], 1);
        assert_eq!(events[3]["data"]["turn"], 1);
        assert_eq!(events[5]["data"]["step"], 2);
        assert_eq!(events[8]["data"]["step"], 2);
        assert_eq!(events[11]["data"]["turn"], 2);
        // surfaceOp presence on the append-required kinds.
        for kind in ["user/message", "assistant/message", "tool/result"] {
            assert!(
                events.iter().any(|e| e["type"] == kind && e["surfaceOp"] == "append"),
                "{kind} must carry surfaceOp append"
            );
        }
        let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        let mut sorted = seqs.clone();
        sorted.sort();
        assert_eq!(seqs, sorted, "seq must be monotone");
    }

    #[test]
    fn live_stream_frames_carry_strict_revisions() {
        let mut mapper = SessionMapper::default();
        let mut all_frames = Vec::new();
        let envelopes = vec![
            json!({ "event": "turn.started", "turn_id": "turn_1", "timestamp": "2026-11-06T10:00:00Z", "payload": {} }),
            json!({ "event": "item.started", "item_id": "item_r", "timestamp": "2026-11-06T10:00:01Z",
                    "payload": { "item": { "id": "item_r", "kind": "agent_reasoning", "status": "in_progress", "detail": "" } } }),
            json!({ "event": "item.delta", "item_id": "item_r", "timestamp": "2026-11-06T10:00:02Z",
                    "payload": { "delta": "thin", "kind": "agent_reasoning" } }),
            json!({ "event": "item.delta", "item_id": "item_r", "timestamp": "2026-11-06T10:00:03Z",
                    "payload": { "delta": "king", "kind": "agent_reasoning" } }),
            json!({ "event": "item.completed", "item_id": "item_r", "timestamp": "2026-11-06T10:00:04Z",
                    "payload": { "item": { "id": "item_r", "kind": "agent_reasoning", "status": "completed", "detail": "thinking" } } }),
            json!({ "event": "item.started", "item_id": "item_m", "timestamp": "2026-11-06T10:00:05Z",
                    "payload": { "item": { "id": "item_m", "kind": "agent_message", "status": "in_progress", "detail": "" } } }),
            json!({ "event": "item.delta", "item_id": "item_m", "timestamp": "2026-11-06T10:00:06Z",
                    "payload": { "delta": "hi", "kind": "agent_message" } }),
            json!({ "event": "item.completed", "item_id": "item_m", "timestamp": "2026-11-06T10:00:07Z",
                    "payload": { "item": { "id": "item_m", "kind": "agent_message", "status": "completed", "detail": "hi" } } }),
            json!({ "event": "turn.completed", "turn_id": "turn_1", "timestamp": "2026-11-06T10:00:08Z",
                    "payload": { "turn": { "status": "completed" } } }),
        ];
        for envelope in &envelopes {
            all_frames.extend(mapper.map_envelope(envelope));
        }

        // Revisions across every stream frame are exactly 1..=N in order.
        let revisions: Vec<u64> = all_frames
            .iter()
            .filter_map(|f| match f {
                Frame::Stream(frame) => Some(frame["revision"].as_u64().unwrap()),
                Frame::Event(_) => None,
            })
            .collect();
        assert_eq!(
            revisions,
            (1..=revisions.len() as u64).collect::<Vec<_>>(),
            "stream revisions must be strictly contiguous from 1"
        );

        let kinds: Vec<String> = all_frames
            .iter()
            .map(|f| match f {
                Frame::Stream(frame) => format!("stream:{}", frame["type"].as_str().unwrap()),
                Frame::Event(event) => format!("event:{}", event["type"].as_str().unwrap()),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "event:turn/start",
                "event:step/start",
                "stream:start",
                "stream:chunk",
                "stream:chunk",
                "stream:chunk",
                "stream:chunk",
                "event:assistant/message",
                "stream:end",
                "event:step/end",
                "event:step/start",
                "stream:start",
                "stream:chunk",
                "stream:chunk",
                "stream:chunk",
                "event:assistant/message",
                "stream:end",
                "event:step/end",
                "event:turn/end",
            ]
        );

        // Chunk shapes and dense per-attempt indexes.
        let chunk = |i: usize| match &all_frames[i] {
            Frame::Stream(frame) => frame.clone(),
            _ => panic!("expected stream frame at {i}"),
        };
        assert_eq!(
            chunk(3)["chunk"],
            json!({ "type": "block-start", "index": 0, "blockType": "reasoning" })
        );
        assert_eq!(
            chunk(4)["chunk"],
            json!({ "type": "reasoning-delta", "index": 0, "text": "thin" })
        );
        assert_eq!(
            chunk(5)["chunk"],
            json!({ "type": "reasoning-delta", "index": 0, "text": "king" })
        );
        assert_eq!(chunk(6)["chunk"]["type"], "block-end");
        assert_eq!(
            chunk(6)["chunk"]["block"],
            json!({ "type": "reasoning", "text": "thinking" })
        );
        for (i, expected) in [(3, 0u64), (4, 1), (5, 2), (6, 3)] {
            assert_eq!(chunk(i)["index"], expected, "dense chunk index at frame {i}");
        }

        // The committed end names the message event that preceded it.
        let message = match &all_frames[7] {
            Frame::Event(event) => event.clone(),
            _ => panic!("expected event at 7"),
        };
        assert_eq!(message["type"], "assistant/message");
        assert_eq!(message["surfaceOp"], "append");
        assert_eq!(message["data"]["turn"], 1);
        assert_eq!(message["data"]["step"], 1);
        let end = chunk(8);
        assert_eq!(end["outcome"]["kind"], "committed");
        assert_eq!(end["outcome"]["seq"], message["seq"], "end names the preceding settlement seq");
        assert_eq!(end["index"], 4, "end counts the chunk frames it represents");

        // The text attempt used its own step.
        let message2 = match &all_frames[15] {
            Frame::Event(event) => event.clone(),
            _ => panic!("expected event at 15"),
        };
        assert_eq!(message2["data"]["step"], 2);
        assert_eq!(message2["data"]["message"]["content"][0]["type"], "text");
    }

    #[test]
    fn tool_events_carry_live_name_and_arguments() {
        let mut mapper = SessionMapper::default();
        let _ = mapper.map_envelope(&json!({
            "event": "turn.started", "turn_id": "turn_1", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        let frames = mapper.map_envelope(&json!({
            "event": "item.started", "item_id": "item_t", "timestamp": "2026-11-06T10:00:01Z",
            "payload": {
                "item": { "id": "item_t", "kind": "tool_call", "status": "in_progress", "detail": "", "summary": "" },
                "tool": { "id": "call_1", "name": "bash", "input": { "command": "ls" } }
            }
        }));
        let call = match &frames[0] {
            Frame::Event(event) => event.clone(),
            _ => panic!("expected event frame"),
        };
        assert_eq!(call["type"], "tool/call");
        assert_eq!(call["data"]["name"], "bash");
        assert_eq!(call["data"]["arguments"], json!("{\"command\":\"ls\"}"));
        assert_eq!(call["data"]["turn"], 1);

        let frames = mapper.map_envelope(&json!({
            "event": "item.completed", "item_id": "item_t", "timestamp": "2026-11-06T10:00:02Z",
            "payload": { "item": { "id": "item_t", "kind": "tool_call", "status": "completed", "detail": "file1" } }
        }));
        let result = match &frames[0] {
            Frame::Event(event) => event.clone(),
            _ => panic!("expected event frame"),
        };
        assert_eq!(result["type"], "tool/result");
        assert_eq!(result["surfaceOp"], "append");
        assert_eq!(result["data"]["message"]["toolCallId"], "item_t");
        assert_eq!(result["data"]["message"]["content"][0]["text"], "file1");
    }

    #[test]
    fn abandon_closes_open_attempts_with_contiguous_revision() {
        let mut mapper = SessionMapper::default();
        let _ = mapper.map_envelope(&json!({
            "event": "turn.started", "turn_id": "turn_1", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        let _ = mapper.map_envelope(&json!({
            "event": "item.started", "item_id": "item_m", "timestamp": "2026-11-06T10:00:01Z",
            "payload": { "item": { "id": "item_m", "kind": "agent_message", "status": "in_progress", "detail": "" } }
        }));
        let _ = mapper.map_envelope(&json!({
            "event": "item.delta", "item_id": "item_m", "timestamp": "2026-11-06T10:00:02Z",
            "payload": { "delta": "partial", "kind": "agent_message" }
        }));
        let abandoned = mapper.abandon_open_attempts();
        assert_eq!(abandoned.len(), 1);
        let end = match &abandoned[0] {
            Frame::Stream(frame) => frame.clone(),
            _ => panic!("expected stream frame"),
        };
        assert_eq!(end["type"], "end");
        assert_eq!(end["outcome"]["kind"], "abandoned");
        // start (1), block-start (2), delta (3), abandon end (4).
        assert_eq!(end["revision"], 4, "abandon keeps revisions contiguous");
        assert!(mapper.abandon_open_attempts().is_empty(), "second abandon is a no-op");
    }
}
