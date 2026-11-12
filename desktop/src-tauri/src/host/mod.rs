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
pub mod credentials;
pub mod directories;
pub mod events;
pub mod feedback;
pub mod files;
pub mod goals;
pub mod host_drop;
pub mod jobs;
pub mod llm;
pub mod mapper;
pub mod mux;
pub mod office;
pub mod open_in_app;
pub mod permissions;
pub mod references;
pub mod schedule;
pub mod sessions;
pub mod settings;
pub mod terminal;
pub mod tickets;
pub mod uploads;
pub mod workspace;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The shell's Tauri handle, used only by the native directory picker's
    /// dialog plugin. Tests and bridge-only hosts leave it absent; `pick`
    /// then answers `directory-picker/unavailable` rather than lying.
    pub app_handle: Option<tauri::AppHandle>,
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
    /// Per-workspace `@` file index (see `host::references`); dropped when a
    /// turn settles because the turn may have written files.
    pub reference_index: references::ReferenceIndex,
    /// Sessions whose approved `exit_plan_mode` still owes the plan-mode
    /// handoff (see `sessions::restore_plan_handoff`). The approval is
    /// delivered mid-turn, and the runtime saves the thread when the turn
    /// settles — a PATCH sent at approval time is overwritten by that save,
    /// so the restore waits for the turn's own end event.
    pub plan_handoffs: Mutex<HashSet<String>>,
    /// The mode each session was in when the bridge last switched it to
    /// `plan` (see `sessions::set_mode`). The runtime thread records only the
    /// current mode, so the bridge stashes the pre-plan mode — the REPL's
    /// `plan_restore_mode` — and the handoff hands the session back to it.
    pub plan_return_modes: Mutex<HashMap<String, String>>,
    /// Serializes Workspace registry mutations (create/rename/delete/order):
    /// each is a read-modify-write of `desktop/workspaces.json`.
    pub workspace_lock: tokio::sync::Mutex<()>,
    /// Bridge-local GoalBar mutations per session (revision bumps, a desktop
    /// pause/clear tombstone). Hydrated from `desktop/goals.json` on first
    /// use so the mutations outlive a bridge restart; see `host::goals`.
    pub goal_overlays: Mutex<HashMap<String, goals::Overlay>>,
    /// Whether `goal_overlays` was hydrated from disk (the first projection
    /// or mutation loads it once).
    pub goal_overlays_loaded: AtomicBool,
    /// The sidebar terminal tabs (see `host::terminal`): session-owned user
    /// shells on bridge-spawned PTYs. In-memory only — terminals die with
    /// the bridge process.
    pub terminals: terminal::Registry,
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
    /// the UI preference document lives. `CODESMITH_HOME` names the CodeSmith
    /// root itself (`codesmith_config::codesmith_home` — the same rule the
    /// attachment store root follows), not the parent `.codesmith` hangs
    /// under, so it must not be joined again.
    pub fn settings_home(&self) -> PathBuf {
        codesmith_config::codesmith_home().unwrap_or_else(|_| self.cwd.clone())
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

/// Dispatch a unary client endpoint. `payload` is the raw `{args: …}` body.
pub async fn dispatch(host: &SharedHost, endpoint: &str, payload: &Value) -> Value {
    let host_arc = host;
    let host: &HostState = host;
    let _ = payload.get("args");
    match endpoint {
        "session/list" => session_list(host).await,
        "session/create" => session_create(host, payload.get("args").unwrap_or(&Value::Null)).await,
        "session/modelCatalog" => model_catalog(host).await,
        "session/page" => {
            match sessions::page(host, payload.get("args").unwrap_or(&Value::Null)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error("gateway/internal", format!("session/page: {err:#}")),
            }
        }
        "session/events" => match sessions::events(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("session/events: {err:#}")),
        },
        // One session's projection baseline without a follow stream: the
        // vendored controller reads it when restoring a blank session and
        // when a connection generation is re-established.
        "session/projections" => match sessions::projections(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("session/projections: {err:#}")),
        },
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
        // Subagent child sessions (host::sessions): continuation maps to
        // `agent_send_input`, interrupt to the manager's cancel.
        "subagents/prompt" => match sessions::subagent_prompt(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => {
                let message = format!("subagents/prompt: {err:#}");
                let code = if message.contains("subagent/attachment-invalid") {
                    "subagent/attachment-invalid"
                } else {
                    "subagent/delivery-unavailable"
                };
                rpc_error(code, message)
            }
        },
        "subagents/interruptByUser" => {
            match sessions::subagent_interrupt(host, args_of(payload)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(
                    "subagent/delivery-unavailable",
                    format!("subagents/interruptByUser: {err:#}"),
                ),
            }
        }
        "session/selectModel" => match sessions::select_model(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("session/selectModel: {err:#}")),
        },
        // The GoalBar (host::goals): the live goal view and its four
        // mutations over the runtime's `GET|POST /v1/threads/{id}/goal`.
        "goals/get" => goals::get(host, args_of(payload)).await,
        "goals/edit" | "goals/pause" | "goals/resume" | "goals/clear" => {
            goals::action(host, endpoint, args_of(payload)).await
        }
        // Per-message and session feedback (host::feedback): a desktop-local
        // compare-and-set sidecar. The vendored surface reads the list once
        // per session and updates from each mutation's reply.
        "messageFeedback/list" => feedback::message_list(host, args_of(payload)).await,
        "messageFeedback/put" => feedback::message_put(host, args_of(payload)).await,
        "messageFeedback/delete" => feedback::message_delete(host, args_of(payload)).await,
        "sessionFeedback/record" => feedback::session_record(host, args_of(payload)).await,
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
                endpoint == "workspace/archiveSession",
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
            match workspace::pin_or_unpin(
                host,
                endpoint == "workspace/pinSession",
                args_of(payload),
            )
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
        // The Workspace registry (host::workspace): host-directory
        // registrations the sidebar groups sessions under. Every mutation
        // answers the vendored controller's value and re-publishes the
        // complete follow baseline.
        "workspace/create" => match workspace::create(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(err.code, err.message),
        },
        "workspace/rename" => match workspace::rename(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(err.code, err.message),
        },
        "workspace/delete" => match workspace::delete(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(err.code, err.message),
        },
        "workspace/insertBefore" => {
            match workspace::insert_workspace_before(host, args_of(payload)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(err.code, err.message),
            }
        }
        // The workspace picker's directory primitives (host::directories).
        "directoryPicker/list" => directories::list(args_of(payload)).await,
        "directoryPicker/createDirectory" => directories::create_directory(args_of(payload)).await,
        "directoryPicker/pick" => directories::pick(host).await,
        "skills/list" => match sessions::skills(host, args_of(payload)).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error("gateway/internal", format!("skills/list: {err:#}")),
        },
        "settings/describe" => {
            // The provider namespace reads config.toml and probes the
            // credential store for custom-route key presence.
            let home = host.settings_home();
            match tokio::task::spawn_blocking(move || settings::describe(&home)).await {
                Ok(Ok(value)) => rpc_ok(value),
                Ok(Err(err)) => {
                    rpc_error("gateway/internal", format!("settings/describe: {err:#}"))
                }
                Err(err) => rpc_error(
                    "gateway/internal",
                    format!("settings/describe join failed: {err}"),
                ),
            }
        }
        "settings/mutate" | "settings/update" => {
            match settings::mutate(host, &host.settings_home(), endpoint, args_of(payload)).await {
                Ok(value) => rpc_ok(value),
                Err(err) => rpc_error(err.code, format!("{endpoint}: {}", err.message)),
            }
        }
        "settings/openSettingsDocument" => match settings::open_document(host).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(
                "gateway/internal",
                format!("settings/openSettingsDocument: {err:#}"),
            ),
        },
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
            Err(err) => rpc_error(err.code, err.message),
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
        "workspace/initializeDefault" => match workspace::initialize_default(host).await {
            Ok(value) => rpc_ok(value),
            Err(err) => rpc_error(err.code, err.message),
        },
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
        // The sidebar terminal tabs (host::terminal): user shells the bridge
        // itself spawns. They are the user's terminals, not the model's —
        // `allow_shell` does not gate them.
        "terminal/environment" => terminal::environment(host, args_of(payload)).await,
        "terminal/shells" => terminal::shells(host, args_of(payload)).await,
        "terminal/list" => terminal::list(host, args_of(payload)).await,
        "terminal/create" => terminal::create(host, args_of(payload)).await,
        "terminal/rename" => terminal::rename(host, args_of(payload)).await,
        "terminal/close" => terminal::close(host, args_of(payload)).await,
        // The '/' catalog: compaction plus the per-thread mode switches
        // (host::commands). The REPL's OTHER slash commands are REPL-local
        // and stay out of the catalog, so their "/"-prefixed lines fall
        // through to ordinary prompts, exactly what the runtime does with
        // them. (/model needs no row: the vendored client registers it as a
        // client contribution driving session/selectModel.)
        "commands/list" => rpc_ok(commands::catalog()),
        "commands/execute" => match commands::execute(host_arc, args_of(payload)).await {
            Ok(commands::Outcome::Executed(value)) => rpc_ok(value),
            // An admission miss is ok-with-no-value: the client reads the
            // absent value as its "unknown or malformed command" outcome.
            Ok(commands::Outcome::Miss) => json!({ "ok": true }),
            Err(err) => rpc_error(err.code, err.message),
        },
        // The permission picker's process catalog (host::permissions): the
        // upstream preset values over CodeSmith's per-thread mode. The default
        // is the new-session preference stored in the desktop settings
        // document.
        "permissionPresets/catalog" => {
            let home = host.settings_home();
            match tokio::task::spawn_blocking(move || settings::default_permission_preset(&home))
                .await
            {
                Ok(default) => rpc_ok(permissions::catalog(&default)),
                Err(err) => rpc_error(
                    "gateway/internal",
                    format!("permissionPresets/catalog: {err}"),
                ),
            }
        }
        // The files tree and preview reads (host::files), over the session
        // workspace.
        "workspaceFileScope" => files::scope(host, args_of(payload)).await,
        "workspaceFiles/list" => files::list(host, args_of(payload)).await,
        "workspaceFiles/read" => files::read(host, args_of(payload)).await,
        "workspaceFiles/stat" => files::stat(host, args_of(payload)).await,
        // The office preview conversion's provider generation (the render
        // half answers multipart in `unary_handler`).
        "officeToPdf/generation" => rpc_ok(json!(office::generation_value())),
        // The file/directory open controls (client-ui-open-in-app and
        // client-ui-file-actions): this desktop opens and reveals inside a
        // session workspace, and registers no per-path application catalog.
        "session/canOpenWorkspacePath" => rpc_ok(json!(true)),
        "session/workspacePathApplications" => files::workspace_path_applications(),
        "session/openWorkspacePath" => files::open_workspace_path(host, args_of(payload)).await,
        // The `@` menu's file source (host::references); session references
        // are not a CodeSmith capability — the resolver answers an empty
        // list, which the vendored source degrades to file-only rows.
        "fileReferences/list" => references::list(host, args_of(payload)).await,
        "sessionReferenceResolver/candidates" => rpc_ok(json!([])),
        "agentPresets/list" => rpc_ok(json!({ "presets": [] })),
        "dynamicCordisRunner/syncInspectManifest" => rpc_ok(Value::Null),
        "dynamicCordisRunner/inventory" => rpc_ok(json!([])),
        // The Models settings page's provider surface (host::llm): the
        // configurable directory from config.toml, the live route list, and
        // one endpoint interrogation.
        "llm/listConfigurableProviders" => llm::list_configurable_providers().await,
        "llm/listProviders" => llm::list_providers(host).await,
        "llm/discoverModels" => llm::discover_models(host, args_of(payload)).await,
        // The Models settings page's credential surface (host::credentials),
        // mapped onto the keyring; values never ride back out.
        "credentials/describe" => credentials::describe(args_of(payload)).await,
        "credentials/set" => credentials::set(host, args_of(payload)).await,
        "credentials/unset" => credentials::unset(host, args_of(payload)).await,
        "web/listProviders" => rpc_ok(json!({ "search": [], "fetch": [] })),
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
/// being dropped unremoved — the sink registries prune closed senders. The
/// `uplink` receiver carries the stream's client-to-host items; a handler
/// that never reads it leaves it dropped (the mux swallows such items).
pub async fn open_stream(
    host: &SharedHost,
    endpoint: &str,
    payload: &Value,
    tx: mpsc::Sender<Value>,
    uplink: mpsc::Receiver<mux::UplinkFrame>,
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
        // The files tab's watch feed (host::files).
        "workspaceFiles/changes" => files::changes(host, args_of(payload), &tx).await,
        // The runtime's user-input requests carry no tool call id, so the
        // client never opens a foreground claim; an explicit attachWait (a
        // continued question) has no answerable wait here and ends with no
        // frames, exactly like a Host whose wait already ended.
        "userQuestions/attachWait" => Ok(()),
        // The sidebar terminal's duplex screen stream and its window hold
        // (host::terminal).
        "terminal/attach" => terminal::attach(host, args_of(payload), &tx, uplink).await,
        "terminal/retain" => terminal::retain(host, args_of(payload), &tx).await,
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
            let baseline = workspace::baseline(host).await;
            Ok(vec![json!({
                "type": "baseline",
                "value": workspace::baseline_value(&baseline),
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

/// The cwd half of a runtime thread record. `/v1/threads/summary` carries the
/// row's status fields but no workspace, so the list joins the full thread
/// list by id — one extra loopback read for the whole page, not one detail
/// read per row, and no runtime protocol change.
#[derive(serde::Deserialize)]
struct ThreadWorkspace {
    id: String,
    workspace: PathBuf,
}

/// One `session/list` row. Client optional fields must be ABSENT, not null —
/// a null reaches the client as a typed value it then dereferences.
fn session_row(t: ThreadSummary, cwd: &std::path::Path) -> Value {
    // Client timestamps are epoch milliseconds; the wire `time` and list
    // `updatedAt` must both be integer numbers.
    let updated_ms =
        mapper::epoch_ms(&t.updated_at).unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let running = matches!(
        t.latest_turn_status.as_deref(),
        Some("in_progress") | Some("queued")
    );
    // The sidebar files tree and the `@` file-chip resolver read the row's
    // `cwd`; serve the thread's own workspace, which may not be this bridge
    // process's working directory (a session left by an earlier run in
    // another directory).
    //
    // `formatStatus` is the persistence-format vocabulary (`current` |
    // `migration-required`), never a turn marker: the client's strict codec
    // rejects an unknown value and drops the whole page. CodeSmith threads
    // are written in the current format, so every row reports `current`.
    let row = json!({
        "sessionId": t.id,
        "updatedAt": updated_ms,
        "running": running,
        "blank": t.latest_turn_id.is_none(),
        "agentAvailable": true,
        "formatStatus": "current",
        "cwd": cwd.to_string_lossy(),
    });
    row
}

async fn session_list(host: &HostState) -> Value {
    let (summaries, records) = tokio::join!(
        host.backend
            .get::<Vec<ThreadSummary>>("/v1/threads/summary?limit=200"),
        host.backend
            .get::<Vec<ThreadWorkspace>>("/v1/threads?limit=200"),
    );
    let workspaces: HashMap<String, PathBuf> = match records {
        Ok(records) => records
            .into_iter()
            .map(|record| (record.id, record.workspace))
            .collect(),
        Err(err) => {
            // A list without the full records still renders; those rows fall
            // back to this process's working directory (the pre-join shape).
            eprintln!("codesmith desktop: session/list thread workspaces: {err:#}");
            HashMap::new()
        }
    };
    match summaries {
        Ok(threads) => rpc_ok(json!({
            "items": threads
                .into_iter()
                .map(|t| {
                    let cwd = workspaces.get(&t.id).cloned().unwrap_or_else(|| host.cwd.clone());
                    session_row(t, &cwd)
                })
                .collect::<Vec<_>>(),
        })),
        Err(err) => rpc_error("gateway/internal", format!("session/list: {err:#}")),
    }
}

/// `session/create` — create (or reuse a blank) runtime thread in the
/// requested Workspace. The vendored controller packs its single `request`
/// parameter object-keyed: `{workspaceId?, cwd?, sessionId?, agentPreset?}`.
/// `agentPreset` has no CodeSmith composition seat and is ignored; the
/// response omits it (the client reads null as a present value).
async fn session_create(host: &HostState, payload: &Value) -> Value {
    let request = payload.get("request").unwrap_or(payload);
    let requested_workspace = match request.get("workspaceId").and_then(Value::as_str) {
        Some(workspace_id) => match workspace::path_for_id(host, workspace_id).await {
            Some(path) => Some(path),
            None => {
                return rpc_error(
                    "workspace/not-found",
                    format!("workspace {workspace_id} not found"),
                )
            }
        },
        None => match request.get("cwd").and_then(Value::as_str) {
            Some(cwd) => {
                let cwd = cwd.to_string();
                match tokio::task::spawn_blocking(move || {
                    let path = PathBuf::from(&cwd);
                    if !path.is_absolute() {
                        bail!("cwd is not an absolute path");
                    }
                    let canonical = std::fs::canonicalize(&path)?;
                    if !canonical.is_dir() {
                        bail!("cwd is not a directory");
                    }
                    Ok::<PathBuf, anyhow::Error>(canonical)
                })
                .await
                {
                    Ok(Ok(path)) => Some(path),
                    Ok(Err(err)) => return rpc_error("workspace/invalid-path", format!("{err:#}")),
                    Err(err) => {
                        return rpc_error(
                            "gateway/internal",
                            format!("session/create cwd join failed: {err}"),
                        )
                    }
                }
            }
            None => None,
        },
    };
    // A named blank the client still owns is reused instead of creating a
    // second empty thread (the upstream `connectWorkspace` contract). A
    // thread that has turns is not blank and is left alone.
    if let Some(session_id) = request.get("sessionId").and_then(Value::as_str) {
        if let Ok(detail) = host
            .backend
            .get::<Value>(&format!("/v1/threads/{session_id}"))
            .await
        {
            let blank = detail
                .get("turns")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty);
            let same_workspace = match requested_workspace.as_ref() {
                Some(path) => detail
                    .pointer("/thread/workspace")
                    .and_then(Value::as_str)
                    .is_some_and(|workspace| workspace == path.to_string_lossy()),
                None => true,
            };
            if blank && same_workspace {
                return rpc_ok(json!({ "sessionId": session_id }));
            }
        }
    }
    let body = match requested_workspace {
        Some(path) => json!({ "workspace": path }),
        None => json!({}),
    };
    match host.backend.post::<Value>("/v1/threads", &body).await {
        Ok(thread) => {
            let session_id = thread
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // The permission row's new-session default (desktop settings
            // state). Apply it after creation; a runtime refusal is logged,
            // not fatal — the session exists and the runtime's default
            // (workspace-write / agent) is the fallback.
            let home = host.settings_home();
            let preset =
                tokio::task::spawn_blocking(move || settings::default_permission_preset(&home))
                    .await
                    .unwrap_or_else(|_| permissions::DEFAULT_PRESET.to_string());
            if let Some(mode) = permissions::mode_of_preset(&preset) {
                if mode != "agent" {
                    match sessions::set_mode(host, &session_id, mode).await {
                        Ok(Ok(())) => {}
                        Ok(Err((_status, message))) => eprintln!(
                            "codesmith desktop: permission default {preset:?} refused for {session_id}: {message}"
                        ),
                        Err(err) => eprintln!(
                            "codesmith desktop: permission default {preset:?} failed for {session_id}: {err:#}"
                        ),
                    }
                }
            }
            // Re-publish the baseline so the session lands in the Workspace
            // that groups its directory (or trails under Ungrouped).
            workspace::push_replace(host).await;
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
            // Approving a plan is the plan-mode handoff: the REPL host
            // restores the session mode when it approves exit_plan_mode, and
            // the desktop bridge mirrors that so the next turn can write.
            // The decision lands mid-turn; the runtime saves the thread when
            // the turn settles, so a PATCH now would be overwritten. Record
            // the handoff BEFORE delivering, because the turn can complete
            // while the decision POST is still in flight; the turn-end event
            // in the follow/watcher stream performs the restore
            // (see `sessions::restore_plan_handoff`).
            let plan_handoff = decision == "allow"
                && pending
                    .frame
                    .pointer("/request/toolName")
                    .and_then(Value::as_str)
                    == Some("exit_plan_mode");
            if plan_handoff {
                host.plan_handoffs
                    .lock()
                    .await
                    .insert(pending.session_id.clone());
            }
            let delivered = match host
                .backend
                .post_settle(
                    &format!("/v1/approvals/{}", pending.input_id),
                    &json!({ "decision": decision, "remember": false }),
                )
                .await
                .context("deliver approval decision")
            {
                Ok(delivered) => delivered,
                Err(err) => {
                    if plan_handoff {
                        host.plan_handoffs.lock().await.remove(&pending.session_id);
                    }
                    return Err(err);
                }
            };
            if plan_handoff && !delivered {
                host.plan_handoffs.lock().await.remove(&pending.session_id);
            }
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
    // `workspaceFiles/readBytes` answers multipart/form-data: the client's
    // strict result codec only accepts a real Uint8Array, which JSON cannot
    // carry — the envelope holds `data: null` and an attachments manifest,
    // and the bytes ride as a binary part the page's carrier plants.
    if endpoint == "workspaceFiles/readBytes" {
        return match files::read_bytes(&host, args_of(&payload)).await {
            Ok((mut value, bytes)) => multipart_response(&mut value, bytes),
            Err(error) => Json(error).into_response(),
        };
    }
    // The office preview conversion answers the same binary shape (the PDF
    // rides the bytes attachment; the value carries `data: null`).
    if endpoint == "officeToPdf/render" {
        return match office::render(&host, args_of(&payload)).await {
            Ok((mut value, bytes)) => multipart_response(&mut value, bytes),
            Err(error) => Json(error).into_response(),
        };
    }
    let result = dispatch(&host, &endpoint, &payload).await;
    Json(result).into_response()
}

/// One binary-answer RPC response: the `metadata` field (the `{ok, value}`
/// envelope with an attachments manifest naming the bytes part and its path
/// inside the value) plus the part itself. Both ends are ours, so the
/// envelope is the plain rpc shape, not the connection's server-response.
fn multipart_response(value: &mut Value, bytes: Vec<u8>) -> Response {
    let part = "bytes-0";
    // The manifest rides the ENVELOPE, not the value: the page's carrier
    // reads `envelope.attachments` and plants each part at its manifest path.
    // A manifest nested inside `value` is never seen, leaving `data: null`
    // for the strict codec.
    let envelope = json!({
        "ok": true,
        "value": value,
        "attachments": [{ "codec": "bytes", "part": part, "path": ["data"] }],
    });
    let boundary = format!("cds-{}", uuid::Uuid::new_v4());
    let mut body: Vec<u8> = Vec::new();
    let metadata = envelope.to_string();
    body.extend_from_slice(format!("--{boundary}\r\ncontent-disposition: form-data; name=\"metadata\"\r\ncontent-type: application/json\r\n\r\n{metadata}\r\n").as_bytes());
    // The binary part MUST carry a filename: without one the Fetch
    // `multipart/form-data` parser treats the part as UTF-8 text and rejects
    // arbitrary bytes (PNG and friends) as a failed fetch. A filename makes
    // it a File, which the page's carrier reads back as a Blob.
    body.extend_from_slice(format!("--{boundary}\r\ncontent-disposition: form-data; name=\"{part}\"; filename=\"{part}.bin\"\r\ncontent-type: application/octet-stream\r\n\r\n").as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&format!("multipart/form-data; boundary={boundary}"))
                .expect("boundary is header-safe"),
        )],
        body,
    )
        .into_response()
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
        .route("/api/changes.summary", get(changes::route::summary))
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
        // The application-open catalog and launch (host::open_in_app). These
        // are host-origin routes upstream serves beside the document, not
        // `/api/…` unary RPCs; the page carrier redirects them here.
        .route("/open-in-app/apps", get(open_in_app::apps))
        .route("/open-in-app/icon/{id}", get(open_in_app::icon))
        .route("/open-in-app/open", post(open_in_app::open))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binary_answers_name_the_part_as_a_file() {
        // A part without a filename is decoded as UTF-8 text by the Fetch
        // multipart parser; arbitrary bytes (a PNG) then fail the whole
        // response. The binary part must be a File.
        let mut value = json!({ "data": Value::Null });
        let response = multipart_response(&mut value, vec![0x89, b'P', b'N', b'G', 0x00, 0xff]);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.starts_with("multipart/form-data; boundary=")),
            Some(true)
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("name=\"bytes-0\"; filename=\"bytes-0.bin\""),
            "binary part must be a file: {body}"
        );
        // The manifest rides the envelope; the page carrier never looks
        // inside `value` for it.
        let metadata = body
            .split("\r\n--")
            .next()
            .expect("metadata part")
            .split("\r\n\r\n")
            .nth(1)
            .expect("metadata body");
        let metadata: Value = serde_json::from_str(metadata).expect("metadata json");
        assert_eq!(metadata["attachments"][0]["path"], json!(["data"]));
        assert_eq!(metadata["attachments"][0]["part"], "bytes-0");
        assert!(
            metadata["value"].get("attachments").is_none(),
            "attachments must not nest inside value"
        );
    }

    #[test]
    fn session_rows_carry_the_threads_workspace_and_status_fields() {
        let active = session_row(
            ThreadSummary {
                id: "thr_1".into(),
                latest_turn_id: Some("turn_1".into()),
                latest_turn_status: Some("queued".into()),
                updated_at: "2026-11-06T10:00:00Z".into(),
            },
            std::path::Path::new("/w/other"),
        );
        // A session left by a run in another directory keeps its own cwd;
        // the bridge's process directory is only the fallback.
        assert_eq!(active["cwd"], "/w/other");
        assert_eq!(active["running"], true);
        assert_eq!(active["blank"], false);
        assert_eq!(
            active["formatStatus"], "current",
            "formatStatus is the persistence-format enum, not a turn marker"
        );
        assert!(active["updatedAt"].as_i64().unwrap() > 0);

        let blank = session_row(
            ThreadSummary {
                id: "thr_2".into(),
                latest_turn_id: None,
                latest_turn_status: None,
                updated_at: "2026-11-06T10:00:00Z".into(),
            },
            std::path::Path::new("/w"),
        );
        assert_eq!(blank["blank"], true);
        assert_eq!(blank["running"], false);
        assert_eq!(blank["formatStatus"], "current");
    }

    // === Dispatch contract tests (fake runtime) ===

    #[derive(Clone)]
    struct FakeRuntime {
        workspace: std::path::PathBuf,
        archived: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
        /// The thread detail's item list (feedback targets, goal tools).
        /// Shared so a test can append a newer journal item.
        items: Items,
        /// Every `POST /v1/threads/{id}/goal` body, in order.
        goal_actions: GoalActions,
        /// The live goal an engine would report; an evicted engine is
        /// `loaded:false`.
        live_goal: LiveGoal,
    }

    type GoalActions = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;
    type Items = std::sync::Arc<std::sync::Mutex<Value>>;

    /// The fake runtime's live goal state. Mutations apply only while the
    /// engine is loaded: an evicted POST is recorded but changes nothing
    /// (the bridge's own persisted snapshot is what the tests exercise).
    #[derive(Clone, Default)]
    struct LiveGoal {
        goal: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
        loaded: std::sync::Arc<AtomicBool>,
    }

    impl LiveGoal {
        fn evicted() -> Self {
            Self::default()
        }

        fn loaded(goal: Value) -> Self {
            let live = Self::default();
            live.set(Some(goal));
            live
        }

        fn set(&self, goal: Option<Value>) {
            *self.goal.lock().unwrap_or_else(|err| err.into_inner()) = goal;
            self.loaded.store(true, Ordering::SeqCst);
        }

        fn evict(&self) {
            *self.goal.lock().unwrap_or_else(|err| err.into_inner()) = None;
            self.loaded.store(false, Ordering::SeqCst);
        }
    }

    async fn fake_threads(
        axum::extract::State(fake): axum::extract::State<FakeRuntime>,
    ) -> Json<Value> {
        let archived = fake
            .archived
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        Json(json!([
            { "id": "thr_a", "workspace": fake.workspace, "archived": archived.contains("thr_a") },
            { "id": "thr_b", "workspace": fake.workspace, "archived": archived.contains("thr_b") },
        ]))
    }

    async fn fake_summary(
        axum::extract::State(fake): axum::extract::State<FakeRuntime>,
        uri: axum::http::Uri,
    ) -> Json<Value> {
        let archived = fake
            .archived
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        let archived_only = uri.query().unwrap_or("").contains("archived_only=true");
        let rows: Vec<Value> = ["thr_a", "thr_b"]
            .into_iter()
            .filter(|id| archived.contains(*id) == archived_only)
            .map(|id| json!({ "id": id, "updated_at": "2026-11-11T00:00:00Z" }))
            .collect();
        Json(Value::Array(rows))
    }

    async fn fake_thread(
        axum::extract::State(fake): axum::extract::State<FakeRuntime>,
        axum::extract::Path(id): axum::extract::Path<String>,
    ) -> Result<Json<Value>, axum::http::StatusCode> {
        if id == "thr_missing" {
            return Err(axum::http::StatusCode::NOT_FOUND);
        }
        let items = fake
            .items
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        Ok(Json(json!({
            // The runtime's thread record carries the workspace every
            // per-session file surface resolves against; the fake mirrors it
            // so the ticket-board read path is exercised end to end.
            "thread": { "id": id, "workspace": fake.workspace.to_string_lossy() },
            "turns": [],
            "items": items,
        })))
    }

    async fn fake_patch_thread(
        axum::extract::State(fake): axum::extract::State<FakeRuntime>,
        axum::extract::Path(id): axum::extract::Path<String>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        if let Some(archived) = body.get("archived").and_then(Value::as_bool) {
            let mut set = fake.archived.lock().unwrap_or_else(|err| err.into_inner());
            if archived {
                set.insert(id);
            } else {
                set.remove(&id);
            }
        }
        Json(json!({}))
    }

    async fn fake_create_thread() -> Json<Value> {
        Json(json!({ "id": "thr_new" }))
    }

    async fn fake_goal(
        axum::extract::State(fake): axum::extract::State<FakeRuntime>,
        axum::extract::Path(_id): axum::extract::Path<String>,
    ) -> Json<Value> {
        let goal = fake
            .live_goal
            .goal
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        let loaded = fake.live_goal.loaded.load(Ordering::SeqCst);
        Json(json!({ "loaded": loaded, "goal": goal }))
    }

    async fn fake_goal_post(
        axum::extract::State(fake): axum::extract::State<FakeRuntime>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        fake.goal_actions
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(body.clone());
        let mut goal = fake
            .live_goal
            .goal
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if fake.live_goal.loaded.load(Ordering::SeqCst) {
            match body
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "edit" => {
                    let objective = body
                        .get("objective")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let same = goal
                        .as_ref()
                        .and_then(|goal| goal.get("objective"))
                        .and_then(Value::as_str)
                        == Some(objective.as_str());
                    // GoalState::edit: an unchanged objective keeps the
                    // budget unless a new one is supplied; a changed one
                    // starts fresh.
                    let budget = match body.get("token_budget").cloned() {
                        Some(budget) if !budget.is_null() => budget,
                        _ if same => goal
                            .as_ref()
                            .and_then(|goal| goal.get("token_budget"))
                            .cloned()
                            .unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    let rounds = if same {
                        goal.as_ref()
                            .and_then(|goal| goal.get("rounds_started"))
                            .cloned()
                            .unwrap_or(json!(0))
                    } else {
                        json!(0)
                    };
                    *goal = Some(json!({
                        "objective": objective,
                        "status": "active",
                        "token_budget": budget,
                        "elapsed_seconds": 0,
                        "evidence": null,
                        "blocker": null,
                        "rounds_started": rounds,
                        "max_goal_rounds": 25,
                    }));
                }
                "pause" => {
                    if let Some(goal) = goal.as_mut() {
                        goal["status"] = json!("paused");
                        goal["blocker"] = json!("paused from the desktop");
                    }
                }
                "resume" => {
                    if let Some(goal) = goal.as_mut() {
                        goal["status"] = json!("active");
                        goal["blocker"] = Value::Null;
                    }
                }
                "clear" => *goal = None,
                _ => {}
            }
        }
        Json(json!({ "goal": goal.clone() }))
    }

    async fn fake_runtime(workspace: std::path::PathBuf) -> String {
        fake_runtime_with_items(workspace, json!([])).await
    }

    async fn fake_runtime_with_items(workspace: std::path::PathBuf, items: Value) -> String {
        fake_runtime_with_actions(workspace, items).await.0
    }

    async fn fake_runtime_with_actions(
        workspace: std::path::PathBuf,
        items: Value,
    ) -> (String, GoalActions, Items) {
        fake_runtime_with_assets(workspace, items, LiveGoal::evicted()).await
    }

    async fn fake_runtime_with_live_goal(
        workspace: std::path::PathBuf,
        items: Value,
        goal: Value,
    ) -> (String, GoalActions, Items, LiveGoal) {
        let live = LiveGoal::loaded(goal);
        let (url, actions, items) = fake_runtime_with_assets(workspace, items, live.clone()).await;
        (url, actions, items, live)
    }

    async fn fake_runtime_with_assets(
        workspace: std::path::PathBuf,
        items: Value,
        live_goal: LiveGoal,
    ) -> (String, GoalActions, Items) {
        let goal_actions: GoalActions = Default::default();
        let items: Items = std::sync::Arc::new(std::sync::Mutex::new(items));
        let app = Router::new()
            .route("/v1/threads", get(fake_threads).post(fake_create_thread))
            .route("/v1/threads/summary", get(fake_summary))
            .route(
                "/v1/threads/{id}",
                get(fake_thread).patch(fake_patch_thread),
            )
            .route("/v1/threads/{id}/goal", get(fake_goal).post(fake_goal_post))
            .with_state(FakeRuntime {
                workspace,
                archived: Default::default(),
                items: items.clone(),
                goal_actions: goal_actions.clone(),
                live_goal,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake runtime bind");
        let addr = listener.local_addr().expect("fake runtime addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), goal_actions, items)
    }

    static TEST_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An isolated `CODESMITH_HOME` for one dispatch test. The env var is
    /// process-global, so the guard also serializes the tests that set it.
    struct TestHome {
        _guard: std::sync::MutexGuard<'static, ()>,
        path: std::path::PathBuf,
        previous: Option<String>,
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("CODESMITH_HOME", value),
                None => std::env::remove_var("CODESMITH_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_home(tag: &str) -> TestHome {
        let guard = TEST_HOME_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let path = std::env::temp_dir().join(format!(
            "cds-dispatch-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("test home");
        let previous = std::env::var("CODESMITH_HOME").ok();
        std::env::set_var("CODESMITH_HOME", &path);
        TestHome {
            _guard: guard,
            path,
            previous,
        }
    }

    fn test_host(base_url: String, cwd: std::path::PathBuf) -> SharedHost {
        Arc::new(HostState {
            backend: codesmith::CodesmithProcess::for_test(base_url, "test-token".into()),
            app_handle: None,
            cwd,
            event_sinks: Default::default(),
            live_sinks: Default::default(),
            pending_inputs: Default::default(),
            projection_clock: Default::default(),
            model_choices: Default::default(),
            waterfall_watchers: Default::default(),
            uploads: Default::default(),
            drop_bytes: Default::default(),
            reference_index: Default::default(),
            plan_handoffs: Default::default(),
            plan_return_modes: Default::default(),
            workspace_lock: Default::default(),
            goal_overlays: Default::default(),
            goal_overlays_loaded: Default::default(),
            terminals: Default::default(),
        })
    }

    /// The picker's E2E shape at the bridge: a failed/cancelled native pick
    /// registers nothing, one successful pick adopts exactly one directory,
    /// and the adoption reaches every open `workspace/follow` stream as the
    /// complete baseline the sidebar renders. A second adoption of the same
    /// path is idempotent (one row, `created:false`, no duplicate push).
    #[tokio::test]
    async fn native_picker_adoption_publishes_one_workspace_and_cancel_adds_nothing() {
        let home = test_home("picker-e2e");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let picked = home.path.join("picked");
        std::fs::create_dir_all(&picked).expect("picked");
        let picked = std::fs::canonicalize(&picked).expect("canonical picked");
        let host = test_host(fake_runtime(cwd.clone()).await, cwd.clone());

        // A cancelled chooser (or a bridge-only host) creates nothing: the
        // only row is the implicit default workspace.
        let pick = dispatch(&host, "directoryPicker/pick", &json!({})).await;
        assert_eq!(
            pick["error"]["code"], "directory-picker/unavailable",
            "{pick}"
        );
        let baseline = workspace::baseline(&host).await;
        let rows = json!(baseline.items);
        assert_eq!(
            rows.as_array().map(Vec::len),
            Some(1),
            "cancel must add no row: {rows}"
        );
        assert!(
            rows[0]["path"] != picked.to_string_lossy().as_ref(),
            "cancel must not register the un-picked path"
        );

        // A real pick arrives as an absolute path; the client's adoption is
        // one `workspace/create`, whose reply is the row it inserts.
        let (tx, mut rx) = mpsc::channel(8);
        host.live_sinks
            .lock()
            .await
            .insert("picker-follow".into(), ("workspace/follow".into(), tx));
        let adopted = dispatch(
            &host,
            "workspace/create",
            &json!({ "args": { "request": { "path": picked } } }),
        )
        .await;
        assert_eq!(adopted["value"]["created"], true, "{adopted}");
        assert_eq!(
            adopted["value"]["workspace"]["path"],
            picked.to_string_lossy().as_ref(),
            "{adopted}"
        );
        assert_eq!(
            adopted["value"]["workspace"]["sessionIds"],
            json!([]),
            "the new workspace starts empty"
        );

        // The follow stream gets the full replacement baseline the sidebar
        // renders, and the new row is in it exactly once.
        let frame = rx.recv().await.expect("a workspace/follow replace frame");
        assert_eq!(frame["type"], "replace", "{frame}");
        let rows = frame["value"]["items"].as_array().expect("items");
        let matching: Vec<&Value> = rows
            .iter()
            .filter(|row| row["path"] == picked.to_string_lossy().as_ref())
            .collect();
        assert_eq!(matching.len(), 1, "one row per registered path: {frame}");

        // Re-picking an already registered directory is idempotent.
        let again = dispatch(
            &host,
            "workspace/create",
            &json!({ "args": { "request": { "path": picked } } }),
        )
        .await;
        assert_eq!(again["value"]["created"], false, "{again}");
        assert_eq!(
            again["value"]["workspace"]["workspaceId"],
            adopted["value"]["workspace"]["workspaceId"]
        );
        // The idempotent branch answers the existing row without a second
        // baseline push (nothing changed), so the stream holds no frame.
        assert!(
            rx.try_recv().is_err(),
            "an idempotent adoption must not re-publish the same baseline"
        );
    }

    /// The feedback sidecar's full compare-and-set contract over a fake runtime:
    /// session/target validation, the UUID version fence, idempotent matching
    /// puts, the note/category vocabulary, delete's absent-success, and the
    /// session-level record. These are the paths a second client or a reload
    /// reconciles through, so they are exercised through `dispatch`, not the
    /// store alone.
    #[tokio::test]
    async fn feedback_dispatch_enforces_the_sidecar_contract() {
        let home = test_home("feedback");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let items = json!([
            { "id": "item_msg", "kind": "agent_message", "status": "completed" },
            { "id": "item_tool", "kind": "tool", "status": "completed" },
        ]);
        let host = test_host(fake_runtime_with_items(cwd.clone(), items).await, cwd);

        let request = |value: Value| json!({ "args": { "request": value } });

        // A session the runtime does not know is a business rejection, not
        // an empty list (the client would otherwise stamp nothing).
        let missing = dispatch(
            &host,
            "messageFeedback/list",
            &request(json!({ "sessionId": "thr_missing" })),
        )
        .await;
        assert_eq!(missing["value"]["ok"], false, "{missing}");
        assert_eq!(missing["value"]["error"]["code"], "session-not-found");

        let empty = dispatch(
            &host,
            "messageFeedback/list",
            &request(json!({ "sessionId": "thr_a" })),
        )
        .await;
        assert_eq!(empty["value"]["ok"], true, "{empty}");
        assert_eq!(empty["value"]["value"]["items"], json!([]));

        // A create carries `ifVersion: null`; the reply is the durable item.
        let created = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "rating": "positive",
                "note": "clear answer", "category": "task-result", "ifVersion": null,
            })),
        )
        .await;
        assert_eq!(created["value"]["ok"], true, "{created}");
        let item = created["value"]["value"].clone();
        let version = item["version"].as_str().expect("version").to_string();
        assert_eq!(item["messageId"], "item_msg");
        assert_eq!(item["note"], "clear answer");
        assert_eq!(item["createdAt"], item["updatedAt"]);

        // A second create against the same (now existing) target with a
        // stale null version loses the race and gets the authoritative item.
        let conflict = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "rating": "negative",
                "ifVersion": null,
            })),
        )
        .await;
        assert_eq!(conflict["value"]["ok"], false, "{conflict}");
        assert_eq!(conflict["value"]["error"]["code"], "version-conflict");
        assert_eq!(conflict["value"]["error"]["current"]["version"], version);

        // A matching no-op keeps the version (the client's CAS survives).
        let noop = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "rating": "positive",
                "note": "clear answer", "category": "task-result", "ifVersion": version,
            })),
        )
        .await;
        assert_eq!(noop["value"]["ok"], true, "{noop}");
        assert_eq!(noop["value"]["value"]["version"], version);

        // A real change mints a new version.
        let changed = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "rating": "negative",
                "note": "missed the constraint", "ifVersion": version,
            })),
        )
        .await;
        assert_eq!(changed["value"]["ok"], true, "{changed}");
        let changed_version = changed["value"]["value"]["version"]
            .as_str()
            .expect("changed version")
            .to_string();
        assert_ne!(changed_version, version);
        assert_eq!(changed["value"]["value"]["createdAt"], item["createdAt"]);

        // Only finalized assistant messages are targets.
        let tool_target = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_tool", "rating": "positive",
                "ifVersion": null,
            })),
        )
        .await;
        assert_eq!(tool_target["value"]["error"]["code"], "target-not-found");

        // Vocabulary and size refusals are business failures.
        let blank = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "rating": "positive",
                "note": "   ", "ifVersion": changed_version,
            })),
        )
        .await;
        assert_eq!(blank["value"]["error"]["code"], "note-blank");
        let huge = "x".repeat(8193);
        let too_large = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "rating": "positive",
                "note": huge, "ifVersion": changed_version,
            })),
        )
        .await;
        assert_eq!(too_large["value"]["error"]["code"], "note-too-large");
        assert_eq!(too_large["value"]["error"]["maxBytes"], 8192);
        let category = dispatch(
            &host,
            "messageFeedback/put",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "rating": "positive",
                "category": "bogus", "ifVersion": changed_version,
            })),
        )
        .await;
        assert_eq!(
            category["error"]["code"], "gateway/arguments-invalid",
            "{category}"
        );

        // Delete's version fence, then its idempotent absent success.
        let stale_delete = dispatch(
            &host,
            "messageFeedback/delete",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "ifVersion": version,
            })),
        )
        .await;
        assert_eq!(
            stale_delete["value"]["error"]["code"], "version-conflict",
            "{stale_delete}"
        );
        let deleted = dispatch(
            &host,
            "messageFeedback/delete",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "ifVersion": changed_version,
            })),
        )
        .await;
        assert_eq!(deleted["value"]["value"]["absent"], true, "{deleted}");
        let again = dispatch(
            &host,
            "messageFeedback/delete",
            &request(json!({
                "sessionId": "thr_a", "messageId": "item_msg", "ifVersion": changed_version,
            })),
        )
        .await;
        assert_eq!(again["value"]["value"]["absent"], true, "{again}");
        let empty = dispatch(
            &host,
            "messageFeedback/list",
            &request(json!({ "sessionId": "thr_a" })),
        )
        .await;
        assert_eq!(empty["value"]["value"]["items"], json!([]));

        // The dialog's session-level remark appends to the sidecar.
        let recorded = dispatch(
            &host,
            "sessionFeedback/record",
            &request(json!({
                "sessionId": "thr_a", "text": "  the queue dock helped  ",
                "category": "product-interaction",
            })),
        )
        .await;
        assert_eq!(recorded["value"]["value"]["recorded"], true, "{recorded}");
        let sidecar =
            std::fs::read_to_string(host.settings_home().join("desktop").join("feedback.json"))
                .expect("feedback sidecar");
        let sidecar: Value = serde_json::from_str(&sidecar).expect("sidecar json");
        assert_eq!(
            sidecar["sessions"]["thr_a"]["records"][0]["text"], "the queue dock helped",
            "records trim their text"
        );
    }

    /// A desktop pause outlives the Host process (the engine-eviction case): the
    /// overlay is persisted, a fresh Host hydrates it, a read never loads or
    /// mutates the engine, and the pre-prompt reconcile re-applies the stop to
    /// the freshly loaded goal. An edit supersedes the persisted stop.
    #[tokio::test]
    async fn desktop_goal_pause_survives_a_reload_and_reconciles_before_prompt() {
        let home = test_home("goals-persist");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let snapshot = json!({
            "objective": "ship it", "status": "active", "token_budget": 100,
            "elapsed_seconds": 1, "evidence": null, "blocker": null,
        });
        let items = json!([{
            "id": "item_create",
            "status": "completed",
            "summary": "create_goal",
            "detail": snapshot.to_string(),
            "started_at": "2026-11-11T10:00:00Z",
            "ended_at": "2026-11-11T10:00:05Z",
        }]);
        let (url, actions, _) = fake_runtime_with_actions(cwd.clone(), items.clone()).await;
        let host = test_host(url, cwd.clone());

        // A read never loads an engine and never posts a goal action.
        let read = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(read["value"]["phase"], "active", "{read}");
        assert_eq!(read["value"]["revision"], json!(1), "{read}");
        assert!(
            actions.lock().unwrap().is_empty(),
            "reading a session must not load or mutate the engine"
        );

        // The mutation's compare-and-set: a stale or absent ref is refused
        // (upstream's GOAL_STALE_REVISION guard), the exact current one
        // passes.
        let stale = goals::action(
            &host,
            "goals/pause",
            &json!({ "agentId": "thr_a", "ref": { "id": "goal_item_create", "revision": 99 } }),
        )
        .await;
        assert_eq!(stale["error"]["code"], "goal/stale-revision", "{stale}");
        let missing = goals::action(&host, "goals/pause", &json!({ "agentId": "thr_a" })).await;
        assert_eq!(
            missing["error"]["code"], "gateway/arguments-invalid",
            "{missing}"
        );
        let paused = goals::action(
            &host,
            "goals/pause",
            &json!({ "agentId": "thr_a", "ref": { "id": "goal_item_create", "revision": 1 } }),
        )
        .await;
        assert_eq!(paused["value"]["phase"], "paused", "{paused}");
        assert_eq!(paused["value"]["revision"], json!(2), "{paused}");
        assert_eq!(actions.lock().unwrap().last().unwrap()["action"], "pause");

        // A new Host with the same home is the bridge-restart / engine-reload
        // shape: the overlay is hydrated from `desktop/goals.json`, and the
        // revision survived — the restart-stable ref still passes the CAS.
        let (url2, actions2, _) = fake_runtime_with_actions(cwd.clone(), items).await;
        let reloaded = test_host(url2, cwd);
        let after = goals::get(&reloaded, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(after["value"]["phase"], "paused", "{after}");
        assert_eq!(after["value"]["revision"], json!(2), "{after}");
        assert!(
            actions2.lock().unwrap().is_empty(),
            "the hydrated read must not load an engine"
        );

        // The pre-prompt reconcile re-applies the pause to the loaded engine.
        goals::reconcile(&reloaded, "thr_a").await;
        assert_eq!(
            actions2.lock().unwrap().last().unwrap()["action"],
            "pause",
            "reconcile must post the persisted pause"
        );

        // An edit supersedes the persisted stop; a later reconcile is inert.
        // The ref the reloaded host reports (revision 2 — the paused
        // mutation's bump) is the one that passes.
        let edited = goals::action(
            &reloaded,
            "goals/edit",
            &json!({
                "agentId": "thr_a",
                "ref": { "id": "goal_item_create", "revision": 2 },
                "request": { "objective": "ship it" },
            }),
        )
        .await;
        assert_eq!(edited["value"]["phase"], "active", "{edited}");
        assert_eq!(edited["value"]["revision"], json!(3), "{edited}");
        actions2.lock().unwrap().clear();
        goals::reconcile(&reloaded, "thr_a").await;
        assert!(
            actions2
                .lock()
                .unwrap()
                .iter()
                .all(|body| body["action"] != "pause"),
            "an edit must clear the persisted pause: {:?}",
            actions2.lock().unwrap()
        );

        // A second client holding the pre-edit ref loses the race.
        let raced = goals::action(
            &reloaded,
            "goals/pause",
            &json!({ "agentId": "thr_a", "ref": { "id": "goal_item_create", "revision": 2 } }),
        )
        .await;
        assert_eq!(raced["error"]["code"], "goal/stale-revision", "{raced}");
    }

    /// A goal created through the desktop `/goal` command has no journal item.
    /// After an engine eviction the bridge's persisted command snapshot must
    /// answer the projection from `goals.json`, without loading an engine.
    #[tokio::test]
    async fn goal_command_survives_an_engine_eviction_without_loading() {
        let home = test_home("goals-command-evict");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let (url, actions, _) = fake_runtime_with_actions(cwd.clone(), json!([])).await;
        let host = test_host(url, cwd);

        let ran = dispatch(
            &host,
            "commands/execute",
            &json!({ "args": {
                "agentId": "thr_a",
                "line": "/goal ship it budget: 100",
            } }),
        )
        .await;
        assert_eq!(ran["value"]["result"]["kind"], "success", "{ran}");
        let posted = actions.lock().unwrap().clone();
        assert_eq!(posted.len(), 1, "{posted:?}");
        assert_eq!(posted[0]["action"], "edit", "{posted:?}");
        assert_eq!(posted[0]["objective"], "ship it", "{posted:?}");
        assert_eq!(posted[0]["token_budget"], json!(100), "{posted:?}");

        // The engine stays evicted (the fake's GET answers loaded:false) and
        // a read never loads it: the snapshot in goals.json supplies the bar.
        let read = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(read["value"]["objective"], "ship it", "{read}");
        assert_eq!(read["value"]["tokenBudget"], json!(100), "{read}");
        assert_eq!(read["value"]["phase"], "active", "{read}");
        assert_eq!(read["value"]["id"], "goal_thr_a", "{read}");
        assert_eq!(read["value"]["revision"], json!(2), "{read}");

        let projections = dispatch(
            &host,
            "session/projections",
            &json!({ "args": { "sessionId": "thr_a" } }),
        )
        .await;
        let goal = &projections["value"]["values"]["goal"];
        assert_eq!(goal["goal"]["objective"], "ship it", "{projections}");
        assert_eq!(goal["goal"]["id"], "goal_thr_a", "{projections}");
        assert!(
            goal["goal"]["revision"]
                .as_u64()
                .is_some_and(|revision| revision > 0),
            "{projections}"
        );
        assert_eq!(
            actions.lock().unwrap().len(),
            1,
            "reading a session must not load an engine"
        );
    }

    /// The pre-prompt reconcile re-creates the command goal in the evicted
    /// engine with an idempotent edit, then re-applies the persisted pause.
    #[tokio::test]
    async fn goal_command_reseeds_the_engine_and_reapplies_the_pause() {
        let home = test_home("goals-command-reseed");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let (url, actions, _) = fake_runtime_with_actions(cwd.clone(), json!([])).await;
        let host = test_host(url, cwd);

        let ran = dispatch(
            &host,
            "commands/execute",
            &json!({ "args": {
                "agentId": "thr_a",
                "line": "/goal ship it budget: 100",
            } }),
        )
        .await;
        assert_eq!(ran["value"]["result"]["kind"], "success", "{ran}");

        let paused = goals::action(
            &host,
            "goals/pause",
            &json!({ "agentId": "thr_a", "ref": { "id": "goal_thr_a", "revision": 2 } }),
        )
        .await;
        assert_eq!(paused["value"]["phase"], "paused", "{paused}");

        actions.lock().unwrap().clear();
        goals::reconcile(&host, "thr_a").await;
        let posted = actions.lock().unwrap().clone();
        assert!(
            posted.iter().any(|body| {
                body["action"] == "edit"
                    && body["objective"] == "ship it"
                    && body["token_budget"] == json!(100)
            }),
            "reconcile must re-seed the command goal: {posted:?}"
        );
        assert!(
            posted.iter().any(|body| body["action"] == "pause"),
            "reconcile must re-apply the pause: {posted:?}"
        );
        let read = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(read["value"]["phase"], "paused", "{read}");
        assert_eq!(read["value"]["objective"], "ship it", "{read}");
    }

    /// A bridge restart rehydrates `goals.json`: the command goal keeps its
    /// id and revision, so the ref a bar held before the restart still passes
    /// the compare-and-set while an older one is refused.
    #[tokio::test]
    async fn goal_command_ref_survives_a_bridge_restart() {
        let home = test_home("goals-command-restart");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let (url, _, _) = fake_runtime_with_actions(cwd.clone(), json!([])).await;
        let host = test_host(url, cwd.clone());
        let ran = dispatch(
            &host,
            "commands/execute",
            &json!({ "args": {
                "agentId": "thr_a",
                "line": "/goal ship it budget: 100",
            } }),
        )
        .await;
        assert_eq!(ran["value"]["result"]["kind"], "success", "{ran}");
        let before = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        let id = before["value"]["id"].as_str().expect("goal id").to_string();
        let revision = before["value"]["revision"].as_u64().expect("goal revision");

        // A new Host over the same CODESMITH_HOME is the bridge-restart shape.
        let (url2, actions2, _) = fake_runtime_with_actions(cwd.clone(), json!([])).await;
        let reloaded = test_host(url2, cwd);
        let after = goals::get(&reloaded, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(after["value"]["id"], json!(id), "{after}");
        assert_eq!(after["value"]["revision"], json!(revision), "{after}");
        assert_eq!(after["value"]["objective"], "ship it", "{after}");
        assert!(
            actions2.lock().unwrap().is_empty(),
            "the hydrated read must not load an engine"
        );

        let paused = goals::action(
            &reloaded,
            "goals/pause",
            &json!({ "agentId": "thr_a", "ref": { "id": id, "revision": revision } }),
        )
        .await;
        assert_eq!(paused["value"]["phase"], "paused", "{paused}");
        let stale = goals::action(
            &reloaded,
            "goals/pause",
            &json!({ "agentId": "thr_a", "ref": { "id": "goal_thr_a", "revision": 1 } }),
        )
        .await;
        assert_eq!(stale["error"]["code"], "goal/stale-revision", "{stale}");
    }

    /// A model goal recorded after `/goal` is newer than the command
    /// snapshot, so the journal wins (and its own identity).
    #[tokio::test]
    async fn a_newer_journal_goal_outranks_the_command_snapshot() {
        let home = test_home("goals-command-journal-newer");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let (url, _, items) = fake_runtime_with_actions(cwd.clone(), json!([])).await;
        let host = test_host(url, cwd);
        let ran = dispatch(
            &host,
            "commands/execute",
            &json!({ "args": { "agentId": "thr_a", "line": "/goal ship it" } }),
        )
        .await;
        assert_eq!(ran["value"]["result"]["kind"], "success", "{ran}");
        let read = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(read["value"]["objective"], "ship it", "{read}");

        // The model records a goal after the command; its item is newer.
        let later = (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339();
        items
            .lock()
            .unwrap()
            .as_array_mut()
            .expect("items")
            .push(json!({
                "id": "item_model",
                "status": "completed",
                "summary": "create_goal",
                "detail": json!({
                    "objective": "model goal", "status": "active",
                    "token_budget": null, "elapsed_seconds": 0,
                    "evidence": null, "blocker": null,
                }).to_string(),
                "started_at": later,
                "ended_at": later,
            }));
        let read = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(read["value"]["objective"], "model goal", "{read}");
        assert_eq!(read["value"]["id"], "goal_item_model", "{read}");
    }

    /// A `/goal` after the journal goal is newer than it: the command
    /// snapshot wins, but it keeps the journal's identity so the bar's CAS
    /// ref survives an edit.
    #[tokio::test]
    async fn a_command_newer_than_the_journal_outranks_it() {
        let home = test_home("goals-command-newer");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let items = json!([{
            "id": "item_create",
            "status": "completed",
            "summary": "create_goal",
            "detail": json!({
                "objective": "journal goal", "status": "active",
                "token_budget": 500, "elapsed_seconds": 0,
                "evidence": null, "blocker": null,
            }).to_string(),
            "started_at": "2026-11-11T10:00:00Z",
            "ended_at": "2026-11-11T10:00:05Z",
        }]);
        let (url, _, _) = fake_runtime_with_actions(cwd.clone(), items).await;
        let host = test_host(url, cwd);
        let ran = dispatch(
            &host,
            "commands/execute",
            &json!({ "args": {
                "agentId": "thr_a",
                "line": "/goal command goal budget: 100",
            } }),
        )
        .await;
        assert_eq!(ran["value"]["result"]["kind"], "success", "{ran}");
        let read = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(read["value"]["objective"], "command goal", "{read}");
        assert_eq!(read["value"]["tokenBudget"], json!(100), "{read}");
        // The journal still supplies the identity and the revision base.
        assert_eq!(read["value"]["id"], "goal_item_create", "{read}");
        assert_eq!(read["value"]["revision"], json!(2), "{read}");
    }

    /// A terminal command goal survives an eviction but is never re-seeded as
    /// active, and a cleared one stays absent.
    #[tokio::test]
    async fn terminal_goal_commands_are_not_resurrected() {
        let home = test_home("goals-command-terminal");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let (url, actions, _, live) = fake_runtime_with_live_goal(
            cwd.clone(),
            json!([]),
            json!({
                "objective": "ship it", "status": "active", "token_budget": 100,
                "elapsed_seconds": 0, "evidence": null, "blocker": null,
                "rounds_started": 0, "max_goal_rounds": 25,
            }),
        )
        .await;
        let host = test_host(url, cwd);
        let ran = dispatch(
            &host,
            "commands/execute",
            &json!({ "args": {
                "agentId": "thr_a",
                "line": "/goal ship it budget: 100",
            } }),
        )
        .await;
        assert_eq!(ran["value"]["result"]["kind"], "success", "{ran}");
        let active = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(active["value"]["phase"], "active", "{active}");
        assert_eq!(active["value"]["revision"], json!(2), "{active}");

        // The model completes the goal; the loaded read is what makes that
        // phase durable in the command snapshot.
        live.set(Some(json!({
            "objective": "ship it", "status": "complete", "token_budget": 100,
            "elapsed_seconds": 9, "evidence": "done", "blocker": null,
            "rounds_started": 3, "max_goal_rounds": 25,
        })));
        let completed = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(completed["value"]["phase"], "complete", "{completed}");

        live.evict();
        let evicted = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(evicted["value"]["phase"], "complete", "{evicted}");
        actions.lock().unwrap().clear();
        goals::reconcile(&host, "thr_a").await;
        assert!(
            actions
                .lock()
                .unwrap()
                .iter()
                .all(|body| body["action"] != "edit"),
            "a complete goal must not be re-seeded: {:?}",
            actions.lock().unwrap()
        );

        // Blocked is terminal as well.
        live.set(Some(json!({
            "objective": "ship it", "status": "blocked", "token_budget": 100,
            "elapsed_seconds": 12, "evidence": null,
            "blocker": "needs input", "rounds_started": 3, "max_goal_rounds": 25,
        })));
        let blocked = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(blocked["value"]["phase"], "blocked", "{blocked}");
        assert_eq!(
            blocked["value"]["blockedReason"]["message"], "needs input",
            "{blocked}"
        );
        live.evict();
        let evicted = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert_eq!(evicted["value"]["phase"], "blocked", "{evicted}");
        assert_eq!(
            evicted["value"]["blockedReason"]["message"], "needs input",
            "{evicted}"
        );
        actions.lock().unwrap().clear();
        goals::reconcile(&host, "thr_a").await;
        assert!(
            actions
                .lock()
                .unwrap()
                .iter()
                .all(|body| body["action"] != "edit"),
            "a blocked goal must not be re-seeded: {:?}",
            actions.lock().unwrap()
        );

        // Clear suppresses the projection and the re-seed.
        let cleared = goals::action(
            &host,
            "goals/clear",
            &json!({ "agentId": "thr_a", "ref": { "id": "goal_thr_a", "revision": 2 } }),
        )
        .await;
        assert_eq!(cleared["ok"], true, "{cleared}");
        let gone = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert!(gone.get("value").is_none(), "{gone}");
        actions.lock().unwrap().clear();
        goals::reconcile(&host, "thr_a").await;
        let posted = actions.lock().unwrap().clone();
        assert!(
            posted.iter().all(|body| body["action"] != "edit"),
            "a cleared goal must not be re-seeded: {posted:?}"
        );
        let gone = goals::get(&host, &json!({ "agentId": "thr_a" })).await;
        assert!(gone.get("value").is_none(), "{gone}");
    }

    /// `session/projections` answers the same sequenced envelope a follow
    /// snapshot carries, so the vendored controller's blank-restore and
    /// reconnect reads seed the same values the stream would.
    #[tokio::test]
    async fn session_projections_matches_the_follow_snapshot_shape() {
        use serde_json::json;
        let home = test_home("session-projections");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let host = test_host(fake_runtime(cwd.clone()).await, cwd);
        let value = dispatch(
            &host,
            "session/projections",
            &json!({ "args": { "sessionId": "thr_a" } }),
        )
        .await;
        assert_eq!(value["value"]["kind"], "sequenced", "{value}");
        assert!(
            value["value"]["asOfSeq"]
                .as_u64()
                .is_some_and(|seq| seq > 0),
            "the projection write carries a control seq: {value}"
        );
        // The value set is the per-session baseline: title/plan/queue and
        // the other computed keys, never the sidebar's whole-session map.
        let values = &value["value"]["values"];
        assert!(values.get("title").is_some(), "{value}");
        assert!(values.get("plan").is_some(), "{value}");
        assert!(values.get("inbox").is_some(), "{value}");
        // An unknown thread is an RPC failure, not an empty value set.
        let missing = dispatch(
            &host,
            "session/projections",
            &json!({ "args": { "sessionId": "thr_missing" } }),
        )
        .await;
        assert_eq!(missing["error"]["code"], "gateway/internal", "{missing}");
    }

    /// The coordinator ticket board rides `session/projections` as the
    /// `agentTeam` value the shared-task panel reads: the bridge reads the
    /// workspace's durable ticket store without loading an engine, and maps
    /// reports to the panel's status vocabulary.
    #[tokio::test]
    async fn ticket_board_projects_from_the_durable_coordinator_store() {
        let home = test_home("ticket-board");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(cwd.join(".codesmith/state")).expect("ticket state dir");
        std::fs::write(
            cwd.join(".codesmith/state/tickets.v1.json"),
            br#"{
                "schema_version": 1,
                "tickets": [
                    { "id": "worker-a", "session_name": "anvil",
                      "title": "Ship the board", "scope": "Build the shared-task panel",
                      "reports": [{ "state": "progress", "summary": "working" }] },
                    { "id": "worker-b", "session_name": "forge",
                      "title": "Review it", "scope": "Check the wire shape",
                      "reports": [{ "state": "ready", "summary": "done" }] },
                    { "id": "worker-c", "session_name": "lathe",
                      "title": "Stuck", "scope": "Needs a human",
                      "reports": [{ "state": "blocked", "summary": "blocked" }] }
                ]
            }"#,
        )
        .expect("ticket store");
        let host = test_host(fake_runtime(cwd.clone()).await, cwd);
        let value = dispatch(
            &host,
            "session/projections",
            &json!({ "args": { "sessionId": "thr_a" } }),
        )
        .await;
        let team = &value["value"]["values"]["agentTeam"];
        assert_eq!(team["enabled"], json!(true), "{value}");
        assert_eq!(
            team["tasks"][0]["subject"],
            json!("Ship the board"),
            "{value}"
        );
        assert_eq!(team["tasks"][0]["status"], json!("in_progress"), "{value}");
        assert_eq!(team["tasks"][0]["ownerName"], json!("anvil"), "{value}");
        assert_eq!(team["tasks"][1]["status"], json!("completed"), "{value}");
        assert_eq!(team["tasks"][2]["status"], json!("pending"), "{value}");
        assert_eq!(team["tasks"][2]["ready"], json!(false), "{value}");
    }

    /// The vendored Workspace remote's value shapes, through dispatch: a
    /// bare row or a null-wrapped value is a client-side TypeError, not a
    /// graceful error state.
    #[tokio::test]
    async fn workspace_dispatch_answers_the_vendored_value_shapes() {
        let home = test_home("workspace");
        let cwd = home.path.join("cwd");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let workspace = home.path.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let workspace = std::fs::canonicalize(&workspace).expect("canonical workspace");
        let host = test_host(fake_runtime(workspace.clone()).await, cwd);

        let init = dispatch(&host, "workspace/initializeDefault", &json!({})).await;
        assert_eq!(init["ok"], true, "{init}");
        assert_eq!(
            init["value"]["workspace"]["workspaceId"],
            DEFAULT_WORKSPACE_ID
        );
        assert!(init["value"].get("workspace").is_some(), "{init}");

        let created = dispatch(
            &host,
            "workspace/create",
            &json!({ "args": { "request": { "path": workspace } } }),
        )
        .await;
        assert_eq!(created["value"]["created"], true, "{created}");
        let workspace_id = created["value"]["workspace"]["workspaceId"]
            .as_str()
            .expect("workspace id")
            .to_string();
        assert_eq!(
            created["value"]["workspace"]["sessionIds"],
            json!(["thr_a", "thr_b"])
        );

        let moved = dispatch(
            &host,
            "workspace/insertSessionBefore",
            &json!({ "args": { "request": {
                "workspaceId": workspace_id,
                "sessionId": "thr_b",
                "beforeSessionId": "thr_a",
            } } }),
        )
        .await;
        assert_eq!(moved["ok"], true, "{moved}");
        assert_eq!(
            moved["value"]["workspace"]["sessionIds"],
            json!(["thr_b", "thr_a"]),
            "insertSessionBefore answers the updated WorkspaceValue"
        );

        let order = dispatch(
            &host,
            "workspace/insertBefore",
            &json!({ "args": { "request": {
                "workspaceId": workspace_id,
                "beforeWorkspaceId": DEFAULT_WORKSPACE_ID,
            } } }),
        )
        .await;
        assert_eq!(order["value"]["workspaceIds"][0], workspace_id, "{order}");

        let renamed = dispatch(
            &host,
            "workspace/rename",
            &json!({ "args": { "request": {
                "workspaceId": workspace_id,
                "title": "Renamed",
            } } }),
        )
        .await;
        assert_eq!(
            renamed["value"]["workspace"]["title"], "Renamed",
            "{renamed}"
        );

        let pinned = dispatch(
            &host,
            "workspace/pinSession",
            &json!({ "args": { "request": { "sessionId": "thr_a" } } }),
        )
        .await;
        assert_eq!(
            pinned["value"]["pinnedSessionIds"],
            json!(["thr_a"]),
            "{pinned}"
        );
        let unpinned = dispatch(
            &host,
            "workspace/unpinSession",
            &json!({ "args": { "request": { "sessionId": "thr_a" } } }),
        )
        .await;
        assert_eq!(
            unpinned["value"]["pinnedSessionIds"],
            json!([]),
            "{unpinned}"
        );

        let deleted = dispatch(
            &host,
            "workspace/delete",
            &json!({ "args": { "request": { "workspaceId": workspace_id } } }),
        )
        .await;
        assert_eq!(deleted["value"]["deleted"], true, "{deleted}");

        let list = dispatch(&host, "session/list", &json!({})).await;
        let row = &list["value"]["items"][0];
        assert_eq!(row["formatStatus"], "current", "{list}");
        assert_eq!(row["cwd"], workspace.to_string_lossy().as_ref(), "{list}");
        assert_eq!(row["blank"], true, "{list}");

        let session = dispatch(
            &host,
            "session/create",
            &json!({ "args": { "request": { "workspaceId": DEFAULT_WORKSPACE_ID } } }),
        )
        .await;
        assert_eq!(session["value"]["sessionId"], "thr_new", "{session}");

        // A bridge-only host has no native chooser; it must say so rather
        // than claim a picker opened.
        let pick = dispatch(&host, "directoryPicker/pick", &json!({})).await;
        assert_eq!(
            pick["error"]["code"], "directory-picker/unavailable",
            "{pick}"
        );

        let archived = dispatch(
            &host,
            "workspace/archiveSession",
            &json!({ "args": { "request": { "sessionId": "thr_a" } } }),
        )
        .await;
        assert_eq!(
            archived["value"]["archivedSessionIds"],
            json!(["thr_a"]),
            "{archived}"
        );
        // `unarchiveSession` ends with `archiveSession`; an exact-match
        // dispatch must not archive it again.
        let unarchived = dispatch(
            &host,
            "workspace/unarchiveSession",
            &json!({ "args": { "request": { "sessionId": "thr_a" } } }),
        )
        .await;
        assert_eq!(
            unarchived["value"]["archivedSessionIds"],
            json!([]),
            "{unarchived}"
        );
    }

    // === Terminal host tests (host::terminal over a real PTY) ===

    /// The first frame matching `pred` within 10s, keeping every other frame
    /// (they are still asserted by the callers that care).
    async fn terminal_frame_matching(
        rx: &mut mpsc::Receiver<Value>,
        mut pred: impl FnMut(&Value) -> bool,
    ) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout_at(deadline, rx.recv()).await.ok()??;
            if pred(&frame) {
                return Some(frame);
            }
        }
    }

    async fn terminal_create(host: &SharedHost, session: &str, id: &str) -> Value {
        dispatch(
            host,
            "terminal/create",
            &json!({ "args": { "agentId": session, "request": {
                "id": id, "cols": 80, "rows": 24, "shellPath": "/bin/sh",
            } } }),
        )
        .await
    }

    fn spawn_attach(
        host: &SharedHost,
        session: &str,
        id: &str,
    ) -> (
        mpsc::Receiver<Value>,
        mpsc::Sender<mux::UplinkFrame>,
        tokio::task::JoinHandle<Result<(), Value>>,
    ) {
        let (tx, rx) = mpsc::channel(64);
        let (up_tx, up_rx) = mpsc::channel(16);
        let host = host.clone();
        let payload = json!({ "args": { "agentId": session, "id": id } });
        let task = tokio::spawn(async move {
            open_stream(&host, "terminal/attach", &payload, tx, up_rx).await
        });
        (rx, up_tx, task)
    }

    /// The full lifecycle over a real shell: create spawns, the attachment's
    /// first frame is the snapshot with input control, a written command
    /// echoes back as ordered output, close ends the attachment, and a
    /// closed identity can never be recreated.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_create_streams_output_and_close_is_final() {
        let home = test_home("terminal-lifecycle");
        let workspace = home.path.clone();
        let base = fake_runtime(workspace.clone()).await;
        let host = test_host(base, workspace);

        let environment = dispatch(
            &host,
            "terminal/environment",
            &json!({ "args": { "agentId": "thr_a" } }),
        )
        .await;
        assert_eq!(environment["value"]["maxCols"], json!(500), "{environment}");
        assert_eq!(
            environment["value"]["scrollback"],
            json!(1000),
            "{environment}"
        );

        let created = terminal_create(&host, "thr_a", "t1").await;
        assert_eq!(created["ok"], json!(true), "{created}");
        assert_eq!(created["value"]["state"], json!("running"), "{created}");
        assert_eq!(created["value"]["title"], json!("sh"), "{created}");
        assert_eq!(created["value"]["exitCode"], Value::Null, "{created}");

        // Idempotent create: the same identity returns the same terminal.
        let again = terminal_create(&host, "thr_a", "t1").await;
        assert_eq!(again["ok"], json!(true), "{again}");

        let (mut rx, up_tx, attach) = spawn_attach(&host, "thr_a", "t1");
        let snapshot = rx.recv().await.expect("snapshot frame");
        assert_eq!(snapshot["type"], json!("snapshot"), "{snapshot}");
        assert_eq!(snapshot["sequence"], json!(0), "{snapshot}");
        assert_eq!(snapshot["info"]["id"], json!("t1"), "{snapshot}");
        assert_eq!(snapshot["writable"], json!(true), "{snapshot}");

        up_tx
            .send(mux::UplinkFrame::Item(json!({
                "type": "write", "sequence": 1, "data": "echo cds_term_probe\r",
            })))
            .await
            .unwrap();
        let ack = terminal_frame_matching(&mut rx, |frame| frame["type"] == "ack").await;
        let ack = ack.expect("ack for the write");
        assert_eq!(ack["sequence"], json!(1), "{ack}");
        assert!(ack.get("refusal").is_none(), "{ack}");
        let output = terminal_frame_matching(&mut rx, |frame| {
            frame["type"] == "output"
                && frame["data"]
                    .as_str()
                    .is_some_and(|d| d.contains("cds_term_probe"))
        })
        .await;
        assert!(output.is_some(), "the echoed command never came back");

        let listed = dispatch(
            &host,
            "terminal/list",
            &json!({ "args": { "sessionId": "thr_a" } }),
        )
        .await;
        assert_eq!(
            listed["value"].as_array().map(Vec::len),
            Some(1),
            "{listed}"
        );

        // Close ends the attachment (a delivered `end`, not a failure)…
        let closed = dispatch(
            &host,
            "terminal/close",
            &json!({ "args": { "agentId": "thr_a", "id": "t1" } }),
        )
        .await;
        assert_eq!(closed["ok"], json!(true), "{closed}");
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), attach)
            .await
            .expect("the attachment ends after close")
            .expect("attach task joinable");
        assert_eq!(outcome, Ok(()));

        // …repeated closes succeed, the registry is empty, and the identity
        // is closed for good.
        let closed_again = dispatch(
            &host,
            "terminal/close",
            &json!({ "args": { "agentId": "thr_a", "id": "t1" } }),
        )
        .await;
        assert_eq!(closed_again["ok"], json!(true), "{closed_again}");
        let listed = dispatch(
            &host,
            "terminal/list",
            &json!({ "args": { "sessionId": "thr_a" } }),
        )
        .await;
        assert_eq!(listed["value"], json!([]), "{listed}");
        let recreated = terminal_create(&host, "thr_a", "t1").await;
        assert_eq!(
            recreated["error"]["code"], "terminal/unavailable",
            "{recreated}"
        );
    }

    /// Input control is exclusive to the newest attachment (older ones ack
    /// `read-only`), and a sequence gap fails the attachment without
    /// stopping the shell.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_attach_controller_and_sequence_discipline() {
        let home = test_home("terminal-discipline");
        let workspace = home.path.clone();
        let base = fake_runtime(workspace.clone()).await;
        let host = test_host(base, workspace);
        assert_eq!(
            terminal_create(&host, "thr_a", "t1").await["ok"],
            json!(true)
        );

        // The first attachment starts writable…
        let (mut ra, up_a, attach_a) = spawn_attach(&host, "thr_a", "t1");
        let snapshot = ra.recv().await.expect("snapshot a");
        assert_eq!(snapshot["writable"], json!(true), "{snapshot}");

        // …the second takes control, demoting the first with a state frame.
        let (mut rb, up_b, attach_b) = spawn_attach(&host, "thr_a", "t1");
        let demoted = terminal_frame_matching(&mut ra, |frame| {
            frame["type"] == "state" && frame["writable"] == json!(false)
        })
        .await;
        assert!(demoted.is_some(), "the first attachment was never demoted");

        // The demoted attachment's write is refused read-only…
        up_a.send(mux::UplinkFrame::Item(json!({
            "type": "write", "sequence": 1, "data": "true\r",
        })))
        .await
        .unwrap();
        let refusal = terminal_frame_matching(&mut ra, |frame| frame["type"] == "ack").await;
        let refusal = refusal.expect("ack for the demoted write");
        assert_eq!(refusal["sequence"], json!(1), "{refusal}");
        assert_eq!(refusal["refusal"], json!("read-only"), "{refusal}");

        // …the controller's write acks cleanly…
        up_b.send(mux::UplinkFrame::Item(json!({
            "type": "write", "sequence": 1, "data": "true\r",
        })))
        .await
        .unwrap();
        let ack = terminal_frame_matching(&mut rb, |frame| frame["type"] == "ack").await;
        let ack = ack.expect("ack for the controller write");
        assert_eq!(ack["sequence"], json!(1), "{ack}");
        assert!(ack.get("refusal").is_none(), "{ack}");

        // …and a sequence gap fails only that attachment. A failed
        // controller is not promoted back (upstream's rule), so the demoted
        // attachment stays read-only…
        up_b.send(mux::UplinkFrame::Item(json!({
            "type": "write", "sequence": 3, "data": "true\r",
        })))
        .await
        .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), attach_b)
            .await
            .expect("the gap ends the attachment")
            .expect("attach task joinable");
        assert_eq!(outcome.unwrap_err()["code"], "gateway/internal");
        up_a.send(mux::UplinkFrame::Item(json!({
            "type": "write", "sequence": 2, "data": "true\r",
        })))
        .await
        .unwrap();
        let refusal = terminal_frame_matching(&mut ra, |frame| {
            frame["type"] == "ack" && frame["sequence"] == json!(2)
        })
        .await;
        let refusal = refusal.expect("ack for the demoted write");
        assert_eq!(refusal["refusal"], json!("read-only"), "{refusal}");

        // …while the shell itself is unharmed: a fresh attachment takes
        // control, writes, and sees the output.
        let (mut rc, up_c, attach_c) = spawn_attach(&host, "thr_a", "t1");
        let snapshot = rc.recv().await.expect("snapshot c");
        assert_eq!(snapshot["writable"], json!(true), "{snapshot}");
        up_c.send(mux::UplinkFrame::Item(json!({
            "type": "write", "sequence": 1, "data": "echo cds_term_alive\r",
        })))
        .await
        .unwrap();
        let alive = terminal_frame_matching(&mut rc, |frame| {
            frame["type"] == "output"
                && frame["data"]
                    .as_str()
                    .is_some_and(|d| d.contains("cds_term_alive"))
        })
        .await;
        assert!(alive.is_some(), "the shell died with the failed attachment");
        attach_a.abort();
        attach_c.abort();
    }

    /// Identity rules and the per-session quota: malformed ids never spawn,
    /// the ninth terminal is refused with the limit, and closing frees the
    /// slot's identity forever.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_limit_and_identity_rules() {
        let home = test_home("terminal-limits");
        let workspace = home.path.clone();
        let base = fake_runtime(workspace.clone()).await;
        let host = test_host(base, workspace);

        let bad_id = dispatch(
            &host,
            "terminal/create",
            &json!({ "args": { "agentId": "thr_a", "request": {
                "id": "not allowed", "cols": 80, "rows": 24,
            } } }),
        )
        .await;
        assert_eq!(
            bad_id["error"]["code"], "gateway/arguments-invalid",
            "{bad_id}"
        );

        let bad_dims = dispatch(
            &host,
            "terminal/create",
            &json!({ "args": { "agentId": "thr_a", "request": {
                "id": "t0", "cols": 1, "rows": 24,
            } } }),
        )
        .await;
        assert_eq!(
            bad_dims["error"]["code"], "gateway/arguments-invalid",
            "{bad_dims}"
        );

        for index in 0..8 {
            let created = terminal_create(&host, "thr_a", &format!("t{index}")).await;
            assert_eq!(created["ok"], json!(true), "terminal {index}: {created}");
        }
        let ninth = terminal_create(&host, "thr_a", "t8").await;
        assert_eq!(ninth["error"]["code"], "terminal/limit-reached", "{ninth}");

        // The quota is per session.
        let other = terminal_create(&host, "thr_b", "t8").await;
        assert_eq!(other["ok"], json!(true), "{other}");

        // A rename publishes a valid title; an empty one is refused.
        let renamed = dispatch(
            &host,
            "terminal/rename",
            &json!({ "args": { "agentId": "thr_a", "id": "t0", "title": "  build  " } }),
        )
        .await;
        assert_eq!(renamed["ok"], json!(true), "{renamed}");
        let listed = dispatch(
            &host,
            "terminal/list",
            &json!({ "args": { "sessionId": "thr_a" } }),
        )
        .await;
        let title = listed["value"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == json!("t0"))
            .map(|row| row["title"].clone());
        assert_eq!(title, Some(json!("build")), "{listed}");
        let bad_title = dispatch(
            &host,
            "terminal/rename",
            &json!({ "args": { "agentId": "thr_a", "id": "t0", "title": "   " } }),
        )
        .await;
        assert_eq!(
            bad_title["error"]["code"], "gateway/arguments-invalid",
            "{bad_title}"
        );

        // Unknown terminals answer the unavailable error on every route.
        let missing = dispatch(
            &host,
            "terminal/rename",
            &json!({ "args": { "agentId": "thr_a", "id": "nope", "title": "x" } }),
        )
        .await;
        assert_eq!(
            missing["error"]["code"], "terminal/unavailable",
            "{missing}"
        );
    }

    // === Office preview conversion tests (host::office) ===

    /// `officeToPdf/render` answers the workspace fence + the wire value
    /// shape, and an absent engine answers the unavailable reason (the
    /// panel's localized unavailable state) instead of pretending.
    #[cfg(unix)]
    #[tokio::test]
    async fn office_render_converts_and_reports_an_absent_engine() {
        static SOFFICE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let home = test_home("office-render");
        let workspace = home.path.clone();
        let base = fake_runtime(workspace.clone()).await;
        let host = test_host(base, workspace.clone());

        let soffice = workspace.join("soffice-fake");
        std::fs::write(
            &soffice,
            "#!/bin/sh\nout=\nprev=\nfor a in \"$@\"; do\n  if [ \"$prev\" = \"--outdir\" ]; then out=\"$a\"; fi\n  prev=\"$a\"\ndone\nprintf '%%PDF-1.4\\nbody\\n%%%%EOF\\n' > \"$out/source.pdf\"\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&soffice, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(workspace.join("doc.docx"), b"pretend docx").unwrap();

        let _guard = SOFFICE_LOCK.lock().await;
        std::env::set_var("CODESMITH_DESKTOP_SOFFICE", &soffice);
        let rendered = office::render(
            &host,
            &json!({ "workspaceFileScopeId": "thr_a", "path": "doc.docx", "priority": "foreground" }),
        )
        .await;
        std::env::set_var("CODESMITH_DESKTOP_SOFFICE", workspace.join("missing"));
        let unavailable = office::render(
            &host,
            &json!({ "workspaceFileScopeId": "thr_a", "path": "doc.docx", "priority": "background" }),
        )
        .await;
        std::env::remove_var("CODESMITH_DESKTOP_SOFFICE");
        drop(_guard);

        let (value, bytes) = rendered.expect("render succeeds");
        assert!(bytes.starts_with(b"%PDF-"), "{bytes:?}");
        assert_eq!(value["offset"], json!(0), "{value}");
        assert_eq!(value["eof"], json!(true), "{value}");
        assert_eq!(value["bytes"], json!(bytes.len()), "{value}");
        assert_eq!(value["missingFonts"], json!([]), "{value}");
        assert_eq!(value["generation"], json!(office::generation_value()));
        assert!(
            value["absolutePath"]
                .as_str()
                .unwrap()
                .ends_with("doc.docx"),
            "{value}"
        );

        let error = unavailable.expect_err("a missing engine is unavailable");
        assert_eq!(error["error"]["code"], "document-render/failed", "{error}");
        assert_eq!(
            error["error"]["details"]["reason"],
            json!("unavailable"),
            "{error}"
        );

        // A non-office extension is refused before any engine is consulted.
        std::fs::write(workspace.join("note.txt"), b"text").unwrap();
        let refused = office::render(
            &host,
            &json!({ "workspaceFileScopeId": "thr_a", "path": "note.txt" }),
        )
        .await
        .expect_err("non-office extensions are refused");
        assert_eq!(
            refused["error"]["details"]["reason"],
            json!("unsupported-format"),
            "{refused}"
        );

        // The generation RPC is a stable per-process value.
        let generation = dispatch(&host, "officeToPdf/generation", &json!({ "args": {} })).await;
        assert_eq!(
            generation["value"],
            json!(office::generation_value()),
            "{generation}"
        );
    }
}
