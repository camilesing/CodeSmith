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
//! wrapper sits at the client boundary, so every call served through the
//! wrapped handle — including utility/seam/compaction calls that share it
//! — is recorded. Known gap: a cross-provider `[utility_model]` client is
//! built separately and remains unwrapped, so recordings of sessions that
//! exercise it are incomplete.
//!
//! # Known limitations
//!
//! - Replay is strict FIFO: each `create_message_stream`/`create_message`
//!   consumes the next line; it does not match requests. Failed calls are
//!   recorded too (an `error` outcome) and replay returns the same
//!   failure, so a live run's transparent retries line up with their
//!   recorded counterparts.
//! - A recorded stream replays verbatim, including a missing terminal
//!   `MessageStop` (the engine treats that as a disconnect, exactly like
//!   the original run). Unlike the test mock, replay never auto-appends.
//! - Recording is best-effort *for the session*: on the first write error
//!   it stops recording and logs loudly (target `codesmith_llm_record`);
//!   the live turn is never failed by the recorder.
//! - Writes are synchronous `fs` IO under a mutex, executed from inside
//!   the async call path (opt-in dev mode, one small line per model call —
//!   accepted tradeoff; a dedicated writer thread would change failure
//!   semantics for little gain).
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
    /// The call failed before yielding a response (HTTP error, auth
    /// failure, stream that never opened). Replayed as the same failure
    /// so strict-FIFO order stays aligned with the recorded run.
    Error { message: String },
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
            let result = inner.create_message(request.clone()).await;
            // Record failures too: every call through the wrapper must
            // yield exactly one line — a failed call that vanishes
            // desyncs strict-FIFO replay for every later call.
            let line = RecordLine {
                version: RECORD_VERSION,
                provider,
                model,
                base_url,
                request,
                outcome: match &result {
                    Ok(response) => RecordOutcome::Message {
                        response_message: Box::new(response.clone()),
                    },
                    Err(e) => RecordOutcome::Error {
                        message: e.to_string(),
                    },
                },
            };
            write_line_shared(&writer, &path, &line);
            result
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
            let stream = match inner.create_message_stream(request.clone()).await {
                Ok(stream) => stream,
                Err(e) => {
                    // Same one-line-per-call invariant as `create_message`:
                    // a stream that never opened still consumed a model
                    // call slot in the recorded run.
                    write_line_shared(
                        &writer,
                        &path,
                        &RecordLine {
                            version: RECORD_VERSION,
                            provider,
                            model,
                            base_url,
                            request,
                            outcome: RecordOutcome::Error {
                                message: e.to_string(),
                            },
                        },
                    );
                    return Err(e);
                }
            };
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
                        Some(Err(e)) => {
                            // The engine's stream reducer returns on the
                            // FIRST error and drops the stream — without a
                            // write here, a disconnect is never recorded at
                            // all, desyncing strict-FIFO replay. The
                            // `written` flag keeps a later drain from
                            // double-writing.
                            state.write_line();
                            Some((Err(e), state))
                        }
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

    // Forwarded, unrecorded: the recorder is a decorator and must not
    // disable capabilities the inner client supports. Before these
    // forwards, the trait defaults took over under CODESMITH_RECORD_LLM —
    // silently breaking FIM completions (the TUI hands this same handle to
    // the FIM tool) and translation with errors that named the real
    // provider. Recording these calls can come later; the JSONL contract
    // only covers create_message/create_message_stream.
    fn fim_completion(
        &self,
        model: String,
        prompt: String,
        suffix: String,
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        self.inner.fim_completion(model, prompt, suffix, max_tokens)
    }

    fn translate(
        &self,
        text: String,
        model: String,
        target_language: String,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        self.inner.translate(text, model, target_language)
    }

    fn health_check(&self) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>> {
        self.inner.health_check()
    }

    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        self.inner.list_models()
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
                // `events` is never read after this one-shot write.
                response_events: std::mem::take(&mut self.events),
            },
        };
        write_line_shared(&self.writer, &self.path, &line);
    }
}

impl Drop for TeeState {
    fn drop(&mut self) {
        // Cancelled mid-stream (turn cancelled, shutdown): the consumer
        // drops the tee before any terminal proof — still emit the partial
        // line so the one-line-per-call invariant (and strict-FIFO replay)
        // holds. Idempotent via `written`.
        self.write_line();
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
        for (idx, line) in text
            .lines()
            // Physical line numbers in load errors: enumerate before the
            // blank-line filter so "line {idx + 1}" names the real line.
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
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
        Box::pin(async move {
            // Pop at first poll, not at call time: every other LlmClient
            // impl is lazy, and a future built then dropped unpolled (a
            // losing select! branch, a timeout kill) must not silently
            // consume a fixture line.
            let line = self.pop_line("create_message")?;
            match line.outcome {
                RecordOutcome::Message { response_message } => Ok(*response_message),
                RecordOutcome::Stream { response_events } => {
                    Ok(synthesize_message_response(&response_events, &line.model))
                }
                RecordOutcome::Error { message } => {
                    Err(anyhow!("ReplayClient: recorded call failed: {message}"))
                }
            }
        })
    }

    fn create_message_stream(
        &self,
        _request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
        Box::pin(async move {
            // Lazy for the same reason as `create_message`.
            let line = self.pop_line("create_message_stream")?;
            match line.outcome {
                RecordOutcome::Stream { response_events } => {
                    let s = futures_util::stream::iter(response_events.into_iter().map(Ok));
                    Ok(Box::pin(s) as StreamEventBox)
                }
                RecordOutcome::Message { .. } => Err(anyhow!(
                    "ReplayClient: next fixture line is a non-stream record; \
                     the recorded call order does not match this run"
                )),
                RecordOutcome::Error { message } => {
                    Err(anyhow!("ReplayClient: recorded call failed: {message}"))
                }
            }
        })
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Collapse streamed events into a `MessageResponse` (text deltas joined,
/// last non-None stop_reason wins — the same accumulation the executor's
/// stream reducer performs) — the fallback when a stream record is consumed
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

    // ── RecordingClient tee behavior ─────────────────────────────────────

    fn temp_recording_path(tag: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "codesmith-record-replay-test-{}-{tag}-{n}.jsonl",
            std::process::id()
        ))
    }

    fn minimal_request() -> MessageRequest {
        MessageRequest {
            model: "test-model".into(),
            messages: vec![crate::models::Message {
                role: "user".into(),
                content: vec![ContentBlock::Text {
                    text: "hi".into(),
                    cache_control: None,
                }],
            }],
            max_tokens: 16,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: Some(true),
            temperature: None,
            top_p: None,
        }
    }

    /// A mock whose stream yields two events and then errors — the
    /// disconnect shape the engine drops the stream on.
    struct DisconnectingMock;

    impl LlmClient for DisconnectingMock {
        fn provider_name(&self) -> &'static str {
            "mock"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
            Box::pin(async { Err(anyhow!("not used in this test")) })
        }

        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            Box::pin(async {
                let events = vec![
                    Ok(StreamEvent::ContentBlockStart {
                        index: 0,
                        content_block: ContentBlockStart::Text {
                            text: String::new(),
                        },
                    }),
                    Ok(StreamEvent::ContentBlockDelta {
                        index: 0,
                        delta: Delta::TextDelta {
                            text: "partial".into(),
                        },
                    }),
                    Err(anyhow!("connection reset")),
                ];
                Ok(Box::pin(futures_util::stream::iter(events)) as StreamEventBox)
            })
        }
    }

    #[tokio::test]
    async fn stream_error_records_partial_events() {
        // The engine's reducer returns on the first error and drops the
        // stream; without a write on the error path the call was never
        // recorded, desyncing strict-FIFO replay.
        let path = temp_recording_path("err");
        let recorder = RecordingClient::new(Arc::new(DisconnectingMock), &path).expect("open");
        let handle: LlmClientHandle = Arc::new(recorder);
        let mut stream = handle
            .create_message_stream(minimal_request())
            .await
            .expect("stream starts");

        let mut saw_error = false;
        while let Some(event) = futures_util::StreamExt::next(&mut stream).await {
            if event.is_err() {
                saw_error = true;
                break; // drop the stream, exactly like the reducer does
            }
        }
        assert!(saw_error);
        drop(stream);

        let text = std::fs::read_to_string(&path).expect("recording file written");
        let line: RecordLine =
            serde_json::from_str(text.lines().next().expect("one line")).expect("valid JSONL");
        match line.outcome {
            RecordOutcome::Stream { response_events } => assert_eq!(
                response_events.len(),
                2,
                "the events seen before the error are recorded partial"
            ),
            other => panic!("expected stream outcome, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A mock whose non-streaming call always fails — the shape that used
    /// to vanish from recordings entirely.
    struct FailingMock;

    impl LlmClient for FailingMock {
        fn provider_name(&self) -> &'static str {
            "mock"
        }
        fn model(&self) -> &str {
            "test-model"
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
            Box::pin(async { Err(anyhow!("429 rate limited")) })
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            Box::pin(async { Err(anyhow!("not used in this test")) })
        }
    }

    #[tokio::test]
    async fn failed_create_message_records_one_line_and_replays_as_err() {
        // One line per call, success or failure: without the error line a
        // failed call is invisible to the recording and strict-FIFO replay
        // pops the wrong line for every later call.
        let path = temp_recording_path("fail");
        let recorder = RecordingClient::new(Arc::new(FailingMock), &path).expect("open");
        let handle: LlmClientHandle = Arc::new(recorder);
        let err = handle
            .create_message(minimal_request())
            .await
            .expect_err("the call fails");
        assert!(err.to_string().contains("429"));

        let text = std::fs::read_to_string(&path).expect("failure was recorded");
        let line: RecordLine = serde_json::from_str(text.lines().next().expect("exactly one line"))
            .expect("valid JSONL");
        match line.outcome {
            RecordOutcome::Error { message } => {
                assert_eq!(message, "429 rate limited");
            }
            other => panic!("expected error outcome, got {other:?}"),
        }

        let replay = ReplayClient::load(&path).expect("load");
        assert_eq!(replay.remaining(), 1);
        let replayed = replay
            .create_message(minimal_request())
            .await
            .expect_err("failure replays as a failure");
        assert_eq!(
            replayed.to_string(),
            "ReplayClient: recorded call failed: 429 rate limited"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A mock stream that yields one event and then never resolves — the
    /// cancelled-mid-stream shape (turn cancelled, shutdown).
    struct SlowStartMock;

    impl LlmClient for SlowStartMock {
        fn provider_name(&self) -> &'static str {
            "mock"
        }
        fn model(&self) -> &str {
            "test-model"
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
            Box::pin(async { Err(anyhow!("not used in this test")) })
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            Box::pin(async {
                let head = vec![Ok(StreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: ContentBlockStart::Text {
                        text: String::new(),
                    },
                })];
                let s = futures_util::stream::iter(head)
                    .chain(futures_util::stream::pending::<Result<StreamEvent>>());
                Ok(Box::pin(s) as StreamEventBox)
            })
        }
    }

    #[tokio::test]
    async fn dropped_mid_stream_records_partial_line() {
        // The tee's Drop closes the cancellation gap: dropping the stream
        // before any terminal proof must still write the partial line.
        let path = temp_recording_path("drop");
        let recorder = RecordingClient::new(Arc::new(SlowStartMock), &path).expect("open");
        let handle: LlmClientHandle = Arc::new(recorder);
        let mut stream = handle
            .create_message_stream(minimal_request())
            .await
            .expect("stream starts");
        let first = futures_util::StreamExt::next(&mut stream)
            .await
            .expect("one event before cancellation");
        assert!(first.is_ok());
        drop(stream); // cancel mid-stream, no stop and no error

        let text = std::fs::read_to_string(&path).expect("partial line written by Drop");
        let line: RecordLine =
            serde_json::from_str(text.lines().next().expect("one line")).expect("valid JSONL");
        match line.outcome {
            RecordOutcome::Stream { response_events } => assert_eq!(
                response_events.len(),
                1,
                "the event seen before cancellation is recorded partial"
            ),
            other => panic!("expected stream outcome, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fim_completion_is_forwarded_not_stubbed() {
        // The recorder is a decorator: before the forwards, the trait
        // default answered with a hard error naming the real provider.
        struct FimMock;

        impl LlmClient for FimMock {
            fn provider_name(&self) -> &'static str {
                "mock"
            }
            fn model(&self) -> &str {
                "test-model"
            }
            fn create_message(
                &self,
                _request: MessageRequest,
            ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
                Box::pin(async { Err(anyhow!("not used in this test")) })
            }
            fn create_message_stream(
                &self,
                _request: MessageRequest,
            ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
                Box::pin(async { Err(anyhow!("not used in this test")) })
            }
            fn fim_completion(
                &self,
                _model: String,
                prompt: String,
                _suffix: String,
                _max_tokens: u32,
            ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
                Box::pin(async move { Ok(format!("fim:{prompt}")) })
            }
        }

        let path = temp_recording_path("fim");
        let recorder = RecordingClient::new(Arc::new(FimMock), &path).expect("open");
        let handle: LlmClientHandle = Arc::new(recorder);
        let out = handle
            .fim_completion("test-model".into(), "prefix".into(), "suffix".into(), 32)
            .await
            .expect("forwarded to the inner client");
        assert_eq!(out, "fim:prefix");
        let _ = std::fs::remove_file(&path);
    }
}
