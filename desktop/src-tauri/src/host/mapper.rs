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
//! Frame `time` is the runtime's own instant, never the mapping clock: live,
//! every frame an envelope yields carries its `timestamp`; on replay,
//! start-side events take the record's `started_at` and settle-side events
//! its `ended_at` — the instants the live envelopes carried — so a reload
//! keeps the clocks and turn durations the client derives from them.
//!
//! Known limitations: tool-call output deltas are not streamed (the vendored
//! cards don't consume live output either — a running card shows its shimmer
//! and settles from `tool/result`, so this matches the upstream interaction);
//! durable tool items carry their arguments in additive `metadata.arguments`
//! (the runtime records them at call start and preserves them across the
//! result), so replayed cards render their arguments; an item
//! whose `item.started` was missed (follow opened mid-item) is adopted with a
//! synthesized `step/start` + stream start, so its block may miss early deltas
//! and its step clock starts at the first delta seen. An envelope or record
//! without a timestamp falls back to the bridge clock (a foreign record
//! missing `ended_at` replays its turn as instant).
//! Tool names are normalized to the vendored client's card vocabulary
//! (`tool_view_name`): the runtime record keeps CodeSmith's own names, this is
//! presentation-layer translation only.
//!
//! Token usage: the runtime reports usage per TURN (the final agent item's
//! completion payload and `turns[].usage` carry it); the client accounts per
//! settled step, so the mapper attaches the turn's sample to that final
//! agent item's `assistant/message` — the client's turn fold aggregates the
//! per-attempt samples into the exact turn total (`TurnTokenUsage`).

use std::collections::HashMap;

use serde_json::{json, Value};

/// CodeSmith tool name → the vendored client's toolview slot key. Unknown
/// names pass through and land on the generic tool card. The row models parse
/// argument shapes the two vocabularies share (`command`, `todos`, `pattern`,
/// `path`); anything richer degrades to the row's plain summary.
pub fn tool_view_name(name: &str) -> String {
    match name {
        "exec_shell" | "exec_shell_wait" | "exec_shell_interact" => "bash".into(),
        "read_file" => "read".into(),
        "edit_file" => "edit".into(),
        "grep_files" => "grep".into(),
        "file_search" | "symbol_search" => "glob".into(),
        "fetch_url" | "web_run" => "web_fetch".into(),
        "web_search" => "web_search".into(),
        "todo" => "todo_write".into(),
        // The registry's primary checklist writer is `checklist_write`; its
        // whole-list input is the card's todo shape.
        "checklist_write" => "todo_write".into(),
        "user_input" => "ask_user_question".into(),
        other => other.into(),
    }
}

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

/// Allocates the monotone per-session wire seq. Seqs are 0-based, the
/// upstream journal convention: an empty journal's cursor is -1 and its first
/// event takes seq 0. The client's journal stream requires an opening page to
/// end exactly at its cursor and every live event to follow the previous one
/// by 1, so a 1-based journal would fail to open while still empty.
#[derive(Default)]
pub struct WireSeq {
    /// The seq the next event takes.
    next: u64,
}

impl WireSeq {
    /// Continue after journal cursor `last` (-1 = empty journal).
    pub fn resume_after(last: i64) -> Self {
        Self {
            next: (last + 1).max(0) as u64,
        }
    }

    /// The journal cursor: the last allocated seq, -1 before any event.
    pub fn last(&self) -> i64 {
        self.next as i64 - 1
    }

    /// Allocate the next seq for an event that happened at `time` (epoch ms).
    pub fn event(&mut self, kind: &str, data: Value, surface_append: bool, time: i64) -> Value {
        let seq = self.next;
        self.next += 1;
        let mut event = json!({
            "type": kind,
            "seq": seq,
            "time": time,
            "data": data,
        });
        if surface_append {
            event["surfaceOp"] = json!("append");
        }
        event
    }
}

/// Epoch milliseconds (the client's time unit) of an RFC 3339 runtime
/// timestamp.
pub fn epoch_ms(rfc3339: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// The first of `keys` on a runtime envelope or record that holds a
/// timestamp, in epoch milliseconds.
fn record_time(record: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| record.get(*key).and_then(Value::as_str).and_then(epoch_ms))
}

fn content_text(detail: Option<&str>) -> String {
    detail.unwrap_or_default().to_string()
}

/// The instant a replayed turn's trailing events carry: its `ended_at` when
/// the record stamped one, else the turn's start.
fn ended_instant(turn: &Value, turn_start: i64) -> i64 {
    record_time(turn, &["ended_at"]).unwrap_or(turn_start)
}

/// A user message's content parts: the text part, then one part per
/// attachment the item's additive metadata records (the runtime's turn API
/// writes them; see `StartTurnRequest.attachments`). Upstream's
/// `UserMessage.content` is the shared ContentBlock union, so image parts
/// render their thumbnail (bytes fetched over `session/attachment`) and file
/// parts their chip. `width`/`height` default to 0 when the sender supplied
/// none (a foreign HTTP client): the client's reference schema requires the
/// numbers, and a 0-sized thumbnail reads as unknown, not as a lie.
fn user_message_content(item: &Value, detail_text: Option<&str>) -> Value {
    let mut parts = vec![json!({ "type": "text", "text": content_text(detail_text) })];
    let attachments = item
        .pointer("/metadata/attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for attachment in attachments {
        // Copy the recorded fields that are present (the client's reference
        // schemas make name optional, the rest required).
        let mut reference = serde_json::Map::new();
        let fields: &[&str] = match attachment.get("kind").and_then(Value::as_str) {
            Some("image") => &["attachmentId", "mediaType", "bytes", "name"],
            Some("file") => &["attachmentId", "name", "bytes"],
            _ => continue,
        };
        for field in fields {
            if let Some(value) = attachment.get(*field).filter(|value| !value.is_null()) {
                reference.insert((*field).to_string(), value.clone());
            }
        }
        if attachment.get("kind") == Some(&json!("image")) {
            reference.insert(
                "width".into(),
                attachment.get("width").cloned().unwrap_or(json!(0)),
            );
            reference.insert(
                "height".into(),
                attachment.get("height").cloned().unwrap_or(json!(0)),
            );
            parts.push(json!({ "type": "image", "attachment": reference }));
        } else {
            parts.push(json!({ "type": "file", "attachment": reference }));
        }
    }
    Value::Array(parts)
}

/// The runtime's per-turn `Usage` record as the client's usage sample
/// (`inputTokens` = uncached prompt input, `cacheReadTokens` the cached
/// share, `totalTokens` the provider-exact aggregate). The client's
/// `normalizeUsage` rejects a sample whose buckets contradict its total, so
/// the arithmetic here must round-trip: with the runtime's convention
/// `input_tokens = cache_hit + cache_miss`, `total = input + output` keeps
/// `exactPrompt == knownPrompt`.
fn client_usage_sample(usage: &Value) -> Option<Value> {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64);
    let input = count("input_tokens")?;
    let output = count("output_tokens")?;
    // Uncached input: the miss share when the provider splits it; when only
    // the hit share is reported, subtract it so the buckets still add up.
    let hit = count("prompt_cache_hit_tokens");
    let uncached = count("prompt_cache_miss_tokens")
        .or_else(|| hit.map(|hit| input.saturating_sub(hit)))
        .unwrap_or(input);
    let mut sample = json!({
        "inputTokens": uncached,
        "outputTokens": output,
        "totalTokens": input + output,
    });
    if let Some(hit) = hit {
        sample["cacheReadTokens"] = json!(hit);
    }
    if let Some(reasoning) = count("reasoning_tokens") {
        if reasoning <= output {
            sample["reasoningTokens"] = json!(reasoning);
        }
    }
    Some(sample)
}

/// A user message's `source`: the client's correlation id rides the item's
/// additive metadata (`metadata.rpc_id`, recorded by the runtime's queue and
/// steer routes), so the client retires its optimistic copy by identity —
/// never by arrival order, which a queue reorders (steer) and thins
/// (remove).
pub fn user_message_source(item: &Value) -> Value {
    match item.pointer("/metadata/rpc_id").and_then(Value::as_str) {
        Some(rpc_id) => json!({ "kind": "user", "rpcId": rpc_id }),
        None => json!({ "kind": "user" }),
    }
}

/// One queued item as the client's inbox row: the flat `UserMessage` the
/// queue dock renders (`id`, text + attachment content parts, and the
/// `source.rpcId` that retires the composer's optimistic copy). The item
/// carries its text and attachment descriptors the way a settled user item
/// does, so the content parts come from the same builder.
pub fn queue_row(item: &Value) -> Value {
    let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    let text = item.get("text").and_then(Value::as_str);
    let as_item = json!({
        "metadata": {
            "attachments": item.pointer("/attachments_metadata/attachments").cloned().unwrap_or(json!([])),
            "rpc_id": item.get("rpc_id").cloned().unwrap_or(Value::Null),
        }
    });
    json!({
        "id": id,
        "role": "user",
        "content": user_message_content(&as_item, text),
        "source": user_message_source(&as_item),
    })
}

/// The `inbox` projection value for a thread's queue (upstream's
/// `InboxWireState`): queued items are `next-turn` rows; CodeSmith has no
/// separately pending steering (a steer reaches the engine at once), so
/// `next-step` is always empty.
pub fn inbox_projection(queue: &[Value]) -> Value {
    json!({
        "next-turn": queue.iter().map(queue_row).collect::<Vec<_>>(),
        "next-step": [],
    })
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
    /// The (provider, model) stamped on settled assistant messages — the
    /// usage fold derives its `routes` from `message.source`. Empty strings
    /// leave the route unattributed, which the client renders by omitting it.
    model_source: (String, String),
    /// Whether the current turn settled a file change — its end then
    /// announces `workspace/changes` (the changed-files card's anchor).
    turn_has_file_changes: bool,
    /// The latest `write_plan_file` body seen in this stream. `exit_plan_mode`
    /// carries an empty argument schema, so the bridge attaches this body as
    /// its `arguments.plan` — the vendored plan card/preview matches a
    /// `tool/call` whose arguments name `plan`.
    plan_content: Option<String>,
    /// Whether this turn already published the `exit_plan_mode` call the plan
    /// card anchors on. The runtime records an approved exit as a `status`
    /// item, not a tool item, so the mapper synthesizes that call from the
    /// plan body at the turn's approval status.
    plan_call_emitted: bool,
}

impl SessionMapper {
    /// Resume a live tail after a replayed journal. `turn_no` is the replayed
    /// turn count (a live `turn.started` for the next turn increments past it;
    /// a mid-turn tail without a fresh `turn.started` stays on the current
    /// turn), `step` the replayed step watermark of that turn, so live steps
    /// never collide with replayed ones.
    pub fn resume_at(last_seq: i64, turn_no: u64, step: u64) -> Self {
        Self {
            seq: WireSeq::resume_after(last_seq),
            turn_no,
            step,
            ..Self::default()
        }
    }

    /// Stamp the (provider, model) settled assistant messages carry as their
    /// `message.source` — the follow loop refreshes it from the session's
    /// model selection before mapping each envelope.
    pub fn set_model_source(&mut self, provider: &str, model: &str) {
        self.model_source = (provider.to_string(), model.to_string());
    }

    /// Seed the plan body from the replayed snapshot: a follow stream can
    /// connect mid-turn, after the `write_plan_file` call that the live tail
    /// will never re-deliver.
    pub fn seed_plan_content(&mut self, content: Option<String>) {
        self.plan_content = content;
    }

    /// `ModelMessageSource` for a settled assistant message.
    fn model_source(&self) -> Value {
        json!({
            "kind": "model",
            "provider": self.model_source.0,
            "model": self.model_source.1,
        })
    }

    fn started_after_seq(&self) -> i64 {
        self.seq.last()
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
            Attempt {
                chunk_index: 0,
                block_open: false,
            },
        );
        Some(Frame::Stream(frame))
    }

    /// Emit one chunk frame for a registered attempt.
    fn chunk(&mut self, item_id: &str, chunk: Value, time: i64) -> Option<Frame> {
        let revision = &mut self.revision;
        let attempt = self.attempts.get_mut(item_id)?;
        *revision += 1;
        let frame = json!({
            "type": "chunk",
            "attemptId": item_id,
            "revision": *revision,
            "index": attempt.chunk_index,
            "time": time,
            "chunk": chunk,
        });
        attempt.chunk_index += 1;
        Some(Frame::Stream(frame))
    }

    /// Map one live SSE envelope into follow frames.
    pub fn map_envelope(&mut self, envelope: &Value) -> Vec<Frame> {
        let mut frames = Vec::new();
        let event_name = envelope
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
        let item_id = envelope
            .get("item_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // `timestamp` is the canonical event time, `created_at` its alias
        // (docs/RUNTIME_API.md: clients must not require both).
        let time = record_time(envelope, &["timestamp", "created_at"])
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        match event_name {
            "turn.started" => {
                self.turn_no += 1;
                self.step = 0;
                self.turn_has_file_changes = false;
                self.plan_call_emitted = false;
                frames.push(Frame::Event(self.seq.event(
                    "turn/start",
                    json!({ "turn": self.turn_no }),
                    false,
                    time,
                )));
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
                // The turn's changed-files announcement precedes its end:
                // the deliverables card anchors on this event's seq and the
                // bridge's /api/changes.* routes serve its summary from the
                // thread record.
                if self.turn_has_file_changes {
                    frames.push(Frame::Event(self.seq.event(
                        "workspace/changes",
                        json!({ "turn": self.turn_no }),
                        false,
                        time,
                    )));
                }
                self.turn_has_file_changes = false;
                frames.push(Frame::Event(self.seq.event(
                    "turn/end",
                    json!({ "turn": self.turn_no, "reason": { "kind": reason } }),
                    false,
                    time,
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
                            time,
                        )));
                        if let Some(frame) = self.start_attempt(&item_id) {
                            frames.push(frame);
                        }
                    }
                    "tool_call" | "command_execution" | "file_change" => {
                        let tool = payload.get("tool").cloned().unwrap_or(Value::Null);
                        let raw_name = tool
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| tool_name_from_summary(&item));
                        // The plan file is the source of truth for the plan
                        // card: remember its latest body so the following
                        // exit_plan_mode call can carry it as arguments.plan.
                        if raw_name == "write_plan_file" {
                            if let Some(content) = tool
                                .pointer("/input/content")
                                .and_then(Value::as_str)
                            {
                                self.plan_content = Some(content.to_string());
                            }
                        }
                        let name = tool_view_name(&raw_name);
                        if name == "exit_plan_mode" {
                            // A runtime that records the call itself needs no
                            // synthesized copy at the approval status.
                            self.plan_call_emitted = true;
                        }
                        let arguments = if name == "exit_plan_mode" {
                            match &self.plan_content {
                                Some(plan) => plan_call_arguments(plan),
                                None => tool
                                    .get("input")
                                    .cloned()
                                    .map(|input| input.to_string())
                                    .unwrap_or_else(|| "{}".into()),
                            }
                        } else {
                            tool.get("input")
                                .cloned()
                                .map(|input| input.to_string())
                                .unwrap_or_else(|| "{}".into())
                        };
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
                            time,
                        )));
                    }
                    "user_message" => {
                        let detail = item.get("detail").and_then(Value::as_str);
                        frames.push(Frame::Event(self.seq.event(
                            "user/message",
                            // Flat UserMessage contract: the client's message
                            // node reads data.id and data.source.kind directly,
                            // its content parts carry the attachments.
                            json!({
                                "id": item_id,
                                "role": "user",
                                "content": user_message_content(&item, detail),
                                "source": user_message_source(&item),
                            }),
                            true,
                            time,
                        )));
                    }
                    _ => {}
                }
            }
            "item.delta" => {
                let kind = payload
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let (block_type, chunk_type) = match kind {
                    "agent_message" => ("text", "text-delta"),
                    "agent_reasoning" => ("reasoning", "reasoning-delta"),
                    // Tool output deltas are not streamed; the tool node
                    // settles from its durable result.
                    _ => return frames,
                };
                let text = payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !self.attempts.contains_key(&item_id) {
                    // Mid-item adoption: no step/start or stream start was
                    // seen. Synthesize both so the fold stays consistent.
                    self.step += 1;
                    frames.push(Frame::Event(self.seq.event(
                        "step/start",
                        json!({ "turn": self.turn_no, "step": self.step }),
                        false,
                        time,
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
                            time,
                        ) {
                            frames.push(frame);
                        }
                    }
                }
                if let Some(frame) = self.chunk(
                    &item_id,
                    json!({ "type": chunk_type, "index": 0, "text": text }),
                    time,
                ) {
                    frames.push(frame);
                }
            }
            "item.completed" | "item.failed" => {
                let item = payload.get("item").cloned().unwrap_or(Value::Null);
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
                let detail = item.get("detail").and_then(Value::as_str);
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match kind {
                    "agent_message" | "agent_reasoning" => {
                        let block_type = if kind == "agent_message" {
                            "text"
                        } else {
                            "reasoning"
                        };
                        let text = content_text(detail);
                        if self.attempts.get(&item_id).is_some_and(|a| a.block_open) {
                            if let Some(frame) = self.chunk(
                                &item_id,
                                json!({ "type": "block-end", "index": 0, "block": { "type": block_type, "text": text } }),
                                time,
                            ) {
                                frames.push(frame);
                            }
                        }
                        // The durable message event must precede the end frame
                        // (the client fold stages the settlement, then the end
                        // frame publishes it).
                        let usage = payload
                            .get("usage")
                            .filter(|usage| !usage.is_null())
                            .and_then(client_usage_sample);
                        let mut data = json!({
                            "turn": self.turn_no,
                            "step": self.step,
                            "message": {
                                "id": item_id,
                                "role": "assistant",
                                "content": [{ "type": block_type, "text": text }],
                                // ModelMessageSource + empty stream records: the
                                // usage fold reads source.provider/model and
                                // stream.length on every settlement.
                                "source": self.model_source(),
                            },
                            "stream": [],
                        });
                        if let Some(sample) = usage {
                            data["usage"] = sample;
                        }
                        let message = self.seq.event("assistant/message", data, true, time);
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
                            time,
                        )));
                    }
                    "tool_call" | "command_execution" | "file_change" => {
                        // A failed or denied file call changed nothing: the
                        // changes card must not announce it.
                        if kind == "file_change" && status == "completed" {
                            self.turn_has_file_changes = true;
                        }
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
                            time,
                        )));
                    }
                    // user_message settles at item.started (the durable
                    // item already carries its text); re-emitting here would
                    // duplicate the message.
                    "context_compaction" | "error" | "status" => {
                        // An approved plan exits as a status item, not a
                        // tool item: publish the `exit_plan_mode` call the
                        // plan card/preview anchors on from the plan body
                        // this stream already holds.
                        if is_plan_approved(&item) && !self.plan_call_emitted {
                            if let Some(plan) = &self.plan_content {
                                self.plan_call_emitted = true;
                                frames.push(Frame::Event(self.seq.event(
                                    "tool/call",
                                    json!({
                                        "turn": self.turn_no,
                                        "step": self.step,
                                        "callId": item_id,
                                        "name": "exit_plan_mode",
                                        "arguments": plan_call_arguments(plan),
                                    }),
                                    false,
                                    time,
                                )));
                            }
                        }
                        frames.push(Frame::Event(self.seq.event(
                            "system/message",
                            json!({
                                "message": { "id": item_id, "content": [{ "type": "text", "text": content_text(detail) }] }
                            }),
                            true,
                            time,
                        )));
                    }
                    _ => {}
                }
            }
            "item.interrupted" => {
                // The runtime interrupts an in-flight item (user Esc): close
                // any streaming attempt as abandoned so the client retires its
                // transient row instead of waiting for a settlement that never
                // comes. Tool nodes keep their last state; the turn's own
                // interrupted end follows.
                if let Some(attempt) = self.attempts.remove(&item_id) {
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
pub(super) fn tool_name_from_summary(item: &Value) -> String {
    let raw = item
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let head = raw.split(':').next().unwrap_or("tool");
    // A running item's summary is "<name> started"; drop the suffix so the
    // replayed call keeps its real tool name.
    head.strip_suffix(" started")
        .unwrap_or(head)
        .trim()
        .to_string()
}

/// Replayed tool arguments. The runtime persists tool INPUT only on the live
/// `item.started` payload — durable items keep result metadata — so replayed
/// cards render from summary/detail and the arguments stay empty unless a
/// future runtime revision records them under `metadata.arguments`.
pub(super) fn replay_tool_arguments(item: &Value) -> String {
    for key in ["arguments", "input"] {
        if let Some(args) = item.pointer(&format!("/metadata/{key}")).cloned() {
            if args.is_object() {
                return args.to_string();
            }
        }
    }
    "{}".into()
}

/// One named string argument of a durable tool item (`metadata.arguments` is
/// the shape the runtime records; `input` is the older alias).
pub(super) fn tool_argument(item: &Value, key: &str) -> Option<String> {
    for container in ["arguments", "input"] {
        if let Some(value) = item
            .pointer(&format!("/metadata/{container}/{key}"))
            .and_then(Value::as_str)
        {
            return Some(value.to_string());
        }
    }
    None
}

/// Whether plan markdown already opens with the `# <title>` heading the
/// vendored plan card requires (`submittedPlan` drops a plan without one).
fn plan_has_title(markdown: &str) -> bool {
    let Some(rest) = markdown.trim_start().strip_prefix('#') else {
        return false;
    };
    // The title text must sit on the heading's own line: `#\nStep` is an
    // empty heading followed by a paragraph, not a document title.
    let line = rest.split(['\r', '\n']).next().unwrap_or_default();
    let mut chars = line.chars();
    match chars.next() {
        Some(space) if space.is_whitespace() => chars.any(|c| !c.is_whitespace()),
        _ => false,
    }
}

/// `exit_plan_mode`'s wire arguments. The plan file is the source of truth,
/// but the vendored card needs a `# title` heading to identify the document;
/// a plan the model wrote without one gets the generic heading prepended on
/// the wire only (the file itself stays exactly as written).
pub(super) fn plan_call_arguments(content: &str) -> String {
    let markdown = if plan_has_title(content) {
        content.to_string()
    } else {
        format!("# Plan\n\n{content}")
    };
    json!({ "plan": markdown }).to_string()
}

/// A status item's text: its summary (how the runtime stores it), else the
/// detail body.
fn item_status_text(item: &Value) -> &str {
    item.get("summary")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| item.get("detail").and_then(Value::as_str))
        .unwrap_or_default()
}

/// Whether a status item records the approved `exit_plan_mode` handoff.
fn is_plan_approved(item: &Value) -> bool {
    item.get("kind").and_then(Value::as_str) == Some("status")
        && item_status_text(item).contains("Plan approved")
}

/// The latest plan-file body in a thread record, in item order. Seeding a
/// live mapper with it covers a follow stream that connects after the
/// `write_plan_file` call but before `exit_plan_mode`.
pub(super) fn latest_plan_content(detail: &Value) -> Option<String> {
    let items = detail.get("items").and_then(Value::as_array)?;
    let mut content = None;
    for item in items {
        if !matches!(
            item.get("kind").and_then(Value::as_str),
            Some("tool_call") | Some("command_execution") | Some("file_change")
        ) {
            continue;
        }
        if tool_name_from_summary(item) == "write_plan_file" {
            if let Some(body) = tool_argument(item, "content") {
                content = Some(body);
            }
        }
    }
    content
}

/// Replay a `ThreadDetail` (GET /v1/threads/{id}) into wire events.
/// Returns the events in order; turns still running get no `turn/end`.
/// In-progress agent items are skipped (an unsettled assistant message has no
/// durable content; the live stream owns it while it runs). Each event takes
/// the record instant its live counterpart's envelope carried: start-side
/// events `started_at`, settle-side events `ended_at`.
pub fn replay_events(detail: &Value) -> Vec<Value> {
    replay_events_sourced(detail, "", "")
}

/// [`replay_events`] with the (provider, model) settled assistant messages
/// carry as their `source` — the caller resolves it from the thread record
/// and the model catalog the way the live path does.
pub fn replay_events_sourced(detail: &Value, provider: &str, model: &str) -> Vec<Value> {
    let mut seq = WireSeq::default();
    let mut events = Vec::new();
    // The latest plan file body before the current item; `exit_plan_mode`
    // carries it as arguments.plan (its own durable arguments are empty).
    let mut plan_content: Option<String> = None;
    let now = chrono::Utc::now().timestamp_millis();
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
        // A queued turn has no `started_at` yet; `created_at` is always set.
        let turn_start = record_time(turn, &["started_at", "created_at"]).unwrap_or(now);
        events.push(seq.event("turn/start", json!({ "turn": turn_no }), false, turn_start));
        let turn_id = turn
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut turn_items: Vec<&Value> = items
            .iter()
            .filter(|item| item.get("turn_id").and_then(Value::as_str) == Some(turn_id.as_str()))
            .collect();
        turn_items.sort_by_key(|item| {
            item.get("started_at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        });
        let mut step: u64 = 0;
        // Whether this turn already published its `exit_plan_mode` call (see
        // the live arm's plan-call synthesis).
        let mut plan_call_emitted = false;
        // Where this turn's usage sample lands: the last settled agent
        // message (preferring a text message over reasoning), mirroring the
        // live path's attachment to the final agent item.
        let mut usage_target: Option<usize> = None;
        // Whether the turn settled a completed file change (the replay
        // announces `workspace/changes` like the live path does; a failed or
        // denied call changed nothing and announces nothing).
        let turn_has_file_changes = turn_items.iter().any(|item| {
            item.get("kind").and_then(Value::as_str) == Some("file_change")
                && item.get("status").and_then(Value::as_str) == Some("completed")
        });
        for item in turn_items {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let detail_text = item.get("detail").and_then(Value::as_str);
            let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let started = record_time(item, &["started_at"]).unwrap_or(turn_start);
            let ended = record_time(item, &["ended_at"]).unwrap_or(started);
            match kind {
                "user_message" => events.push(seq.event(
                    "user/message",
                    // Flat UserMessage contract (see the live arm above).
                    json!({
                        "id": item_id,
                        "role": "user",
                        "content": user_message_content(item, detail_text),
                        "source": user_message_source(item),
                    }),
                    true,
                    started,
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
                        started,
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
                                "source": { "kind": "model", "provider": provider, "model": model },
                            },
                            "stream": []
                        }),
                        true,
                        ended,
                    ));
                    if kind == "agent_message" {
                        // A text message outranks an earlier reasoning-only
                        // target for the turn's usage.
                        usage_target = Some(events.len() - 1);
                    } else if usage_target.is_none() {
                        usage_target = Some(events.len() - 1);
                    }
                    events.push(seq.event(
                        "step/end",
                        json!({ "turn": turn_no, "step": step }),
                        false,
                        ended,
                    ));
                }
                "tool_call" | "command_execution" | "file_change" => {
                    let raw_name = tool_name_from_summary(item);
                    if raw_name == "write_plan_file" {
                        if let Some(content) = tool_argument(item, "content") {
                            plan_content = Some(content);
                        }
                    }
                    let name = tool_view_name(&raw_name);
                    if name == "exit_plan_mode" {
                        plan_call_emitted = true;
                    }
                    let arguments = if name == "exit_plan_mode" {
                        match &plan_content {
                            Some(plan) => plan_call_arguments(plan),
                            None => replay_tool_arguments(item),
                        }
                    } else {
                        replay_tool_arguments(item)
                    };
                    events.push(seq.event(
                        "tool/call",
                        json!({
                            "turn": turn_no,
                            "step": step,
                            "callId": item_id,
                            "name": name,
                            "arguments": arguments,
                        }),
                        false,
                        started,
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
                            ended,
                        ));
                    }
                }
                "context_compaction" | "error" | "status" => {
                    if is_plan_approved(item) && !plan_call_emitted {
                        if let Some(plan) = &plan_content {
                            plan_call_emitted = true;
                            events.push(seq.event(
                                "tool/call",
                                json!({
                                    "turn": turn_no,
                                    "step": step,
                                    "callId": item_id,
                                    "name": "exit_plan_mode",
                                    "arguments": plan_call_arguments(plan),
                                }),
                                false,
                                ended,
                            ));
                        }
                    }
                    events.push(seq.event(
                        "system/message",
                        json!({
                            "message": { "id": item_id, "content": [{ "type": "text", "text": content_text(detail_text) }] }
                        }),
                        true,
                        ended,
                    ));
                }
                _ => {}
            }
        }
        // The turn's provider-reported usage, on the last settled agent
        // message — the same placement the live path stamps.
        if let (Some(index), Some(sample)) = (
            usage_target,
            turn.get("usage")
                .filter(|usage| !usage.is_null())
                .and_then(client_usage_sample),
        ) {
            if let Some(data) = events.get_mut(index).map(|event| &mut event["data"]) {
                data["usage"] = sample;
            }
        }
        // The changed-files announcement, when the turn settled a file
        // change — the live path emits the same event before `turn/end`.
        if turn_has_file_changes {
            events.push(seq.event(
                "workspace/changes",
                json!({ "turn": turn_no }),
                false,
                ended_instant(turn, turn_start),
            ));
        }
        if let Some(reason) = turn_end_reason(turn) {
            events.push(seq.event(
                "turn/end",
                json!({ "turn": turn_no, "reason": { "kind": reason } }),
                false,
                // The runtime stamps `ended_at` on every terminal turn.
                record_time(turn, &["ended_at"]).unwrap_or(turn_start),
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
    fn replay_carries_user_message_attachments_as_content_parts() {
        // The runtime's turn API records what each attachment was on the
        // user item's additive metadata; the settled message must render it
        // (image thumbnails, file chips) like upstream's UserMessage blocks.
        let detail = json!({
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [{
                "id": "item_user_message", "turn_id": "turn_1", "kind": "user_message",
                "status": "completed", "detail": "look at these", "summary": "s",
                "started_at": "2026-11-06T10:00:00Z",
                "metadata": { "attachments": [
                    { "kind": "image", "attachmentId": "sha256:aa", "mediaType": "image/png",
                      "name": "shot.png", "bytes": 8, "width": 3, "height": 2 },
                    { "kind": "file", "attachmentId": "sha256:bb", "name": "notes.md", "bytes": 12 },
                    // An image whose sender supplied no dimensions still gets
                    // the numbers the client's reference schema requires.
                    { "kind": "image", "attachmentId": "sha256:cc", "mediaType": "image/gif",
                      "bytes": 4 },
                ]},
            }],
        });
        let events = replay_events(&detail);
        let user_message = events
            .iter()
            .find(|event| event["type"] == "user/message")
            .expect("the user message replays");
        let parts = user_message["data"]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 4, "text part plus one per attachment");
        assert_eq!(parts[0], json!({ "type": "text", "text": "look at these" }));
        assert_eq!(
            parts[1],
            json!({ "type": "image", "attachment": {
                "attachmentId": "sha256:aa", "mediaType": "image/png",
                "name": "shot.png", "bytes": 8, "width": 3, "height": 2,
            }})
        );
        assert_eq!(
            parts[2],
            json!({ "type": "file", "attachment": {
                "attachmentId": "sha256:bb", "name": "notes.md", "bytes": 12,
            }})
        );
        assert_eq!(parts[3]["attachment"]["width"], json!(0));
        assert_eq!(parts[3]["attachment"]["height"], json!(0));
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
                "turn/start",
                "user/message",
                "step/start",
                "assistant/message",
                "step/end",
                "step/start",
                "assistant/message",
                "step/end",
                "tool/call",
                "tool/result",
                // in-progress agent message skipped
                "turn/end",
                "turn/start",
                "user/message",
                "turn/end",
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
                events
                    .iter()
                    .any(|e| e["type"] == kind && e["surfaceOp"] == "append"),
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
            assert_eq!(
                chunk(i)["index"],
                expected,
                "dense chunk index at frame {i}"
            );
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
        assert_eq!(
            end["outcome"]["seq"], message["seq"],
            "end names the preceding settlement seq"
        );
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
                "tool": { "id": "call_1", "name": "exec_shell", "input": { "command": "ls" } }
            }
        }));
        let call = match &frames[0] {
            Frame::Event(event) => event.clone(),
            _ => panic!("expected event frame"),
        };
        assert_eq!(call["type"], "tool/call");
        // CodeSmith tool names are normalized to the client card vocabulary.
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
    fn user_messages_carry_their_rpc_id_by_identity() {
        // The runtime records the client's correlation id on the user item;
        // the settled message must carry it whatever order messages arrive
        // in (a queue steer jumps ahead of queued items, a remove thins
        // them), live and on replay alike.
        let user_item = json!({
            "id": "item_u", "turn_id": "turn_1", "kind": "user_message",
            "status": "completed", "detail": "steered", "summary": "steered",
            "started_at": "2026-11-06T10:00:00Z",
            "metadata": { "rpc_id": "rpc_7" },
        });
        let mut mapper = SessionMapper::default();
        let _ = mapper.map_envelope(&json!({
            "event": "turn.started", "turn_id": "turn_1", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        let frames = mapper.map_envelope(&json!({
            "event": "item.started", "item_id": "item_u", "timestamp": "2026-11-06T10:00:01Z",
            "payload": { "item": user_item }
        }));
        let live = match &frames[0] {
            Frame::Event(event) => event.clone(),
            _ => panic!("expected event frame"),
        };
        assert_eq!(live["type"], "user/message");
        assert_eq!(
            live["data"]["source"],
            json!({ "kind": "user", "rpcId": "rpc_7" })
        );

        let replayed = replay_events(&json!({
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [user_item],
        }));
        let replayed = replayed
            .iter()
            .find(|event| event["type"] == "user/message")
            .expect("the user message replays");
        assert_eq!(replayed["data"]["source"]["rpcId"], "rpc_7");

        // No recorded id (a foreign HTTP client's turn): a plain user source.
        assert_eq!(
            user_message_source(&json!({ "metadata": null })),
            json!({ "kind": "user" })
        );
    }

    #[test]
    fn queued_items_become_inbox_rows() {
        // Upstream's QueueDock renders `next-turn` rows (flat UserMessages)
        // and retires the composer's optimistic copy against `source.rpcId`.
        let queue = vec![
            json!({
                "id": "qitem_a", "text": "look", "rpc_id": "rpc_a",
                "input_summary": "look", "created_at": "2026-11-06T10:00:00Z",
                "image_paths": ["/store/aa.png"],
                "attachments_metadata": { "attachments": [
                    { "kind": "image", "attachmentId": "sha256:aa", "mediaType": "image/png",
                      "name": "shot.png", "bytes": 8, "width": 3, "height": 2 },
                ]},
            }),
            json!({
                "id": "qitem_b", "text": "plain", "input_summary": "plain",
                "created_at": "2026-11-06T10:00:01Z",
            }),
        ];
        let inbox = inbox_projection(&queue);
        assert_eq!(
            inbox["next-step"],
            json!([]),
            "no separately pending steering"
        );
        let rows = inbox["next-turn"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "one row per queued item, FIFO");
        assert_eq!(rows[0]["id"], "qitem_a");
        assert_eq!(rows[0]["role"], "user");
        assert_eq!(
            rows[0]["source"],
            json!({ "kind": "user", "rpcId": "rpc_a" })
        );
        assert_eq!(
            rows[0]["content"][0],
            json!({ "type": "text", "text": "look" })
        );
        assert_eq!(rows[0]["content"][1]["type"], "image");
        assert_eq!(
            rows[0]["content"][1]["attachment"]["attachmentId"],
            "sha256:aa"
        );
        assert_eq!(
            rows[1]["source"],
            json!({ "kind": "user" }),
            "no rpc id, plain source"
        );
        assert_eq!(rows[1]["content"].as_array().unwrap().len(), 1, "text only");
    }
    #[test]
    fn interrupted_items_abandon_their_open_attempts() {
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
        let frames = mapper.map_envelope(&json!({
            "event": "item.interrupted", "item_id": "item_m", "timestamp": "2026-11-06T10:00:03Z", "payload": {}
        }));
        let end = match frames.first().expect("interrupt frame") {
            Frame::Stream(frame) => frame.clone(),
            other => panic!("expected stream frame, got {other:?}"),
        };
        assert_eq!(end["type"], "end");
        assert_eq!(end["outcome"]["kind"], "abandoned");
        // start (1), block-start (2), delta (3), interrupt end (4) stay dense.
        assert_eq!(end["revision"], 4);
    }

    #[test]
    fn resume_counters_keep_live_turns_and_steps_off_replayed_ones() {
        // A fresh turn.started after two replayed turns becomes turn 3 and
        // restarts the step count inside its own turn.
        let mut mapper = SessionMapper::resume_at(9, 2, 3);
        let frames = mapper.map_envelope(&json!({
            "event": "turn.started", "turn_id": "turn_3", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        let start = match &frames[0] {
            Frame::Event(event) => event.clone(),
            _ => panic!("expected event frame"),
        };
        assert_eq!(start["data"]["turn"], 3);
        assert!(
            start["seq"].as_u64().unwrap() > 9,
            "seq continues the replay"
        );

        // A tail that opens mid-turn (no fresh turn.started) adopts the item
        // onto the CURRENT replayed turn with the next free step number, so
        // live steps never collide with replayed ones.
        let mut mid = SessionMapper::resume_at(9, 2, 3);
        let frames = mid.map_envelope(&json!({
            "event": "item.delta", "item_id": "item_m", "timestamp": "2026-11-06T10:00:01Z",
            "payload": { "delta": "hi", "kind": "agent_message" }
        }));
        let step_start = frames
            .iter()
            .map(|f| match f {
                Frame::Event(event) => event.clone(),
                Frame::Stream(_) => json!(null),
            })
            .find(|e| e["type"] == "step/start")
            .expect("adopted step/start");
        assert_eq!(
            step_start["data"]["turn"], 2,
            "no fresh turn.started keeps the current turn"
        );
        assert_eq!(
            step_start["data"]["step"], 4,
            "step watermark continues past the replay"
        );
    }

    #[test]
    fn replay_recovers_tool_names_and_metadata_arguments() {
        let detail = json!({
            "turns": [{ "id": "turn_1", "status": "in_progress" }],
            "items": [
                { "id": "item_a", "turn_id": "turn_1", "kind": "command_execution",
                  "status": "in_progress", "detail": "", "summary": "exec_shell started",
                  "started_at": "2026-11-06T10:00:00Z" },
                { "id": "item_b", "turn_id": "turn_1", "kind": "file_change",
                  "status": "completed", "detail": "patched", "summary": "edit_file: patched",
                  "metadata": { "arguments": { "path": "a.rs" } },
                  "started_at": "2026-11-06T10:00:01Z" },
            ],
        });
        let events = replay_events(&detail);
        let call_a = events
            .iter()
            .find(|e| e["data"]["callId"] == "item_a")
            .unwrap();
        assert_eq!(
            call_a["data"]["name"], "bash",
            "running summary '<name> started' loses the suffix"
        );
        let call_b = events
            .iter()
            .find(|e| e["data"]["callId"] == "item_b")
            .unwrap();
        assert_eq!(call_b["data"]["name"], "edit");
        assert_eq!(call_b["data"]["arguments"], json!("{\"path\":\"a.rs\"}"));
    }

    #[test]
    fn usage_samples_map_the_runtime_buckets() {
        // Runtime convention: input_tokens = cache_hit + cache_miss. The
        // client's normalizeUsage rejects contradictory totals, so the
        // arithmetic must round-trip (input + output).
        let split = client_usage_sample(&json!({
            "input_tokens": 1200, "output_tokens": 300,
            "prompt_cache_hit_tokens": 1000, "prompt_cache_miss_tokens": 200,
            "reasoning_tokens": 80,
        }))
        .expect("split sample maps");
        assert_eq!(
            split,
            json!({
                "inputTokens": 200, "outputTokens": 300, "totalTokens": 1500,
                "cacheReadTokens": 1000, "reasoningTokens": 80,
            })
        );
        // No cache split: the whole input is the uncached share.
        assert_eq!(
            client_usage_sample(&json!({ "input_tokens": 10, "output_tokens": 5 })).unwrap(),
            json!({ "inputTokens": 10, "outputTokens": 5, "totalTokens": 15 })
        );
        // Hit reported without a miss share: subtract so buckets still add up.
        assert_eq!(
            client_usage_sample(&json!({
                "input_tokens": 10, "output_tokens": 5, "prompt_cache_hit_tokens": 4,
            }))
            .unwrap(),
            json!({ "inputTokens": 6, "outputTokens": 5, "totalTokens": 15, "cacheReadTokens": 4 })
        );
        // Reasoning beyond output contradicts the client's bucket check.
        let beyond = client_usage_sample(&json!({
            "input_tokens": 1, "output_tokens": 2, "reasoning_tokens": 3,
        }))
        .unwrap();
        assert!(beyond.get("reasoningTokens").is_none());
        // No output count: no sample (the fold treats the attempt unbilled).
        assert!(client_usage_sample(&json!({ "input_tokens": 1 })).is_none());
    }

    #[test]
    fn final_agent_message_carries_turn_usage_and_route() {
        // The runtime attaches the turn's usage to the final agent item's
        // completion payload; the client's turn fold aggregates per-attempt
        // samples into the exact turn total.
        let mut mapper = SessionMapper::default();
        mapper.set_model_source("ollama", "qwen2.5");
        let mut frames = Vec::new();
        for envelope in [
            json!({ "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {} }),
            json!({ "event": "item.started", "item_id": "item_m", "timestamp": "2026-11-06T10:00:01Z",
                    "payload": { "item": { "id": "item_m", "kind": "agent_message", "status": "in_progress" } } }),
            json!({ "event": "item.delta", "item_id": "item_m", "timestamp": "2026-11-06T10:00:01Z",
                    "payload": { "delta": "hi", "kind": "agent_message" } }),
            json!({ "event": "item.completed", "item_id": "item_m", "timestamp": "2026-11-06T10:00:02Z",
                    "payload": {
                        "item": { "id": "item_m", "kind": "agent_message", "status": "completed", "detail": "hi" },
                        "usage": { "input_tokens": 100, "output_tokens": 7,
                                   "prompt_cache_hit_tokens": 90, "prompt_cache_miss_tokens": 10 },
                    } }),
            json!({ "event": "turn.completed", "timestamp": "2026-11-06T10:00:03Z",
                    "payload": { "turn": { "status": "completed" } } }),
        ] {
            frames.extend(mapper.map_envelope(&envelope));
        }
        let message = frames
            .iter()
            .find_map(|frame| match frame {
                Frame::Event(event) if event["type"] == "assistant/message" => Some(event.clone()),
                _ => None,
            })
            .expect("the assistant message settles");
        assert_eq!(
            message["data"]["usage"],
            json!({ "inputTokens": 10, "outputTokens": 7, "totalTokens": 107, "cacheReadTokens": 90 })
        );
        assert_eq!(
            message["data"]["message"]["source"],
            json!({ "kind": "model", "provider": "ollama", "model": "qwen2.5" })
        );
    }

    #[test]
    fn turns_with_file_changes_announce_workspace_changes() {
        // Live: the announcement precedes turn/end when the turn settled a
        // file change (whatever else it did), and never appears otherwise.
        let mut mapper = SessionMapper::default();
        let mut frames = Vec::new();
        for envelope in [
            json!({ "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {} }),
            json!({ "event": "item.started", "item_id": "item_f", "timestamp": "2026-11-06T10:00:01Z",
                    "payload": { "item": { "id": "item_f", "kind": "file_change", "status": "in_progress" },
                                 "tool": { "name": "write_file", "input": { "path": "a.txt" } } } }),
            json!({ "event": "item.completed", "item_id": "item_f", "timestamp": "2026-11-06T10:00:02Z",
                    "payload": { "item": { "id": "item_f", "kind": "file_change", "status": "completed", "detail": "@@ -0,0 +1 @@\n+hi\n" } } }),
            json!({ "event": "turn.completed", "timestamp": "2026-11-06T10:00:03Z",
                    "payload": { "turn": { "status": "completed" } } }),
        ] {
            frames.extend(mapper.map_envelope(&envelope));
        }
        let kinds: Vec<String> = frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Event(event) => Some(event["type"].as_str().unwrap().to_string()),
                Frame::Stream(_) => None,
            })
            .collect();
        let announce = kinds
            .iter()
            .position(|kind| kind == "workspace/changes")
            .expect("the file-change turn announces");
        let end = kinds
            .iter()
            .position(|kind| kind == "turn/end")
            .expect("the turn ends");
        assert!(announce < end, "the announcement precedes the turn end");

        // A turn with no file change announces nothing (live and replay).
        let mut quiet = SessionMapper::default();
        let frames = quiet.map_envelope(&json!({
            "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        assert!(frames
            .iter()
            .all(|frame| frame.to_wire()["event"]["type"] != "workspace/changes"));

        // Replay: the same announcement before the turn's end event.
        let detail = json!({
            "turns": [{ "id": "turn_1", "status": "completed", "ended_at": "2026-11-06T10:00:09Z" }],
            "items": [
                { "id": "item_f", "turn_id": "turn_1", "kind": "file_change",
                  "status": "completed", "detail": "@@ -0,0 +1 @@\n+hi\n",
                  "summary": "write_file: wrote", "started_at": "2026-11-06T10:00:01Z",
                  "ended_at": "2026-11-06T10:00:02Z" },
            ],
        });
        let events = replay_events(&detail);
        let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert!(types.contains(&"workspace/changes"));
        assert_eq!(
            events.iter().find(|e| e["type"] == "workspace/changes").unwrap()["data"]["turn"],
            1
        );
        // And its seq resolves through the replay — the changes routes
        // address a summary by exactly this coordinate.
        let seq = events
            .iter()
            .find(|e| e["type"] == "workspace/changes")
            .unwrap()["seq"]
            .as_i64()
            .unwrap();
        assert!(seq > 0);
    }

    #[test]
    fn failed_file_changes_do_not_announce_the_changes_card() {
        // A denied file tool changed nothing: neither the live stream nor the
        // replay may publish the card's `workspace/changes` anchor.
        let mut mapper = SessionMapper::default();
        let mut frames = Vec::new();
        for envelope in [
            json!({ "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {} }),
            json!({ "event": "item.started", "item_id": "item_f", "timestamp": "2026-11-06T10:00:01Z",
                    "payload": { "item": { "id": "item_f", "kind": "file_change", "status": "in_progress" },
                                 "tool": { "name": "write_file", "input": { "path": "a.txt" } } } }),
            json!({ "event": "item.failed", "item_id": "item_f", "timestamp": "2026-11-06T10:00:02Z",
                    "payload": { "item": { "id": "item_f", "kind": "file_change", "status": "failed",
                                           "detail": "denied" } } }),
            json!({ "event": "turn.completed", "timestamp": "2026-11-06T10:00:03Z",
                    "payload": { "turn": { "status": "completed" } } }),
        ] {
            frames.extend(mapper.map_envelope(&envelope));
        }
        assert!(
            !frames.iter().any(|frame| matches!(
                frame,
                Frame::Event(event) if event["type"] == "workspace/changes"
            )),
            "a failed file call announces no changes"
        );

        let detail = json!({
            "turns": [{ "id": "turn_1", "status": "completed", "ended_at": "2026-11-06T10:00:09Z" }],
            "items": [
                { "id": "item_f", "turn_id": "turn_1", "kind": "file_change",
                  "status": "failed", "detail": "denied", "summary": "write_file failed",
                  "started_at": "2026-11-06T10:00:01Z", "ended_at": "2026-11-06T10:00:02Z" },
            ],
        });
        let events = replay_events(&detail);
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "workspace/changes"),
            "the replay announces no changes either"
        );
    }

    #[test]
    fn replay_stamps_turn_usage_on_the_last_agent_message() {
        // The turn's usage rides the LAST settled agent message (a text
        // message outranks an earlier reasoning-only target), so the client's
        // turn fold accounts the turn exactly once.
        let detail = json!({
            "thread": { "model": "qwen2.5" },
            "turns": [{
                "id": "turn_1", "status": "completed",
                "usage": { "input_tokens": 50, "output_tokens": 9,
                           "prompt_cache_miss_tokens": 50 },
            }],
            "items": [
                { "id": "item_u", "turn_id": "turn_1", "kind": "user_message",
                  "status": "completed", "detail": "hi", "summary": "hi",
                  "started_at": "2026-11-06T10:00:00Z" },
                { "id": "item_r", "turn_id": "turn_1", "kind": "agent_reasoning",
                  "status": "completed", "detail": "hmm", "summary": "hmm",
                  "started_at": "2026-11-06T10:00:01Z", "ended_at": "2026-11-06T10:00:02Z" },
                { "id": "item_m", "turn_id": "turn_1", "kind": "agent_message",
                  "status": "completed", "detail": "done", "summary": "done",
                  "started_at": "2026-11-06T10:00:03Z", "ended_at": "2026-11-06T10:00:04Z" },
                { "id": "item_r2", "turn_id": "turn_1", "kind": "agent_reasoning",
                  "status": "completed", "detail": "tail", "summary": "tail",
                  "started_at": "2026-11-06T10:00:05Z", "ended_at": "2026-11-06T10:00:06Z" },
            ],
        });
        let events = super::replay_events_sourced(&detail, "ollama", "qwen2.5");
        let messages: Vec<&Value> = events
            .iter()
            .filter(|event| event["type"] == "assistant/message")
            .collect();
        assert_eq!(messages.len(), 3);
        for (index, message) in messages.iter().enumerate() {
            let has_usage = message["data"].get("usage").is_some();
            assert_eq!(
                has_usage,
                index == 1,
                "only the agent_message (messages[1]) carries the turn usage"
            );
            assert_eq!(
                message["data"]["message"]["source"],
                json!({ "kind": "model", "provider": "ollama", "model": "qwen2.5" })
            );
        }
        assert_eq!(
            messages[1]["data"]["usage"],
            json!({ "inputTokens": 50, "outputTokens": 9, "totalTokens": 59 })
        );
    }

    #[test]
    fn journal_seqs_are_zero_based_so_an_empty_journal_opens_at_minus_one() {
        // The client opens an empty journal only at cursor -1 and accepts a
        // live event only at cursor + 1, so the first event must be seq 0.
        assert!(replay_events(&json!({ "turns": [], "items": [] })).is_empty());
        let mut fresh = SessionMapper::resume_at(-1, 0, 0);
        let frames = fresh.map_envelope(&json!({
            "event": "turn.started", "turn_id": "turn_1", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        match &frames[0] {
            Frame::Event(event) => assert_eq!(event["seq"], 0),
            other => panic!("expected event frame, got {other:?}"),
        }
        let replayed = replay_events(&json!({
            "turns": [{ "id": "turn_1", "status": "completed" }],
            "items": [item("user_message", "completed", "hello")],
        }));
        let seqs: Vec<u64> = replayed
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(
            seqs,
            vec![0, 1, 2],
            "dense from 0: turn/start, user/message, turn/end"
        );
        // A tail resumed after that replay continues at cursor + 1.
        let mut tail = SessionMapper::resume_at(2, 1, 0);
        let frames = tail.map_envelope(&json!({
            "event": "turn.started", "turn_id": "turn_2", "timestamp": "2026-11-06T10:00:01Z", "payload": {}
        }));
        match &frames[0] {
            Frame::Event(event) => assert_eq!(event["seq"], 3),
            other => panic!("expected event frame, got {other:?}"),
        }
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
        assert!(
            mapper.abandon_open_attempts().is_empty(),
            "second abandon is a no-op"
        );
    }

    #[test]
    fn exit_plan_mode_carries_the_last_plan_file_body() {
        // Replay: durable items keep write_plan_file's input; exit_plan_mode's
        // own schema is empty, so the bridge attaches the plan body.
        let detail = json!({
            "turns": [{ "id": "turn_1", "status": "completed",
                "created_at": "2026-11-06T10:00:00Z", "started_at": "2026-11-06T10:00:00Z",
                "ended_at": "2026-11-06T10:00:09Z" }],
            "items": [
                { "id": "item_w", "turn_id": "turn_1", "kind": "tool_call",
                  "status": "completed", "detail": "ok", "summary": "write_plan_file: ok",
                  "metadata": { "arguments": { "content": "# Plan\nStep 1" } },
                  "started_at": "2026-11-06T10:00:05Z", "ended_at": "2026-11-06T10:00:06Z" },
                { "id": "item_e", "turn_id": "turn_1", "kind": "tool_call",
                  "status": "completed", "detail": "ok", "summary": "exit_plan_mode: ok",
                  "metadata": { "arguments": {} },
                  "started_at": "2026-11-06T10:00:07Z", "ended_at": "2026-11-06T10:00:08Z" },
            ],
        });
        let events = replay_events(&detail);
        let exit = events
            .iter()
            .find(|event| {
                event["type"] == "tool/call" && event["data"]["name"] == "exit_plan_mode"
            })
            .expect("exit_plan_mode call");
        let args: Value =
            serde_json::from_str(exit["data"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["plan"], "# Plan\nStep 1");

        // Live: the mapper remembers the write and injects it on the exit.
        let mut mapper = SessionMapper::default();
        let _ = mapper.map_envelope(&json!({
            "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        let _ = mapper.map_envelope(&json!({
            "event": "item.started", "item_id": "item_w",
            "timestamp": "2026-11-06T10:00:05Z",
            "payload": {
                "item": { "id": "item_w", "kind": "tool_call", "status": "in_progress" },
                "tool": { "name": "write_plan_file", "input": { "content": "# Live plan" } }
            }
        }));
        let frames = mapper.map_envelope(&json!({
            "event": "item.started", "item_id": "item_e",
            "timestamp": "2026-11-06T10:00:07Z",
            "payload": {
                "item": { "id": "item_e", "kind": "tool_call", "status": "in_progress" },
                "tool": { "name": "exit_plan_mode", "input": {} }
            }
        }));
        let call = frames
            .iter()
            .find_map(|frame| match frame {
                Frame::Event(event) if event["data"]["name"] == "exit_plan_mode" => {
                    Some(event.clone())
                }
                _ => None,
            })
            .expect("live exit_plan_mode call");
        let args: Value =
            serde_json::from_str(call["data"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["plan"], "# Live plan");

        // A follow stream that joined after the write seeds the body.
        let mut mapper = SessionMapper::default();
        mapper.seed_plan_content(latest_plan_content(&detail));
        let _ = mapper.map_envelope(&json!({
            "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        let frames = mapper.map_envelope(&json!({
            "event": "item.started", "item_id": "item_e",
            "timestamp": "2026-11-06T10:00:07Z",
            "payload": {
                "item": { "id": "item_e", "kind": "tool_call", "status": "in_progress" },
                "tool": { "name": "exit_plan_mode", "input": {} }
            }
        }));
        let call = frames
            .iter()
            .find_map(|frame| match frame {
                Frame::Event(event) if event["data"]["name"] == "exit_plan_mode" => {
                    Some(event.clone())
                }
                _ => None,
            })
            .expect("seeded exit_plan_mode call");
        let args: Value =
            serde_json::from_str(call["data"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["plan"], "# Plan\nStep 1");
    }

    #[test]
    fn plan_arguments_gain_a_title_only_when_the_body_lacks_one() {
        assert!(plan_has_title("# Plan\n\nStep"));
        assert!(plan_has_title("  # Plan"));
        assert!(plan_has_title("#\tPlan"));
        assert!(!plan_has_title("## Step"));
        assert!(!plan_has_title("#\nStep"));
        assert!(!plan_has_title("# \nStep"));
        assert!(!plan_has_title("Plan\n# Later"));

        let titled: Value =
            serde_json::from_str(&plan_call_arguments("# Real title\nbody")).unwrap();
        assert_eq!(titled["plan"], "# Real title\nbody");
        let untitled: Value =
            serde_json::from_str(&plan_call_arguments("## Step 1\nbody")).unwrap();
        assert_eq!(untitled["plan"], "# Plan\n\n## Step 1\nbody");
    }

    #[test]
    fn approved_plan_without_a_tool_item_synthesizes_the_plan_call() {
        // CodeSmith records an approved exit_plan_mode as a status item, so
        // replay attaches the plan call the card anchors on.
        let detail = json!({
            "turns": [{ "id": "turn_1", "status": "completed",
                "created_at": "2026-11-06T10:00:00Z", "started_at": "2026-11-06T10:00:00Z",
                "ended_at": "2026-11-06T10:00:09Z" }],
            "items": [
                { "id": "item_w", "turn_id": "turn_1", "kind": "file_change",
                  "status": "completed", "detail": "ok", "summary": "write_plan_file: ok",
                  "metadata": { "arguments": { "content": "## Step 1" } },
                  "started_at": "2026-11-06T10:00:05Z", "ended_at": "2026-11-06T10:00:06Z" },
                { "id": "item_s", "turn_id": "turn_1", "kind": "status",
                  "status": "completed", "summary": "Plan approved — the turn ends here",
                  "detail": "Plan approved — the turn ends here",
                  "started_at": "2026-11-06T10:00:08Z", "ended_at": "2026-11-06T10:00:08Z" },
            ],
        });
        let events = replay_events(&detail);
        let call = events
            .iter()
            .find(|event| event["type"] == "tool/call" && event["data"]["name"] == "exit_plan_mode")
            .expect("synthesized exit_plan_mode call");
        assert_eq!(call["data"]["callId"], "item_s");
        let args: Value =
            serde_json::from_str(call["data"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["plan"], "# Plan\n\n## Step 1");

        // The live stream synthesizes the same call at the approval status,
        // once.
        let mut mapper = SessionMapper::default();
        let _ = mapper.map_envelope(&json!({
            "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {}
        }));
        let _ = mapper.map_envelope(&json!({
            "event": "item.started", "item_id": "item_w",
            "timestamp": "2026-11-06T10:00:05Z",
            "payload": {
                "item": { "id": "item_w", "kind": "file_change", "status": "in_progress" },
                "tool": { "name": "write_plan_file", "input": { "content": "## Step 1" } }
            }
        }));
        let completed = json!({
            "event": "item.completed", "item_id": "item_s",
            "timestamp": "2026-11-06T10:00:08Z",
            "payload": { "item": { "id": "item_s", "kind": "status", "status": "completed",
                "summary": "Plan approved — the turn ends here",
                "detail": "Plan approved — the turn ends here" } }
        });
        let frames = mapper.map_envelope(&completed);
        let call = frames
            .iter()
            .find_map(|frame| match frame {
                Frame::Event(event) if event["data"]["name"] == "exit_plan_mode" => {
                    Some(event.clone())
                }
                _ => None,
            })
            .expect("live synthesized exit_plan_mode call");
        assert_eq!(call["data"]["callId"], "item_s");
        let args: Value =
            serde_json::from_str(call["data"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["plan"], "# Plan\n\n## Step 1");
        let again = mapper.map_envelope(&completed);
        assert!(
            !again.iter().any(|frame| matches!(
                frame,
                Frame::Event(event) if event["data"]["name"] == "exit_plan_mode"
            )),
            "the status re-delivery must not duplicate the plan call"
        );
    }

    #[test]
    fn event_times_are_the_runtime_instants_live_and_on_replay() {
        // The client renders message clocks from event `time` and a turn's
        // "took Ns" from turn/end − turn/start, so a reload's replay must
        // show the instants the live stream did — never the reload's clock.
        let ms = |at: &str| epoch_ms(at).expect("fixture timestamp parses");
        let expected: Vec<(String, i64)> = [
            ("turn/start", "2026-11-06T10:00:00Z"),
            ("user/message", "2026-11-06T10:00:00.100Z"),
            ("step/start", "2026-11-06T10:00:01Z"),
            ("assistant/message", "2026-11-06T10:00:04Z"),
            ("step/end", "2026-11-06T10:00:04Z"),
            ("tool/call", "2026-11-06T10:00:05Z"),
            ("tool/result", "2026-11-06T10:00:06Z"),
            ("turn/end", "2026-11-06T10:00:09Z"),
        ]
        .into_iter()
        .map(|(kind, at)| (kind.to_string(), ms(at)))
        .collect();
        let times = |events: Vec<Value>| -> Vec<(String, i64)> {
            events
                .iter()
                .map(|e| {
                    (
                        e["type"].as_str().unwrap().to_string(),
                        e["time"].as_i64().unwrap(),
                    )
                })
                .collect()
        };

        let user = json!({ "id": "item_u", "turn_id": "turn_1", "kind": "user_message",
            "status": "completed", "detail": "hi", "summary": "hi",
            "started_at": "2026-11-06T10:00:00.100Z", "ended_at": "2026-11-06T10:00:00.100Z" });
        let replayed = replay_events(&json!({
            "turns": [{ "id": "turn_1", "status": "completed",
                "created_at": "2026-11-06T10:00:00Z", "started_at": "2026-11-06T10:00:00Z",
                "ended_at": "2026-11-06T10:00:09Z" }],
            "items": [
                user,
                { "id": "item_r", "turn_id": "turn_1", "kind": "agent_reasoning",
                  "status": "completed", "detail": "hmm", "summary": "hmm",
                  "started_at": "2026-11-06T10:00:01Z", "ended_at": "2026-11-06T10:00:04Z" },
                { "id": "item_t", "turn_id": "turn_1", "kind": "tool_call",
                  "status": "completed", "detail": "ok", "summary": "exec_shell: ok",
                  "started_at": "2026-11-06T10:00:05Z", "ended_at": "2026-11-06T10:00:06Z" },
            ],
        }));
        assert_eq!(times(replayed), expected, "replay stamps record instants");

        let mut mapper = SessionMapper::default();
        let mut frames = Vec::new();
        for envelope in [
            json!({ "event": "turn.started", "timestamp": "2026-11-06T10:00:00Z", "payload": {} }),
            json!({ "event": "item.started", "item_id": "item_u", "timestamp": "2026-11-06T10:00:00.100Z",
                    "payload": { "item": user } }),
            json!({ "event": "item.started", "item_id": "item_r", "timestamp": "2026-11-06T10:00:01Z",
                    "payload": { "item": { "id": "item_r", "kind": "agent_reasoning", "status": "in_progress" } } }),
            json!({ "event": "item.delta", "item_id": "item_r", "timestamp": "2026-11-06T10:00:02Z",
                    "payload": { "delta": "hmm", "kind": "agent_reasoning" } }),
            json!({ "event": "item.completed", "item_id": "item_r", "timestamp": "2026-11-06T10:00:04Z",
                    "payload": { "item": { "id": "item_r", "kind": "agent_reasoning", "status": "completed", "detail": "hmm" } } }),
            json!({ "event": "item.started", "item_id": "item_t", "timestamp": "2026-11-06T10:00:05Z",
                    "payload": { "item": { "id": "item_t", "kind": "tool_call", "status": "in_progress" },
                                 "tool": { "name": "exec_shell", "input": {} } } }),
            // Only the `created_at` alias: clients must not require both.
            json!({ "event": "item.completed", "item_id": "item_t", "created_at": "2026-11-06T10:00:06Z",
                    "payload": { "item": { "id": "item_t", "kind": "tool_call", "status": "completed", "detail": "ok" } } }),
            json!({ "event": "turn.completed", "timestamp": "2026-11-06T10:00:09Z",
                    "payload": { "turn": { "status": "completed" } } }),
        ] {
            frames.extend(mapper.map_envelope(&envelope));
        }
        let live: Vec<Value> = frames
            .iter()
            .filter_map(|f| match f {
                Frame::Event(event) => Some(event.clone()),
                Frame::Stream(_) => None,
            })
            .collect();
        assert_eq!(times(live), expected, "live stamps envelope instants");
        // Stream chunks share their envelope's instant (the client times the
        // first token against step/start).
        let delta = frames
            .iter()
            .find_map(|f| match f {
                Frame::Stream(frame) if frame["chunk"]["type"] == "reasoning-delta" => Some(frame),
                _ => None,
            })
            .expect("the delta streams");
        assert_eq!(delta["time"], ms("2026-11-06T10:00:02Z"));
    }
}
