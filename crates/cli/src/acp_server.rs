//! Agent Client Protocol stdio adapter.
//!
//! Baseline: `initialize`, `session/new`, `session/prompt`,
//! `session/cancel`, `shutdown`. Round-8 TODO-51 adds the session surface
//! and puts prompts through the engine.
//!
//! **An ACP session is a saved session**: the same
//! `~/.codesmith/sessions/<id>.json` record the REPL writes, with a plain
//! UUID id. `session/list` enumerates the store (scoped to a `cwd`'s
//! checkout when given), `session/resume` — and `session/load`, the ACP v1
//! name — reloads one and replays its turns as `session/update` chunks, and
//! `session/close` releases the live one. A conversation started in the
//! editor continues with `/resume` in the terminal, and the other way round.
//!
//! **A prompt is an engine turn.** Each live session owns one engine —
//! [`headless_engine_config`], the same configuration `codesmith exec` runs
//! with — created lazily at its first prompt, plus one pump task that is the
//! engine's only event reader: text deltas relay as `agent_message_chunk`,
//! reasoning as `agent_thinking_chunk`, tool calls as `tool_call` /
//! `tool_call_update`. A prompt completes through a per-session oneshot the
//! pump resolves on `TurnComplete`; because the pump is the single reader, a
//! cancelled turn cannot be confused with its successor. Two prompts are
//! never in flight on one session: the second is refused with
//! `invalid_params` (ACP clients await one response before the next).
//!
//! **Approvals ask the client.** When the engine gates a tool call, the pump
//! sends `session/request_permission` (allow once / allow for this session /
//! deny) and waits up to [`PERMISSION_TIMEOUT`] for the client's response,
//! which the read loop routes back by JSON-RPC id. Anything but an explicit
//! allow — a denial, an error, a cancellation, no answer — refuses the
//! call: the surface fails closed, as `codesmith exec` does without
//! `--auto`. "Allow for this session" remembers the approval grouping key
//! for the live session, like the REPL's session grants.
//!
//! Persistence follows "model-visible means logged": a completed turn is
//! written before `session/prompt` answers, from the engine's
//! `SessionUpdated` transcript. When the write fails, the reply is still
//! delivered and the prompt is answered with a JSON-RPC error naming the
//! cause — the editor keeps the text, and a missing record is reported
//! rather than silent. The store is opened when the server starts; an
//! unusable sessions directory fails the start.
//!
//! Stdout stays protocol-clean for editor clients.
//!
//! Known limitations (recorded beside the behaviour they bound):
//!
//! - Only approval gates are asked over the protocol. Path access outside
//!   the workspace, sandbox elevation, and `request_user_input` are refused
//!   outright — they have no `session/request_permission` shape of their own
//!   yet.
//! - Session "always" grants live in memory with the live session; they are
//!   not persisted and do not reach the REPL's grant store.
//! - Replay carries text blocks only: tool calls and results in a REPL
//!   session are skipped in the update stream while staying in the history
//!   the engine syncs in.
//! - `session/list` has no paging; the store's own retention cap bounds it.
//! - One engine per live session; `MAX_ACP_SESSIONS` bounds the count.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context as _, Result};
use codesmith_agent_runtime::mode::ApprovalMode;
use codesmith_agent_runtime::session_store::{
    FileSessionStore, SessionMetadata, create_saved_session_with_id_and_mode, update_session,
    workspace_scope_matches,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use crate::app_state::AppMode;
use crate::config::Config;
use crate::core::engine::{
    EngineHandle, EngineHost, HeadlessEngineOptions, headless_engine_config, spawn_engine,
};
use crate::core::events::{Event, TurnOutcomeStatus};
use crate::models::{ContentBlock, Message, SystemPrompt};
use crate::prompts::PromptAppendSource;
use crate::settings::Settings;
use codesmith_agent_runtime::ops::Op;

const ACP_PROTOCOL_VERSION: u64 = 1;

/// Upper bound on tracked sessions; the oldest is evicted beyond it.
const MAX_ACP_SESSIONS: usize = 128;

/// Appended to the engine's base prompt for every ACP session, so the
/// request stays recognizable as an ACP turn (and the model knows its
/// audience).
const ACP_EDITOR_NOTE: &str = "You are answering inside an ACP-compatible \
editor client. Give concise, actionable responses.";

/// Stdout is shared between the read loop, per-session pumps, and prompt
/// tasks, which write `session/update` notifications and JSON-RPC responses.
type SharedWriter<W> = Arc<tokio::sync::Mutex<W>>;

pub async fn run_acp_server(config: Config, model: String) -> Result<()> {
    let store = FileSessionStore::default_location()
        .context("ACP sessions are saved sessions; the sessions directory must be usable")?;
    let settings = Settings::load().unwrap_or_default();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let writer: SharedWriter<tokio::io::BufWriter<tokio::io::Stdout>> =
        Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdout)));
    let mut server = AcpServer::new(config, model, store, settings);

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
                // A client RESPONSE (id, no method, `result`/`error`) must
                // never be replied to — JSON-RPC 2.0 forbids answering a
                // response, and strict clients (Zed among them) drop the
                // connection on it. Swallow it whether or not its waiter is
                // still pending: a late answer to a timed-out permission
                // request, or a duplicated reply, is not a protocol error.
                if message.get("result").is_some() || message.get("error").is_some() {
                    route_client_response(&server.shared, &message);
                    continue;
                }
                if route_client_response(&server.shared, &message) {
                    continue;
                }
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

/// What `session/prompt` is answered with once the turn completes.
struct PromptOutcome {
    status: TurnOutcomeStatus,
    error: Option<String>,
    /// Input + output tokens of the completed turn.
    tokens: u64,
}

struct AcpServer {
    config: Config,
    model: String,
    store: FileSessionStore,
    settings: Settings,
    shared: AcpShared,
}

struct AcpSession {
    /// Creation stamp; the oldest session is evicted once the map grows
    /// past `MAX_ACP_SESSIONS`.
    created_at: Instant,
    /// Monotonic identity of this map entry: bumped every time the id is
    /// re-inserted (a `session/new`/`session/resume` replacing an entry) so
    /// `ensure_engine` can detect that the session it built an engine for
    /// was replaced while the build was in flight.
    generation: u64,
    /// The editor's `cwd` from `session/new`, or the saved session's
    /// workspace on resume — what the record's `workspace` field says.
    workspace: PathBuf,
    /// The transcript as the engine last reported it (`SessionUpdated`), or
    /// the saved messages on resume until the engine takes over. What the
    /// next save writes.
    history: Arc<Vec<Message>>,
    /// Same provenance as [`AcpSession::history`].
    system_prompt: Option<SystemPrompt>,
    /// Created at the session's first prompt; one engine per session.
    engine: Option<EngineHandle>,
    /// The engine's only event reader.
    pump_task: Option<tokio::task::AbortHandle>,
    /// The in-flight prompt's completion slot, if any.
    pending_prompt: Option<oneshot::Sender<PromptOutcome>>,
    /// Approval grouping keys the client allowed "for this session".
    approved_keys: HashSet<String>,
}

/// Lock the session map, tolerating a poisoned lock: the map is only ever
/// held for brief critical sections, so a panic elsewhere must not take
/// every session down with it.
fn lock_sessions(
    sessions: &Mutex<HashMap<String, AcpSession>>,
) -> std::sync::MutexGuard<'_, HashMap<String, AcpSession>> {
    sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Same poisoned-lock recovery for the permission-waiter map.
fn lock_waiters(
    waiters: &Mutex<HashMap<String, oneshot::Sender<Value>>>,
) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<Value>>> {
    waiters
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// State shared by the read loop, the per-session pumps, and prompt tasks.
#[derive(Clone, Default)]
struct AcpShared {
    sessions: Arc<Mutex<HashMap<String, AcpSession>>>,
    /// Server-initiated requests (`session/request_permission`) awaiting the
    /// client's response, keyed by the JSON-RPC id the client echoes.
    permission_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
}

/// How long the client has to answer `session/request_permission` before
/// the call is refused — the runtime-thread API's approval timeout.
const PERMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// The client's answer to one permission request.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Permission {
    Allow,
    /// Allow now and remember the approval's grouping key for the session.
    AllowAlways,
    Deny,
}

/// Deliver a client RESPONSE (an id, no method) to its waiter, if any.
/// Returns whether the message was one of ours.
fn route_client_response(shared: &AcpShared, message: &Value) -> bool {
    let Some(id) = message.get("id") else {
        return false;
    };
    let key = match id {
        Value::String(key) => key.clone(),
        other => other.to_string(),
    };
    let waiter = lock_waiters(&shared.permission_waiters).remove(&key);
    match waiter {
        Some(waiter) => waiter.send(message.clone()).is_ok(),
        None => false,
    }
}

/// The options a `session/request_permission` offers, in order.
fn permission_options() -> Value {
    json!([
        { "optionId": "allow_once", "name": "Allow", "kind": "allow_once" },
        { "optionId": "allow_always", "name": "Allow for this session", "kind": "allow_always" },
        { "optionId": "reject_once", "name": "Deny", "kind": "reject_once" },
    ])
}

/// Map the client's response to a decision. Fail closed: anything but an
/// explicit allow is a denial (an error object, a cancellation, a timeout,
/// or an unknown optionId).
fn permission_from_response(response: &Value) -> Permission {
    if response.get("error").is_some() {
        return Permission::Deny;
    }
    let outcome = response.pointer("/result/outcome");
    let selected = outcome
        .and_then(|o| o.get("outcome"))
        .and_then(Value::as_str)
        == Some("selected");
    if !selected {
        return Permission::Deny;
    }
    match outcome
        .and_then(|o| o.get("optionId"))
        .and_then(Value::as_str)
    {
        Some("allow_always") => Permission::AllowAlways,
        Some("allow_once") | Some("allow") => Permission::Allow,
        _ => Permission::Deny,
    }
}

/// Ask the client, with a caller-chosen deadline (the production timeout is
/// [`PERMISSION_TIMEOUT`]; tests shrink it).
async fn ask_permission_within<W>(
    shared: &AcpShared,
    writer: &SharedWriter<W>,
    session_id: &str,
    tool_call: Value,
    timeout: std::time::Duration,
) -> Permission
where
    W: AsyncWrite + Unpin,
{
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    lock_waiters(&shared.permission_waiters).insert(request_id.clone(), tx);
    let sent = {
        let mut writer = writer.lock().await;
        write_json_line(
            &mut *writer,
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "session/request_permission",
                "params": {
                    "sessionId": session_id,
                    "toolCall": tool_call,
                    "options": permission_options(),
                }
            }),
        )
        .await
    };
    if sent.is_err() {
        lock_waiters(&shared.permission_waiters).remove(&request_id);
        return Permission::Deny;
    }
    let answer = tokio::time::timeout(timeout, rx).await;
    lock_waiters(&shared.permission_waiters).remove(&request_id);
    match answer {
        Ok(Ok(response)) => permission_from_response(&response),
        // A dropped waiter or the deadline: refused, never waved through.
        _ => Permission::Deny,
    }
}

async fn ask_permission<W>(
    shared: &AcpShared,
    writer: &SharedWriter<W>,
    session_id: &str,
    tool_call: Value,
) -> Permission
where
    W: AsyncWrite + Unpin,
{
    ask_permission_within(shared, writer, session_id, tool_call, PERMISSION_TIMEOUT).await
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
    fn new(config: Config, model: String, store: FileSessionStore, settings: Settings) -> Self {
        Self {
            config,
            model,
            store,
            settings,
            shared: AcpShared::default(),
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
        let mut sessions = lock_sessions(&self.shared.sessions);
        evict_oldest_if_full(&mut sessions);
        sessions.insert(
            session_id.clone(),
            AcpSession {
                created_at: Instant::now(),
                generation: 0,
                workspace,
                history: Arc::new(Vec::new()),
                system_prompt: None,
                engine: None,
                pump_task: None,
                pending_prompt: None,
                approved_keys: HashSet::new(),
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
    /// its own id (replacing one already open here). The engine is created
    /// lazily at the next prompt, with this history synced in.
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
                write_session_update(&mut *writer, &session_id, chunk_update(kind, text))
                    .await
                    .map_err(AcpError::internal)?;
            }
        }
        // Capture the entry's generation BEFORE closing: the fresh entry
        // must carry generation+1 so an engine built against the removed
        // entry (in-flight `ensure_engine`) is never installed into it.
        let next_generation = lock_sessions(&self.shared.sessions)
            .get(&session_id)
            .map_or(0, |session| session.generation + 1);
        self.close_session(&session_id);
        let mut sessions = lock_sessions(&self.shared.sessions);
        evict_oldest_if_full(&mut sessions);
        sessions.insert(
            session_id,
            AcpSession {
                created_at: Instant::now(),
                generation: next_generation,
                workspace: saved.metadata.workspace,
                history: Arc::new(saved.messages),
                system_prompt: saved.system_prompt.map(SystemPrompt::Text),
                engine: None,
                pump_task: None,
                pending_prompt: None,
                approved_keys: HashSet::new(),
            },
        );
        Ok(())
    }

    /// Validate the prompt request and reserve the session's one prompt
    /// slot, then run the turn on its own task so the read loop stays free
    /// to accept `session/cancel` (and other requests) while it runs. The
    /// task owns the response for `id`.
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
        let prompt = extract_prompt_text(params.get("prompt"))
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| AcpError::invalid_params("prompt must include text content"))?;
        let (completion_tx, completion_rx) = oneshot::channel::<PromptOutcome>();
        {
            let mut sessions = lock_sessions(&self.shared.sessions);
            let Some(session) = sessions.get_mut(&session_id) else {
                return Err(AcpError::invalid_params("unknown sessionId"));
            };
            if session.pending_prompt.is_some() {
                return Err(AcpError::invalid_params(
                    "a prompt is already in flight for this session — session/cancel it first",
                ));
            }
            session.pending_prompt = Some(completion_tx);
        }

        let request = TurnRequest {
            config: self.config.clone(),
            settings: self.settings.clone(),
            model: self.model.clone(),
            store: self.store.clone(),
            session_id,
            prompt,
        };
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let outcome = run_turn(request, completion_rx, &shared, writer.clone()).await;
            let mut writer = writer.lock().await;
            let _ = match outcome {
                Ok(outcome) => answer_completed_prompt(&mut *writer, id, &outcome).await,
                Err(err) => write_jsonrpc_error(&mut *writer, id, -32603, err.to_string()).await,
            };
        });
        Ok(())
    }

    /// End the in-flight turn of `session_id`, if any: the engine cancels,
    /// the pump resolves the pending prompt as interrupted, and the
    /// cancelled prompt's own task answers its request with `cancelled`.
    fn cancel_session(&mut self, session_id: &str) {
        let sessions = lock_sessions(&self.shared.sessions);
        if let Some(session) = sessions.get(session_id)
            && let Some(engine) = session.engine.as_ref()
        {
            engine.cancel();
        }
    }

    /// Drop the session from memory — engine shut down, pump aborted,
    /// in-flight prompt (if any) answered `cancelled` by its own task when
    /// its completion slot drops. The saved record stays: close is not
    /// delete.
    fn close_session(&mut self, session_id: &str) {
        let mut sessions = lock_sessions(&self.shared.sessions);
        if let Some(session) = sessions.remove(session_id) {
            release(session);
        }
    }
}

/// What one prompt carries into its turn.
struct TurnRequest {
    config: Config,
    settings: Settings,
    model: String,
    store: FileSessionStore,
    session_id: String,
    prompt: String,
}

/// Run one prompt: resolve the route, ensure the session's engine exists,
/// send the message, and wait for the pump to report the turn's outcome.
/// Persistence happens here, before the prompt is answered.
async fn run_turn<W>(
    request: TurnRequest,
    completion: oneshot::Receiver<PromptOutcome>,
    shared: &AcpShared,
    writer: SharedWriter<W>,
) -> anyhow::Result<PromptOutcome>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let TurnRequest {
        config,
        settings,
        model,
        store,
        session_id,
        prompt,
    } = request;
    let route = crate::main_root::resolve_cli_auto_route(&config, &model, &prompt).await;
    let reasoning_effort = route
        .reasoning_effort
        .map(|effort| effort.as_setting().to_string());

    let engine = ensure_engine(
        shared,
        &config,
        &settings,
        &session_id,
        &route.model,
        writer,
    )
    .await?;
    engine
        .send(Op::SendMessage {
            content: prompt,
            image_paths: Vec::new(),
            mode: AppMode::Agent,
            model: route.model.clone(),
            goal_objective: None,
            allowed_tools: None,
            blocked_tools: Vec::new(),
            reasoning_effort,
            reasoning_effort_auto: route.auto_model,
            auto_model: route.auto_model,
            allow_shell: config.allow_shell(),
            trust_mode: false,
            auto_approve: false,
            translation_enabled: false,
            show_thinking: settings.show_thinking,
            is_simple: settings.is_simple,
            approval_mode: config
                .approval_policy
                .as_deref()
                .and_then(ApprovalMode::from_config_value)
                .unwrap_or_default(),
        })
        .await
        .context("send prompt to the engine")?;

    let outcome = match completion.await {
        Ok(outcome) => outcome,
        // The session closed (or its pump died); the turn is over either way.
        Err(_) => {
            engine.cancel();
            return Ok(PromptOutcome {
                status: TurnOutcomeStatus::Interrupted,
                error: None,
                tokens: 0,
            });
        }
    };

    // Persist the engine's transcript before answering; on failure the
    // caller reports it instead of answering success.
    let (history, system_prompt, workspace) = {
        let sessions = lock_sessions(&shared.sessions);
        match sessions.get(session_id.as_str()) {
            Some(session) => (
                session.history.clone(),
                session.system_prompt.clone(),
                session.workspace.clone(),
            ),
            None => (
                Arc::new(Vec::new()),
                None,
                std::env::current_dir().unwrap_or_default(),
            ),
        }
    };
    let persist = if history.is_empty() {
        Ok(())
    } else {
        let store_model = model;
        with_store(&store, move |store| {
            persist_turn(
                store,
                &session_id,
                &workspace,
                &store_model,
                &history[..],
                outcome.tokens,
                system_prompt.as_ref(),
            )
        })
        .await
    };
    match persist {
        Ok(()) => Ok(outcome),
        Err(err) => Err(anyhow::anyhow!(
            "reply delivered but the session was not saved: {err}"
        )),
    }
}

/// Create the session's engine and pump at its first prompt, or return the
/// existing one. A resumed session's saved transcript is synced in.
async fn ensure_engine<W>(
    shared: &AcpShared,
    config: &Config,
    settings: &Settings,
    session_id: &str,
    model: &str,
    writer: SharedWriter<W>,
) -> anyhow::Result<EngineHandle>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (existing, workspace, history, system_prompt, generation) = {
        let sessions = lock_sessions(&shared.sessions);
        let Some(session) = sessions.get(session_id) else {
            anyhow::bail!("unknown sessionId {session_id:?}");
        };
        (
            session.engine.clone(),
            session.workspace.clone(),
            session.history.clone(),
            session.system_prompt.clone(),
            session.generation,
        )
    };
    if let Some(engine) = existing {
        return Ok(engine);
    }
    let engine_config = headless_engine_config(
        config,
        settings,
        HeadlessEngineOptions {
            model: model.to_string(),
            workspace: workspace.clone(),
            auto_approve: false,
            trust_mode: false,
            max_subagents: config.max_subagents(),
            override_system_prompt: None,
            append_system_prompts: vec![PromptAppendSource::inline("acp-editor", ACP_EDITOR_NOTE)],
            cache_breaker: None,
            allowed_tools: None,
            team_context: None,
        },
    )?;
    let engine = spawn_engine(engine_config, config, EngineHost::default());
    if !history.is_empty() {
        engine
            .send(Op::SyncSession {
                session_id: Some(session_id.to_string()),
                messages: (*history).clone(),
                system_prompt,
                system_prompt_override: false,
                model: model.to_string(),
                workspace,
            })
            .await
            .context("sync the resumed session into the engine")?;
    }
    let pump = tokio::spawn(pump_events(
        engine.clone(),
        shared.clone(),
        session_id.to_string(),
        writer,
    ));
    let mut sessions = lock_sessions(&shared.sessions);
    match sessions.get_mut(session_id) {
        Some(session) if session.generation == generation => {
            session.engine = Some(engine.clone());
            session.pump_task = Some(pump.abort_handle());
        }
        // Closed or REPLACED while the engine was being built (e.g. a
        // concurrent `session/resume` for the same id removed and re-inserted
        // the entry): a plain `get_mut` success here would install an engine
        // built from the replaced entry's workspace/history into the fresh
        // session. The generation mismatch names that case.
        _ => {
            pump.abort();
            release_engine(engine);
            anyhow::bail!("session {session_id:?} went away while its engine was built");
        }
    }
    Ok(engine)
}

/// Write one session update on behalf of the pump. `false` means stdout is
/// gone (editor crashed / pipe closed): once writes fail, every further
/// event would stream into a void while an in-flight `session/prompt` waits
/// out its completion — so the caller stops the pump, and the pending
/// prompt's sender is dropped here, which the waiter resolves as
/// `Interrupted` (cancelling the engine) instead of hanging.
async fn pump_emit<W>(
    writer: &SharedWriter<W>,
    shared: &AcpShared,
    session_id: &str,
    update: Value,
) -> bool
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut writer = writer.lock().await;
    let ok = write_session_update(&mut *writer, session_id, update)
        .await
        .is_ok();
    drop(writer);
    if !ok {
        let mut sessions = lock_sessions(&shared.sessions);
        if let Some(session) = sessions.get_mut(session_id) {
            drop(session.pending_prompt.take());
        }
    }
    ok
}

/// Stream engine events to the client as `session/update` notifications
/// until the engine (or the write side) goes away; the pump is the engine's
/// only event reader, so `ensure_engine` aborts it when a session closes and
/// the successor must not be confused with its predecessor.
async fn pump_events<W>(
    engine: EngineHandle,
    shared: AcpShared,
    session_id: String,
    writer: SharedWriter<W>,
) where
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let event = {
            let mut rx = engine.rx_event.write().await;
            rx.recv().await
        };
        let Some(event) = event else {
            return;
        };
        match event {
            Event::MessageDelta { content, .. } => {
                if !pump_emit(
                    &writer,
                    &shared,
                    &session_id,
                    chunk_update("agent_message_chunk", content),
                )
                .await
                {
                    return;
                }
            }
            Event::ThinkingDelta { content, .. } => {
                if !pump_emit(
                    &writer,
                    &shared,
                    &session_id,
                    chunk_update("agent_thinking_chunk", content),
                )
                .await
                {
                    return;
                }
            }
            Event::ToolCallStarted { id, name, input } => {
                if !pump_emit(
                    &writer,
                    &shared,
                    &session_id,
                    json!({
                        "sessionUpdate": "tool_call",
                        "toolCallId": id,
                        "title": tool_title(&name, &input),
                        "kind": tool_kind(&name),
                        "status": "in_progress",
                    }),
                )
                .await
                {
                    return;
                }
            }
            Event::ToolCallProgress { id, output } => {
                if !pump_emit(
                    &writer,
                    &shared,
                    &session_id,
                    tool_call_update(&id, "in_progress", &output),
                )
                .await
                {
                    return;
                }
            }
            Event::ToolCallComplete { id, result, .. } => {
                let (status, text) = match result {
                    Ok(output) => (
                        if output.success {
                            "completed"
                        } else {
                            "failed"
                        },
                        output.content,
                    ),
                    Err(err) => ("failed", err.to_string()),
                };
                if !pump_emit(
                    &writer,
                    &shared,
                    &session_id,
                    tool_call_update(&id, status, &text),
                )
                .await
                {
                    return;
                }
            }
            // The permission channel: a session "always" grant answers
            // directly; otherwise the client is asked over
            // `session/request_permission`, and no answer is a refusal.
            Event::ApprovalRequired {
                id,
                tool_name,
                input,
                approval_grouping_key,
                intent_summary,
                ..
            } => {
                let granted = {
                    let sessions = lock_sessions(&shared.sessions);
                    sessions.get(&session_id).is_some_and(|session| {
                        session.approved_keys.contains(&approval_grouping_key)
                    })
                };
                let decision = if granted {
                    Permission::Allow
                } else {
                    let title = intent_summary
                        .filter(|intent| !intent.trim().is_empty())
                        .map(|intent| format!("{tool_name}: {intent}"))
                        .unwrap_or_else(|| tool_title(&tool_name, &input));
                    ask_permission(
                        &shared,
                        &writer,
                        &session_id,
                        json!({
                            "toolCallId": id.clone(),
                            "title": title,
                            "kind": tool_kind(&tool_name),
                            "status": "pending",
                            "rawInput": input,
                        }),
                    )
                    .await
                };
                match decision {
                    Permission::AllowAlways => {
                        {
                            let mut sessions = lock_sessions(&shared.sessions);
                            if let Some(session) = sessions.get_mut(&session_id) {
                                session.approved_keys.insert(approval_grouping_key);
                            }
                        }
                        let _ = engine.approve_tool_call(id).await;
                    }
                    Permission::Allow => {
                        let _ = engine.approve_tool_call(id).await;
                    }
                    Permission::Deny => {
                        let _ = engine.deny_tool_call(id).await;
                    }
                }
            }
            // Path access, sandbox elevation, and ask-user have no protocol
            // channel of their own yet: refused, never waved through.
            Event::PathAccessRequired { id, .. } => {
                let _ = engine.deny_tool_call(id).await;
            }
            Event::ElevationRequired { tool_id, .. } => {
                let _ = engine.deny_tool_call(tool_id).await;
            }
            Event::UserInputRequired { id, .. } => {
                let _ = engine.cancel_user_input(id).await;
            }
            Event::SessionUpdated {
                messages,
                system_prompt,
                ..
            } => {
                let mut sessions = lock_sessions(&shared.sessions);
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.history = messages;
                    session.system_prompt = system_prompt;
                }
            }
            Event::TurnComplete {
                status,
                error,
                usage,
                ..
            } => {
                let outcome = PromptOutcome {
                    status,
                    error,
                    tokens: u64::from(usage.input_tokens) + u64::from(usage.output_tokens),
                };
                let mut sessions = lock_sessions(&shared.sessions);
                if let Some(session) = sessions.get_mut(&session_id)
                    && let Some(pending) = session.pending_prompt.take()
                {
                    let _ = pending.send(outcome);
                }
            }
            _ => {}
        }
    }
}

/// Answer one completed prompt: the record is already written, so a failure
/// here is the turn's own.
async fn answer_completed_prompt<W>(
    writer: &mut W,
    id: Option<Value>,
    outcome: &PromptOutcome,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let Some(id) = id else {
        return Ok(());
    };
    match &outcome.status {
        TurnOutcomeStatus::Failed => {
            write_jsonrpc_error(
                writer,
                Some(id),
                -32603,
                outcome
                    .error
                    .clone()
                    .unwrap_or_else(|| "turn failed".to_string()),
            )
            .await
        }
        status => {
            write_jsonrpc_result(writer, id, json!({ "stopReason": stop_reason(status) })).await
        }
    }
}

fn stop_reason(status: &TurnOutcomeStatus) -> &'static str {
    match status {
        TurnOutcomeStatus::Completed => "end_turn",
        TurnOutcomeStatus::Interrupted => "cancelled",
        // Failed turns are answered as errors before reaching here; this is
        // the unreachable mapping.
        TurnOutcomeStatus::Failed => "end_turn",
    }
}

/// Abort the pump and shut the engine down.
fn release(session: AcpSession) {
    if let Some(pump) = session.pump_task {
        pump.abort();
    }
    if let Some(engine) = session.engine {
        release_engine(engine);
    }
}

fn release_engine(engine: EngineHandle) {
    tokio::spawn(async move {
        let _ = engine.send(Op::Shutdown).await;
    });
}

/// Bound the tracked-session count: evict the oldest beyond
/// `MAX_ACP_SESSIONS` (aborting its pump and shutting its engine down).
fn evict_oldest_if_full(sessions: &mut HashMap<String, AcpSession>) {
    if sessions.len() >= MAX_ACP_SESSIONS
        && let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, session)| session.created_at)
            .map(|(id, _)| id.clone())
        && let Some(session) = sessions.remove(&oldest)
    {
        release(session);
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
/// and mode; otherwise the record is created with the engine's prompt and
/// the `acp` mode.
fn persist_turn(
    store: &FileSessionStore,
    session_id: &str,
    workspace: &Path,
    model: &str,
    messages: &[Message],
    turn_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
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
                system_prompt,
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

/// A `sessionUpdate` carrying one text chunk.
fn chunk_update(kind: &str, text: String) -> Value {
    json!({
        "sessionUpdate": kind,
        "content": { "type": "text", "text": text }
    })
}

/// A `tool_call_update` with one text content block.
fn tool_call_update(tool_call_id: &str, status: &str, text: &str) -> Value {
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": tool_call_id,
        "status": status,
        "content": [ { "type": "content", "content": { "type": "text", "text": text } } ]
    })
}

/// The ACP activity kind for a CodeSmith tool name.
fn tool_kind(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| name.contains(n));
    if has(&["edit", "write", "patch", "create", "delete", "remove"]) {
        "edit"
    } else if has(&["shell", "exec", "command", "run_"]) {
        "execute"
    } else if has(&["fetch", "web", "http"]) {
        // Before the read list: `web_search` is a fetch, not a read.
        "fetch"
    } else if has(&["read", "grep", "glob", "search", "list", "find", "tree"]) {
        "read"
    } else {
        "other"
    }
}

/// A one-line title for a `tool_call`: the tool name plus a capped argument
/// summary.
fn tool_title(name: &str, input: &Value) -> String {
    let args = input.to_string();
    if args.is_empty() || args == "{}" {
        return name.to_string();
    }
    let capped: String = args.chars().take(120).collect();
    format!("{name} {capped}")
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

async fn write_session_update<W>(writer: &mut W, session_id: &str, update: Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": update
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
        AcpServer::new(
            Config::default(),
            "test-model".to_string(),
            store,
            Settings::default(),
        )
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
        lock_sessions(&server.shared.sessions).insert(
            id.to_string(),
            AcpSession {
                created_at: Instant::now(),
                generation: 0,
                workspace: workspace.to_path_buf(),
                history: Arc::new(Vec::new()),
                system_prompt: None,
                engine: None,
                pump_task: None,
                pending_prompt: None,
                approved_keys: HashSet::new(),
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

    #[test]
    fn tool_calls_map_to_acp_kinds() {
        assert_eq!(tool_kind("edit_file"), "edit");
        assert_eq!(tool_kind("write_file"), "edit");
        assert_eq!(tool_kind("exec_shell"), "execute");
        assert_eq!(tool_kind("read_file"), "read");
        assert_eq!(tool_kind("grep"), "read");
        assert_eq!(tool_kind("web_search"), "fetch");
        assert_eq!(tool_kind("agent_open"), "other");

        let title = tool_title("read_file", &serde_json::json!({"path": "src/lib.rs"}));
        assert!(title.starts_with("read_file "));
        assert!(title.contains("src/lib.rs"));
        assert_eq!(tool_title("plan", &serde_json::json!({})), "plan");
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
        {
            let sessions = lock_sessions(&server.shared.sessions);
            assert_eq!(sessions[&id].workspace, cwd);
            assert!(sessions[&id].engine.is_none(), "engines are lazy");
        }

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
        persist_turn(
            &store,
            "acp-1",
            &cwd,
            "test-model",
            &first_turn,
            42,
            Some(&SystemPrompt::Text("acp prompt".to_string())),
        )
        .unwrap();

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
        persist_turn(&store, "acp-1", &cwd, "test-model", &second_turn, 8, None).unwrap();
        let saved = store.load_session("acp-1").unwrap();
        assert_eq!(saved.metadata.total_tokens, 50);
        assert_eq!(saved.metadata.message_count, 4);
        assert_eq!(saved.metadata.mode.as_deref(), Some("acp"));
        assert_eq!(saved.system_prompt.as_deref(), Some("acp prompt"));
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
        persist_turn(&store, "repl-1", &cwd, "test-model", &next_turn, 5, None).unwrap();

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
            let sessions = lock_sessions(&server.shared.sessions);
            let session = &sessions["sess-replay"];
            assert_eq!(
                session.history.len(),
                5,
                "tool turns stay in the history the engine syncs in"
            );
            assert_eq!(session.workspace, cwd);
            assert!(
                session.engine.is_none(),
                "the engine is created at the next prompt"
            );
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
    async fn a_second_prompt_while_one_is_in_flight_is_refused() {
        let (tmp, store) = temp_store();
        let mut server = server_over(store);
        let cwd = tmp.path().join("proj");
        open_session(&mut server, "acp-busy", &cwd);

        // Reserve the prompt slot directly, as spawn_prompt does; no engine
        // is needed to test the one-prompt-per-session rule.
        let (tx, _rx) = oneshot::channel();
        lock_sessions(&server.shared.sessions)
            .get_mut("acp-busy")
            .expect("session opened")
            .pending_prompt = Some(tx);

        let err = server
            .handle_request(
                "session/prompt",
                json!({ "sessionId": "acp-busy", "prompt": "again" }),
                Some(json!(1)),
                sink(),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, -32602);
        assert!(err.message.contains("in flight"), "{}", err.message);
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
            None,
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
        assert!(lock_sessions(&server.shared.sessions).is_empty());
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
            chunk_update("agent_message_chunk", "hello\nworld".to_string()),
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

    /// Drain what the sink writer has collected so far.
    async fn take_lines(writer: &SharedWriter<Vec<u8>>) -> Vec<Value> {
        let mut bytes = writer.lock().await;
        let text = String::from_utf8(std::mem::take(&mut *bytes)).expect("utf8");
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each line is json"))
            .collect()
    }

    #[test]
    fn permission_responses_map_fail_closed() {
        let allow =
            json!({"result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}}});
        assert_eq!(permission_from_response(&allow), Permission::Allow);
        let always =
            json!({"result": {"outcome": {"outcome": "selected", "optionId": "allow_always"}}});
        assert_eq!(permission_from_response(&always), Permission::AllowAlways);
        for refused in [
            json!({"result": {"outcome": {"outcome": "cancelled"}}}),
            json!({"result": {"outcome": {"outcome": "selected", "optionId": "mystery"}}}),
            json!({"error": {"code": -1, "message": "user dismissed"}}),
            json!({"result": {}}),
        ] {
            assert_eq!(
                permission_from_response(&refused),
                Permission::Deny,
                "{refused}"
            );
        }
    }

    #[test]
    fn a_client_response_reaches_its_waiter() {
        let shared = AcpShared::default();
        let (tx, mut rx) = oneshot::channel();
        lock_waiters(&shared.permission_waiters).insert("perm-7".to_string(), tx);

        let response = json!({"jsonrpc": "2.0", "id": "perm-7", "result": {"outcome": {}}});
        assert!(route_client_response(&shared, &response));
        assert_eq!(
            rx.try_recv().expect("the response was delivered"),
            response,
            "the waiter sees the whole response object"
        );
        assert!(
            !route_client_response(&shared, &response),
            "the waiter is gone once answered"
        );
        assert!(
            !route_client_response(&shared, &json!({"result": {}})),
            "no id"
        );
    }

    /// The permission round-trip without an engine: the request goes out on
    /// the wire, the client's response comes back through the read loop's
    /// router, and a client that never answers is refused at the deadline.
    #[tokio::test]
    async fn ask_permission_round_trips_and_times_out() {
        let shared = AcpShared::default();
        let writer = sink();
        let tool_call = json!({"toolCallId": "call-1", "title": "write_file", "kind": "edit"});

        let ask = tokio::spawn({
            let shared = shared.clone();
            let writer = writer.clone();
            let tool_call = tool_call.clone();
            async move {
                ask_permission_within(
                    &shared,
                    &writer,
                    "sess-1",
                    tool_call,
                    std::time::Duration::from_secs(5),
                )
                .await
            }
        });

        // Read the request the server wrote, answer it as the client would.
        let request = loop {
            let lines = take_lines(&writer).await;
            if let Some(line) = lines.into_iter().find(|line| {
                line.get("method").and_then(Value::as_str) == Some("session/request_permission")
            }) {
                break line;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert_eq!(request["params"]["sessionId"], "sess-1");
        assert_eq!(request["params"]["toolCall"], tool_call);
        assert!(
            !request["params"]["options"]
                .as_array()
                .expect("options offered")
                .is_empty()
        );
        let answer = json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}}
        });
        assert!(route_client_response(&shared, &answer));
        assert_eq!(ask.await.expect("task"), Permission::Allow);

        // No answer inside the deadline: refused.
        let denied = ask_permission_within(
            &shared,
            &sink(),
            "sess-1",
            tool_call,
            std::time::Duration::from_millis(10),
        )
        .await;
        assert_eq!(denied, Permission::Deny);
    }
}
