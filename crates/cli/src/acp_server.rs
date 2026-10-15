//! Minimal Agent Client Protocol stdio adapter.
//!
//! This intentionally starts with the ACP baseline: initialize, new session,
//! prompt, and cancel. It keeps stdout protocol-clean for editor clients and
//! routes prompts through the same configured DeepSeek client as one-shot CLI
//! mode.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::config::Config;
use crate::models::{ContentBlock, Message, MessageRequest, SystemPrompt};

const ACP_PROTOCOL_VERSION: u64 = 1;

/// Upper bound on tracked sessions; the oldest is evicted beyond it.
const MAX_ACP_SESSIONS: usize = 128;

/// Stdout is shared between the read loop and spawned prompt tasks, which
/// write `session/update` notifications and their JSON-RPC response as the
/// prompt completes.
type SharedWriter<W> = Arc<tokio::sync::Mutex<W>>;

pub async fn run_acp_server(config: Config, model: String) -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let writer: SharedWriter<tokio::io::BufWriter<tokio::io::Stdout>> =
        Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdout)));
    let mut server = AcpServer::new(config, model);

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let mut writer = writer.lock().await;
                write_jsonrpc_error(&mut *writer, None, -32700, format!("invalid json: {err}"))
                    .await?;
                continue;
            }
        };

        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            // JSON-RPC 2.0: a notification (no `id`) must never receive a
            // reply — only the `-32700` parse-error case above may answer
            // with a null id, because there the request id is undetectable.
            if let Some(id) = message.get("id").cloned() {
                let mut writer = writer.lock().await;
                write_jsonrpc_error(
                    &mut *writer,
                    Some(id),
                    -32600,
                    "jsonrpc version must be 2.0",
                )
                .await?;
            }
            continue;
        }

        let id = message.get("id").cloned();
        let method = match message.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => {
                if let Some(id) = id.clone() {
                    let mut writer = writer.lock().await;
                    write_jsonrpc_error(&mut *writer, Some(id), -32600, "missing method").await?;
                }
                continue;
            }
        };
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

        match server.handle_request(method, params, id.clone(), writer.clone()) {
            Ok(AcpDispatch::Response(result)) => {
                if let Some(id) = id {
                    let mut writer = writer.lock().await;
                    write_jsonrpc_result(&mut *writer, id, result).await?;
                }
            }
            // The spawned prompt task writes its own response/error lines.
            Ok(AcpDispatch::Spawned) => {}
            Ok(AcpDispatch::Shutdown) => {
                if let Some(id) = id {
                    let mut writer = writer.lock().await;
                    write_jsonrpc_result(&mut *writer, id, json!(null)).await?;
                }
                break;
            }
            Err(err) => {
                // Notifications (no `id`) get no reply, per JSON-RPC 2.0.
                if let Some(id) = id {
                    let mut writer = writer.lock().await;
                    write_jsonrpc_error(&mut *writer, Some(id), err.code, err.message).await?;
                }
            }
        }
    }

    Ok(())
}

struct AcpServer {
    config: Config,
    model: String,
    sessions: Arc<Mutex<HashMap<String, AcpSession>>>,
}

struct AcpSession {
    /// Creation stamp; the oldest session is evicted once the map grows
    /// past `MAX_ACP_SESSIONS`.
    created_at: Instant,
    /// In-flight prompt task for this session, if any.
    prompt_task: Option<tokio::task::AbortHandle>,
    /// Completed turns (user prompt + assistant reply). Replayed as the
    /// `messages` vector on the next `session/prompt` — the protocol's
    /// stateful `sessionId` implies the editor conversation keeps its
    /// context instead of restarting from zero each turn.
    history: Vec<Message>,
}

/// Lock the session map, tolerating a poisoned lock: the map is only ever
/// held for brief clone/insert critical sections, so a panic elsewhere
/// must not take prompt history down with it.
fn lock_sessions(
    sessions: &Mutex<HashMap<String, AcpSession>>,
) -> std::sync::MutexGuard<'_, HashMap<String, AcpSession>> {
    sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum AcpDispatch {
    Response(Value),
    /// A prompt task was spawned; it will answer the request itself.
    Spawned,
    Shutdown,
}

struct AcpError {
    code: i32,
    message: String,
}

impl AcpServer {
    fn new(config: Config, model: String) -> Self {
        Self {
            config,
            model,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn handle_request<W>(
        &mut self,
        method: &str,
        params: Value,
        id: Option<Value>,
        writer: SharedWriter<W>,
    ) -> std::result::Result<AcpDispatch, AcpError>
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        match method {
            "initialize" => Ok(AcpDispatch::Response(initialize_result(
                params.get("protocolVersion").and_then(Value::as_u64),
            ))),
            "session/new" => Ok(AcpDispatch::Response(self.new_session(params)?)),
            "session/prompt" => {
                self.spawn_prompt(id, params, writer)?;
                Ok(AcpDispatch::Spawned)
            }
            "session/cancel" => {
                if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                    self.cancel_session(session_id);
                }
                Ok(AcpDispatch::Response(json!(null)))
            }
            "shutdown" => Ok(AcpDispatch::Shutdown),
            _ => Err(AcpError::method_not_found(method)),
        }
    }

    fn new_session(&mut self, _params: Value) -> std::result::Result<Value, AcpError> {
        let session_id = format!("codesmith-{}", uuid::Uuid::new_v4());
        let mut sessions = lock_sessions(&self.sessions);
        // Bound the tracked-session count: evict the oldest beyond
        // MAX_ACP_SESSIONS, aborting its in-flight prompt (if any) so it
        // cannot write into a session that no longer exists.
        if sessions.len() >= MAX_ACP_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .min_by(|a, b| a.1.created_at.cmp(&b.1.created_at))
                .map(|(id, _)| id.clone())
            && let Some(session) = sessions.remove(&oldest)
            && let Some(task) = session.prompt_task
        {
            task.abort();
        }
        sessions.insert(
            session_id.clone(),
            AcpSession {
                created_at: Instant::now(),
                prompt_task: None,
                history: Vec::new(),
            },
        );
        Ok(json!({ "sessionId": session_id }))
    }

    /// Validate the prompt request, then run the LLM call on its own task so
    /// the read loop stays free to accept `session/cancel` (and other
    /// requests) while the prompt is in flight. The task owns the response
    /// for `id`.
    fn spawn_prompt<W>(
        &mut self,
        id: Option<Value>,
        params: Value,
        writer: SharedWriter<W>,
    ) -> std::result::Result<(), AcpError>
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("sessionId is required"))?
            .to_string();
        // Snapshot the conversation so far under the lock; the prompt task
        // appends its completed turn back into the map when the reply lands.
        let history = {
            let sessions = lock_sessions(&self.sessions);
            match sessions.get(&session_id) {
                Some(session) => session.history.clone(),
                None => return Err(AcpError::invalid_params("unknown sessionId")),
            }
        };
        let prompt = extract_prompt_text(params.get("prompt"))
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| AcpError::invalid_params("prompt must include text content"))?;

        let config = self.config.clone();
        let model = self.model.clone();
        let task_session_id = session_id.clone();
        let sessions = Arc::clone(&self.sessions);
        // One in-flight prompt per session: a new prompt supersedes (aborts)
        // any stale one still running.
        let task = tokio::spawn(async move {
            let mut messages = history;
            messages.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: prompt.clone(),
                    cache_control: None,
                }],
            });
            let output = match run_prompt(&config, &model, &prompt, messages.clone()).await {
                Ok(output) => output,
                Err(err) => {
                    let mut writer = writer.lock().await;
                    let _ = write_jsonrpc_error(&mut *writer, id, -32603, err.to_string()).await;
                    return;
                }
            };
            // Persist the completed turn so the next `session/prompt` on
            // this session replays the conversation. A turn without a
            // reply (aborted, empty) is left out entirely.
            if !output.is_empty() {
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text: output.clone(),
                        cache_control: None,
                    }],
                });
                let mut sessions = lock_sessions(&sessions);
                if let Some(session) = sessions.get_mut(&task_session_id) {
                    session.history = messages;
                }
            }
            let mut writer = writer.lock().await;
            if !output.is_empty() {
                let _ = write_session_update(&mut *writer, &task_session_id, output).await;
            }
            if let Some(id) = id {
                let _ = write_jsonrpc_result(&mut *writer, id, json!({ "stopReason": "end_turn" }))
                    .await;
            }
        });
        let mut sessions = lock_sessions(&self.sessions);
        if let Some(session) = sessions.get_mut(&session_id)
            && let Some(previous) = session.prompt_task.replace(task.abort_handle())
        {
            previous.abort();
        }
        Ok(())
    }

    /// Abort the in-flight prompt task for `session_id`, if any. Dropping
    /// the task's future cancels the awaited LLM call.
    fn cancel_session(&mut self, session_id: &str) {
        let mut sessions = lock_sessions(&self.sessions);
        if let Some(session) = sessions.get_mut(session_id)
            && let Some(task) = session.prompt_task.take()
        {
            task.abort();
        }
    }
}

async fn run_prompt(
    config: &Config,
    model: &str,
    prompt: &str,
    messages: Vec<Message>,
) -> Result<String> {
    // No ScopedCurrentDir: the guard mutated the PROCESS-wide cwd and
    // held that change across the awaited LLM calls, so any concurrent
    // runtime work resolving a relative path observed the wrong session
    // directory. `resolve_cli_auto_route` takes no cwd (heuristic /
    // router-model only) and the request below is fully explicit, so
    // the chdir served nothing.
    let client = crate::core::engine::resolve_llm_client(config)?;
    let route = crate::main_root::resolve_cli_auto_route(config, model, prompt).await;
    let reasoning_effort = route
        .reasoning_effort
        .map(|effort| effort.as_setting().to_string());

    let request = MessageRequest {
        model: route.model,
        messages,
        max_tokens: 4096,
        system: Some(SystemPrompt::Text(
            "You are a coding assistant inside an ACP-compatible editor. Give concise, actionable responses.".to_string(),
        )),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort,
        stream: Some(false),
        temperature: Some(0.2),
        top_p: Some(0.9),
    };

    let response = client.create_message(request).await?;
    let mut output = String::new();
    for block in response.content {
        if let ContentBlock::Text { text, .. } = block {
            output.push_str(&text);
        }
    }
    Ok(output)
}

impl AcpError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }
}

fn initialize_result(client_protocol_version: Option<u64>) -> Value {
    json!({
        "protocolVersion": client_protocol_version
            .map(|version| version.min(ACP_PROTOCOL_VERSION))
            .unwrap_or(ACP_PROTOCOL_VERSION),
        "agentCapabilities": {
            "loadSession": false,
            "promptCapabilities": {
                "image": false,
                "audio": false,
                "embeddedContext": true
            },
            "mcpCapabilities": {
                "http": false,
                "sse": false
            },
            "sessionCapabilities": {}
        },
        "agentInfo": {
            "name": "codesmith",
            "title": "codesmith",
            "version": env!("CARGO_PKG_VERSION")
        },
        "authMethods": []
    })
}

fn extract_prompt_text(prompt: Option<&Value>) -> Option<String> {
    match prompt? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let parts = blocks
                .iter()
                .filter_map(content_block_text)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        _ => None,
    }
}

fn content_block_text(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => block
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        "resource" => resource_text(block),
        "resource_link" | "resourceLink" => resource_link_text(block),
        _ => None,
    }
}

fn resource_text(block: &Value) -> Option<String> {
    let resource = block.get("resource").unwrap_or(block);
    if let Some(text) = resource.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    resource_link_text(resource)
}

fn resource_link_text(block: &Value) -> Option<String> {
    let uri = block
        .get("uri")
        .or_else(|| block.pointer("/resource/uri"))
        .and_then(Value::as_str)?;
    Some(format!("@{uri}"))
}

async fn write_session_update<W>(writer: &mut W, session_id: &str, text: String) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": text
                }
            }
        }
    });
    write_json_line(writer, notification).await
}

async fn write_jsonrpc_result<W>(writer: &mut W, id: Value, result: Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let id = jsonrpc_response_id(id);
    write_json_line(
        writer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
    )
    .await
}

async fn write_jsonrpc_error<W>(
    writer: &mut W,
    id: Option<Value>,
    code: i32,
    message: impl Into<String>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let id = id.map(jsonrpc_response_id);
    write_json_line(
        writer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message.into()
            }
        }),
    )
    .await
}

async fn write_json_line<W>(writer: &mut W, value: Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(value.to_string().as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn jsonrpc_response_id(id: Value) -> Value {
    match id {
        Value::Null => Value::Null,
        Value::String(_) => id,
        Value::Number(number) => Value::String(number.to_string()),
        other => Value::String(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_advertises_baseline_acp_agent() {
        let result = initialize_result(Some(1));

        assert_eq!(result["protocolVersion"], 1);
        assert_eq!(result["agentInfo"]["name"], "codesmith");
        assert_eq!(result["agentCapabilities"]["loadSession"], false);
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            true
        );
        assert_eq!(result["authMethods"], json!([]));
    }

    #[test]
    fn extract_prompt_text_accepts_text_and_resource_blocks() {
        let prompt = json!([
            { "type": "text", "text": "Review this file" },
            {
                "type": "resource",
                "resource": {
                    "uri": "file:///tmp/app.rs",
                    "mimeType": "text/rust",
                    "text": "fn main() {}"
                }
            },
            { "type": "resource_link", "uri": "file:///tmp/lib.rs" }
        ]);

        let text = extract_prompt_text(Some(&prompt)).expect("prompt text");

        assert!(text.contains("Review this file"));
        assert!(text.contains("fn main() {}"));
        assert!(text.contains("@file:///tmp/lib.rs"));
    }

    #[tokio::test]
    async fn session_update_is_protocol_clean_single_line_json() {
        let mut out = Vec::new();

        write_session_update(&mut out, "sess_1", "hello\nworld".to_string())
            .await
            .expect("write update");

        let line = String::from_utf8(out).expect("utf8");
        assert_eq!(line.lines().count(), 1);
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["method"], "session/update");
        assert_eq!(value["params"]["sessionId"], "sess_1");
        assert_eq!(value["params"]["update"]["content"]["text"], "hello\nworld");
    }

    #[tokio::test]
    async fn jsonrpc_result_stringifies_numeric_ids_for_zed_acp() {
        let mut out = Vec::new();

        write_jsonrpc_result(&mut out, json!(1), json!({"ok": true}))
            .await
            .expect("write result");

        let line = String::from_utf8(out).expect("utf8");
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["id"], "1");
        assert_eq!(value["result"], json!({"ok": true}));
    }

    #[tokio::test]
    async fn jsonrpc_error_keeps_absent_id_null() {
        let mut out = Vec::new();

        write_jsonrpc_error(&mut out, None, -32700, "invalid json")
            .await
            .expect("write error");

        let line = String::from_utf8(out).expect("utf8");
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["id"], Value::Null);
        assert_eq!(value["error"]["code"], -32700);
    }
}
