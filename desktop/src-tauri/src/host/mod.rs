//! host: the vendored client's RPC surface, served over a loopback HTTP
//! bridge and translated onto `codesmith serve --http`.
//!
//! Known limitations (see docs/DESKTOP.md):
//! - the RPC surface is a subset; unimplemented endpoints answer
//!   `gateway/method-unavailable` (the client renders an error state,
//!   it never pretends success);
//! - attachment staging is in-memory (see `host::uploads`): receipts die
//!   with the bridge process.

pub mod changes;
pub mod codesmith;
pub mod commands;
pub mod events;
pub mod host_drop;
pub mod jobs;
pub mod mapper;
pub mod mux;
pub mod schedule;
pub mod sessions;
pub mod settings;
pub mod uploads;
pub mod workspace;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use codesmith::CodesmithProcess;

/// Page origins the bridge serves: the vite dev page and the Tauri webview
/// (CORS for the unary routes, the upgrade check for the stream mux).
pub const PAGE_ORIGINS: [&str; 4] = [
    "http://localhost:1420",
    "http://127.0.0.1:1420",
    "tauri://localhost",
    "https://tauri.localhost",
];

/// The single implicit workspace the bridge presents (the sidebar's pins
/// and manual order are desktop-local state, see `host::workspace`).
const DEFAULT_WORKSPACE_ID: &str = "codesmith-default";

pub struct HostState {
    pub backend: CodesmithProcess,
    pub cwd: PathBuf,
    /// Open `$events` sinks: sink id -> frame sender (the approval and
    /// user-question waterfalls push here; a still-pending waterfall is
    /// re-delivered to every stream that opens after it fired).
    pub event_sinks: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    /// Open follow streams that accept live increments (workspace/follow,
    /// session/control): sink id -> (endpoint, frame sender).
    pub live_sinks: Mutex<HashMap<String, (String, mpsc::Sender<Value>)>>,
    /// Waterfalls awaiting their `$events/result`, by event id.
    pub pending_inputs: Mutex<HashMap<String, events::PendingInput>>,
    /// The seq every projection write carries (see `next_projection_seq`).
    pub projection_clock: AtomicU64,
    /// The provider half of each session's last `session/selectModel` in this
    /// process. The runtime thread records only a model id, and one id can
    /// sit under several catalog providers; recomputed projections keep the
    /// user's provider while the thread still runs that model id.
    pub model_choices: Mutex<HashMap<String, (String, String)>>,
    /// One background waterfall watcher per prompted session (see
    /// `sessions::ensure_waterfall_watcher`): session id -> task handle.
    pub waterfall_watchers: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Staged uploads awaiting the prompt that consumes them (see
    /// `host::uploads`): receipt id -> staged bytes. In-memory and
    /// session-bound, like upstream's per-Session staging.
    pub uploads: uploads::Staging,
    /// One-shot staged bytes of the latest host-path drop (see
    /// `host::host_drop`): id -> image bytes the page fetches once. An `Arc`
    /// because the window's drag-drop handler fills it before this state
    /// exists.
    pub drop_bytes: host_drop::DropStaging,
}

impl HostState {
    /// Draw the seq for one projection write — control baseline, follow
    /// snapshot, live frame, or rename echo. The client keeps a projection
    /// only when its seq beats the row it holds, and the runtime journals no
    /// event for a model switch, so projections cannot ride the journal
    /// cursor; one process-wide counter orders every write instead. Draw it
    /// BEFORE reading runtime state, so a later read never carries an older
    /// seq. A bridge restart is a new Host generation, which clears the
    /// client's rows, so the counter restarts with the process.
    pub fn next_projection_seq(&self) -> u64 {
        self.projection_clock.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The desktop shell's settings home (`<codesmith home>/desktop`), where
    /// the UI preference document lives.
    pub fn settings_home(&self) -> PathBuf {
        std::env::var("CODESMITH_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| self.cwd.clone()))
            .join(".codesmith")
    }

    /// Push one increment frame to every open stream of `endpoint`. Sinks of
    /// aborted streams (a cancelled mux stream never reaches its own removal)
    /// are pruned here and in `push_event_frame`.
    pub async fn push_live(&self, endpoint: &str, frame: Value) {
        let mut sinks = self.live_sinks.lock().await;
        sinks.retain(|_, (_, tx)| !tx.is_closed());
        for (_, tx) in sinks.values().filter(|(ep, _)| ep == endpoint) {
            let _ = tx.send(frame.clone()).await;
        }
    }

    /// Push one frame to every open `$events` sink. `true` when at least one
    /// sink took it (a waterfall with no sink would hang the turn).
    pub async fn push_event_frame(&self, frame: Value) -> bool {
        let mut sinks = self.event_sinks.lock().await;
        sinks.retain(|_, tx| !tx.is_closed());
        let mut delivered = false;
        for tx in sinks.values() {
            if tx.send(frame.clone()).await.is_ok() {
                delivered = true;
            }
        }
        delivered
    }

    /// End a pending waterfall (runtime decided or turn ended): drop the
    /// registry entry and cancel it on every `$events` sink.
    pub async fn cancel_pending(&self, event_id: &str) {
        self.pending_inputs.lock().await.remove(event_id);
        self.push_event_frame(events::cancel_frame(event_id)).await;
    }

    /// End every pending waterfall of one session (its turn ended).
    pub async fn cancel_pending_for_session(&self, session_id: &str) {
        let ids: Vec<String> = self
            .pending_inputs
            .lock()
            .await
            .iter()
            .filter(|(_, pending)| pending.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.cancel_pending(&id).await;
        }
    }
}

pub type SharedHost = Arc<HostState>;

/// The RPC error envelope the client turns into an error state.
pub fn rpc_error(code: &str, message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message.into(), "details": {} } })
}

pub fn rpc_ok(value: Value) -> Value {
    json!({ "ok": true, "value": value })
}

/// A stream's terminal failure, as the client's mux `error` frame carries it.
pub fn stream_failure(code: &str, message: impl Into<String>) -> Value {
    json!({ "code": code, "message": message.into(), "details": {} })
}

fn workspace_view(cwd: &std::path::Path) -> Value {
    let now = chrono::Utc::now().to_rfc3339();
    let title = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "CodeSmith".into());
    json!({
        "workspaceId": DEFAULT_WORKSPACE_ID,
        "path": cwd.to_string_lossy(),
        "title": title,
        "sessionIds": [],
        "createdAt": now,
        "updatedAt": now,
    })
}

/// The single implicit workspace with the live session list folded in — the
/// client associates a selected session with a workspace through this list.
/// The list order is the sidebar's manual order (desktop-local, see
/// `host::workspace`); never-dragged sessions follow in runtime recency
/// order.
pub(super) async fn workspace_view_live(host: &HostState) -> Value {
    let mut view = workspace_view(&host.cwd);
    if let Ok(threads) = host
        .backend
        .get::<Vec<ThreadSummary>>("/v1/threads/summary?limit=200")
        .await
    {
        let live: Vec<String> = threads.into_iter().map(|t| t.id).collect();
        view["sessionIds"] = json!(workspace::ordered_live(&host.settings_home(), &live));
    }
    view
}

/// The workspace view plus the live and archived session id lists (the
/// sidebar's archived filter reads the archived set, the pin baseline the
/// live one — the merged `sessionIds` carries exactly the live set).
async fn workspace_view_with_archived(host: &HostState) -> (Value, Vec<String>, Vec<String>) {
    let view = workspace_view_live(host).await;
    let live: Vec<String> = view["sessionIds"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let archived = host
        .backend
        .get::<Vec<ThreadSummary>>("/v1/threads/summary?limit=200&archived_only=true")
        .await
        .map(|threads| threads.into_iter().map(|t| t.id).collect())
        .unwrap_or_default();
    (view, live, archived)
}

/// Dispatch a unary client endpoint. `payload` is the raw `{args: …}` body.
pub async fn dispatch(host: &SharedHost, endpoint: &str, payload: &Value) -> Value {
    let host_arc = host;
    let host: &HostState = host;
    let _ = payload.get("args");
    match endpoint {
        "session/list" => session_list(host).await,
        "session/create" => session_create(host).await,
        "session/modelCatalog" => model_catalog(host).await,
        "session/page" => {
            match sessions::page(host, payload.get("args").unwrap_or(&Value::Null)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error("gateway/internal", format!("session/page: {err:#}")),
            }
        }
        "session/prompt" => {
            match sessions::prompt(host_arc, payload.get("args").unwrap_or(&Value::Null)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error("gateway/internal", format!("session/prompt: {err:#}")),
            }
        }
        "session/cancel" => {
            match sessions::cancel(host, payload.get("args").unwrap_or(&Value::Null)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error("gateway/internal", format!("session/cancel: {err:#}")),
            }
        }
        "session/selectModel" => match sessions::select_model(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("session/selectModel: {err:#}")),
        },
        "$events/result" => {
            match events_result(host, payload.get("args").unwrap_or(&Value::Null)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(
                    "gateway/arguments-invalid",
                    format!("$events/result: {err:#}"),
                ),
            }
        }
        "session/rename" => match sessions::rename(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("session/rename: {err:#}")),
        },
        "session/fork" => match sessions::fork(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("session/fork: {err:#}")),
        },
        "session/search" => match sessions::search(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("session/search: {err:#}")),
        },
        "workspace/archiveSession" | "workspace/unarchiveSession" => {
            match sessions::set_archived(
                host,
                args_of(payload),
                endpoint.ends_with("archiveSession"),
            )
            .await
            {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error("gateway/internal", format!("{endpoint}: {err:#}")),
            }
        }
        // Pinning and the sidebar's manual order are desktop-local state
        // (see `host::workspace`); the mutations echo `workspace/follow`
        // increments the same way the Host registry does.
        "workspace/pinSession" | "workspace/unpinSession" => {
            match workspace::pin_or_unpin(host, endpoint.ends_with("pinSession"), args_of(payload))
                .await
            {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(err.code, err.message),
            }
        }
        "workspace/insertSessionBefore" => {
            match workspace::insert_session_before(host, args_of(payload)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(err.code, err.message),
            }
        }
        "skills/list" => match sessions::skills(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("skills/list: {err:#}")),
        },
        "settings/describe" => rpc_ok(settings::describe(&host.settings_home())),
        "settings/mutate" | "settings/update" => {
            match settings::mutate(host, &host.settings_home(), endpoint, args_of(payload)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error("gateway/arguments-invalid", format!("{endpoint}: {err:#}")),
            }
        }
        "settings/openSettingsDocument" => rpc_ok(json!({ "opened": false })),
        "userQuestions/answer" => rpc_ok(json!(false)),
        "schedule/catalog" => match schedule::catalog(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("schedule/catalog: {err:#}")),
        },
        "schedule/create" => match schedule::create(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(
                "gateway/arguments-invalid",
                format!("schedule/create: {err:#}"),
            ),
        },
        "schedule/edit" => match schedule::edit(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(
                "gateway/arguments-invalid",
                format!("schedule/edit: {err:#}"),
            ),
        },
        "schedule/run" => match schedule::run(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("schedule/run: {err:#}")),
        },
        "schedule/history" => match schedule::history(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("schedule/history: {err:#}")),
        },
        "schedule/deleteInactive" => {
            match schedule::delete_inactive(host, args_of(payload)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(
                    "gateway/internal",
                    format!("schedule/deleteInactive: {err:#}"),
                ),
            }
        }
        "schedule/deliveryLimits" => rpc_ok(json!({ "days": 30, "records": 100 })),
        // The jobs popover's kill (host::jobs): the runtime's task cancel.
        "job/kill" => jobs::kill(host, args_of(payload)).await,
        "session/updateQueue" => match sessions::update_queue(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(err.code, err.message),
        },
        "workspace/initializeDefault" => rpc_ok(workspace_view_live(host).await),
        // Attachment staging (host::uploads): receipts resolve when the
        // prompt that names them reaches the runtime.
        "fileUploads/upload" => match uploads::upload_rpc(&host.uploads, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(err.code, err.message),
        },
        "session/attachment" => {
            let store_root = match uploads::attachment_store_root() {
                Some(root) => root,
                None => {
                    return rpc_error(
                        "gateway/internal",
                        "session/attachment: the attachment store root is unavailable",
                    )
                }
            };
            match uploads::attachment(&store_root, args_of(payload)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(err.code, err.message),
            }
        }
        // The '/' catalog: compaction plus the per-thread mode switches
        // (host::commands). The REPL's OTHER slash commands are REPL-local
        // and stay out of the catalog, so their "/"-prefixed lines fall
        // through to ordinary prompts, exactly what the runtime does with
        // them. (/model needs no row: the vendored client registers it as a
        // client contribution driving session/selectModel.)
        "commands/list" => rpc_ok(commands::catalog()),
        "commands/execute" => match commands::execute(host, args_of(payload)).await {
            Ok(commands::Outcome::Executed(value)) => rpc_ok(value),
            // An admission miss is ok-with-no-value: the client reads the
            // absent value as its "unknown or malformed command" outcome.
            Ok(commands::Outcome::Miss) => json!({ "ok": true }),
            Err(err) => rpc_error(err.code, err.message),
        },
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

/// The `args` member of a client request: object remotes send `{args: {…}}`,
/// positional remotes `{args: […]}`.
fn args_of(payload: &Value) -> &Value {
    payload.get("args").unwrap_or(&Value::Null)
}

/// Serve one logical client stream into `tx` (the mux frames each value as an
/// `item`). `Ok` ends the stream (`end`), `Err` fails it with the
/// client-visible failure. A consumer that goes away (cancel, socket close)
/// aborts the task mid-await, so every registration made here must survive
/// being dropped unremoved — the sink registries prune closed senders.
pub async fn open_stream(
    host: &SharedHost,
    endpoint: &str,
    payload: &Value,
    tx: mpsc::Sender<Value>,
) -> Result<(), Value> {
    match endpoint {
        "session/follow" => sessions::follow(host, args_of(payload), &tx)
            .await
            .map_err(|err| stream_failure("gateway/internal", format!("session/follow: {err:#}"))),
        "$events" => {
            events_stream(host, &tx).await;
            Ok(())
        }
        // The trajectory inspector's on-demand tool-result lookup
        // (host::sessions): one value frame, then the stream parks.
        "toolResults/watch" => sessions::tool_results_watch(host, args_of(payload), &tx)
            .await
            .map_err(|err| {
                stream_failure("gateway/internal", format!("toolResults/watch: {err:#}"))
            }),
        // The jobs popover's roster and follow streams (host::jobs).
        "job/list" => jobs::list(host, args_of(payload), &tx)
            .await
            .map_err(|err| stream_failure("gateway/internal", format!("job/list: {err:#}"))),
        "job/follow" => jobs::follow(host, args_of(payload), &tx).await,
        // The runtime's user-input requests carry no tool call id, so the
        // client never opens a foreground claim; an explicit attachWait (a
        // continued question) has no answerable wait here and ends with no
        // frames, exactly like a Host whose wait already ended.
        "userQuestions/attachWait" => Ok(()),
        _ => {
            let frames = stream_baseline(host, endpoint)
                .await
                .map_err(|err| stream_failure("gateway/method-unavailable", err.to_string()))?;
            for frame in frames {
                if tx.send(frame).await.is_err() {
                    return Ok(());
                }
            }
            // Follow semantics: park, pumping live increments, until the
            // consumer goes away — ending would read as a stream failure.
            let (live_tx, mut live_rx) = mpsc::channel::<Value>(16);
            let sink_id = uuid::Uuid::new_v4().to_string();
            host.live_sinks
                .lock()
                .await
                .insert(sink_id.clone(), (endpoint.to_string(), live_tx));
            loop {
                let frame = tokio::select! {
                    frame = live_rx.recv() => match frame {
                        Some(frame) => frame,
                        None => break,
                    },
                    _ = tx.closed() => break,
                };
                if tx.send(frame).await.is_err() {
                    break;
                }
            }
            host.live_sinks.lock().await.remove(&sink_id);
            Ok(())
        }
    }
}

/// `$events`: the ready frame (this stream's own clientId — the client echoes
/// it back on `$events/result`; upstream issues one per connected client),
/// then the still-pending waterfalls (a stream that connects after one fired
/// must still see it), then the live pump.
async fn events_stream(host: &HostState, tx: &mpsc::Sender<Value>) {
    let ready = json!({
        "type": "ready",
        "clientId": uuid::Uuid::new_v4().to_string(),
        "host": { "home": host.cwd.to_string_lossy() },
    });
    if tx.send(ready).await.is_err() {
        return;
    }
    let (event_tx, mut event_rx) = mpsc::channel::<Value>(64);
    let sink_id = uuid::Uuid::new_v4().to_string();
    host.event_sinks
        .lock()
        .await
        .insert(sink_id.clone(), event_tx.clone());
    for pending in host.pending_inputs.lock().await.values() {
        if event_tx.send(pending.frame.clone()).await.is_err() {
            break;
        }
    }
    loop {
        let frame = tokio::select! {
            frame = event_rx.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
            _ = tx.closed() => break,
        };
        if tx.send(frame).await.is_err() {
            break;
        }
    }
    host.event_sinks.lock().await.remove(&sink_id);
}

/// Baseline frames of a parked stream endpoint; `Err` fails the stream.
async fn stream_baseline(host: &HostState, endpoint: &str) -> Result<Vec<Value>> {
    match endpoint {
        "session/control" => sessions::control_baseline(host).await,
        "workspace/follow" => {
            let (view, live, archived) = workspace_view_with_archived(host).await;
            Ok(vec![json!({
                "type": "baseline",
                "value": {
                    "items": [view],
                    "archivedSessionIds": archived,
                    "pinnedSessionIds": workspace::pinned_live(&host.settings_home(), &live),
                }
            })])
        }
        "remoteAccess/follow" => Ok(vec![json!({ "type": "baseline", "owner": true })]),
        // CodeSmith has no hosted account: the client still opens this
        // stream, so it gets the signed-out shape with no links to follow.
        "account/watch" => Ok(vec![json!({
            "status": "signed-out",
            "attempt": null,
            "links": { "usageUrl": "", "topUpUrl": "" }
        })]),
        other => bail!("codesmith desktop: stream {other} is not implemented yet"),
    }
}

#[derive(serde::Deserialize)]
struct ThreadSummary {
    id: String,
    #[serde(default)]
    latest_turn_id: Option<String>,
    #[serde(default)]
    latest_turn_status: Option<String>,
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
                // Client optional fields must be ABSENT, not null — a null
                // reaches the client as a typed value it then dereferences.
                .map(|t| {
                    // Client timestamps are epoch milliseconds; the wire `time`
                    // and list `updatedAt` must both be integer numbers.
                    let updated_ms = mapper::epoch_ms(&t.updated_at)
                        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                    let running = matches!(
                        t.latest_turn_status.as_deref(),
                        Some("in_progress") | Some("queued")
                    );
                    let mut row = json!({
                        "sessionId": t.id,
                        "updatedAt": updated_ms,
                        "running": running,
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
            let session_id = thread
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // The new session must land in the workspace the sidebar groups
            // by: push an upsert increment to every open workspace follow.
            let mut view = workspace_view_live(host).await;
            if let Some(ids) = view.get_mut("sessionIds").and_then(Value::as_array_mut) {
                if !ids
                    .iter()
                    .any(|id| id.as_str() == Some(session_id.as_str()))
                {
                    ids.push(json!(session_id));
                }
            }
            host.push_live(
                "workspace/follow",
                json!({ "type": "upsert", "workspace": view }),
            )
            .await;
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
            // One group per runtime provider: the client matches a session's
            // modelSelection projection against group ids, so the group id
            // must be the provider the runtime itself reports.
            let mut provider_ids: Vec<&str> = Vec::new();
            for model in &models {
                if !provider_ids.contains(&model.provider.as_str()) {
                    provider_ids.push(model.provider.as_str());
                }
            }
            let groups: Vec<Value> = provider_ids
                .iter()
                .map(|provider| {
                    json!({
                        "id": provider,
                        "name": provider,
                        "models": models
                            .iter()
                            .filter(|m| m.provider == *provider)
                            .map(|m| json!({ "id": m.id, "name": m.id }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            rpc_ok(json!({
                "default": default,
                "routableProviders": provider_ids,
                "groups": groups,
                "failures": [],
            }))
        }
        Err(err) => rpc_error("gateway/internal", format!("session/modelCatalog: {err:#}")),
    }
}

/// `$events/result` — deliver one client waterfall outcome to the runtime.
/// A result for an already-settled or unknown event is an idempotent no-op
/// (`{delivered:false}`), and so is a delivery the runtime answers 404 (it
/// already decided or timed out): the upstream gateway drops superseded
/// results the same way, and erroring here would kill the client's whole
/// `$events` generation over a harmless late answer. A delivered decision is
/// followed by the waterfall's cancel frame — the upstream host cancels an
/// event once decided, and that frame is what retires the answering card.
async fn events_result(host: &HostState, args: &Value) -> Result<Value> {
    args.get("clientId")
        .and_then(Value::as_str)
        .context("$events/result without clientId")?;
    let event_id = args
        .get("eventId")
        .and_then(Value::as_str)
        .context("$events/result without eventId")?
        .to_string();
    let outcome = args.get("outcome").cloned().unwrap_or(Value::Null);
    let Some(pending) = host.pending_inputs.lock().await.remove(&event_id) else {
        return Ok(json!({ "delivered": false }));
    };
    match pending.kind {
        events::InputKind::Approval => {
            let decision = events::approval_decision(&outcome);
            let delivered = host
                .backend
                .post_settle(
                    &format!("/v1/approvals/{}", pending.input_id),
                    &json!({ "decision": decision, "remember": false }),
                )
                .await
                .context("deliver approval decision")?;
            // The upstream host cancels an event once decided; without this
            // frame the answering client's card never learns the request
            // ended (the runtime's own `approval.decided` echo and the
            // turn-end sweep only cancel what is still pending).
            host.cancel_pending(&event_id).await;
            Ok(json!({ "delivered": delivered, "decision": decision }))
        }
        events::InputKind::Questions => {
            let Some(answers) = events::question_answers(&outcome) else {
                // Nobody answered (an unclaimed `next`): keep the request
                // pending for another answerer. No HTTP cancel route exists
                // for user input (known limitation): report undelivered; the
                // runtime's own deadline closes the question.
                host.pending_inputs.lock().await.insert(event_id, pending);
                return Ok(json!({ "delivered": false }));
            };
            let delivered = host
                .backend
                .post_settle(
                    &format!("/v1/user-input/{}/{}", pending.session_id, pending.input_id),
                    &json!({ "answers": answers }),
                )
                .await
                .context("deliver user-input answers")?;
            host.cancel_pending(&event_id).await;
            Ok(json!({ "delivered": delivered }))
        }
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

/// The vendored client's raw-bytes upload route — every image and every
/// Blob-bodied file posts here with `sessionId`/`name`/`mediaType` in the
/// query. Answers HTTP 200 always (the client reads business failures from
/// the `{ok, …}` envelope); a body over the limit is a transport failure
/// either way, so axum's 413 is fine here.
async fn upload_binary_handler(
    State(host): State<SharedHost>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    body: axum::body::Bytes,
) -> Response {
    let query = query.unwrap_or_default();
    Json(uploads::upload_binary(&host.uploads, &query, &body).await).into_response()
}

async fn health() -> &'static str {
    "ok"
}

/// One staged drop's bytes, served exactly once (`host::host_drop`). A spent
/// or unknown id is 404 — the page then degrades that member to a path
/// citation.
async fn drop_bytes_handler(State(host): State<SharedHost>, Path(id): Path<String>) -> Response {
    match host_drop::take_bytes(&host.drop_bytes, &id) {
        Some(bytes) => (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            )],
            bytes,
        )
            .into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serve the bridge on `port`: unary calls on `/api/<endpoint>` (CORS admits
/// the page origins), every stream on the `/api/remote.mux` WebSocket.
pub async fn serve_bridge(host: SharedHost, port: u16) -> Result<()> {
    let origins: Vec<HeaderValue> = PAGE_ORIGINS
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
        .route("/api/remote.mux", get(mux::upgrade))
        // The upload route takes raw bytes, not the unary JSON envelope —
        // it must not fall through to the wildcard's `Option<Json<_>>`
        // extractor (which would reject `application/octet-stream`).
        .route("/api/session/uploadFileBinary", post(upload_binary_handler))
        .route("/api/host/dropBytes/{id}", get(drop_bytes_handler))
        // The changes-review routes (host::changes): plain authenticated
        // GET/POSTs the page's fetch hook carries here, ahead of the unary
        // wildcard.
        .route(
            "/api/changes.summary",
            get(changes::route::summary),
        )
        .route("/api/changes.diff", get(changes::route::diff))
        .route(
            "/api/changes.open",
            get(changes::route::open).post(changes::route::open),
        )
        .route("/api/present.host", get(changes::route::present_host))
        .route(
            "/api/present.open",
            get(changes::route::present_open).post(changes::route::present_open),
        )
        .route("/api/{*endpoint}", post(unary_handler))
        .layer(cors)
        // The runtime's per-file attachment cap is 10MB; the same bytes as
        // canonical base64 (the unary `fileUploads/upload` path) are ~13.7MB
        // of JSON — both past axum's 2MB default.
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .with_state(host);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    eprintln!("codesmith desktop: client bridge on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
