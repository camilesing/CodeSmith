//! dsh_host: the DSH client's RPC surface, served over a loopback HTTP
//! bridge and translated onto `codesmith serve --http`.
//!
//! Known limitations (spec docs/superpowers/specs/2026-11-06-codesmith-desktop-design.md):
//! - the RPC surface is a subset; unimplemented endpoints answer
//!   `gateway/method-unavailable` (the DSH client renders an error state,
//!   it never pretends success);
//! - `session/updateQueue` (edit/remove of queued input) has no CodeSmith
//!   equivalent and is answered as unavailable;
//! - streams emit their baseline and then stay open without further frames
//!   (M2 wires the live event mapper into `$events` and `session/follow`);
//! - one parked server task per open stream is retained until process exit
//!   (bounded by the session count; acceptable for the desktop shell).

pub mod codesmith;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use codesmith::CodesmithProcess;

pub struct HostState {
    pub backend: CodesmithProcess,
    pub cwd: PathBuf,
    pub client_id: String,
    /// Open `$events` sinks: stream id -> frame sender (M2 mapper pushes here).
    pub event_sinks: Mutex<HashMap<String, mpsc::Sender<Value>>>,
}

pub type SharedHost = Arc<HostState>;

/// The DSH error envelope the client turns into an error state.
pub fn rpc_error(code: &str, message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message.into(), "details": {} } })
}

pub fn rpc_ok(value: Value) -> Value {
    json!({ "ok": true, "value": value })
}

fn workspace_view(cwd: &PathBuf) -> Value {
    let now = chrono::Utc::now().to_rfc3339();
    let title = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "CodeSmith".into());
    json!({
        "workspaceId": "codesmith-default",
        "path": cwd.to_string_lossy(),
        "title": title,
        "sessionIds": [],
        "createdAt": now,
        "updatedAt": now,
    })
}

/// Dispatch a unary DSH endpoint. `payload` is the raw `{args: …}` body.
pub async fn dispatch(host: &HostState, endpoint: &str, payload: &Value) -> Value {
    let _ = payload.get("args");
    match endpoint {
        "session/list" => session_list(host).await,
        "session/create" => session_create(host).await,
        "session/modelCatalog" => model_catalog(host).await,
        "workspace/initializeDefault" => rpc_ok(workspace_view(&host.cwd)),
        "settings/describe" => rpc_ok(json!({ "writable": true, "hasDocument": false, "namespaces": [] })),
        "agentPresets/list" => rpc_ok(json!({ "presets": [] })),
        "dynamicCordisRunner/syncInspectManifest" => rpc_ok(Value::Null),
        "dynamicCordisRunner/inventory" => rpc_ok(json!([])),
        "credentials/describe" => rpc_ok(json!({})),
        "web/listProviders" => rpc_ok(json!({ "search": [], "fetch": [] })),
        "permissionPresets/catalog" => rpc_ok(json!({ "options": [] })),
        "account/getProfile" | "account/getBalance" | "account/getUnnotifiedBonuses" => {
            rpc_ok(Value::Null)
        }
        "account/ackBonusNotified" => rpc_ok(json!(true)),
        other => rpc_error(
            "gateway/method-unavailable",
            format!("codesmith desktop: {other} is not implemented yet"),
        ),
    }
}

/// Baseline frames of a stream endpoint; `Err` maps to an error frame.
pub async fn stream_baseline(host: &HostState, endpoint: &str, stream_id: String) -> Result<Vec<Value>> {
    match endpoint {
        "session/control" => Ok(vec![json!({ "type": "baseline", "value": { "projections": {} } })]),
        "workspace/follow" => Ok(vec![json!({
            "type": "baseline",
            "value": {
                "items": [workspace_view(&host.cwd)],
                "archivedSessionIds": [],
                "pinnedSessionIds": [],
            }
        })]),
        "remoteAccess/follow" => Ok(vec![json!({ "type": "baseline", "owner": true })]),
        "account/watch" => Ok(vec![json!({
            "status": "signed-out",
            "attempt": null,
            "links": {
                "usageUrl": "https://platform.deepseek.com/usage",
                "topUpUrl": "https://platform.deepseek.com/top_up",
            }
        })]),
        "$events" => {
            let (tx, _rx) = mpsc::channel::<Value>(64);
            host.event_sinks.lock().await.insert(stream_id, tx);
            Ok(vec![json!({
                "type": "ready",
                "clientId": host.client_id,
                "host": { "home": host.cwd.to_string_lossy() },
            })])
        }
        other => Err(anyhow!(
            "codesmith desktop: stream {other} is not implemented yet"
        )),
    }
}

#[derive(serde::Deserialize)]
struct ThreadSummary {
    id: String,
    #[serde(default)]
    latest_turn_id: Option<String>,
    updated_at: String,
}

async fn session_list(host: &HostState) -> Value {
    match host
        .backend
        .get::<Vec<ThreadSummary>>("/v1/threads/summary?limit=200")
        .await
    {
        Ok(threads) => rpc_ok(json!({
            "items": threads
                .into_iter()
                // DSH optional fields must be ABSENT, not null — a null
                // reaches the client as a typed value it then dereferences.
                .map(|t| {
                    let mut row = json!({
                        "sessionId": t.id,
                        "updatedAt": t.updated_at,
                        "running": false,
                        "blank": t.latest_turn_id.is_none(),
                        "agentAvailable": true,
                    });
                    let obj = row.as_object_mut().unwrap();
                    if let Some(turn) = t.latest_turn_id {
                        obj.insert("formatStatus".into(), json!(format!("turn:{turn}")));
                    }
                    row
                })
                .collect::<Vec<_>>(),
        })),
        Err(err) => rpc_error("gateway/internal", format!("session/list: {err:#}")),
    }
}

async fn session_create(host: &HostState) -> Value {
    match host.backend.post::<Value>("/v1/threads", &json!({})).await {
        Ok(thread) => {
            let session_id = thread.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            // `agentPreset` stays absent (not null): the client treats null as
            // a present preset value and dereferences it.
            rpc_ok(json!({ "sessionId": session_id }))
        }
        Err(err) => rpc_error("gateway/internal", format!("session/create: {err:#}")),
    }
}

async fn model_catalog(host: &HostState) -> Value {
    #[derive(serde::Deserialize)]
    struct ModelEntry {
        id: String,
        provider: String,
        default: bool,
    }
    match host.backend.get::<Vec<ModelEntry>>("/v1/models").await {
        Ok(models) => {
            let default_model = models.iter().find(|m| m.default).or_else(|| models.first());
            let default = default_model
                .map(|m| json!({ "provider": m.provider, "model": m.id }))
                .unwrap_or(Value::Null);
            rpc_ok(json!({
                "default": default,
                "routableProviders": [],
                "groups": [{
                    "id": "codesmith",
                    "name": "CodeSmith",
                    "models": models
                        .iter()
                        .map(|m| json!({ "id": m.id, "name": m.id }))
                        .collect::<Vec<_>>(),
                }],
                "failures": [],
            }))
        }
        Err(err) => rpc_error("gateway/internal", format!("session/modelCatalog: {err:#}")),
    }
}

async fn unary_handler(
    State(host): State<SharedHost>,
    Path(endpoint): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let payload = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let result = dispatch(&host, &endpoint, &payload).await;
    Json(result).into_response()
}

/// NDJSON streaming: baseline frames flush immediately, then the body stays
/// open (follow semantics) until the client disconnects or the app exits.
async fn stream_handler(State(host): State<SharedHost>, Path(endpoint): Path<String>) -> Response {
    let stream_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel::<Result<String, std::io::Error>>(16);
    let host_for_stream = host.clone();
    let endpoint_stream = endpoint.clone();
    tokio::spawn(async move {
        match stream_baseline(&host_for_stream, &endpoint_stream, stream_id).await {
            Ok(frames) => {
                for frame in frames {
                    if tx.send(Ok(format!("{frame}\n"))).await.is_err() {
                        return;
                    }
                }
            }
            Err(err) => {
                let _ = tx
                    .send(Ok(format!(
                        "{}\n",
                        json!({ "type": "error", "error": { "code": "gateway/method-unavailable", "message": err.to_string() } })
                    )))
                    .await;
                return;
            }
        }
        // Hold the stream open; M2 forwards mapper frames through the
        // registered sinks before this pending future.
        std::future::pending::<()>().await;
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(axum::body::Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
        .unwrap()
}

async fn health() -> &'static str {
    "ok"
}

/// Serve the bridge on `port`; CORS allows the vite dev origin and the
/// Tauri webview origins.
pub async fn serve_bridge(host: SharedHost, port: u16) -> Result<()> {
    let allow = [
        "http://localhost:1420",
        "http://127.0.0.1:1420",
        "tauri://localhost",
        "https://tauri.localhost",
    ];
    let origins: Vec<HeaderValue> = allow
        .iter()
        .map(|origin| origin.parse::<HeaderValue>().unwrap())
        .collect();
    let cors = tower_http::cors::CorsLayer::new()
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([header::CONTENT_TYPE])
        // One call with the full list: each allow_origin() call REPLACES the
        // previous list, so chaining them would keep only the last origin.
        .allow_origin(origins);
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/stream/{*endpoint}", post(stream_handler))
        .route("/api/{*endpoint}", post(unary_handler))
        .layer(cors)
        .with_state(host);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    eprintln!("codesmith desktop: dsh bridge on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
