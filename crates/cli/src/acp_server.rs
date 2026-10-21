//! Agent Client Protocol stdio adapter.
//!
//! Baseline: `initialize`, `session/new`, `session/prompt`,
//! `session/cancel`, `shutdown`. Round-8 TODO-51 adds the session surface,
//! and the decision behind it is that **an ACP session is a saved session**:
//! the same `~/.codesmith/sessions/<id>.json` record the REPL writes, with a
//! plain UUID id. `session/list` enumerates the store (scoped to a `cwd`'s
//! checkout when given), `session/resume` — and `session/load`, the ACP v1
//! name — reloads one and replays its turns as `session/update` chunks, and
//! `session/close` releases the live one. A conversation started in the
//! editor continues with `/resume` in the terminal, and the other way round.
//!
//! Stdout stays protocol-clean for editor clients; prompts go through the
//! same configured client as one-shot CLI mode, without tools.
//!
//! Persistence follows "model-visible means logged": a completed turn is
//! written before `session/prompt` answers. When the write fails, the reply
//! chunk is still delivered and the prompt is answered with a JSON-RPC error
//! naming the cause — the editor keeps the text, and a missing record is
//! reported rather than silent. The store is opened when the server starts;
//! an unusable sessions directory fails the start.
//!
//! Known limitations (recorded beside the behaviour they bound):
//!
//! - No tools, so no `session/request_permission`. The permission channel
//!   (`ApprovalRouter`) is the next TODO-51 slice and needs prompts to run
//!   through the engine first.
//! - Replay carries text blocks only: tool calls and results in a REPL
//!   session are skipped in the update stream while staying in the history
//!   the next prompt sends.
//! - The ACP system prompt is recorded only for sessions created here. A
//!   resumed REPL session keeps its own prompt on disk; ACP requests on it
//!   still use the ACP prompt.
//! - `session/list` has no paging; the store's own retention cap bounds it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context as _, Result};
use codesmith_agent_runtime::session_store::{
    FileSessionStore, SessionMetadata, create_saved_session_with_id_and_mode, update_session,
    workspace_scope_matches,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::config::Config;
use crate::models::{ContentBlock, Message, MessageRequest, SystemPrompt};

const ACP_PROTOCOL_VERSION: u64 = 1;

/// Upper bound on tracked sessions; the oldest is evicted beyond it.
const MAX_ACP_SESSIONS: usize = 128;

/// System prompt every ACP prompt runs with; recorded on sessions created
/// here (see the module note on resumed sessions).
const ACP_SYSTEM_PROMPT: &str = "You are a coding assistant inside an ACP-compatible editor. Give concise, actionable responses.";

/// Stdout is shared between the read loop and spawned prompt tasks, which
/// write `session/update` notifications and their JSON-RPC response as the
/// prompt completes.
type SharedWriter<W> = Arc<tokio::sync::Mutex<W>>;

pub async fn run_acp_server(config: Config, model: String) -> Result<()> {
    let store = FileSessionStore::default_location()
        .context("ACP sessions are saved sessions; the sessions directory must be usable")?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let writer: SharedWriter<tokio::io::BufWriter<tokio::io::Stdout>> =
        Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdout)));
    let mut server = AcpServer::new(config, model, store);

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

        match server
            .handle_request(method, params, id.clone(), writer.clone())
            .await
        {
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
    store: FileSessionStore,
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
    /// The editor's `cwd` from `session/new`, or the saved session's
    /// workspace on resume — what the record's `workspace` field says.
    workspace: PathBuf,
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

#[derive(Debug)]
enum AcpDispatch {
    Response(Value),
    /// A prompt task was spawned; it will answer the request itself.
    Spawned,
    Shutdown,
}

#[derive(Debug)]
struct AcpError {
    code: i32,
    message: String,
}

impl AcpServer {
    fn new(config: Config, model: String, store: FileSessionStore) -> Self {
        Self {
            config,
            model,
            store,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn handle_request<W>(
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
            "session/list" => Ok(AcpDispatch::Response(self.list_sessions(params).await?)),
            "session/load" | "session/resume" => {
                self.resume_session(params, writer).await?;
                Ok(AcpDispatch::Response(json!(null)))
            }
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
            "session/close" => {
                let session_id = required_session_id(&params)?.to_string();
                self.close_session(&session_id);
                Ok(AcpDispatch::Response(json!(null)))
            }
            "shutdown" => Ok(AcpDispatch::Shutdown),
            _ => Err(AcpError::method_not_found(method)),
        }
    }

    fn new_session(&mut self, params: Value) -> std::result::Result<Value, AcpError> {
        // ACP's `session/new` carries the editor's `cwd`; a lenient client
        // that omits it still gets the process cwd rather than an error.
        let workspace = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| AcpError::invalid_params("cwd is required"))?;
        // Plain UUID, like a REPL session: the record is shared, and the
        // REPL's 8-char prefix display must not read "codesmit" for every
        // ACP session.
        let session_id = uuid::Uuid::new_v4().to_string();
        let mut sessions = lock_sessions(&self.sessions);
        evict_oldest_if_full(&mut sessions);
        sessions.insert(
            session_id.clone(),
            AcpSession {
                created_at: Instant::now(),
                prompt_task: None,
                history: Vec::new(),
                workspace,
            },
        );
        Ok(json!({ "sessionId": session_id }))
    }

    /// `session/list [cwd]` — the saved sessions, newest first, scoped to
    /// `cwd`'s checkout when given (the REPL's workspace rule).
    async fn list_sessions(&self, params: Value) -> std::result::Result<Value, AcpError> {
        let cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        let sessions = with_store(&self.store, |store| store.list_sessions())
            .await
            .map_err(AcpError::internal)?;
        let sessions: Vec<Value> = sessions
            .into_iter()
            .filter(|meta| {
                cwd.as_deref()
                    .is_none_or(|cwd| workspace_scope_matches(&meta.workspace, cwd))
            })
            .map(session_summary)
            .collect();
        Ok(json!({ "sessions": sessions }))
    }

    /// `session/resume` / `session/load` — reload a saved session, replay
    /// its text turns as update chunks, and make it the live session under
    /// its own id (replacing one already open here).
    async fn resume_session<W>(
        &mut self,
        params: Value,
        writer: SharedWriter<W>,
    ) -> std::result::Result<(), AcpError>
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let session_id = required_session_id(&params)?.to_string();
        let lookup = session_id.clone();
        let saved = match with_store(&self.store, move |store| store.load_session(&lookup)).await {
            Ok(saved) => saved,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                ) =>
            {
                return Err(AcpError::invalid_params(format!(
                    "unknown sessionId {session_id:?} ({err})"
                )));
            }
            Err(err) => return Err(AcpError::internal(err)),
        };
        {
            let mut writer = writer.lock().await;
            for message in &saved.messages {
                let kind = match message.role.as_str() {
                    "user" => "user_message_chunk",
                    "assistant" => "agent_message_chunk",
                    _ => continue,
                };
                let text = message_text(message);
                if text.trim().is_empty() {
                    continue;
                }
                write_session_update(&mut *writer, &session_id, kind, text)
                    .await
                    .map_err(AcpError::internal)?;
            }
        }
        self.close_session(&session_id);
        let mut sessions = lock_sessions(&self.sessions);
        evict_oldest_if_full(&mut sessions);
        sessions.insert(
            session_id,
            AcpSession {
                created_at: Instant::now(),
                prompt_task: None,
                history: saved.messages,
                workspace: saved.metadata.workspace,
            },
        );
        Ok(())
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
        let session_id = required_session_id(&params)?.to_string();
        // Snapshot the conversation so far under the lock; the prompt task
        // appends its completed turn back into the map when the reply lands.
        let (history, workspace) = {
            let sessions = lock_sessions(&self.sessions);
            match sessions.get(&session_id) {
                Some(session) => (session.history.clone(), session.workspace.clone()),
                None => return Err(AcpError::invalid_params("unknown sessionId")),
            }
        };
        let prompt = extract_prompt_text(params.get("prompt"))
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| AcpError::invalid_params("prompt must include text content"))?;

        let config = self.config.clone();
        let model = self.model.clone();
        let store = self.store.clone();
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
            let (output, turn_tokens) = match run_prompt(&config, &model, &prompt, messages.clone())
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    let mut writer = writer.lock().await;
                    let _ = write_jsonrpc_error(&mut *writer, id, -32603, err.to_string()).await;
                    return;
                }
            };
            // Persist the completed turn so the next `session/prompt` on
            // this session replays the conversation. A turn without a
            // reply (aborted, empty) is left out entirely.
            if output.is_empty() {
                let mut writer = writer.lock().await;
                if let Some(id) = id {
                    let _ =
                        write_jsonrpc_result(&mut *writer, id, json!({ "stopReason": "end_turn" }))
                            .await;
                }
                return;
            }
            messages.push(Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: output.clone(),
                    cache_control: None,
                }],
            });
            {
                let mut sessions = lock_sessions(&sessions);
                if let Some(session) = sessions.get_mut(&task_session_id) {
                    session.history = messages.clone();
                }
            }
            // The record is written before the prompt is answered. The
            // reply chunk goes out either way, so a failed record is a
            // reported error, not a silently missing one.
            let record = {
                let session_id = task_session_id.clone();
                with_store(&store, move |store| {
                    persist_turn(
                        store,
                        &session_id,
                        &workspace,
                        &model,
                        &messages,
                        turn_tokens,
                    )
                })
                .await
            };
            let mut writer = writer.lock().await;
            let _ = write_session_update(
                &mut *writer,
                &task_session_id,
                "agent_message_chunk",
                output,
            )
            .await;
            if let Some(id) = id {
                let _ = match record {
                    Ok(()) => {
                        write_jsonrpc_result(&mut *writer, id, json!({ "stopReason": "end_turn" }))
                            .await
                    }
                    Err(err) => {
                        write_jsonrpc_error(
                            &mut *writer,
                            Some(id),
                            -32603,
                            format!("reply delivered but the session was not saved: {err}"),
                        )
                        .await
                    }
                };
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

    /// Drop the session from memory, aborting its in-flight prompt. The
    /// saved record stays — close is not delete.
    fn close_session(&mut self, session_id: &str) {
        let mut sessions = lock_sessions(&self.sessions);
        if let Some(session) = sessions.remove(session_id)
            && let Some(task) = session.prompt_task
        {
            task.abort();
        }
    }
}

/// Bound the tracked-session count: evict the oldest beyond
/// `MAX_ACP_SESSIONS`, aborting its in-flight prompt (if any) so it cannot
/// write into a session that no longer exists.
fn evict_oldest_if_full(sessions: &mut HashMap<String, AcpSession>) {
    if sessions.len() >= MAX_ACP_SESSIONS
        && let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, session)| session.created_at)
            .map(|(id, _)| id.clone())
        && let Some(session) = sessions.remove(&oldest)
        && let Some(task) = session.prompt_task
    {
        task.abort();
    }
}

fn required_session_id(params: &Value) -> std::result::Result<&str, AcpError> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::invalid_params("sessionId is required"))
}

/// The store is `std::fs`; its reads and writes run off the runtime worker.
async fn with_store<T: Send + 'static>(
    store: &FileSessionStore,
    op: impl FnOnce(&FileSessionStore) -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || op(&store))
        .await
        .map_err(std::io::Error::other)?
}

/// Write the session's conversation after a completed turn. An existing
/// record — an earlier ACP turn, or a REPL session resumed here — is
/// reloaded and updated, so metadata the REPL changed meanwhile (a
/// `/rename`) survives and a resumed session keeps its own system prompt
/// and mode; otherwise the record is created with the ACP prompt and mode.
fn persist_turn(
    store: &FileSessionStore,
    session_id: &str,
    workspace: &Path,
    model: &str,
    messages: &[Message],
    turn_tokens: u64,
) -> std::io::Result<()> {
    let saved = match store.load_session(session_id) {
        Ok(existing) => {
            let total = existing.metadata.total_tokens.saturating_add(turn_tokens);
            update_session(existing, messages, total, None)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            create_saved_session_with_id_and_mode(
                session_id.to_string(),
                messages,
                model,
                workspace,
                turn_tokens,
                Some(&SystemPrompt::Text(ACP_SYSTEM_PROMPT.to_string())),
                Some("acp"),
            )
        }
        Err(err) => return Err(err),
    };
    store.save_session(&saved).map(|_| ())
}

/// One `session/list` row.
fn session_summary(meta: SessionMetadata) -> Value {
    json!({
        "sessionId": meta.id,
        "title": meta.title,
        "cwd": meta.workspace.display().to_string(),
        "model": meta.model,
        "messageCount": meta.message_count,
        "createdAt": meta.created_at.to_rfc3339(),
        "updatedAt": meta.updated_at.to_rfc3339(),
    })
}

/// The text blocks of one message, joined.
fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run_prompt(
    config: &Config,
    model: &str,
    prompt: &str,
    messages: Vec<Message>,
) -> Result<(String, u64)> {
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
        system: Some(SystemPrompt::Text(ACP_SYSTEM_PROMPT.to_string())),
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
    let turn_tokens =
        u64::from(response.usage.input_tokens) + u64::from(response.usage.output_tokens);
    Ok((output, turn_tokens))
}

impl AcpError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn internal(err: impl std::fmt::Display) -> Self {
        Self {
            code: -32603,
            message: err.to_string(),
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
            "loadSession": true,
            "promptCapabilities": {
                "image": false,
                "audio": false,
                "embeddedContext": true
            },
            "mcpCapabilities": {
                "http": false,
                "sse": false
            },
            "sessionCapabilities": {
                "list": {},
                "resume": {},
                "close": {}
            }
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

async fn write_session_update<W>(
    writer: &mut W,
    session_id: &str,
    kind: &str,
    text: String,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": kind,
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

    fn text(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_only_assistant() -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({}),
                caller: None,
            }],
        }
    }

    /// The answer to [`tool_only_assistant`]; a paired call loads as saved
    /// (an unpaired one gets a `TOOL_OUTCOME_UNKNOWN` terminator appended).
    fn tool_result() -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "fn main() {}".to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    fn temp_store() -> (tempfile::TempDir, FileSessionStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(tmp.path().join("sessions")).unwrap();
        (tmp, store)
    }

    fn server_over(store: FileSessionStore) -> AcpServer {
        AcpServer::new(Config::default(), "test-model".to_string(), store)
    }

    fn sink() -> SharedWriter<Vec<u8>> {
        Arc::new(tokio::sync::Mutex::new(Vec::new()))
    }

    fn response(dispatch: std::result::Result<AcpDispatch, AcpError>) -> Value {
        match dispatch {
            Ok(AcpDispatch::Response(value)) => value,
            other => panic!("expected a response dispatch, got {other:?}"),
        }
    }

    async fn written_lines(writer: &SharedWriter<Vec<u8>>) -> Vec<Value> {
        let bytes = writer.lock().await;
        String::from_utf8(bytes.clone())
            .expect("utf8")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each line is json"))
            .collect()
    }

    /// Open a live session directly, the way `session/new` does, without
    /// going through the dispatcher.
    fn open_session(server: &mut AcpServer, id: &str, workspace: &Path) {
        lock_sessions(&server.sessions).insert(
            id.to_string(),
            AcpSession {
                created_at: Instant::now(),
                prompt_task: None,
                history: Vec::new(),
                workspace: workspace.to_path_buf(),
            },
        );
    }

    #[test]
    fn initialize_advertises_the_session_surface() {
        let result = initialize_result(Some(1));

        assert_eq!(result["protocolVersion"], 1);
        assert_eq!(result["agentInfo"]["name"], "codesmith");
        assert_eq!(result["agentCapabilities"]["loadSession"], true);
        for method in ["list", "resume", "close"] {
            assert!(
                result["agentCapabilities"]["sessionCapabilities"]
                    .get(method)
                    .is_some(),
                "session capability {method} not advertised"
            );
        }
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
    async fn session_new_keeps_the_editor_cwd_and_saves_nothing_yet() {
        let (tmp, store) = temp_store();
        let mut server = server_over(store);
        let writer = sink();
        let cwd = tmp.path().join("proj");

        let created = response(
            server
                .handle_request(
                    "session/new",
                    json!({ "cwd": cwd.display().to_string() }),
                    Some(json!(1)),
                    writer.clone(),
                )
                .await,
        );
        let id = created["sessionId"].as_str().unwrap().to_string();
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "a plain UUID, like a REPL session: {id}"
        );
        assert_eq!(lock_sessions(&server.sessions)[&id].workspace, cwd);

        let listed = response(
            server
                .handle_request("session/list", json!({}), Some(json!(2)), writer.clone())
                .await,
        );
        assert_eq!(
            listed["sessions"],
            json!([]),
            "no record until a turn completes"
        );
    }

    #[tokio::test]
    async fn a_completed_turn_is_saved_listed_and_scoped_by_cwd() {
        let (tmp, store) = temp_store();
        let mut server = server_over(store.clone());
        let writer = sink();
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        open_session(&mut server, "acp-1", &cwd);

        let first_turn = [text("user", "hello there"), text("assistant", "hi")];
        persist_turn(&store, "acp-1", &cwd, "test-model", &first_turn, 42).unwrap();

        let listed = response(
            server
                .handle_request("session/list", json!({}), Some(json!(1)), writer.clone())
                .await,
        );
        let rows = listed["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["sessionId"], "acp-1");
        assert_eq!(rows[0]["title"], "hello there");
        assert_eq!(rows[0]["messageCount"], 2);
        assert_eq!(rows[0]["cwd"], cwd.display().to_string());

        // Scoped listing: the editor's own project sees it, another does not.
        let scoped = response(
            server
                .handle_request(
                    "session/list",
                    json!({ "cwd": cwd.display().to_string() }),
                    Some(json!(2)),
                    writer.clone(),
                )
                .await,
        );
        assert_eq!(scoped["sessions"].as_array().unwrap().len(), 1);
        let elsewhere = response(
            server
                .handle_request(
                    "session/list",
                    json!({ "cwd": tmp.path().join("other").display().to_string() }),
                    Some(json!(3)),
                    writer.clone(),
                )
                .await,
        );
        assert_eq!(elsewhere["sessions"], json!([]));

        // A later turn updates the record: tokens accumulate, the shape stays.
        let second_turn = [
            text("user", "hello there"),
            text("assistant", "hi"),
            text("user", "again"),
            text("assistant", "yo"),
        ];
        persist_turn(&store, "acp-1", &cwd, "test-model", &second_turn, 8).unwrap();
        let saved = store.load_session("acp-1").unwrap();
        assert_eq!(saved.metadata.total_tokens, 50);
        assert_eq!(saved.metadata.message_count, 4);
        assert_eq!(saved.metadata.mode.as_deref(), Some("acp"));
        assert_eq!(saved.system_prompt.as_deref(), Some(ACP_SYSTEM_PROMPT));
    }

    #[test]
    fn a_resumed_repl_session_keeps_its_prompt_mode_and_model() {
        let (tmp, store) = temp_store();
        let cwd = tmp.path().join("proj");
        let repl = create_saved_session_with_id_and_mode(
            "repl-1".to_string(),
            &[text("user", "q"), text("assistant", "a")],
            "deepseek-chat",
            &cwd,
            10,
            Some(&SystemPrompt::Text("repl prompt".to_string())),
            None,
        );
        store.save_session(&repl).unwrap();

        let next_turn = [
            text("user", "q"),
            text("assistant", "a"),
            text("user", "from acp"),
            text("assistant", "ok"),
        ];
        persist_turn(&store, "repl-1", &cwd, "test-model", &next_turn, 5).unwrap();

        let after = store.load_session("repl-1").unwrap();
        assert_eq!(after.system_prompt.as_deref(), Some("repl prompt"));
        assert_eq!(after.metadata.mode, None);
        assert_eq!(after.metadata.model, "deepseek-chat");
        assert_eq!(after.metadata.total_tokens, 15);
    }

    #[tokio::test]
    async fn resume_replays_text_turns_and_takes_the_saved_workspace() {
        let (tmp, store) = temp_store();
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let messages = vec![
            text("user", "q1"),
            text("assistant", "a1"),
            tool_only_assistant(),
            tool_result(),
            text("user", "q2"),
        ];
        let saved = create_saved_session_with_id_and_mode(
            "sess-replay".to_string(),
            &messages,
            "m",
            &cwd,
            0,
            None,
            None,
        );
        store.save_session(&saved).unwrap();

        let mut server = server_over(store);
        let writer = sink();

        let dispatch = server
            .handle_request(
                "session/resume",
                json!({ "sessionId": "sess-replay" }),
                Some(json!(3)),
                writer.clone(),
            )
            .await;
        assert_eq!(response(dispatch), json!(null));

        let updates = written_lines(&writer).await;
        let kinds: Vec<&str> = updates
            .iter()
            .map(|u| u["params"]["update"]["sessionUpdate"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "user_message_chunk",
                "agent_message_chunk",
                "user_message_chunk"
            ],
            "tool turns are skipped in the replay"
        );
        assert_eq!(updates[1]["params"]["update"]["content"]["text"], "a1");
        assert!(
            updates
                .iter()
                .all(|u| u["params"]["sessionId"] == "sess-replay")
        );

        {
            let sessions = lock_sessions(&server.sessions);
            let session = &sessions["sess-replay"];
            assert_eq!(
                session.history.len(),
                5,
                "tool call and result stay in the history the next prompt sends"
            );
            assert_eq!(session.workspace, cwd);
        }

        // The protocol's v1 name answers the same way.
        let alias = server
            .handle_request(
                "session/load",
                json!({ "sessionId": "sess-replay" }),
                Some(json!(4)),
                writer.clone(),
            )
            .await;
        assert_eq!(response(alias), json!(null));
    }

    #[tokio::test]
    async fn resume_of_an_unknown_session_is_invalid_params() {
        let (_tmp, store) = temp_store();
        let mut server = server_over(store);
        let writer = sink();

        let err = server
            .handle_request(
                "session/resume",
                json!({ "sessionId": "nope" }),
                Some(json!(5)),
                writer.clone(),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, -32602);
        assert!(err.message.contains("nope"), "{}", err.message);
    }

    #[tokio::test]
    async fn close_forgets_the_live_session_and_keeps_the_record() {
        let (tmp, store) = temp_store();
        let mut server = server_over(store.clone());
        let writer = sink();
        let cwd = tmp.path().join("proj");
        let created = response(
            server
                .handle_request(
                    "session/new",
                    json!({ "cwd": cwd.display().to_string() }),
                    Some(json!(1)),
                    writer.clone(),
                )
                .await,
        );
        let id = created["sessionId"].as_str().unwrap().to_string();
        persist_turn(
            &store,
            &id,
            &cwd,
            "test-model",
            &[text("user", "hi"), text("assistant", "yo")],
            3,
        )
        .unwrap();

        let closed = response(
            server
                .handle_request(
                    "session/close",
                    json!({ "sessionId": id }),
                    Some(json!(2)),
                    writer.clone(),
                )
                .await,
        );
        assert_eq!(closed, json!(null));
        assert!(lock_sessions(&server.sessions).is_empty());
        assert_eq!(
            store.list_sessions().unwrap().len(),
            1,
            "close is not delete"
        );
    }

    #[tokio::test]
    async fn session_update_is_protocol_clean_single_line_json() {
        let mut out = Vec::new();

        write_session_update(
            &mut out,
            "sess_1",
            "agent_message_chunk",
            "hello\nworld".to_string(),
        )
        .await
        .expect("write update");

        let line = String::from_utf8(out).expect("utf8");
        assert_eq!(line.lines().count(), 1);
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["method"], "session/update");
        assert_eq!(value["params"]["sessionId"], "sess_1");
        assert_eq!(
            value["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
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
