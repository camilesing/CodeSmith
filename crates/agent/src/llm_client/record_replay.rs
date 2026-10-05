//! Record / replay LLM clients — the session-log seed for keyless replay
//! tests (dev plan product capability 1+2, first slice).
//!
//! [`RecordingClient`] decorates any [`LlmClientHandle`] and writes one
//! JSONL line per model call: the **full request envelope** (the exact
//! `MessageRequest` handed to the client — after every extension
//! transform, including the complete tool schema and call parameters) plus
//! the streamed response events (or the non-streaming response). For the
//! recorded session, any historical provider request is a pure function of
//! the log: `serde_json::from_str::<RecordLine>(line)?.request`.
//!
//! [`ReplayClient`] loads the same file and feeds the recorded events back
//! through the *real* turn loop — a recorded model stream drives the
//! engine with zero network and zero API keys (the dsh `llm-replay`
//! pattern).
//!
//! # Wiring
//!
//! Hosts wrap their resolved client: `CODESMITH_RECORD_LLM=<path>` (the
//! TUI's `build_engine` honors it; opening the file fails loud). The
//! wrapper sits at the client boundary, so utility-model, seam, and
//! compaction calls are recorded too — the log covers **every** model
//! request, which is the point.
//!
//! # Known limitations
//!
//! - Replay is strict FIFO: each `create_message_stream`/`create_message`
//!   consumes the next line; it does not match requests (a retried call
//!   eats an extra line — replaying failures faithfully is future work).
//! - A recorded stream replays verbatim, including a missing terminal
//!   `MessageStop` (the engine treats that as a disconnect, exactly like
//!   the original run). Unlike the test mock, replay never auto-appends.
//! - Recording is best-effort *for the session*: on the first write error
//!   it stops recording and logs loudly (target `codesmith_llm_record`);
//!   the live turn is never failed by the recorder.
//! - The file records full model I/O — never commit fixtures containing
//!   secrets, and mind privacy when sharing recordings.

use std::collections::VecDeque;
use std::future::Future;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use futures_util::StreamExt; // .next() on the tee'd stream
use serde::{Deserialize, Serialize};

use crate::models::{MessageDelta, MessageRequest, MessageResponse, StreamEvent, Usage};

use super::{LlmClient, LlmClientHandle, StreamEventBox};

/// Current `RecordLine::version`.
pub const RECORD_VERSION: u32 = 1;

/// One JSONL line: a full request envelope + its outcome. The request is
/// captured verbatim at the client boundary — after every extension
/// transform — so replaying history needs nothing but the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordLine {
    pub version: u32,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub request: MessageRequest,
    pub outcome: RecordOutcome,
}

/// What the model did with the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RecordOutcome {
    /// A streaming call: the events exactly as the stream yielded them
    /// (a run that died mid-stream is recorded partial — replay
    /// reproduces the disconnect).
    Stream { response_events: Vec<StreamEvent> },
    /// A non-streaming call. Boxed: `MessageResponse` is much larger
    /// than the `Stream` variant (serde-transparent either way).
    Message {
        response_message: Box<MessageResponse>,
    },
}

// === RecordingClient ========================================================

/// LLM-call recorder: decorates an inner client, tee-ing each call to a
/// JSONL file (one [`RecordLine`] per call). See the module docs for the
/// wiring + limitations.
pub struct RecordingClient {
    inner: LlmClientHandle,
    writer: Arc<Mutex<Option<BufWriter<std::fs::File>>>>,
    path: PathBuf,
}

impl RecordingClient {
    /// Wrap `inner`, appending to `path`. The file (and its parent
    /// directory) must already exist — misconfiguration fails loud here,
    /// at the earliest resolvable point.
    pub fn new(inner: LlmClientHandle, path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            inner,
            writer: Arc::new(Mutex::new(Some(BufWriter::new(file)))),
            path,
        })
    }
}

impl LlmClient for RecordingClient {
    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn create_message(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
        let provider = self.inner.provider_name().to_string();
        let model = self.inner.model().to_string();
        let base_url = self.inner.base_url().to_string();
        let writer = Arc::clone(&self.writer);
        let path = self.path.clone();
        let inner = self.inner.clone();
        Box::pin(async move {
            let response = inner.create_message(request.clone()).await?;
            let line = RecordLine {
                version: RECORD_VERSION,
                provider,
                model,
                base_url,
                request,
                outcome: RecordOutcome::Message {
                    response_message: Box::new(response.clone()),
                },
            };
            write_line_shared(&writer, &path, &line);
            Ok(response)
        })
    }

    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
        let provider = self.inner.provider_name().to_string();
        let model = self.inner.model().to_string();
        let base_url = self.inner.base_url().to_string();
        let writer = Arc::clone(&self.writer);
        let path = self.path.clone();
        let inner = self.inner.clone();
        Box::pin(async move {
            let stream = inner.create_message_stream(request.clone()).await?;
            // Tee: forward every event while collecting them. The line is
            // written when the terminal proof (`MessageStop`) is SEEN —
            // consumers may drop the stream right after it instead of
            // draining to end, so stream-end alone is not a reliable write
            // point. Streams that end without it (disconnects) record their
            // partial events at end.
            let tee = futures_util::stream::unfold(
                TeeState {
                    stream,
                    events: Vec::new(),
                    written: false,
                    envelope: RecordEnvelope {
                        request,
                        provider,
                        model,
                        base_url,
                    },
                    writer,
                    path,
                },
                |mut state| async move {
                    match state.stream.next().await {
                        Some(Ok(event)) => {
                            state.events.push(event.clone());
                            let terminal = matches!(event, StreamEvent::MessageStop);
                            if terminal && !state.written {
                                state.write_line();
                            }
                            Some((Ok(event), state))
                        }
                        Some(Err(e)) => Some((Err(e), state)),
                        None => {
                            state.write_line();
                            None
                        }
                    }
                },
            );
            Ok(Box::pin(tee) as StreamEventBox)
        })
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }
}

/// Shared append helper (the boxed futures can't borrow `&self`).
fn write_line_shared(
    writer: &Arc<Mutex<Option<BufWriter<std::fs::File>>>>,
    path: &Path,
    line: &RecordLine,
) {
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    let Some(w) = guard.as_mut() else {
        return;
    };
    let serialized = match serde_json::to_string(line) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                target: "codesmith_llm_record",
                "LLM recording serialize failed; stopping recording to {}: {e}",
                path.display()
            );
            *guard = None;
            return;
        }
    };
    if let Err(e) = writeln!(w, "{serialized}").and_then(|()| w.flush()) {
        tracing::error!(
            target: "codesmith_llm_record",
            "LLM recording write failed; stopping recording to {}: {e}",
            path.display()
        );
        *guard = None;
    }
}

/// Per-stream tee state (the boxed future cannot borrow `&self`).
struct TeeState {
    stream: StreamEventBox,
    events: Vec<StreamEvent>,
    /// Set once the line is written (MessageStop seen, or stream ended);
    /// guards against a double write when a stopped stream is also drained.
    written: bool,
    envelope: RecordEnvelope,
    writer: Arc<Mutex<Option<BufWriter<std::fs::File>>>>,
    path: PathBuf,
}

/// The envelope fields captured before the call (request + client identity).
struct RecordEnvelope {
    request: MessageRequest,
    provider: String,
    model: String,
    base_url: String,
}

impl TeeState {
    fn write_line(&mut self) {
        if self.written {
            return;
        }
        self.written = true;
        let line = RecordLine {
            version: RECORD_VERSION,
            provider: self.envelope.provider.clone(),
            model: self.envelope.model.clone(),
            base_url: self.envelope.base_url.clone(),
            request: self.envelope.request.clone(),
            outcome: RecordOutcome::Stream {
                response_events: self.events.clone(),
            },
        };
        write_line_shared(&self.writer, &self.path, &line);
    }
}

// === ReplayClient ===========================================================

/// Replays a recording file through the real turn loop: strict FIFO, one
/// line per call, no request matching (see the module docs). Loading fails
/// loud — a malformed line names its number.
pub struct ReplayClient {
    lines: Mutex<VecDeque<RecordLine>>,
    model: String,
    base_url: String,
    calls: std::sync::atomic::AtomicUsize,
}

impl ReplayClient {
    /// Load a recording produced by [`RecordingClient`].
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())
            .map_err(|e| anyhow!("replay fixture {}: {e}", path.as_ref().display()))?;
        let mut lines = VecDeque::new();
        for (idx, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
            let parsed: RecordLine = serde_json::from_str(line).map_err(|e| {
                anyhow!(
                    "replay fixture {} line {}: {e}",
                    path.as_ref().display(),
                    idx + 1
                )
            })?;
            if parsed.version != RECORD_VERSION {
                return Err(anyhow!(
                    "replay fixture {} line {}: unsupported version {} (expected {RECORD_VERSION})",
                    path.as_ref().display(),
                    idx + 1,
                    parsed.version
                ));
            }
            lines.push_back(parsed);
        }
        if lines.is_empty() {
            return Err(anyhow!(
                "replay fixture {} is empty",
                path.as_ref().display()
            ));
        }
        let model = lines[0].model.clone();
        let base_url = lines[0].base_url.clone();
        Ok(Self {
            lines: Mutex::new(lines),
            model,
            base_url,
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// How many lines remain (assertions in tests).
    pub fn remaining(&self) -> usize {
        self.lines.lock().map(|l| l.len()).unwrap_or(0)
    }

    fn pop_line(&self, call: &str) -> Result<RecordLine> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.lines
            .lock()
            .map_err(|_| anyhow!("replay queue poisoned"))?
            .pop_front()
            .ok_or_else(|| {
                anyhow!("ReplayClient: fixture exhausted at {call} (call #{n}); record more turns")
            })
    }
}

impl LlmClient for ReplayClient {
    fn provider_name(&self) -> &'static str {
        "replay"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn create_message(
        &self,
        _request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
        let line = match self.pop_line("create_message") {
            Ok(l) => l,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        Box::pin(async move {
            match line.outcome {
                RecordOutcome::Message { response_message } => Ok(*response_message),
                RecordOutcome::Stream { response_events } => {
                    Ok(synthesize_message_response(&response_events, &line.model))
                }
            }
        })
    }

    fn create_message_stream(
        &self,
        _request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
        let line = match self.pop_line("create_message_stream") {
            Ok(l) => l,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        Box::pin(async move {
            match line.outcome {
                RecordOutcome::Stream { response_events } => {
                    let s = futures_util::stream::iter(response_events.into_iter().map(Ok));
                    Ok(Box::pin(s) as StreamEventBox)
                }
                RecordOutcome::Message { .. } => Err(anyhow!(
                    "ReplayClient: next fixture line is a non-stream record; \
                     the recorded call order does not match this run"
                )),
            }
        })
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Collapse streamed events into a `MessageResponse` (text deltas joined,
/// first stop_reason wins) — the fallback when a stream record is consumed
/// by a non-streaming call (compaction paths). Mirrors the test mock's
/// synthesize helper.
fn synthesize_message_response(events: &[StreamEvent], model: &str) -> MessageResponse {
    use crate::models::Delta;

    let mut text = String::new();
    let mut stop_reason: Option<String> = None;
    let mut usage = Usage::default();
    for event in events {
        match event {
            StreamEvent::MessageStart { message } => {
                usage = message.usage.clone();
            }
            StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text: t },
                ..
            } => text.push_str(t),
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: sr, ..
                },
                usage: u,
            } => {
                if sr.is_some() {
                    stop_reason = sr.clone();
                }
                if let Some(u) = u {
                    usage = u.clone();
                }
            }
            _ => {}
        }
    }
    MessageResponse {
        id: "replay_msg".to_string(),
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![crate::models::ContentBlock::Text {
            text,
            cache_control: None,
        }],
        model: model.to_string(),
        stop_reason,
        stop_sequence: None,
        container: None,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentBlock, ContentBlockStart, Delta, MessageDelta};

    /// A minimal streaming turn covering every serializable shape: message
    /// start, text block, tool-use block with json deltas, message delta,
    /// stop.
    fn sample_turn() -> Vec<StreamEvent> {
        vec![
            StreamEvent::MessageStart {
                message: MessageResponse {
                    container: None,
                    id: "msg_1".into(),
                    r#type: "message".into(),
                    role: "assistant".into(),
                    content: Vec::new(),
                    model: "test-model".into(),
                    stop_reason: None,
                    stop_sequence: None,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 0,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                        reasoning_tokens: None,
                        reasoning_replay_tokens: None,
                        server_tool_use: None,
                    },
                },
            },
            StreamEvent::ContentBlockStart {
                index: 0,
                content_block: ContentBlockStart::Text {
                    text: String::new(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::TextDelta {
                    text: "hello".into(),
                },
            },
            StreamEvent::ContentBlockStop { index: 0 },
            StreamEvent::ContentBlockStart {
                index: 1,
                content_block: ContentBlockStart::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::Value::Null,
                    caller: None,
                },
            },
            StreamEvent::ContentBlockDelta {
                index: 1,
                delta: Delta::InputJsonDelta {
                    partial_json: r#"{"path":"README.md"}"#.into(),
                },
            },
            StreamEvent::ContentBlockStop { index: 1 },
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some("tool_use".into()),
                    stop_sequence: None,
                },
                usage: None,
            },
            StreamEvent::MessageStop,
        ]
    }

    #[test]
    fn stream_event_serde_round_trip() {
        for event in sample_turn() {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: StreamEvent = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&back).expect("re-serialize");
            assert_eq!(json, json2, "stable wire shape");
        }
    }

    #[test]
    fn record_line_round_trips_full_request_envelope() {
        // The envelope-snapshot claim: any historical request is a pure
        // function of the log — deserialize a line's request back into a
        // `MessageRequest` with the tool schema intact.
        let request = MessageRequest {
            model: "test-model".into(),
            messages: vec![crate::models::Message {
                role: "user".into(),
                content: vec![ContentBlock::Text {
                    text: "hi".into(),
                    cache_control: None,
                }],
            }],
            max_tokens: 1024,
            system: Some(crate::models::SystemPrompt::Text("sys".into())),
            tools: Some(vec![crate::models::Tool {
                tool_type: Some("function".into()),
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                allowed_callers: None,
                cache_control: None,
                strict: None,
                defer_loading: None,
                input_examples: None,
            }]),
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: Some(true),
            temperature: Some(0.7),
            top_p: None,
        };
        let line = RecordLine {
            version: RECORD_VERSION,
            provider: "mock".into(),
            model: "test-model".into(),
            base_url: "https://example.test".into(),
            request,
            outcome: RecordOutcome::Stream {
                response_events: sample_turn(),
            },
        };
        let json = serde_json::to_string(&line).expect("serialize line");
        let back: RecordLine = serde_json::from_str(&json).expect("deserialize line");
        assert_eq!(back.version, RECORD_VERSION);
        let tools = back.request.tools.expect("tool schema survives");
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(back.request.temperature, Some(0.7));
        match back.outcome {
            RecordOutcome::Stream { response_events } => {
                assert_eq!(response_events.len(), sample_turn().len());
            }
            other => panic!("expected stream outcome, got {other:?}"),
        }
    }
}
