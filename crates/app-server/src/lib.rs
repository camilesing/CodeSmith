use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codesmith_agent::ModelRegistry;
use codesmith_config::{CliRuntimeOverrides, ConfigStore};
use codesmith_core::Runtime;
use codesmith_execpolicy::ExecPolicyEngine;
use codesmith_hooks::{HookDispatcher, JsonlHookSink, StdoutHookSink, UnixSocketHookSink};
use codesmith_mcp::McpManager;
use codesmith_protocol::{
    AppRequest, AppResponse, PromptRequest, PromptResponse, ThreadRequest, ThreadResponse,
};
use codesmith_state::StateStore;
use codesmith_tools::ToolCall;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

const DEFAULT_CORS_ORIGINS: &[&str] = &[
    "http://localhost",
    "http://localhost:1420",
    "http://localhost:3000",
    "http://localhost:5173",
    "http://127.0.0.1",
    "http://127.0.0.1:1420",
    "tauri://localhost",
];

#[derive(Clone)]
pub struct AppServerOptions {
    pub listen: SocketAddr,
    pub config_path: Option<PathBuf>,
    pub auth_token: Option<String>,
    pub insecure_no_auth: bool,
    pub cors_origins: Vec<String>,
}

impl std::fmt::Debug for AppServerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppServerOptions")
            .field("listen", &self.listen)
            .field("config_path", &self.config_path)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("insecure_no_auth", &self.insecure_no_auth)
            .field("cors_origins", &self.cors_origins)
            .finish()
    }
}

#[derive(Clone)]
struct AppState {
    config_path: Option<PathBuf>,
    config: Arc<RwLock<codesmith_config::ConfigToml>>,
    /// Shared runtime. Every handler-reachable `Runtime` method takes `&self`
    /// (the running-thread table and its state store are behind an internal
    /// mutex), so handlers run concurrently with no process-global lock:
    /// hook emission, tool dispatch, MCP startup, and job reads overlap, and
    /// only thread-table mutations serialize against each other. The job
    /// mutators take `&mut self` and are not reachable through this `Arc`.
    runtime: Arc<Runtime>,
    registry: ModelRegistry,
    auth_token: Option<String>,
    /// Serializes the config read-modify-write (memory snapshot + disk
    /// persist) so two concurrent ConfigSet/ConfigUnset requests cannot
    /// lose each other's key: the load→apply→save in `persist_config`
    /// is otherwise last-writer-wins.
    config_write: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCallRequest {
    call: ToolCall,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

#[derive(Debug)]
struct StdioDispatchResult {
    result: Value,
    should_exit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppTransport {
    Http,
    Stdio,
}

#[derive(Debug, Deserialize)]
struct ConfigGetParams {
    key: String,
}

#[derive(Debug, Deserialize)]
struct ConfigSetParams {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct ThreadIdParams {
    thread_id: String,
}

#[derive(Debug, Deserialize)]
struct ThreadMessageParams {
    thread_id: String,
    input: String,
}

pub async fn run(options: AppServerOptions) -> Result<()> {
    let auth_token = resolve_auth_token(&options)?;
    let state = build_state(options.config_path.clone(), auth_token, AppTransport::Http)?;
    let app = app_router(state, &options.cors_origins);

    let listener = tokio::net::TcpListener::bind(options.listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn app_router(state: AppState, cors_origins: &[String]) -> Router {
    let protected_routes = Router::new()
        .route("/thread", post(thread_handler))
        .route("/app", post(app_handler))
        .route("/prompt", post(prompt_handler))
        .route("/tool", post(tool_handler))
        .route("/jobs", get(jobs_handler))
        .route("/mcp/startup", post(mcp_startup_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_app_server_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(protected_routes)
        .layer(cors_layer(cors_origins))
        .with_state(state)
}

pub async fn run_stdio(config_path: Option<PathBuf>) -> Result<()> {
    let state = build_state(config_path, None, AppTransport::Stdio)?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = tokio::io::BufWriter::new(stdout);
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let response = jsonrpc_error(
                    None,
                    JsonRpcError::parse_error(format!("invalid json: {err}")),
                );
                writer.write_all(response.to_string().as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                continue;
            }
        };

        if request
            .jsonrpc
            .as_deref()
            .is_some_and(|version| version != "2.0")
        {
            let response = jsonrpc_error(
                request.id,
                JsonRpcError::invalid_request("jsonrpc version must be 2.0"),
            );
            writer.write_all(response.to_string().as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            continue;
        }

        let response = match dispatch_stdio_request(&state, &request.method, request.params).await {
            Ok(dispatch) => {
                let encoded = jsonrpc_result(request.id, dispatch.result);
                writer.write_all(encoded.to_string().as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                if dispatch.should_exit {
                    break;
                }
                continue;
            }
            Err(err) => jsonrpc_error(request.id, err),
        };

        writer.write_all(response.to_string().as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }

    Ok(())
}

async fn healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "protocol": "v2",
        "service": "codesmith-app-server"
    }))
}

async fn thread_handler(
    State(state): State<AppState>,
    Json(req): Json<ThreadRequest>,
) -> axum::response::Response {
    match state.runtime.handle_thread(req).await {
        Ok(res) => Json(res).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn prompt_handler(
    State(state): State<AppState>,
    Json(req): Json<PromptRequest>,
) -> axum::response::Response {
    let overrides = CliRuntimeOverrides::default();
    match state.runtime.handle_prompt(req, &overrides).await {
        Ok(res) => Json(res).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn tool_handler(
    State(state): State<AppState>,
    Json(req): Json<ToolCallRequest>,
) -> axum::response::Response {
    // No global lock: tool execution can run for minutes and must not
    // block (or be blocked by) any other handler.
    let cwd = req
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // Policy-gate posture: the endpoint never executes a tool (no
    // executor in this runtime), so it cannot mutate anything — the
    // caller-controlled `cwd` only feeds policy evaluation, behind the
    // token gate.
    match state
        .runtime
        .invoke_tool(
            req.call,
            codesmith_execpolicy::AskForApproval::OnRequest,
            &cwd,
        )
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn jobs_handler(State(state): State<AppState>) -> Json<AppResponse> {
    Json(state.runtime.app_status())
}

async fn mcp_startup_handler(State(state): State<AppState>) -> Json<Value> {
    let summary = state.runtime.mcp_startup().await;
    Json(json!({
        "ok": true,
        "summary": summary
    }))
}

async fn app_handler(
    State(state): State<AppState>,
    Json(req): Json<AppRequest>,
) -> Json<AppResponse> {
    Json(process_app_request(&state, req, AppTransport::Http).await)
}

/// Hook sinks for the app server. [`StdoutHookSink`] mirrors every event
/// onto stdout, which is the JSON-RPC stream itself in stdio mode — the
/// protocol reply would be preceded by non-JSON-RPC lines — so it is
/// registered for the HTTP transport only.
fn build_hooks(
    hook_log_path: PathBuf,
    unix_socket_path: Option<PathBuf>,
    transport: AppTransport,
) -> HookDispatcher {
    let mut hooks = HookDispatcher::default();
    if transport == AppTransport::Http {
        hooks.add_sink(Arc::new(StdoutHookSink));
    }
    hooks.add_sink(Arc::new(JsonlHookSink::new(hook_log_path)));
    if let Some(socket_path) = unix_socket_path {
        hooks.add_sink(Arc::new(UnixSocketHookSink::new(socket_path)));
    }
    hooks
}

fn build_state(
    config_path: Option<PathBuf>,
    auth_token: Option<String>,
    transport: AppTransport,
) -> Result<AppState> {
    let store = ConfigStore::load(config_path.clone())?;
    let config = store.config.clone();
    let registry = ModelRegistry::default();

    let state_db_path = config_path
        .as_ref()
        .and_then(|p| p.parent().map(|parent| parent.join("state.db")));
    let state_store = StateStore::open(state_db_path)?;

    let hook_log_path = config_path
        .as_ref()
        .and_then(|p| p.parent().map(|parent| parent.join("events.jsonl")))
        .unwrap_or_else(|| PathBuf::from(".codesmith/events.jsonl"));
    let unix_socket_path = config
        .hook_sinks
        .as_ref()
        .and_then(|sinks| sinks.unix_socket_path.as_ref())
        .filter(|path| !path.as_os_str().is_empty())
        .cloned();
    let hooks = build_hooks(hook_log_path, unix_socket_path, transport);

    let runtime = Runtime::new(
        config.clone(),
        registry.clone(),
        state_store,
        Arc::new(McpManager::default()),
        ExecPolicyEngine::new(Vec::new(), default_denied_exec_prefixes()),
        hooks,
    );

    Ok(AppState {
        config_path,
        config: Arc::new(RwLock::new(config)),
        runtime: Arc::new(runtime),
        registry,
        auth_token,
        config_write: Arc::new(tokio::sync::Mutex::new(())),
    })
}

fn resolve_auth_token(options: &AppServerOptions) -> Result<Option<String>> {
    let configured = options.auth_token.as_ref().map(|token| token.trim());
    if let Some(token) = configured
        && token.is_empty()
    {
        bail!("app-server auth token cannot be empty");
    }

    if options.insecure_no_auth {
        if !options.listen.ip().is_loopback() {
            bail!("refusing unauthenticated app-server bind on non-loopback address");
        }
        eprintln!("warning: app-server HTTP auth disabled by --insecure-no-auth");
        return Ok(None);
    }

    let token = configured
        .map(str::to_string)
        .unwrap_or_else(|| format!("cwapp_{}", Uuid::new_v4().simple()));
    if options.auth_token.is_some() {
        eprintln!("app-server auth: bearer token required for HTTP routes.");
    } else {
        // The generated token never goes to stderr: service managers and
        // containers persist stderr verbatim, which outlives the process.
        // It is written to a 0600 file instead; failing to persist it is a
        // hard error (the operator would have no way to learn it otherwise
        // — pass --auth-token or CODESMITH_APP_SERVER_TOKEN to choose one).
        let token_path = write_generated_token_file(&token)?;
        eprintln!("app-server auth: generated bearer token for this process.");
        eprintln!(
            "  Token written to {} (mode 0600); use it as `Authorization: Bearer <token>`.",
            token_path.display()
        );
        eprintln!("  Pass --auth-token or set CODESMITH_APP_SERVER_TOKEN for a stable token.");
    }
    Ok(Some(token))
}

/// Persist a generated app-server token to `~/.codesmith/app-server.token`
/// (0600, owner-only) and return the path.
fn write_generated_token_file(token: &str) -> Result<PathBuf> {
    let home = codesmith_config::codesmith_home()
        .map_err(|err| anyhow::anyhow!("cannot locate codesmith home for token file: {err}"))?;
    std::fs::create_dir_all(&home)
        .map_err(|err| anyhow::anyhow!("cannot create {}: {err}", home.display()))?;
    let path = home.join("app-server.token");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| anyhow::anyhow!("cannot create {}: {err}", path.display()))?;
        // Newline makes `cat`/`$(<file)` reads convenient; consumers trim.
        file.write_all(token.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|err| anyhow::anyhow!("cannot write {}: {err}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, format!("{token}\n"))
            .map_err(|err| anyhow::anyhow!("cannot write {}: {err}", path.display()))?;
    }
    Ok(path)
}

fn cors_layer(extra_origins: &[String]) -> CorsLayer {
    let mut origins: Vec<HeaderValue> = DEFAULT_CORS_ORIGINS
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();
    for raw in extra_origins {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match HeaderValue::from_str(trimmed) {
            Ok(value) if !origins.contains(&value) => origins.push(value),
            Ok(_) => {}
            Err(err) => {
                eprintln!("warning: ignoring invalid app-server CORS origin `{trimmed}`: {err}")
            }
        }
    }

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

async fn require_app_server_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.auth_token.as_deref() else {
        return next.run(req).await;
    };
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()));

    if authorized {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "app-server bearer token required",
                    "status": StatusCode::UNAUTHORIZED.as_u16(),
                }
            })),
        )
            .into_response()
    }
}

fn params_or_object(params: Value) -> Value {
    if params.is_null() { json!({}) } else { params }
}

/// Length-checked constant-time byte comparison for bearer tokens — avoids
/// the timing side channel of a plain `==` string compare. The length
/// mismatch early-return only leaks the token's length, which is not a
/// secret worth protecting for a locally generated `cwapp_<uuid>`.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn parse_params<T: DeserializeOwned>(params: Value) -> std::result::Result<T, JsonRpcError> {
    serde_json::from_value(params).map_err(|err| JsonRpcError::invalid_params(err.to_string()))
}

fn jsonrpc_result(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    })
}

fn jsonrpc_error(id: Option<Value>, err: JsonRpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": err.code,
            "message": err.message,
            "data": err.data
        }
    })
}

impl JsonRpcError {
    fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data: None,
        }
    }

    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data: None,
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("unsupported method: {method}"),
            data: None,
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }
}

async fn handle_thread_request(
    state: &AppState,
    req: ThreadRequest,
) -> std::result::Result<ThreadResponse, JsonRpcError> {
    state
        .runtime
        .handle_thread(req)
        .await
        .map_err(|err| JsonRpcError::internal(err.to_string()))
}

async fn handle_prompt_request(
    state: &AppState,
    req: PromptRequest,
) -> std::result::Result<PromptResponse, JsonRpcError> {
    state
        .runtime
        .handle_prompt(req, &CliRuntimeOverrides::default())
        .await
        .map_err(|err| JsonRpcError::internal(err.to_string()))
}

async fn dispatch_stdio_request(
    state: &AppState,
    method: &str,
    params: Value,
) -> std::result::Result<StdioDispatchResult, JsonRpcError> {
    let outcome = match method {
        "healthz" | "app/healthz" => StdioDispatchResult {
            result: json!({
                "status": "ok",
                "service": "codesmith-app-server",
                "transport": "stdio"
            }),
            should_exit: false,
        },
        "capabilities" => StdioDispatchResult {
            result: json!({
                "transport": "stdio",
                "families": ["thread/*", "app/*", "prompt/*"],
                "methods": [
                    "healthz",
                    "thread/capabilities",
                    "thread/request",
                    "thread/create",
                    "thread/start",
                    "thread/resume",
                    "thread/fork",
                    "thread/list",
                    "thread/read",
                    "thread/set_name",
                    "thread/archive",
                    "thread/unarchive",
                    "thread/message",
                    "app/capabilities",
                    "app/request",
                    "app/config/get",
                    "app/config/set",
                    "app/config/unset",
                    "app/config/list",
                    "app/models",
                    "app/thread_loaded_list",
                    "prompt/capabilities",
                    "prompt/request",
                    "prompt/run",
                    "shutdown"
                ]
            }),
            should_exit: false,
        },
        "thread/capabilities" => StdioDispatchResult {
            result: json!({
                "methods": [
                    "thread/request",
                    "thread/create",
                    "thread/start",
                    "thread/resume",
                    "thread/fork",
                    "thread/list",
                    "thread/read",
                    "thread/set_name",
                    "thread/archive",
                    "thread/unarchive",
                    "thread/message"
                ]
            }),
            should_exit: false,
        },
        "thread/request" => {
            let request: ThreadRequest = parse_params(params)?;
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/create" => {
            #[derive(Debug, Deserialize)]
            struct CreateParams {
                #[serde(default)]
                metadata: Value,
            }
            let parsed: CreateParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Create {
                    metadata: parsed.metadata,
                },
            )
            .await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/start" => {
            let request = ThreadRequest::Start(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/resume" => {
            let request = ThreadRequest::Resume(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/fork" => {
            let request = ThreadRequest::Fork(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/list" => {
            let request = ThreadRequest::List(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/read" => {
            let request = ThreadRequest::Read(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/set_name" | "thread/set-name" => {
            let request = ThreadRequest::SetName(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/archive" => {
            let parsed: ThreadIdParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Archive {
                    thread_id: parsed.thread_id,
                },
            )
            .await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/unarchive" => {
            let parsed: ThreadIdParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Unarchive {
                    thread_id: parsed.thread_id,
                },
            )
            .await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/message" => {
            let parsed: ThreadMessageParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Message {
                    thread_id: parsed.thread_id,
                    input: parsed.input,
                },
            )
            .await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/capabilities" => {
            let response =
                process_app_request(state, AppRequest::Capabilities, AppTransport::Stdio).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/request" => {
            let request: AppRequest = parse_params(params)?;
            let response = process_app_request(state, request, AppTransport::Stdio).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/config/get" => {
            let parsed: ConfigGetParams = parse_params(params_or_object(params))?;
            let response = process_app_request(
                state,
                AppRequest::ConfigGet { key: parsed.key },
                AppTransport::Stdio,
            )
            .await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/config/set" => {
            let parsed: ConfigSetParams = parse_params(params_or_object(params))?;
            let response = process_app_request(
                state,
                AppRequest::ConfigSet {
                    key: parsed.key,
                    value: parsed.value,
                },
                AppTransport::Stdio,
            )
            .await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/config/unset" => {
            let parsed: ConfigGetParams = parse_params(params_or_object(params))?;
            let response = process_app_request(
                state,
                AppRequest::ConfigUnset { key: parsed.key },
                AppTransport::Stdio,
            )
            .await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/config/list" => {
            let response =
                process_app_request(state, AppRequest::ConfigList, AppTransport::Stdio).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/models" => {
            let response =
                process_app_request(state, AppRequest::Models, AppTransport::Stdio).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "app/thread_loaded_list" | "app/thread-loaded-list" => {
            let response =
                process_app_request(state, AppRequest::ThreadLoadedList, AppTransport::Stdio).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "prompt/capabilities" => StdioDispatchResult {
            result: json!({
                "methods": ["prompt/request", "prompt/run"]
            }),
            should_exit: false,
        },
        "prompt/request" | "prompt/run" => {
            let request: PromptRequest = parse_params(params)?;
            let response = handle_prompt_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "shutdown" => StdioDispatchResult {
            result: json!({"ok": true, "status": "stopped"}),
            should_exit: true,
        },
        _ => return Err(JsonRpcError::method_not_found(method)),
    };
    Ok(outcome)
}

async fn process_app_request(
    state: &AppState,
    req: AppRequest,
    transport: AppTransport,
) -> AppResponse {
    match req {
        AppRequest::Capabilities => AppResponse {
            ok: true,
            data: json!({
                "routes": ["/thread", "/app", "/prompt", "/tool", "/jobs", "/mcp/startup"],
                "config": ["get", "set", "unset", "list"],
                "events": ["response_start", "response_delta", "response_end", "tool_call_start", "tool_call_result", "mcp_startup_update", "mcp_startup_complete"],
                "transport": "stdio+http",
                "config_path": state.config_path.as_ref().map(|p| p.display().to_string()),
            }),
            events: Vec::new(),
        },
        AppRequest::ConfigGet { key } => {
            let cfg = state.config.read().await;
            let value = match transport {
                AppTransport::Http => cfg.get_display_value(&key),
                AppTransport::Stdio => cfg.get_value(&key),
            };
            AppResponse {
                ok: true,
                data: json!({ "key": key, "value": value }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigSet { key, value } => {
            // The whole read-modify-write (disk + memory) runs under the
            // config-write mutex, and the in-memory snapshot is DERIVED
            // from disk: mutating memory first meant a failed persist
            // left ConfigGet reporting a value that was never written,
            // and concurrent requests lost each other's keys.
            let _guard = state.config_write.lock().await;
            let persist_error = persist_config(
                state,
                PersistOp::Set {
                    key: &key,
                    value: &value,
                },
            )
            .await
            .err()
            .map(|e| e.to_string());
            let resync_error = reload_config_snapshot(state)
                .await
                .err()
                .map(|e| format!("config re-sync failed: {e}"));
            let persist_error = persist_error.or(resync_error);
            AppResponse {
                ok: persist_error.is_none(),
                data: json!({
                    "key": key,
                    "value": value,
                    "persist_error": persist_error,
                }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigUnset { key } => {
            let _guard = state.config_write.lock().await;
            let persist_error = persist_config(state, PersistOp::Unset { key: &key })
                .await
                .err()
                .map(|e| e.to_string());
            let resync_error = reload_config_snapshot(state)
                .await
                .err()
                .map(|e| format!("config re-sync failed: {e}"));
            let persist_error = persist_error.or(resync_error);
            AppResponse {
                ok: persist_error.is_none(),
                data: json!({ "key": key, "persist_error": persist_error }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigList => {
            let cfg = state.config.read().await;
            AppResponse {
                ok: true,
                data: json!({ "values": cfg.list_values() }),
                events: Vec::new(),
            }
        }
        AppRequest::Models => AppResponse {
            ok: true,
            data: json!({ "models": state.registry.list() }),
            events: Vec::new(),
        },
        AppRequest::ThreadLoadedList => {
            let response = state
                .runtime
                .handle_thread(codesmith_protocol::ThreadRequest::List(
                    codesmith_protocol::ThreadListParams {
                        include_archived: false,
                        limit: Some(50),
                    },
                ))
                .await;
            match response {
                Ok(thread_resp) => AppResponse {
                    ok: true,
                    data: json!({ "threads": thread_resp.threads }),
                    events: thread_resp.events,
                },
                Err(err) => AppResponse {
                    ok: false,
                    data: json!({ "error": err.to_string() }),
                    events: Vec::new(),
                },
            }
        }
    }
}

/// A single config change to persist — the unit of edit applied to a
/// freshly-loaded store so concurrent external edits survive.
enum PersistOp<'a> {
    Set { key: &'a str, value: &'a str },
    Unset { key: &'a str },
}

async fn persist_config(state: &AppState, op: PersistOp<'_>) -> Result<()> {
    // `None` resolves to the default config file, exactly as it does in
    // `build_state` and `reload_config_snapshot`. An early return on `None`
    // made `app/config/set` answer `ok` in the default launch (no
    // `--config`) without writing anything.
    let path = state.config_path.clone();
    let (key, value) = match op {
        PersistOp::Set { key, value } => (key.to_string(), Some(value.to_string())),
        PersistOp::Unset { key } => (key.to_string(), None),
    };
    // `ConfigStore` does blocking file IO, so it runs on the blocking pool.
    tokio::task::spawn_blocking(move || -> Result<()> {
        // Re-read from disk: applying only this request's key change to the
        // current on-disk state preserves any external edits made since this
        // process started (a whole-snapshot overwrite would discard them).
        let mut store = ConfigStore::load(path)?;
        match value {
            Some(value) => store.config.set_value(&key, &value)?,
            None => store.config.unset_value(&key)?,
        }
        store.save()
    })
    .await?
}

/// Re-derive the in-memory config snapshot from disk. Called after every
/// persisted change (absorbing external edits) and after a failed persist
/// (rolling back whatever the caller speculatively applied).
async fn reload_config_snapshot(state: &AppState) -> Result<()> {
    let path = state.config_path.clone();
    let config =
        tokio::task::spawn_blocking(move || ConfigStore::load(path).map(|store| store.config))
            .await??;
    *state.config.write().await = config;
    Ok(())
}

/// Denied exec prefixes for the app-server tool channel. The channel has no
/// approval path (every untrusted call already short-circuits to
/// `approval_required`), so this list only upgrades clearly destructive
/// commands to an explicit `forbidden` verdict instead of an approval ask.
fn default_denied_exec_prefixes() -> Vec<String> {
    [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf ~",
        "rm -rf $HOME",
        "sudo rm",
        "mkfs",
        "dd if=/dev/zero of=/dev/",
        "chmod -R 777 /",
        "shred /dev/",
        "> /dev/sda",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use codesmith_protocol::{AppRequest, LocalShellParams, ToolPayload};
    use codesmith_tools::ToolCallSource;
    use std::fs;
    use tower::ServiceExt;

    fn app_with_config(auth_token: Option<&str>) -> (Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "api_key = \"sk-codesmith-test-secret\"\n").expect("write config");
        let state = build_state(
            Some(config_path),
            auth_token.map(std::string::ToString::to_string),
            AppTransport::Http,
        )
        .expect("state");
        (app_router(state, &[]), tmp)
    }

    async fn response_body_json(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        serde_json::from_slice(&bytes).expect("json response")
    }

    #[tokio::test]
    async fn http_app_routes_require_bearer_token_when_auth_enabled() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/app")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&AppRequest::ConfigGet {
                            key: "api_key".to_string(),
                        })
                        .expect("request json"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn http_config_get_redacts_sensitive_values_after_auth() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/app")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&AppRequest::ConfigGet {
                            key: "api_key".to_string(),
                        })
                        .expect("request json"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body_json(response).await;
        assert_eq!(body["data"]["value"], "sk-c***cret");
    }

    #[tokio::test]
    async fn cors_does_not_allow_arbitrary_origins() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/healthz")
                    .header(header::ORIGIN, "https://attacker.example")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    #[test]
    fn non_loopback_bind_without_auth_fails_fast() {
        let options = AppServerOptions {
            listen: "0.0.0.0:8787".parse().expect("socket addr"),
            config_path: None,
            auth_token: None,
            insecure_no_auth: true,
            cors_origins: Vec::new(),
        };

        let err = resolve_auth_token(&options).expect_err("non-loopback unauth should fail");
        assert!(
            err.to_string()
                .contains("refusing unauthenticated app-server bind")
        );
    }

    #[tokio::test]
    async fn stdio_transport_keeps_raw_config_get_for_legacy_clients() {
        let (state, _tmp) = state_with_config();
        {
            let mut cfg = state.config.write().await;
            cfg.api_key = Some("sk-codesmith-test-secret".to_string());
        }

        let response = process_app_request(
            &state,
            AppRequest::ConfigGet {
                key: "api_key".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;

        assert_eq!(response.data["value"], "sk-codesmith-test-secret");
    }

    // ── resolve_auth_token ─────────────────────────────────────────────

    #[test]
    fn auth_token_empty_string_fails() {
        let options = AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: Some("  ".to_string()),
            insecure_no_auth: false,
            cors_origins: Vec::new(),
        };
        let err = resolve_auth_token(&options).expect_err("empty token should fail");
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn auth_token_explicit_is_preserved() {
        let options = AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: Some("my-secret".to_string()),
            insecure_no_auth: false,
            cors_origins: Vec::new(),
        };
        let token = resolve_auth_token(&options).unwrap();
        assert_eq!(token.as_deref(), Some("my-secret"));
    }

    #[test]
    fn insecure_no_auth_on_loopback_returns_none() {
        let options = AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: None,
            insecure_no_auth: true,
            cors_origins: Vec::new(),
        };
        let token = resolve_auth_token(&options).unwrap();
        assert!(token.is_none());
    }

    // ── cors_layer ─────────────────────────────────────────────────────

    /// Send a GET carrying `Origin` through a router with `layer` and return
    /// the `Access-Control-Allow-Origin` header it is answered with, if any.
    /// `CorsLayer` has no reader for the origins it was built with, so the
    /// wire is the only honest place to look.
    async fn allow_origin_for(layer: CorsLayer, origin: &str) -> Option<String> {
        let app = Router::new().route("/healthz", get(healthz)).layer(layer);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/healthz")
                    .header(header::ORIGIN, origin)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    #[tokio::test]
    async fn cors_layer_includes_default_origins() {
        for origin in ["http://localhost:5173", "tauri://localhost"] {
            assert_eq!(
                allow_origin_for(cors_layer(&[]), origin).await.as_deref(),
                Some(origin),
                "a default origin must be allowed"
            );
        }
    }

    #[tokio::test]
    async fn cors_layer_adds_extra_origins() {
        let extras = vec!["https://example.com".to_string()];
        // The grant comes from the extra entry: without it the same origin is
        // refused, and the defaults keep their grants alongside it.
        assert_eq!(
            allow_origin_for(cors_layer(&extras), "https://example.com")
                .await
                .as_deref(),
            Some("https://example.com")
        );
        assert!(
            allow_origin_for(cors_layer(&[]), "https://example.com")
                .await
                .is_none()
        );
        assert_eq!(
            allow_origin_for(cors_layer(&extras), "http://localhost:5173")
                .await
                .as_deref(),
            Some("http://localhost:5173")
        );
    }

    // ── JsonRpc helpers ────────────────────────────────────────────────

    #[test]
    fn params_or_object_returns_object_for_null() {
        let result = params_or_object(Value::Null);
        assert_eq!(result, json!({}));
    }

    #[test]
    fn params_or_object_passthrough_for_non_null() {
        let input = json!({"key": "value"});
        let result = params_or_object(input.clone());
        assert_eq!(result, input);
    }

    #[test]
    fn jsonrpc_result_format() {
        let result = jsonrpc_result(Some(json!(1)), json!({"ok": true}));
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], 1);
        assert_eq!(result["result"]["ok"], true);
    }

    #[test]
    fn jsonrpc_result_null_id() {
        let result = jsonrpc_result(None, json!(null));
        assert_eq!(result["id"], Value::Null);
    }

    #[test]
    fn jsonrpc_error_format() {
        let err = jsonrpc_error(Some(json!(2)), JsonRpcError::internal("oops"));
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], 2);
        assert_eq!(err["error"]["code"], -32603);
        assert_eq!(err["error"]["message"], "oops");
    }

    #[test]
    fn jsonrpc_error_codes() {
        assert_eq!(JsonRpcError::parse_error("").code, -32700);
        assert_eq!(JsonRpcError::invalid_request("").code, -32600);
        assert_eq!(JsonRpcError::method_not_found("x").code, -32601);
        assert_eq!(JsonRpcError::invalid_params("").code, -32602);
        assert_eq!(JsonRpcError::internal("").code, -32603);
    }

    // ── AppServerOptions ───────────────────────────────────────────────

    #[test]
    fn app_server_options_debug_does_not_leak_token() {
        let options = AppServerOptions {
            listen: "127.0.0.1:8080".parse().expect("addr"),
            config_path: None,
            auth_token: Some("secret-token".to_string()),
            insecure_no_auth: false,
            cors_origins: vec!["https://example.com".to_string()],
        };
        let debug = format!("{options:?}");
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("8080"));
    }

    // ── Default CORS origins ──────────────────────────────────────────

    #[test]
    fn default_cors_origins_include_common_dev_ports() {
        assert!(DEFAULT_CORS_ORIGINS.contains(&"http://localhost:3000"));
        assert!(DEFAULT_CORS_ORIGINS.contains(&"http://localhost:5173"));
        assert!(DEFAULT_CORS_ORIGINS.contains(&"tauri://localhost"));
    }

    // ── Token comparison / config persistence ─────────────────────────

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"cwapp abc", b"cwapp abc"));
        assert!(!constant_time_eq(b"cwapp abc", b"cwapp abd"));
        assert!(!constant_time_eq(b"cwapp abc", b"cwapp ab"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    /// Hook sink that stalls every emit, used to hold a handler open long
    /// enough to observe lock contention (or its absence).
    struct StallingSink(std::time::Duration);

    #[async_trait::async_trait]
    impl codesmith_hooks::HookSink for StallingSink {
        async fn emit(&self, _event: &codesmith_hooks::HookEvent) -> Result<()> {
            tokio::time::sleep(self.0).await;
            Ok(())
        }
    }

    /// Locking regression: a slow mutating handler (`handle_prompt`, write
    /// lock held through its hook emits) must not block the read-lock
    /// handlers (`/jobs`). Before the `Mutex` → `RwLock` split, every
    /// handler serialized behind the single global runtime mutex.
    #[tokio::test]
    async fn read_handlers_are_not_blocked_by_a_slow_write_handler() {
        let (mut state, _tmp) = state_with_config();
        {
            let mut hooks = codesmith_hooks::HookDispatcher::default();
            hooks.add_sink(Arc::new(StallingSink(std::time::Duration::from_millis(
                200,
            ))));
            let runtime = Arc::get_mut(&mut state.runtime)
                .expect("test state must hold the only runtime Arc");
            runtime.hooks = hooks;
        }

        let prompt_state = state.clone();
        let prompt = tokio::spawn(async move {
            prompt_state
                .runtime
                .handle_prompt(
                    codesmith_protocol::PromptRequest {
                        prompt: "slow".to_string(),
                        model: None,
                        thread_id: None,
                    },
                    &CliRuntimeOverrides::default(),
                )
                .await
                .expect("prompt")
        });

        // Give the prompt handler time to grab the write lock first; then a
        // read handler must still complete immediately.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let started = std::time::Instant::now();
        let _ = jobs_handler(State(state)).await;
        let jobs_elapsed = started.elapsed();

        prompt.await.expect("prompt task");
        assert!(
            jobs_elapsed < std::time::Duration::from_millis(200),
            "/jobs blocked behind a slow write handler: {jobs_elapsed:?}"
        );
    }

    #[tokio::test]
    async fn config_set_preserves_external_edits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "api_key = \"sk-a\"\n").expect("write config");
        let state =
            build_state(Some(config_path.clone()), None, AppTransport::Http).expect("state");

        // An external edit lands after this process loaded its config.
        fs::write(
            &config_path,
            "api_key = \"sk-a\"\nexternal_key = \"kept\"\n",
        )
        .expect("external edit");

        let resp = process_app_request(
            &state,
            AppRequest::ConfigSet {
                key: "hook_sinks.unix_socket_path".to_string(),
                value: "/tmp/cw-events.sock".to_string(),
            },
            AppTransport::Http,
        )
        .await;
        assert!(resp.ok, "set should succeed: {:?}", resp.data);

        let raw = fs::read_to_string(&config_path).expect("read back");
        assert!(
            raw.contains("kept"),
            "external edit must survive a config set, got: {raw}"
        );
        assert!(raw.contains("/tmp/cw-events.sock"));
    }

    // ── stdio dispatcher ────────────────────────────────────────────────

    fn state_with_config() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "api_key = \"sk-codesmith-test-secret\"\n").expect("write config");
        let state = build_state(Some(config_path), None, AppTransport::Http).expect("state");
        (state, tmp)
    }

    async fn spawn_thread_via_stdio(state: &AppState) -> String {
        let outcome = dispatch_stdio_request(state, "thread/create", Value::Null)
            .await
            .expect("thread/create dispatches");
        outcome.result["thread_id"]
            .as_str()
            .expect("thread id")
            .to_string()
    }

    #[tokio::test]
    async fn stdio_healthz_reports_ok_without_exiting() {
        let (state, _tmp) = state_with_config();
        for method in ["healthz", "app/healthz"] {
            let outcome = dispatch_stdio_request(&state, method, Value::Null)
                .await
                .expect("healthz dispatches");
            assert!(!outcome.should_exit, "{method}");
            assert_eq!(outcome.result["status"], "ok", "{method}");
            assert_eq!(outcome.result["service"], "codesmith-app-server");
            assert_eq!(outcome.result["transport"], "stdio");
        }
    }

    #[tokio::test]
    async fn stdio_capabilities_catalogs_and_shutdown_exits() {
        let (state, _tmp) = state_with_config();

        let caps = dispatch_stdio_request(&state, "capabilities", Value::Null)
            .await
            .expect("capabilities");
        assert!(!caps.should_exit);
        let methods = caps.result["methods"].as_array().expect("methods");
        assert!(methods.iter().any(|m| m == "shutdown"));
        assert!(methods.iter().any(|m| m == "thread/create"));
        assert!(methods.iter().any(|m| m == "app/config/get"));
        let families = caps.result["families"].as_array().expect("families");
        assert!(families.iter().any(|f| f == "thread/*"));

        let thread_caps = dispatch_stdio_request(&state, "thread/capabilities", Value::Null)
            .await
            .expect("thread capabilities");
        let thread_methods = thread_caps.result["methods"].as_array().expect("methods");
        assert!(thread_methods.iter().any(|m| m == "thread/list"));
        assert!(!thread_methods.iter().any(|m| m == "shutdown"));

        let prompt_caps = dispatch_stdio_request(&state, "prompt/capabilities", Value::Null)
            .await
            .expect("prompt capabilities");
        assert_eq!(
            prompt_caps.result["methods"],
            json!(["prompt/request", "prompt/run"])
        );

        let shutdown = dispatch_stdio_request(&state, "shutdown", Value::Null)
            .await
            .expect("shutdown");
        assert!(shutdown.should_exit);
        assert_eq!(shutdown.result["ok"], true);
        assert_eq!(shutdown.result["status"], "stopped");
    }

    #[tokio::test]
    async fn stdio_unknown_method_reports_method_not_found() {
        let (state, _tmp) = state_with_config();
        let err = dispatch_stdio_request(&state, "no/such-method", Value::Null)
            .await
            .expect_err("unknown method must error");
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("no/such-method"));
    }

    #[tokio::test]
    async fn stdio_thread_start_resume_read_and_fork() {
        let (state, _tmp) = state_with_config();

        let started = dispatch_stdio_request(&state, "thread/start", json!({}))
            .await
            .expect("thread/start");
        assert_eq!(started.result["status"], "started");
        let thread_id = started.result["thread_id"].as_str().expect("thread id");
        assert!(thread_id.starts_with("thread-"));
        let thread_id = thread_id.to_string();

        let resumed =
            dispatch_stdio_request(&state, "thread/resume", json!({ "thread_id": thread_id }))
                .await
                .expect("thread/resume");
        assert_eq!(resumed.result["status"], "resumed");

        let missing_resume = dispatch_stdio_request(
            &state,
            "thread/resume",
            json!({ "thread_id": "thread-nope" }),
        )
        .await
        .expect("resume of a missing thread is a response, not an error");
        assert_eq!(missing_resume.result["status"], "missing");
        assert_eq!(missing_resume.result["data"]["error"], "thread not found");

        let read = dispatch_stdio_request(&state, "thread/read", json!({ "thread_id": thread_id }))
            .await
            .expect("thread/read");
        assert_eq!(read.result["status"], "ok");
        assert_eq!(read.result["thread"]["id"], thread_id);
        assert!(read.result["data"]["history"].is_array());

        let missing_read =
            dispatch_stdio_request(&state, "thread/read", json!({ "thread_id": "thread-nope" }))
                .await
                .expect("thread/read of a missing thread");
        assert_eq!(missing_read.result["status"], "ok");
        assert!(missing_read.result["thread"].is_null());

        let forked =
            dispatch_stdio_request(&state, "thread/fork", json!({ "thread_id": thread_id }))
                .await
                .expect("thread/fork");
        assert_eq!(forked.result["status"], "forked");
        assert_ne!(
            forked.result["thread_id"].as_str().expect("fork id"),
            thread_id
        );

        let missing_fork =
            dispatch_stdio_request(&state, "thread/fork", json!({ "thread_id": "thread-nope" }))
                .await
                .expect("fork of a missing thread is a response");
        assert_eq!(missing_fork.result["status"], "missing");
    }

    #[tokio::test]
    async fn stdio_thread_create_list_archive_and_message() {
        let (state, _tmp) = state_with_config();

        let created = dispatch_stdio_request(
            &state,
            "thread/create",
            json!({ "metadata": { "origin": "test" } }),
        )
        .await
        .expect("thread/create with metadata");
        assert_eq!(created.result["status"], "created");
        let thread_id = created.result["thread_id"].as_str().expect("thread id");

        let listed = dispatch_stdio_request(&state, "thread/list", Value::Null)
            .await
            .expect("thread/list with null params");
        assert!(
            listed.result["threads"]
                .as_array()
                .expect("threads")
                .iter()
                .any(|t| t["id"] == thread_id)
        );

        let archived =
            dispatch_stdio_request(&state, "thread/archive", json!({ "thread_id": thread_id }))
                .await
                .expect("thread/archive");
        assert_eq!(archived.result["status"], "archived");
        assert_eq!(archived.result["thread_id"], thread_id);

        let active =
            dispatch_stdio_request(&state, "thread/list", json!({ "include_archived": false }))
                .await
                .expect("thread/list active");
        assert!(
            !active.result["threads"]
                .as_array()
                .expect("threads")
                .iter()
                .any(|t| t["id"] == thread_id)
        );

        let all =
            dispatch_stdio_request(&state, "thread/list", json!({ "include_archived": true }))
                .await
                .expect("thread/list all");
        assert!(
            all.result["threads"]
                .as_array()
                .expect("threads")
                .iter()
                .any(|t| t["id"] == thread_id)
        );

        let unarchived = dispatch_stdio_request(
            &state,
            "thread/unarchive",
            json!({ "thread_id": thread_id }),
        )
        .await
        .expect("thread/unarchive");
        assert_eq!(unarchived.result["status"], "unarchived");

        let message = dispatch_stdio_request(
            &state,
            "thread/message",
            json!({ "thread_id": thread_id, "input": "hello" }),
        )
        .await
        .expect("thread/message");
        assert_eq!(message.result["status"], "accepted");
        assert_eq!(
            message.result["events"].as_array().expect("events").len(),
            3
        );

        let missing_message = dispatch_stdio_request(
            &state,
            "thread/message",
            json!({ "thread_id": "thread-nope", "input": "hello" }),
        )
        .await
        .expect_err("message to a missing thread must error");
        assert_eq!(missing_message.code, -32603);
        assert!(missing_message.message.contains("not found"));
    }

    #[tokio::test]
    async fn stdio_thread_set_name_supports_both_aliases() {
        let (state, _tmp) = state_with_config();
        let thread_id = spawn_thread_via_stdio(&state).await;

        let renamed = dispatch_stdio_request(
            &state,
            "thread/set_name",
            json!({ "thread_id": thread_id, "name": "first" }),
        )
        .await
        .expect("thread/set_name");
        assert_eq!(renamed.result["thread"]["name"], "first");

        let renamed_alias = dispatch_stdio_request(
            &state,
            "thread/set-name",
            json!({ "thread_id": thread_id, "name": "second" }),
        )
        .await
        .expect("thread/set-name alias");
        assert_eq!(renamed_alias.result["thread"]["name"], "second");

        let missing = dispatch_stdio_request(
            &state,
            "thread/set_name",
            json!({ "thread_id": "thread-nope", "name": "x" }),
        )
        .await
        .expect("set_name of a missing thread is a response");
        assert_eq!(missing.result["status"], "ok");
        assert!(missing.result["thread"].is_null());
    }

    #[tokio::test]
    async fn stdio_thread_request_roundtrip_and_param_errors() {
        let (state, _tmp) = state_with_config();
        let thread_id = spawn_thread_via_stdio(&state).await;

        let listed = dispatch_stdio_request(
            &state,
            "thread/request",
            json!({ "kind": "list", "include_archived": false }),
        )
        .await
        .expect("thread/request list");
        assert_eq!(listed.result["status"], "ok");
        assert!(
            listed.result["threads"]
                .as_array()
                .expect("threads")
                .iter()
                .any(|t| t["id"] == thread_id)
        );

        let read = dispatch_stdio_request(
            &state,
            "thread/request",
            json!({ "kind": "read", "thread_id": thread_id }),
        )
        .await
        .expect("thread/request read");
        assert_eq!(read.result["thread"]["id"], thread_id);

        let unknown_kind =
            dispatch_stdio_request(&state, "thread/request", json!({ "kind": "explode" }))
                .await
                .expect_err("unknown kind must fail to deserialize");
        assert_eq!(unknown_kind.code, -32602);

        let bad_params = dispatch_stdio_request(&state, "thread/read", json!(42))
            .await
            .expect_err("thread/read needs an object");
        assert_eq!(bad_params.code, -32602);

        let missing_field = dispatch_stdio_request(&state, "thread/archive", json!({}))
            .await
            .expect_err("thread/archive requires thread_id");
        assert_eq!(missing_field.code, -32602);
    }

    #[tokio::test]
    async fn stdio_app_capabilities_and_config_get_are_raw() {
        let (state, _tmp) = state_with_config();

        let caps = dispatch_stdio_request(&state, "app/capabilities", Value::Null)
            .await
            .expect("app/capabilities");
        assert_eq!(caps.result["ok"], true);
        let routes = caps.result["data"]["routes"].as_array().expect("routes");
        assert!(routes.iter().any(|r| r == "/thread"));
        assert!(
            caps.result["data"]["config_path"]
                .as_str()
                .expect("config path")
                .contains("config.toml")
        );

        let got = dispatch_stdio_request(&state, "app/config/get", json!({ "key": "api_key" }))
            .await
            .expect("app/config/get");
        assert_eq!(got.result["ok"], true);
        // Stdio transport keeps the raw value for legacy clients.
        assert_eq!(got.result["data"]["value"], "sk-codesmith-test-secret");

        let unknown =
            dispatch_stdio_request(&state, "app/config/get", json!({ "key": "no_such_key" }))
                .await
                .expect("unknown key is still ok");
        assert_eq!(unknown.result["ok"], true);
        assert!(unknown.result["data"]["value"].is_null());
    }

    #[tokio::test]
    async fn stdio_app_config_set_unset_and_list_round_trip() {
        let (state, tmp) = state_with_config();

        let set = dispatch_stdio_request(
            &state,
            "app/config/set",
            json!({ "key": "hook_sinks.unix_socket_path", "value": "/tmp/cw-test.sock" }),
        )
        .await
        .expect("app/config/set");
        assert_eq!(set.result["ok"], true, "{:?}", set.result);

        let got = dispatch_stdio_request(
            &state,
            "app/config/get",
            json!({ "key": "hook_sinks.unix_socket_path" }),
        )
        .await
        .expect("get after set");
        assert_eq!(got.result["data"]["value"], "/tmp/cw-test.sock");

        let raw = fs::read_to_string(tmp.path().join("config.toml")).expect("config persisted");
        assert!(raw.contains("/tmp/cw-test.sock"));

        let unset = dispatch_stdio_request(
            &state,
            "app/config/unset",
            json!({ "key": "hook_sinks.unix_socket_path" }),
        )
        .await
        .expect("app/config/unset");
        assert_eq!(unset.result["ok"], true, "{:?}", unset.result);

        let gone = dispatch_stdio_request(
            &state,
            "app/config/get",
            json!({ "key": "hook_sinks.unix_socket_path" }),
        )
        .await
        .expect("get after unset");
        assert!(gone.result["data"]["value"].is_null());

        let listed = dispatch_stdio_request(&state, "app/config/list", Value::Null)
            .await
            .expect("app/config/list");
        assert_eq!(listed.result["ok"], true);
        assert!(
            listed.result["data"]["values"]
                .as_object()
                .expect("values")
                .contains_key("api_key")
        );

        let invalid_set =
            dispatch_stdio_request(&state, "app/config/set", json!({ "key": "api_key" }))
                .await
                .expect_err("app/config/set requires a value");
        assert_eq!(invalid_set.code, -32602);
    }

    #[tokio::test]
    async fn stdio_app_request_models_and_loaded_threads() {
        let (state, _tmp) = state_with_config();
        let thread_id = spawn_thread_via_stdio(&state).await;

        let models = dispatch_stdio_request(&state, "app/models", Value::Null)
            .await
            .expect("app/models");
        assert_eq!(models.result["ok"], true);
        assert!(
            !models.result["data"]["models"]
                .as_array()
                .expect("models")
                .is_empty()
        );

        let via_request =
            dispatch_stdio_request(&state, "app/request", json!({ "kind": "models" }))
                .await
                .expect("app/request models");
        assert_eq!(via_request.result["ok"], true);

        for method in ["app/thread_loaded_list", "app/thread-loaded-list"] {
            let loaded = dispatch_stdio_request(&state, method, Value::Null)
                .await
                .expect("loaded list");
            assert_eq!(loaded.result["ok"], true);
            assert!(
                loaded.result["data"]["threads"]
                    .as_array()
                    .expect("threads")
                    .iter()
                    .any(|t| t["id"] == thread_id),
                "{method} must list the created thread"
            );
        }

        let bad = dispatch_stdio_request(&state, "app/request", json!({ "kind": "explode" }))
            .await
            .expect_err("unknown app request kind");
        assert_eq!(bad.code, -32602);
    }

    #[tokio::test]
    async fn stdio_prompt_request_run_and_error_paths() {
        let (state, _tmp) = state_with_config();

        let run = dispatch_stdio_request(&state, "prompt/run", json!({ "prompt": "hello" }))
            .await
            .expect("prompt/run");
        assert!(!run.result["model"].as_str().expect("model").is_empty());
        let output: Value = serde_json::from_str(run.result["output"].as_str().expect("output"))
            .expect("output is a JSON payload");
        assert_eq!(output["prompt"], "hello");
        assert_eq!(output["model"], run.result["model"]);

        let request = dispatch_stdio_request(&state, "prompt/request", json!({ "prompt": "hi" }))
            .await
            .expect("prompt/request");
        assert_eq!(request.result["model"], run.result["model"]);

        let missing_prompt = dispatch_stdio_request(&state, "prompt/request", json!({}))
            .await
            .expect_err("prompt is required");
        assert_eq!(missing_prompt.code, -32602);

        let bad_thread = dispatch_stdio_request(
            &state,
            "prompt/request",
            json!({ "prompt": "hi", "thread_id": "thread-nope" }),
        )
        .await
        .expect_err("prompt against a missing thread must surface an error");
        assert_eq!(bad_thread.code, -32603);
        assert!(bad_thread.message.contains("not found"));
    }

    // ── HTTP handlers (dispatched via oneshot — no server bind) ─────────

    async fn post_json(app: &Router, uri: &str, token: Option<&str>, body: Value) -> Response {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        app.clone()
            .oneshot(builder.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn http_healthz_reports_protocol_v2() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body_json(response).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["protocol"], "v2");
        assert_eq!(body["service"], "codesmith-app-server");
    }

    #[tokio::test]
    async fn http_thread_endpoint_creates_threads_and_maps_errors_to_500() {
        let (app, _tmp) = app_with_config(None);

        let created = post_json(&app, "/thread", None, json!({ "kind": "create" })).await;
        assert_eq!(created.status(), StatusCode::OK);
        let body = response_body_json(created).await;
        assert_eq!(body["status"], "created");
        assert!(
            body["thread_id"]
                .as_str()
                .expect("thread id")
                .starts_with("thread-")
        );

        let failed = post_json(
            &app,
            "/thread",
            None,
            json!({ "kind": "message", "thread_id": "thread-nope", "input": "x" }),
        )
        .await;
        assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response_body_json(failed).await;
        assert!(
            body["error"]
                .as_str()
                .expect("error string")
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn http_prompt_endpoint_returns_payload_and_maps_errors_to_500() {
        let (app, _tmp) = app_with_config(None);

        let ok = post_json(&app, "/prompt", None, json!({ "prompt": "hello" })).await;
        assert_eq!(ok.status(), StatusCode::OK);
        let body = response_body_json(ok).await;
        assert!(!body["model"].as_str().expect("model").is_empty());

        let failed = post_json(
            &app,
            "/prompt",
            None,
            json!({ "prompt": "hello", "thread_id": "thread-nope" }),
        )
        .await;
        assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            response_body_json(failed).await["error"]
                .as_str()
                .expect("error string")
                .contains("not found")
        );
    }

    fn shell_tool_request(command: &str, cwd: &std::path::Path) -> Value {
        serde_json::to_value(ToolCallRequest {
            call: ToolCall {
                name: "exec_shell".to_string(),
                payload: ToolPayload::LocalShell {
                    params: LocalShellParams {
                        command: command.to_string(),
                        cwd: None,
                        timeout_ms: None,
                    },
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: None,
            },
            cwd: Some(cwd.to_path_buf()),
        })
        .expect("tool request json")
    }

    #[tokio::test]
    async fn http_tool_endpoint_denies_forbidden_prefixes() {
        let (app, tmp) = app_with_config(None);
        let response = post_json(
            &app,
            "/tool",
            None,
            shell_tool_request("rm -rf /", tmp.path()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body_json(response).await;
        assert_eq!(body["ok"], false);
        assert_eq!(body["status"], "denied");
        assert!(
            body["error"]
                .as_str()
                .expect("error string")
                .contains("denied prefix rule")
        );
    }

    #[tokio::test]
    async fn http_tool_endpoint_requires_approval_for_unmatched_commands() {
        let (app, tmp) = app_with_config(None);
        let response = post_json(
            &app,
            "/tool",
            None,
            shell_tool_request("echo hello", tmp.path()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body_json(response).await;
        assert_eq!(body["ok"], false);
        assert_eq!(body["status"], "approval_required");
        assert!(body["approval_id"].is_string());
    }

    #[tokio::test]
    async fn http_jobs_and_mcp_startup_endpoints() {
        let (app, _tmp) = app_with_config(None);

        let jobs = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/jobs")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(jobs.status(), StatusCode::OK);
        let body = response_body_json(jobs).await;
        assert_eq!(body["ok"], true);
        assert!(body["data"]["jobs"].is_array());

        let mcp = post_json(&app, "/mcp/startup", None, json!({})).await;
        assert_eq!(mcp.status(), StatusCode::OK);
        let body = response_body_json(mcp).await;
        assert_eq!(body["ok"], true);
        assert!(
            body["summary"]["ready"]
                .as_array()
                .expect("ready list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn http_wrong_bearer_token_is_unauthorized() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = post_json(
            &app,
            "/app",
            Some("wrong-token"),
            json!({ "kind": "config_list" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn http_protected_routes_open_when_auth_is_disabled() {
        let (app, _tmp) = app_with_config(None);
        let response = post_json(
            &app,
            "/app",
            None,
            json!({ "kind": "config_get", "key": "api_key" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body_json(response).await;
        assert_eq!(body["ok"], true);
        // HTTP transport redacts sensitive values even without a token gate.
        assert_eq!(body["data"]["value"], "sk-c***cret");
    }

    #[tokio::test]
    async fn cors_layer_ignores_invalid_extra_origin() {
        // A control character is what a header value cannot hold. (A space
        // it accepts, and such an origin then simply never matches.) The
        // entry is skipped (the branch warns) and the valid entry after it
        // still lands.
        let extras = vec![
            "https://bad\norigin".to_string(),
            "https://good.example".to_string(),
        ];
        assert!(
            HeaderValue::from_str("https://bad\norigin").is_err(),
            "the fixture must be no header value"
        );
        assert_eq!(
            allow_origin_for(cors_layer(&extras), "https://good.example")
                .await
                .as_deref(),
            Some("https://good.example")
        );
    }

    // ── auth token resolution / generated token file ────────────────────

    /// Module-local env-var scope: hold a test-binary-wide mutex while
    /// CODESMITH_HOME points somewhere else. (codesmith-agent-runtime's
    /// test_support is not a dev-dependency here, and adding one is outside
    /// a test-only change.)
    struct ScopedCodeSmithHome {
        previous: Option<std::ffi::OsString>,
        _keep: Option<tempfile::TempDir>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl ScopedCodeSmithHome {
        fn set(value: &std::path::Path, keep: Option<tempfile::TempDir>) -> Self {
            static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            let _lock = LOCK
                .get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = std::env::var_os("CODESMITH_HOME");
            // SAFETY: serialized behind the module-local env mutex; the only
            // concurrent env readers in this binary resolve disposable paths.
            unsafe { std::env::set_var("CODESMITH_HOME", value) };
            Self {
                previous,
                _keep: keep,
                _lock,
            }
        }
    }

    impl Drop for ScopedCodeSmithHome {
        fn drop(&mut self) {
            // SAFETY: still holding the module-local env mutex.
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var("CODESMITH_HOME", previous);
                } else {
                    std::env::remove_var("CODESMITH_HOME");
                }
            }
        }
    }

    fn token_options(auth_token: Option<&str>, insecure_no_auth: bool) -> AppServerOptions {
        AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: auth_token.map(std::string::ToString::to_string),
            insecure_no_auth,
            cors_origins: Vec::new(),
        }
    }

    #[test]
    fn generated_token_is_persisted_owner_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_path_buf();
        let _env = ScopedCodeSmithHome::set(&home, Some(tmp));

        let token = resolve_auth_token(&token_options(None, false))
            .expect("token resolution")
            .expect("generated token present");
        assert!(token.starts_with("cwapp_"));

        let token_path = home.join("app-server.token");
        let raw = fs::read_to_string(&token_path).expect("token file written");
        assert_eq!(raw, format!("{token}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&token_path)
                .expect("token file metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn generated_token_fails_loud_when_home_cannot_be_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, b"file, not a dir").expect("blocker file");
        let unreachable_home = blocker.join("home");
        let _env = ScopedCodeSmithHome::set(&unreachable_home, Some(tmp));

        let err = resolve_auth_token(&token_options(None, false))
            .expect_err("unwritable home must fail token generation");
        assert!(
            err.to_string().contains("cannot create"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn auth_token_is_trimmed() {
        let token = resolve_auth_token(&token_options(Some("  tok  "), false))
            .expect("trim resolution")
            .expect("token present");
        assert_eq!(token, "tok");
    }

    #[test]
    fn insecure_no_auth_wins_over_explicit_token_on_loopback() {
        let token = resolve_auth_token(&token_options(Some("tok"), true)).expect("resolution");
        assert!(token.is_none());
    }

    #[test]
    fn generated_token_fails_when_token_path_is_not_a_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_path_buf();
        fs::create_dir_all(home.join("app-server.token")).expect("token path is a directory");
        let _env = ScopedCodeSmithHome::set(&home, Some(tmp));

        let err = resolve_auth_token(&token_options(None, false))
            .expect_err("un-openable token path must fail token generation");
        assert!(
            err.to_string().contains("cannot create"),
            "unexpected error: {err}"
        );
    }

    // ── malformed stdio params ──────────────────────────────────────────

    #[tokio::test]
    async fn stdio_malformed_params_report_invalid_params_per_method() {
        let (state, _tmp) = state_with_config();
        let cases: &[(&str, Value)] = &[
            ("thread/create", json!(5)),
            ("thread/start", json!({ "cwd": 5 })),
            ("thread/resume", json!(5)),
            ("thread/fork", json!(5)),
            ("thread/list", json!({ "limit": "not a number" })),
            ("thread/set_name", json!({ "thread_id": "t" })),
            ("thread/unarchive", json!(5)),
            ("thread/message", json!({ "thread_id": "t" })),
            ("app/config/get", json!(5)),
            ("app/config/unset", json!(5)),
        ];
        for (method, params) in cases {
            let err = dispatch_stdio_request(&state, method, params.clone())
                .await
                .expect_err("malformed params must error");
            assert_eq!(err.code, -32602, "{method}: {err:?}");
        }
    }

    #[tokio::test]
    async fn stdio_config_mutations_fail_loud_when_disk_config_is_corrupt() {
        let (state, tmp) = state_with_config();
        let config_path = tmp.path().join("config.toml");
        let good = fs::read_to_string(&config_path).expect("original config");
        fs::write(&config_path, "((( not toml").expect("corrupt config");

        let set = dispatch_stdio_request(
            &state,
            "app/config/set",
            json!({ "key": "hook_sinks.unix_socket_path", "value": "/tmp/x.sock" }),
        )
        .await
        .expect("corrupt config still dispatches");
        assert_eq!(set.result["ok"], false, "{:?}", set.result);
        assert!(
            set.result["data"]["persist_error"]
                .as_str()
                .expect("persist error")
                .contains("failed to parse config"),
            "{:?}",
            set.result
        );

        let unset = dispatch_stdio_request(
            &state,
            "app/config/unset",
            json!({ "key": "hook_sinks.unix_socket_path" }),
        )
        .await
        .expect("corrupt config still dispatches");
        assert_eq!(unset.result["ok"], false, "{:?}", unset.result);
        assert!(unset.result["data"]["persist_error"].is_string());

        // Repairing the file restores mutation: the failed attempts above
        // must not have left a wedged in-memory snapshot.
        fs::write(&config_path, good).expect("repaired config");
        let recovered = dispatch_stdio_request(
            &state,
            "app/config/set",
            json!({ "key": "hook_sinks.unix_socket_path", "value": "/tmp/y.sock" }),
        )
        .await
        .expect("set after repair");
        assert_eq!(recovered.result["ok"], true, "{:?}", recovered.result);
    }

    /// The default launch passes no `--config`: reads resolve to the default
    /// config file. Writes resolved to nothing — and still answered `ok`.
    #[tokio::test]
    async fn config_set_without_an_explicit_config_path_writes_the_default_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_path_buf();
        let _env = ScopedCodeSmithHome::set(&home, Some(tmp));
        let state = build_state(None, None, AppTransport::Http).expect("state");

        let set = dispatch_stdio_request(
            &state,
            "app/config/set",
            json!({ "key": "hook_sinks.unix_socket_path", "value": "/tmp/cw-default.sock" }),
        )
        .await
        .expect("app/config/set");
        assert_eq!(set.result["ok"], true, "{:?}", set.result);

        let raw = fs::read_to_string(home.join("config.toml")).expect("default config written");
        assert!(raw.contains("/tmp/cw-default.sock"), "{raw}");
    }

    // ── build_state hook sink wiring ────────────────────────────────────

    /// Stdout is the JSON-RPC stream in stdio mode: hook events mirrored
    /// there put non-JSON-RPC lines in front of every protocol reply.
    #[test]
    fn stdio_hooks_never_mirror_events_to_stdout() {
        let log = PathBuf::from("events.jsonl");
        assert_eq!(
            build_hooks(log.clone(), None, AppTransport::Stdio).sink_count(),
            1,
            "stdio: the jsonl log only"
        );
        assert_eq!(
            build_hooks(log, None, AppTransport::Http).sink_count(),
            2,
            "http: the stdout mirror and the jsonl log"
        );
    }

    #[test]
    fn build_state_registers_unix_socket_hook_sink_when_configured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            "api_key = \"sk-codesmith-test-secret\"\n\n[hook_sinks]\nunix_socket_path = \"/tmp/cw-hook-test.sock\"\n",
        )
        .expect("write config");
        // Construction succeeds; the sink connects lazily per emit and
        // skips silently when no listener owns the socket.
        let _state =
            build_state(Some(config_path), None, AppTransport::Http).expect("state with hook sink");
    }

    // ── tool endpoint cwd fallback ──────────────────────────────────────

    #[tokio::test]
    async fn http_tool_endpoint_falls_back_to_process_cwd() {
        let (app, _tmp) = app_with_config(None);
        let request = serde_json::to_value(ToolCallRequest {
            call: ToolCall {
                name: "exec_shell".to_string(),
                payload: ToolPayload::LocalShell {
                    params: LocalShellParams {
                        command: "echo hello".to_string(),
                        cwd: None,
                        timeout_ms: None,
                    },
                },
                source: ToolCallSource::Direct,
                raw_tool_call_id: None,
            },
            cwd: None,
        })
        .expect("tool request json");
        let response = post_json(&app, "/tool", None, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body_json(response).await;
        // No cwd supplied: the process cwd is used as the policy cwd and the
        // unmatched command still short-circuits to an approval request.
        assert_eq!(body["status"], "approval_required");
    }
}
