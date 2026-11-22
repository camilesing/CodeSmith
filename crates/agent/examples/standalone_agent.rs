//! The standalone-framework walkthrough: a complete agent built from
//! `codesmith-agent` alone — no CLI, no engine, no `codesmith-providers`.
//!
//! Run with `cargo run -p codesmith-agent --example standalone_agent`.
//!
//! What it exercises, in order:
//! 1. [`ChatPromptTemplate`] — role templates + `{var}` interpolation.
//! 2. Retrieval — `InMemoryVectorStore` + `VectorStoreRetriever` behind a
//!    framework `Tool` (the LangChain retriever-tool pattern).
//! 3. [`DefaultAgentExecutor`] — the reference LLM↔tool loop over
//!    `VecChatHistory` with a recording `Callback`.
//! 4. `structured_output` — a typed answer out of one schema-forced call.
//!
//! The LLM is a scripted stand-in so the example runs offline and
//! deterministically. A real deployment swaps in an `LlmClient`
//! implementation — `codesmith-providers` ships one per backend behind a
//! Cargo feature, or a host registers its own `ProviderFactory`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::callback::{Callback, StopReason};
use codesmith_agent::executor::{AgentExecutor, AgentExecutorConfig, DefaultAgentExecutor};
use codesmith_agent::llm_client::{LlmClient, LlmClientHandle, StreamEventBox};
use codesmith_agent::memory::{ChatHistory, VecChatHistory};
use codesmith_agent::models::{
    ContentBlock, ContentBlockStart, Delta, MessageRequest, MessageResponse, StreamEvent,
    SystemPrompt, Usage,
};
use codesmith_agent::prompt_template::{ChatPromptTemplate, PromptRole};
use codesmith_agent::retrieval::{
    Document, Embeddings, InMemoryVectorStore, Retriever, VectorStore, VectorStoreRetriever,
};
use codesmith_agent::structured::{STRUCTURED_OUTPUT_TOOL_NAME, structured_output};
use codesmith_agent::tools::{Tool, ToolError, ToolResult, ToolSet};

// === The scripted LLM =======================================================

/// Pops one canned response per call — the same mock shape the executor
/// tests use. Response 1 calls the search tool; response 2 answers.
struct ScriptedLlm {
    script: Vec<MessageResponse>,
    next: std::sync::Mutex<usize>,
}

impl ScriptedLlm {
    fn step(&self) -> usize {
        let mut next = self.next.lock().unwrap();
        let step = *next;
        *next += 1;
        step
    }
}

impl LlmClient for ScriptedLlm {
    fn provider_name(&self) -> &'static str {
        "scripted"
    }
    fn model(&self) -> &str {
        "scripted-v0"
    }
    fn create_message(
        &self,
        _request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>> {
        let response = self.script[self.step()].clone();
        Box::pin(async move { Ok(response) })
    }
    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<StreamEventBox>> + Send + '_>> {
        // The scripted client is non-streaming; this encoder converts a
        // canned response into a faithful single-chunk-per-block stream so
        // the executor's reducer sees text AND tool_use blocks.
        let fut = self.create_message(request);
        Box::pin(async move {
            let response = fut.await?;
            let mut events = Vec::new();
            for (index, block) in response.content.iter().enumerate() {
                let index = index as u32;
                match block {
                    ContentBlock::Text { text, .. } => {
                        events.push(StreamEvent::ContentBlockStart {
                            index,
                            content_block: ContentBlockStart::Text {
                                text: String::new(),
                            },
                        });
                        events.push(StreamEvent::ContentBlockDelta {
                            index,
                            delta: Delta::TextDelta { text: text.clone() },
                        });
                    }
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        events.push(StreamEvent::ContentBlockStart {
                            index,
                            content_block: ContentBlockStart::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: serde_json::Value::Null,
                                caller: None,
                            },
                        });
                        events.push(StreamEvent::ContentBlockDelta {
                            index,
                            delta: Delta::InputJsonDelta {
                                partial_json: input.to_string(),
                            },
                        });
                    }
                    _ => {}
                }
                events.push(StreamEvent::ContentBlockStop { index });
            }
            events.push(StreamEvent::MessageStop);
            let stream: StreamEventBox =
                Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)));
            Ok(stream)
        })
    }
}

fn text_response(text: &str) -> MessageResponse {
    MessageResponse {
        id: "r".into(),
        r#type: "message".into(),
        role: "assistant".into(),
        content: vec![ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
        model: "scripted-v0".into(),
        stop_reason: Some("end_turn".into()),
        stop_sequence: None,
        container: None,
        usage: Usage::default(),
    }
}

fn tool_call_response(id: &str, name: &str, input: serde_json::Value) -> MessageResponse {
    MessageResponse {
        id: "r".into(),
        r#type: "message".into(),
        role: "assistant".into(),
        content: vec![ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            caller: None,
        }],
        model: "scripted-v0".into(),
        stop_reason: Some("tool_use".into()),
        stop_sequence: None,
        container: None,
        usage: Usage::default(),
    }
}

// === A deterministic embedder (stand-in for a real embedding model) ========

struct HashEmbeddings;

#[async_trait]
impl Embeddings for HashEmbeddings {
    async fn embed_documents(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| Ok(embed(t))).collect()
    }
    async fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(embed(text))
    }
}

fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![1.0; 32];
    for word in text.to_lowercase().split_whitespace() {
        let hash = word.bytes().fold(0usize, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(usize::from(b))
        });
        v[hash % 32] += 1.0;
    }
    v
}

// === The retriever as a framework Tool ======================================

struct SearchDocsTool {
    retriever: VectorStoreRetriever<InMemoryVectorStore>,
}

impl Tool for SearchDocsTool {
    fn name(&self) -> &str {
        "search_docs"
    }
    fn description(&self) -> &str {
        "Search the knowledge base. Input: {\"query\": string}."
    }
    fn run(
        &self,
        input: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + '_>> {
        let retriever = &self.retriever;
        Box::pin(async move {
            let query = input
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let hits =
                retriever
                    .retrieve(&query, 2)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        message: format!("retrieval failed: {e:#}"),
                    })?;
            let content = if hits.is_empty() {
                "no matching documents".to_string()
            } else {
                hits.iter()
                    .map(|d| format!("- {}", d.page_content))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(ToolResult {
                canonical: None,
                content,
                success: true,
                metadata: None,
            })
        })
    }
}

// === An observing callback ==================================================

struct PrintCallback;

impl Callback for PrintCallback {
    fn on_llm_start(
        &self,
        _request: &MessageRequest,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        print_event("llm_start");
        Box::pin(async {})
    }
    fn on_tool_start(
        &self,
        _id: &str,
        name: &str,
        _input: &serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let name = name.to_string();
        print_event(&format!("tool_start:{name}"));
        Box::pin(async {})
    }
    fn on_tool_end(
        &self,
        name: &str,
        result: &Result<ToolResult, ToolError>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let line = match result {
            Ok(r) => format!("tool_end:{name}:{}", r.content),
            Err(e) => format!("tool_end:{name}:err:{e}"),
        };
        print_event(&line);
        Box::pin(async {})
    }
    fn on_complete(&self, reason: &StopReason) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        print_event(&format!("complete:{reason:?}"));
        Box::pin(async {})
    }
}

fn print_event(line: &str) {
    println!("  [callback] {line}");
}

// === The walkthrough ========================================================

/// The typed answer `structured_output` extracts.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct Summary {
    topic: String,
    documented: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("== 1. prompt template ==");
    let template = ChatPromptTemplate::from_messages(vec![
        (
            PromptRole::System,
            "You are {name}, a documentation assistant.".into(),
        ),
        (PromptRole::User, "{question}".into()),
    ]);
    let mut vars = HashMap::new();
    vars.insert("name", "CodeSmith".to_string());
    vars.insert("question", "How do I run the test suite?".to_string());
    let system: SystemPrompt = template
        .format_system(&vars)?
        .expect("template starts with a system part");

    println!("== 2. retrieval ==");
    let store = Arc::new(tokio::sync::Mutex::new(InMemoryVectorStore::new()));
    store
        .lock()
        .await
        .add_documents(
            vec![
                Document::new("run the workspace test suite with cargo test"),
                Document::new("cargo clippy gates lints at -D warnings"),
                Document::new("gardening: water the tomatoes"),
            ],
            &HashEmbeddings,
        )
        .await?;
    let retriever = VectorStoreRetriever::new(store, Arc::new(HashEmbeddings));
    let hits = retriever.retrieve("test suite", 1).await?;
    println!("  top hit: {}", hits[0].page_content);

    println!("== 3. agent loop ==");
    let mut tools = ToolSet::new();
    tools.register(Arc::new(SearchDocsTool { retriever }));
    let client: LlmClientHandle = Arc::new(ScriptedLlm {
        script: vec![
            tool_call_response(
                "t1",
                "search_docs",
                serde_json::json!({"query": "test suite"}),
            ),
            text_response("Use `cargo test` — it runs the workspace test suite."),
            // The structured-output exchange: one schema-forced tool call.
            tool_call_response(
                "t2",
                STRUCTURED_OUTPUT_TOOL_NAME,
                serde_json::json!({"topic": "running the test suite", "documented": true}),
            ),
        ],
        next: std::sync::Mutex::new(0),
    });
    let executor = DefaultAgentExecutor::new(
        client.clone(),
        Arc::new(tools),
        Arc::new(PrintCallback),
        AgentExecutorConfig {
            system: Some(system),
            ..AgentExecutorConfig::default()
        },
    );
    let mut history = VecChatHistory::new();
    let reason = executor
        .run(&mut history, "How do I run the test suite?".to_string())
        .await?;
    println!(
        "  history roles: {:?} ({} messages)",
        history
            .messages()
            .iter()
            .map(|m| m.role.as_str())
            .collect::<Vec<_>>(),
        history.len()
    );
    anyhow::ensure!(reason == StopReason::NoToolCalls, "script ends on text");

    println!("== 4. structured output ==");
    let summary: Summary =
        structured_output(client, "Summarize the documentation in one line.", 1).await?;
    println!(
        "  typed answer: topic={} documented={}",
        summary.topic, summary.documented
    );

    println!("done — every framework surface above is `codesmith-agent` only.");
    Ok(())
}
