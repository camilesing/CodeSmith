//! # Agent executor loop
//!
//! The LangChain `AgentExecutor` analog: a loop that drives
//! [`LlmClient::create_message_stream`] → parses tool calls → invokes
//! [`crate::tools::Tool`]s → feeds [`ToolResult`]s back into the conversation
//! history, with a step cap. This is the **framework-core** executor — minimal
//! and host-agnostic (no compaction, capacity, steer, or sub-agent branches);
//! the production `Engine` in `codesmith-agent-runtime` carries those guardrails
//! and is migrated onto this trait in a later ROADMAP §E slice.
//!
//! The reference impl [`DefaultAgentExecutor`] is validated against an inline
//! mock LLM + mock tool in the tests below — no `codesmith-providers`
//! dependency required, mirroring how the provider foundation slice validated
//! against `mock`.
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use futures_util::StreamExt;

use crate::callback::{Callback, StopReason};
use crate::llm_client::{LlmClientHandle, StreamEventBox};
use crate::memory::ChatHistory;
use crate::models::{
    ContentBlock, ContentBlockStart, Delta, Message, MessageDelta, MessageRequest, StreamEvent,
    SystemPrompt,
};
use crate::tools::{ToolError, ToolSet};

/// Configuration for [`DefaultAgentExecutor`].
#[derive(Debug, Clone)]
pub struct AgentExecutorConfig {
    /// Maximum LLM round-trips (iterations of the tool loop). Once exceeded,
    /// the run stops with [`StopReason::MaxSteps`].
    pub max_steps: u32,
    /// `max_tokens` sent on each `MessageRequest`.
    pub max_tokens: u32,
    /// System prompt attached to every request, if any.
    pub system: Option<SystemPrompt>,
    /// Sampling temperature, if any.
    pub temperature: Option<f32>,
}

impl Default for AgentExecutorConfig {
    fn default() -> Self {
        Self {
            max_steps: 50,
            max_tokens: 4096,
            system: None,
            temperature: None,
        }
    }
}

/// Drives an LLM through a tool-calling loop to completion.
///
/// Dyn-safe: a host holds `Arc<dyn AgentExecutor>`. The single [`run`](Self::run)
/// method appends the user message, loops LLM↔tools, and returns the
/// [`StopReason`]. The conversation transcript is mutated in place through
/// [`ChatHistory`], so the host keeps ownership of where the bytes live.
pub trait AgentExecutor: Send + Sync {
    /// Run the agent to completion against `history`, starting from `user_text`.
    ///
    /// Returns the [`StopReason`]. The future borrows `self` and `history` for
    /// `'a` (boxed, matching [`LlmClient`]'s dyn-safe style).
    fn run<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason>> + Send + 'a>>;
}

/// Reference [`AgentExecutor`] — the minimal LLM↔tool loop.
///
/// Holds an [`LlmClientHandle`], a [`ToolSet`], a [`Callback`], and config.
/// Clone the `Arc`s (cheap) per run; nothing is mutated on `self`.
pub struct DefaultAgentExecutor {
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
}

impl DefaultAgentExecutor {
    /// Construct from the four collaborators + config.
    #[must_use]
    pub fn new(
        client: LlmClientHandle,
        tools: Arc<ToolSet>,
        callback: Arc<dyn Callback>,
        config: AgentExecutorConfig,
    ) -> Self {
        Self {
            client,
            tools,
            callback,
            config,
        }
    }
}

impl AgentExecutor for DefaultAgentExecutor {
    fn run<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason>> + Send + 'a>> {
        Box::pin(self.run_inner(history, user_text))
    }
}

impl DefaultAgentExecutor {
    async fn run_inner<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Result<StopReason> {
        // Cheap Arc clones so the loop body borrows locals, not `&self` fields.
        let client = self.client.clone();
        let tools = self.tools.clone();
        let callback = self.callback.clone();
        let max_steps = self.config.max_steps;
        let max_tokens = self.config.max_tokens;
        let system = self.config.system.clone();
        let temperature = self.config.temperature;

        // Seed the transcript with the user turn.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: user_text,
                cache_control: None,
            }],
        });

        let mut step: u32 = 0;
        loop {
            if step >= max_steps {
                callback.on_complete(&StopReason::MaxSteps).await;
                return Ok(StopReason::MaxSteps);
            }

            let api_tools = tools.to_api_tools();
            let request = MessageRequest {
                model: client.model().to_string(),
                messages: history.messages().to_vec(),
                max_tokens,
                system: system.clone(),
                tools: if api_tools.is_empty() {
                    None
                } else {
                    Some(api_tools)
                },
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: None,
                stream: Some(true),
                temperature,
                top_p: None,
            };

            callback.on_llm_start(&request).await;
            let stream = client.create_message_stream(request).await?;
            let (content, _stop_reason) = accumulate_stream(stream).await?;
            callback.on_llm_end(&content).await;

            // Persist the assistant turn.
            history.push(Message {
                role: "assistant".to_string(),
                content: content.clone(),
            });

            // Collect tool calls (preserve order).
            let tool_uses: Vec<(String, String, serde_json::Value)> = content
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input, .. } => Some((id, name, input)),
                    _ => None,
                })
                .collect();

            if tool_uses.is_empty() {
                callback.on_complete(&StopReason::NoToolCalls).await;
                return Ok(StopReason::NoToolCalls);
            }

            // Execute each tool sequentially and feed the result back as a
            // `role:"user"` `ToolResult` block (Anthropic/OpenAI-compat shape).
            for (id, name, input) in tool_uses {
                callback.on_tool_start(&name, &input).await;
                let result = match tools.get(&name) {
                    Some(tool) => tool.run(input.clone()).await,
                    None => Err(ToolError::NotAvailable {
                        message: format!("no tool named '{name}'"),
                    }),
                };
                callback.on_tool_end(&name, &result).await;

                let (content_str, is_error) = match &result {
                    Ok(r) => (r.content.clone(), !r.success),
                    Err(e) => (format!("Error: {e}"), true),
                };
                history.push(Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: content_str,
                        is_error: Some(is_error),
                        content_blocks: None,
                    }],
                });
            }

            callback.on_step(step).await;
            step += 1;
        }
    }
}

// === Stream accumulator =====================================================

/// A per-index block under construction during a stream.
enum BlockBuild {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        /// Accumulated `InputJsonDelta.partial_json` fragments.
        input_buf: String,
        /// The `input` carried by `ContentBlockStart::ToolUse` (usually null/`{}`);
        /// used verbatim if no deltas arrive for this block.
        start_input: serde_json::Value,
        caller: Option<crate::models::ToolCaller>,
    },
}

/// Reduce a [`StreamEventBox`] into the assembled assistant content blocks plus
/// the terminal `stop_reason`.
///
/// Handles `ContentBlockStart`/`Delta`/`Stop` (text, thinking, tool_use) and
/// `MessageDelta`/`MessageStop`. This is the minimal reducer — the production
/// `Engine` adds early-tool-start, transparent stream-retry, and steer injection
/// (deferred). Returns an error if any stream item errors.
async fn accumulate_stream(
    mut stream: StreamEventBox,
) -> Result<(Vec<ContentBlock>, Option<String>)> {
    use std::collections::BTreeMap;

    let mut blocks: BTreeMap<u32, BlockBuild> = BTreeMap::new();
    let mut stop_reason: Option<String> = None;

    while let Some(item) = stream.next().await {
        let event = item?;
        match event {
            StreamEvent::MessageStart { .. } => {}
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                let build = match content_block {
                    ContentBlockStart::Text { text } => BlockBuild::Text(text),
                    ContentBlockStart::Thinking { thinking } => BlockBuild::Thinking(thinking),
                    ContentBlockStart::ToolUse {
                        id,
                        name,
                        input,
                        caller,
                    } => BlockBuild::ToolUse {
                        id,
                        name,
                        input_buf: String::new(),
                        start_input: input,
                        caller,
                    },
                    ContentBlockStart::ServerToolUse { id, name, input } => BlockBuild::ToolUse {
                        id,
                        name,
                        input_buf: String::new(),
                        start_input: input,
                        caller: None,
                    },
                };
                blocks.insert(index, build);
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                if let Some(build) = blocks.get_mut(&index) {
                    match (build, delta) {
                        (BlockBuild::Text(buf), Delta::TextDelta { text }) => buf.push_str(&text),
                        (BlockBuild::Thinking(buf), Delta::ThinkingDelta { thinking }) => {
                            buf.push_str(&thinking)
                        }
                        (
                            BlockBuild::ToolUse { input_buf, .. },
                            Delta::InputJsonDelta { partial_json },
                        ) => input_buf.push_str(&partial_json),
                        // Delta/block kind mismatch — ignore (provider quirk).
                        _ => {}
                    }
                }
            }
            StreamEvent::ContentBlockStop { .. } => {}
            StreamEvent::MessageDelta {
                delta: MessageDelta { stop_reason: sr, .. },
                ..
            } => {
                if sr.is_some() {
                    stop_reason = sr;
                }
            }
            StreamEvent::MessageStop => break,
            StreamEvent::Ping => {}
        }
    }

    let content = blocks
        .into_values()
        .map(|build| match build {
            BlockBuild::Text(text) => ContentBlock::Text {
                text,
                cache_control: None,
            },
            BlockBuild::Thinking(thinking) => ContentBlock::Thinking { thinking },
            BlockBuild::ToolUse {
                id,
                name,
                input_buf,
                start_input,
                caller,
            } => {
                let input = if !input_buf.is_empty() {
                    serde_json::from_str(&input_buf).unwrap_or(serde_json::Value::Null)
                } else if !start_input.is_null() {
                    start_input
                } else {
                    serde_json::Value::Object(serde_json::Map::new())
                };
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    caller,
                }
            }
        })
        .collect();

    Ok((content, stop_reason))
}

// === Tests ==================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callback::NoopCallback;
    use crate::llm_client::LlmClient;
    use crate::memory::VecChatHistory;
    use crate::models::{ContentBlockStart, Delta, MessageDelta, StreamEvent};
    use crate::tools::{Tool, ToolResult};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    // --- mock LLM client ---

    /// A `LlmClient` that pops canned `StreamEvent` lists from a queue, one per
    /// `create_message_stream` call.
    struct MockLlmClient {
        calls: Mutex<VecDeque<Vec<StreamEvent>>>,
        model: String,
    }

    impl MockLlmClient {
        fn new(model: &str, calls: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                calls: Mutex::new(calls.into_iter().collect()),
                model: model.to_string(),
            }
        }
    }

    impl LlmClient for MockLlmClient {
        fn provider_name(&self) -> &'static str {
            "mock"
        }
        fn model(&self) -> &str {
            &self.model
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<crate::models::MessageResponse>> + Send + '_>>
        {
            Box::pin(async {
                Err(anyhow::anyhow!("mock does not implement create_message"))
            })
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            let next = self.calls.lock().unwrap().pop_front();
            Box::pin(async move {
                let events = next.unwrap_or_default();
                let stream = futures_util::stream::iter(events.into_iter().map(Ok));
                Ok(Box::pin(stream) as StreamEventBox)
            })
        }
    }

    // --- mock tool ---

    struct EchoTool;
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes the input."
        }
        fn run(
            &self,
            input: serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + '_>> {
            Box::pin(async move {
                let text = input
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(ToolResult {
                    content: format!("echo:{text}"),
                    success: true,
                    metadata: None,
                })
            })
        }
    }

    // --- recording callback ---

    struct RecordingCallback {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingCallback {
        fn new() -> Self {
            Self {
                log: Arc::new(Mutex::new(Vec::new())),
            }
        }
        fn lines(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Callback for RecordingCallback {
        fn on_llm_start(
            &self,
            _request: &MessageRequest,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let log = self.log.clone();
            Box::pin(async move {
                log.lock().unwrap().push("llm_start".into());
            })
        }
        fn on_llm_end(
            &self,
            _content: &[ContentBlock],
        ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let log = self.log.clone();
            Box::pin(async move {
                log.lock().unwrap().push("llm_end".into());
            })
        }
        fn on_tool_start(
            &self,
            name: &str,
            _input: &serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let log = self.log.clone();
            let name = name.to_string();
            Box::pin(async move {
                log.lock().unwrap().push(format!("tool_start:{name}"));
            })
        }
        fn on_tool_end(
            &self,
            name: &str,
            result: &Result<ToolResult, ToolError>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let log = self.log.clone();
            let name = name.to_string();
            let outcome = match result {
                Ok(r) => format!("ok:{}", r.content),
                Err(e) => format!("err:{e}"),
            };
            Box::pin(async move {
                log.lock().unwrap().push(format!("tool_end:{name}:{outcome}"));
            })
        }
        fn on_step(&self, step: u32) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let log = self.log.clone();
            Box::pin(async move {
                log.lock().unwrap().push(format!("step:{step}"));
            })
        }
        fn on_complete(&self, reason: &StopReason) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let log = self.log.clone();
            let reason = format!("{reason:?}");
            Box::pin(async move {
                log.lock().unwrap().push(format!("complete:{reason}"));
            })
        }
    }

    // --- stream-event builders ---

    fn text_block(idx: u32, body: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::Text {
                    text: String::new(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::TextDelta {
                    text: body.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn tool_use_block(idx: u32, id: &str, name: &str, input_json: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: serde_json::Value::Null,
                    caller: None,
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::InputJsonDelta {
                    partial_json: input_json.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn finish(stop: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(stop.to_string()),
                    stop_sequence: None,
                },
                usage: None,
            },
            StreamEvent::MessageStop,
        ]
    }

    // --- tests ---

    fn executor(
        calls: Vec<Vec<StreamEvent>>,
        max_steps: u32,
        callback: Arc<dyn Callback>,
    ) -> DefaultAgentExecutor {
        let mut tools = ToolSet::new();
        tools.register(Arc::new(EchoTool));
        DefaultAgentExecutor::new(
            Arc::new(MockLlmClient::new("mock-v0", calls)),
            Arc::new(tools),
            callback,
            AgentExecutorConfig {
                max_steps,
                max_tokens: 1024,
                system: None,
                temperature: None,
            },
        )
    }

    #[tokio::test]
    async fn text_only_stops_with_no_tool_calls() {
        // Single LLM call, text only → NoToolCalls.
        let mut combined = text_block(0, "hello there");
        combined.extend(finish("end_turn"));

        let ex = executor(vec![combined], 10, Arc::new(NoopCallback));
        let mut history = VecChatHistory::new();
        let reason = ex.run(&mut history, "hi".to_string()).await.unwrap();
        assert_eq!(reason, StopReason::NoToolCalls);
        // user + assistant
        assert_eq!(history.len(), 2);
        assert_eq!(history.messages()[0].role, "user");
        assert_eq!(history.messages()[1].role, "assistant");
    }

    #[tokio::test]
    async fn tool_call_then_finish() {
        // Call 1: text + tool_use(echo). Call 2: text-only → NoToolCalls.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let recorder = Arc::new(RecordingCallback::new());
        let ex = executor(vec![call1, call2], 10, recorder.clone());
        let mut history = VecChatHistory::new();
        let reason = ex.run(&mut history, "echo world".to_string()).await.unwrap();
        assert_eq!(reason, StopReason::NoToolCalls);

        // user, assistant(text+tooluse), user(toolresult), assistant(text) = 4
        assert_eq!(history.len(), 4);
        assert_eq!(history.messages()[2].role, "user");
        match &history.messages()[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert_eq!(content, "echo:world");
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        // Callback ordering.
        let lines = recorder.lines();
        assert!(lines
            .iter()
            .any(|l| l == "tool_start:echo"));
        assert!(lines
            .iter()
            .any(|l| l == "tool_end:echo:ok:echo:world"));
        assert!(lines.iter().any(|l| l == "step:0"));
        assert!(lines
            .iter()
            .any(|l| l == "complete:NoToolCalls"));
    }

    #[tokio::test]
    async fn exhausted_steps_stop_with_max_steps() {
        // Mock always returns a tool call → never finishes → hits MaxSteps.
        let make_call = || {
            let mut c = text_block(0, "looping");
            c.extend(tool_use_block(1, "t1", "echo", r#"{"text":"x"}"#));
            c.extend(finish("tool_use"));
            c
        };
        let ex = executor(vec![make_call(), make_call(), make_call()], 2, Arc::new(NoopCallback));
        let mut history = VecChatHistory::new();
        let reason = ex.run(&mut history, "go".to_string()).await.unwrap();
        assert_eq!(reason, StopReason::MaxSteps);
        // 2 iterations: each adds assistant + tool-result, plus initial user.
        // user, (assistant, toolresult) x2 = 1 + 2*2 = 5
        assert_eq!(history.len(), 5);
    }

    #[tokio::test]
    async fn missing_tool_records_error_result() {
        // A tool_use for an unregistered tool → error ToolResult fed back.
        let mut call = text_block(0, "calling ghost");
        call.extend(tool_use_block(1, "t1", "ghost", r#"{}"#));
        call.extend(finish("tool_use"));
        let mut finish_call = text_block(0, "ok");
        finish_call.extend(finish("end_turn"));

        let recorder = Arc::new(RecordingCallback::new());
        let ex = executor(vec![call, finish_call], 10, recorder.clone());
        let mut history = VecChatHistory::new();
        let reason = ex.run(&mut history, "go".to_string()).await.unwrap();
        assert_eq!(reason, StopReason::NoToolCalls);

        match &history.messages()[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert!(content.starts_with("Error:"));
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        assert!(recorder
            .lines()
            .iter()
            .any(|l| l.starts_with("tool_end:ghost:err:")));
    }

    #[tokio::test]
    async fn accumulate_stream_assembles_tool_input_from_deltas() {
        // Directly exercise the reducer: split JSON across two deltas.
        let events = {
            let mut v = text_block(0, "hi");
            v.extend(tool_use_block(1, "t9", "echo", r#"{"text":"par"#));
            // second delta for the same index (continuation)
            v.push(StreamEvent::ContentBlockDelta {
                index: 1,
                delta: Delta::InputJsonDelta {
                    partial_json: r#"ty"}"#.to_string(),
                },
            });
            v.push(StreamEvent::ContentBlockStop { index: 1 });
            v.extend(finish("tool_use"));
            v
        };
        let stream: StreamEventBox = Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)));
        let (content, stop) = accumulate_stream(stream).await.unwrap();
        assert_eq!(stop.as_deref(), Some("tool_use"));
        assert_eq!(content.len(), 2);
        match &content[1] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "echo");
                assert_eq!(input["text"], "party");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }
}
