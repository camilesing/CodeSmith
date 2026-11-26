//! Transport contract tests: the real `bridge_router` bound to an ephemeral
//! port, driven by a real HTTP and WebSocket client the way the vendored UI
//! drives it — the layer the in-process `dispatch()` tests never touch.
//!
//! The fake runtime here mirrors the runtime API's routes one-to-one
//! (threads, queue/steer/interrupt, SSE events, approvals, user-input, tasks,
//! automations, models, goal) so the bridge's runtime-half runs over real
//! TCP too. Mapper-fed surfaces (session/page replay, session/follow) are
//! covered by the real-backend E2E lane instead — a hand-drawn journal would
//! only pin a fixture's guess at the durable item shapes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;

use super::{bridge_router, HostState, SharedHost};

// === The scripted fake runtime ===

/// One automation PATCH or DELETE the fake runtime recorded: (method, id, body).
type AutomationEdit = (&'static str, String, Value);

#[derive(Clone)]
struct FakeRuntime {
    workspace: std::path::PathBuf,
    /// The thread detail's turns (active-turn resolution for steer/interrupt).
    turns: Arc<StdMutex<Vec<Value>>>,
    /// Every `POST /v1/threads/{id}/queue` body, in order.
    queue_posts: Arc<StdMutex<Vec<Value>>>,
    /// Every steer and interrupt POST, in order, tagged by kind.
    turn_posts: Arc<StdMutex<Vec<(&'static str, Value)>>>,
    /// Every approval decision POST, as (approval id, decision).
    approvals: Arc<StdMutex<Vec<(String, String)>>>,
    /// The background tasks store.
    tasks: Arc<StdMutex<Vec<Value>>>,
    /// Automation run POSTs, in order.
    automation_runs: Arc<StdMutex<Vec<String>>>,
    /// The stored automations, as `GET /v1/automations` lists them.
    automations: Arc<StdMutex<Vec<Value>>>,
    /// Every automation PATCH and DELETE, as (method, id, body).
    automation_edits: Arc<StdMutex<Vec<AutomationEdit>>>,
    /// Sub-agent continuation inputs and cancels, as (path suffix, body).
    subagent_posts: Arc<StdMutex<Vec<(&'static str, String)>>>,
    /// Every raw query string `GET /v1/threads/summary` received.
    summary_queries: Arc<StdMutex<Vec<String>>>,
    /// Feed of SSE `data:` lines handed to every events subscriber.
    events_tx: tokio::sync::broadcast::Sender<String>,
    /// How many SSE event subscribers are attached right now.
    watchers: Arc<AtomicUsize>,
    /// Every `POST /v1/presets/apply` body, in order.
    preset_applies: Arc<StdMutex<Vec<Value>>>,
    /// Fork POSTs so far; the child `thr_fork` lists once one landed.
    forks: Arc<AtomicUsize>,
    /// Test gate for `GET /v1/threads` (the first read of a `workspace/follow`
    /// baseline): while `gated` is set the handler signals `entered` and
    /// waits for `release`.
    gated: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl FakeRuntime {
    fn forked(&self) -> bool {
        self.forks.load(Ordering::SeqCst) > 0
    }

    fn new(workspace: std::path::PathBuf) -> Self {
        let (events_tx, _) = tokio::sync::broadcast::channel::<String>(64);
        Self {
            workspace,
            turns: Default::default(),
            queue_posts: Default::default(),
            turn_posts: Default::default(),
            approvals: Default::default(),
            tasks: Default::default(),
            automation_runs: Default::default(),
            automations: Default::default(),
            automation_edits: Default::default(),
            subagent_posts: Default::default(),
            summary_queries: Default::default(),
            events_tx,
            watchers: Default::default(),
            preset_applies: Default::default(),
            forks: Default::default(),
            gated: Default::default(),
            entered: Default::default(),
            release: Default::default(),
        }
    }

    /// Broadcast one runtime event envelope on the SSE feed.
    fn emit(&self, envelope: Value) {
        let _ = self.events_tx.send(envelope.to_string());
    }

    /// Wait until the waterfall watcher (or any client) holds an SSE feed.
    async fn await_watcher(&self) {
        for _ in 0..150 {
            if self.watchers.load(Ordering::SeqCst) > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no SSE watcher attached within 3s");
    }

    fn add_task(&self, task: Value) {
        self.tasks.lock().unwrap().push(task);
    }

    fn add_automation(&self, automation: Value) {
        self.automations.lock().unwrap().push(automation);
    }
}

fn fake_thread_detail(fake: &FakeRuntime, id: &str) -> Value {
    json!({
        "thread": { "id": id, "workspace": fake.workspace.to_string_lossy(), "mode": "agent" },
        "turns": fake.turns.lock().unwrap().clone(),
        "items": [],
        "latest_seq": 0,
    })
}

fn fake_runtime_router() -> Router<FakeRuntime> {
    Router::new()
        .route(
            "/v1/threads",
            get(|State(fake): State<FakeRuntime>| async move {
                if fake.gated.load(Ordering::SeqCst) {
                    fake.entered.notify_one();
                    fake.release.notified().await;
                }
                let mut threads =
                    vec![json!({ "id": "thr_a", "workspace": fake.workspace.to_string_lossy() })];
                if fake.forked() {
                    threads.push(
                        json!({ "id": "thr_fork", "workspace": fake.workspace.to_string_lossy() }),
                    );
                }
                Json(threads)
            }),
        )
        .route(
            "/v1/threads/summary",
            get(
                |(State(fake), axum::extract::RawQuery(query)): (
                    State<FakeRuntime>,
                    axum::extract::RawQuery,
                )| async move {
                    fake.summary_queries
                        .lock()
                        .unwrap()
                        .push(query.unwrap_or_default());
                    let mut summaries =
                        vec![json!({ "id": "thr_a", "updated_at": "2026-11-15T00:00:00Z" })];
                    if fake.forked() {
                        summaries.push(
                            json!({ "id": "thr_fork", "updated_at": "2026-11-15T00:00:01Z" }),
                        );
                    }
                    Json(summaries)
                },
            ),
        )
        .route(
            "/v1/threads/{id}",
            get(
                |State(fake): State<FakeRuntime>, Path(id): Path<String>| async move {
                    if id == "thr_missing" {
                        return axum::http::StatusCode::NOT_FOUND.into_response();
                    }
                    Json(fake_thread_detail(&fake, &id)).into_response()
                },
            )
            .patch(|Json(body): Json<Value>| async move { Json(body).into_response() }),
        )
        .route(
            "/v1/threads/{id}/fork",
            post(|State(fake): State<FakeRuntime>| async move {
                fake.forks.fetch_add(1, Ordering::SeqCst);
                Json(json!({ "id": "thr_fork" }))
            }),
        )
        .route(
            "/v1/threads/{id}/queue",
            post(
                |State(fake): State<FakeRuntime>, Json(body): Json<Value>| async move {
                    fake.queue_posts.lock().unwrap().push(body.clone());
                    Json(json!({ "status": "started", "turn": { "id": "turn_1" } }))
                },
            ),
        )
        .route(
            "/v1/threads/{id}/turns/{turn}/steer",
            post(
                |State(fake): State<FakeRuntime>, Json(body): Json<Value>| async move {
                    fake.turn_posts.lock().unwrap().push(("steer", body));
                    Json(json!({}))
                },
            ),
        )
        .route(
            "/v1/threads/{id}/turns/{turn}/interrupt",
            post(
                |State(fake): State<FakeRuntime>, Json(body): Json<Value>| async move {
                    fake.turn_posts.lock().unwrap().push(("interrupt", body));
                    Json(json!({ "ok": true }))
                },
            ),
        )
        .route(
            "/v1/threads/{id}/events",
            get(|State(fake): State<FakeRuntime>| async move {
                fake.watchers.fetch_add(1, Ordering::SeqCst);
                let rx = fake.events_tx.subscribe();
                let stream = futures_util::stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.ok().map(|line| {
                        let event: Result<Event, std::convert::Infallible> =
                            Ok(Event::default().data(line));
                        (event, rx)
                    })
                });
                Sse::new(stream)
            }),
        )
        .route(
            "/v1/approvals/{id}",
            post(
                |State(fake): State<FakeRuntime>,
                 Path(id): Path<String>,
                 Json(body): Json<Value>| {
                    async move {
                        fake.approvals.lock().unwrap().push((
                            id,
                            body.get("decision")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        ));
                        Json(json!(true))
                    }
                },
            ),
        )
        .route(
            "/v1/user-input/{thread}/{id}",
            post(|Json(body): Json<Value>| async move {
                let _ = body;
                Json(json!(true))
            }),
        )
        .route(
            "/v1/threads/{id}/goal",
            get(|| async move { Json(json!({ "loaded": false, "goal": null })) }).post(
                |Json(body): Json<Value>| async move {
                    let _ = body;
                    Json(json!({}))
                },
            ),
        )
        .route(
            "/v1/models",
            get(|| async move {
                Json(json!([
                    { "id": "m1", "provider": "p1", "default": true },
                    { "id": "m2", "provider": "p1", "default": false },
                    { "id": "m3", "provider": "p2", "default": false },
                ]))
            }),
        )
        .route(
            "/v1/presets",
            get(|| async move {
                Json(json!({
                    "tiers": [{ "name": "middle", "description": "", "source": "builtin" }],
                    "selection": {
                        "selected": "middle", "selectedSource": "default",
                        "effective": "middle", "deviated": []
                    },
                    "keys": [], "fileTree": {}, "restartPending": false, "warnings": []
                }))
            }),
        )
        .route(
            "/v1/presets/apply",
            post(
                |State(fake): State<FakeRuntime>, Json(body): Json<Value>| async move {
                    fake.preset_applies.lock().unwrap().push(body);
                    Json(json!({ "applied": true }))
                },
            ),
        )
        .route(
            "/v1/tasks",
            get(|State(fake): State<FakeRuntime>| async move {
                // The runtime's real envelope (`runtime_api::TasksResponse`).
                Json(json!({
                    "tasks": fake.tasks.lock().unwrap().clone(),
                    "counts": {},
                }))
            }),
        )
        .route(
            "/v1/tasks/{id}",
            get(
                |State(fake): State<FakeRuntime>, Path(id): Path<String>| async move {
                    match fake
                        .tasks
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|t| t["id"] == json!(id))
                    {
                        Some(task) => Json(task.clone()).into_response(),
                        None => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                },
            ),
        )
        .route(
            "/v1/tasks/{id}/cancel",
            post(
                |State(fake): State<FakeRuntime>, Path(id): Path<String>| async move {
                    let mut tasks = fake.tasks.lock().unwrap();
                    if let Some(task) = tasks.iter_mut().find(|t| t["id"] == json!(id)) {
                        task["status"] = json!("canceled");
                    }
                    Json(json!({ "ok": true }))
                },
            ),
        )
        .route(
            "/v1/subagents/{id}/input",
            post(
                |State(fake): State<FakeRuntime>,
                 Path(id): Path<String>,
                 Json(body): Json<Value>| async move {
                    fake.subagent_posts
                        .lock()
                        .unwrap()
                        .push(("input", body.to_string()));
                    Json(json!({ "accepted": true, "id": id }))
                },
            ),
        )
        .route(
            "/v1/subagents/{id}/cancel",
            post(
                |State(fake): State<FakeRuntime>, Path(id): Path<String>| async move {
                    let _ = id;
                    fake.subagent_posts
                        .lock()
                        .unwrap()
                        .push(("cancel", "{}".into()));
                    Json(json!({ "accepted": true }))
                },
            ),
        )
        .route(
            "/v1/automations",
            get(|State(fake): State<FakeRuntime>| async move {
                Json(fake.automations.lock().unwrap().clone())
            }),
        )
        // Patch semantics: a present field replaces, and the revision moves.
        .route(
            "/v1/automations/{id}",
            patch(
                |State(fake): State<FakeRuntime>,
                 Path(id): Path<String>,
                 Json(body): Json<Value>| async move {
                    let mut automations = fake.automations.lock().unwrap();
                    let Some(automation) = automations.iter_mut().find(|a| a["id"] == id) else {
                        return axum::http::StatusCode::NOT_FOUND.into_response();
                    };
                    fake.automation_edits
                        .lock()
                        .unwrap()
                        .push(("PATCH", id, body.clone()));
                    for key in ["name", "prompt", "rrule", "cwds", "status"] {
                        if let Some(value) = body.get(key) {
                            automation[key] = value.clone();
                        }
                    }
                    automation["updated_at"] = json!("2026-11-15T00:00:02Z");
                    Json(automation.clone()).into_response()
                },
            )
            .delete(
                |State(fake): State<FakeRuntime>, Path(id): Path<String>| async move {
                    let mut automations = fake.automations.lock().unwrap();
                    let Some(at) = automations.iter().position(|a| a["id"] == id) else {
                        return axum::http::StatusCode::NOT_FOUND.into_response();
                    };
                    fake.automation_edits
                        .lock()
                        .unwrap()
                        .push(("DELETE", id, Value::Null));
                    // The runtime answers with the record it removed.
                    Json(automations.remove(at)).into_response()
                },
            ),
        )
        .route(
            "/v1/automations/{id}/run",
            post(
                |State(fake): State<FakeRuntime>, Path(id): Path<String>| async move {
                    fake.automation_runs.lock().unwrap().push(id);
                    Json(json!({ "ok": true }))
                },
            ),
        )
        .route(
            "/v1/automations/{id}/runs",
            get(|| async move {
                // The runtime's real `AutomationRunRecord`: no prompt, and
                // the run's own thread id.
                Json(json!([{
                    "id": "run_1",
                    "scheduled_for": "2026-11-15T09:00:00Z",
                    "ended_at": "2026-11-15T09:00:04Z",
                    "thread_id": "thr_a",
                }]))
            }),
        )
}

// === The harness ===

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

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
    // The same lock the dispatch tests take: CODESMITH_HOME is
    // process-global and the test binary runs modules in parallel.
    let guard = super::tests::TEST_HOME_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let path = std::env::temp_dir().join(format!(
        "cds-transport-{tag}-{}-{}",
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

/// One fake runtime + one bridge, both on ephemeral loopback ports:
/// (home guard, http client, bridge base url, runtime state, host, workspace).
async fn spawn_stack(
    tag: &str,
) -> (
    TestHome,
    reqwest::Client,
    String,
    Arc<FakeRuntime>,
    SharedHost,
    std::path::PathBuf,
) {
    let home = test_home(tag);
    let workspace =
        std::env::temp_dir().join(format!("cds-transport-ws-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let fake = Arc::new(FakeRuntime::new(workspace.clone()));
    let runtime_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("runtime bind");
    let runtime_addr = runtime_listener.local_addr().expect("runtime addr");
    let runtime_app = fake_runtime_router().with_state((*fake).clone());
    tokio::spawn(async move {
        let _ = axum::serve(runtime_listener, runtime_app).await;
    });

    let host: SharedHost = Arc::new(HostState {
        backend: super::codesmith::CodesmithProcess::for_test(
            format!("http://{runtime_addr}"),
            "test-token".into(),
        ),
        app_handle: None,
        cwd: workspace.clone(),
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
    });
    let bridge_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bridge bind");
    let bridge_addr = bridge_listener.local_addr().expect("bridge addr");
    let serve_host = Arc::clone(&host);
    tokio::spawn(async move {
        let _ = axum::serve(bridge_listener, bridge_router(serve_host)).await;
    });

    (
        home,
        reqwest::Client::new(),
        format!("http://{bridge_addr}"),
        fake,
        host,
        workspace,
    )
}

/// A page that resolves its own hostname to `127.0.0.1` sends a same-origin
/// fetch whose `Host` is that name; a foreign page's POST carries its own
/// `Origin`. CORS labels responses but never rejects a request — the gate
/// does, before any route runs.
#[tokio::test]
async fn http_routes_refuse_a_foreign_host_and_a_foreign_origin() {
    let (_home, client, base, _fake, _host, _workspace) = spawn_stack("host-gate").await;
    let port = base.rsplit(':').next().expect("port").to_string();
    let args = json!({ "args": { "request": { "sessionId": "thr_a" } } });

    let rebinding = client
        .post(format!("{base}/api/job/list"))
        .header("host", format!("attacker.example:{port}"))
        .json(&args)
        .send()
        .await
        .expect("request");
    assert_eq!(rebinding.status(), reqwest::StatusCode::FORBIDDEN);

    let foreign = client
        .post(format!("{base}/api/job/list"))
        .header("origin", "https://attacker.example")
        .json(&args)
        .send()
        .await
        .expect("request");
    assert_eq!(foreign.status(), reqwest::StatusCode::FORBIDDEN);

    // The page's own origins, and a local client with no origin at all, pass.
    let mut admitted = 0;
    for origin in ["tauri://localhost", "http://127.0.0.1:1420", ""] {
        let mut request = client.post(format!("{base}/api/job/list")).json(&args);
        if !origin.is_empty() {
            request = request.header("origin", origin);
        }
        let response = request.send().await.expect("request");
        assert_ne!(
            response.status(),
            reqwest::StatusCode::FORBIDDEN,
            "origin {origin:?}"
        );
        admitted += 1;
    }
    assert_eq!(admitted, 3);
}

/// POST one unary bridge RPC and return the full envelope.
pub(super) async fn rpc(
    client: &reqwest::Client,
    base: &str,
    endpoint: &str,
    args: Value,
) -> Value {
    client
        .post(format!("{base}/api/{endpoint}"))
        .json(&json!({ "args": args }))
        .send()
        .await
        .expect("bridge request")
        .json::<Value>()
        .await
        .expect("bridge json")
}

/// Open a mux WebSocket and send one `open` frame for `endpoint`.
pub(super) async fn open_mux_stream(
    base: &str,
    stream_id: &str,
    endpoint: &str,
    payload: Value,
) -> Ws {
    let url = format!("{}/api/remote.mux", base.replacen("http://", "ws://", 1));
    let request: Request<()> = url.into_client_request().expect("ws request");
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("ws connect");
    ws.send(Message::Text(
        json!({ "type": "open", "streamId": stream_id, "endpoint": endpoint, "payload": payload })
            .to_string()
            .into(),
    ))
    .await
    .expect("open frame");
    ws
}

/// The next mux frame as JSON, failing on timeout.
pub(super) async fn next_frame(ws: &mut Ws) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("frame within 5s")
        .expect("stream open")
        .expect("ws ok");
    match message {
        Message::Text(text) => serde_json::from_str(text.as_str()).expect("frame json"),
        Message::Close(frame) => panic!("unexpected close: {frame:?}"),
        other => panic!("unexpected frame: {other:?}"),
    }
}

/// Read frames until the socket closes; return the close code.
async fn close_code(mut ws: Ws) -> u16 {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("frame within 5s")
            .expect("stream open")
            .expect("ws ok");
        if let Message::Close(Some(frame)) = message {
            return frame.code.into();
        }
    }
}

// === Lane A: the HTTP/WS transport contract ===

#[tokio::test]
async fn health_answers_ok() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("health").await;
    let response = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "ok");
    drop(home);
}

#[tokio::test]
async fn unknown_method_answers_method_unavailable_envelope() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("unknown").await;
    let value = rpc(&client, &base, "does/not/exist", json!({})).await;
    assert_eq!(value["ok"], json!(false));
    assert_eq!(value["error"]["code"], json!("gateway/method-unavailable"));
    drop(home);
}

#[tokio::test]
async fn malformed_json_body_is_rejected_not_hung() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("badjson").await;
    let response = client
        .post(format!("{base}/api/session/list"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    // A parse failure must fail the request loudly (4xx), never fall through
    // as a silent Null payload.
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    drop(home);
}

#[tokio::test]
async fn cors_preflight_admits_page_origins_only() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("cors").await;
    for origin in super::PAGE_ORIGINS {
        let response = client
            .request(reqwest::Method::OPTIONS, format!("{base}/api/session/list"))
            .header("Origin", origin)
            .header("Access-Control-Request-Method", "POST")
            .send()
            .await
            .unwrap();
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_some_and(|value| value == origin.as_bytes()),
            "page origin {origin} must be allowed"
        );
    }
    let response = client
        .request(reqwest::Method::OPTIONS, format!("{base}/api/session/list"))
        .header("Origin", "https://evil.example")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "a foreign origin must not be allowed"
    );
    drop(home);
}

#[tokio::test]
async fn mux_upgrade_admits_originless_and_rejects_foreign_origin() {
    let (home, _client, base, _fake, _host, _ws) = spawn_stack("muxorigin").await;
    // Originless (a non-browser local client) upgrades fine.
    let mut ws = open_mux_stream(&base, "s1", "$events", json!({ "args": {} })).await;
    let ready = next_frame(&mut ws).await;
    assert_eq!(ready["value"]["type"], json!("ready"));
    // A foreign origin is refused before the upgrade.
    let url = format!("{}/api/remote.mux", base.replacen("http://", "ws://", 1));
    let mut request: Request<()> = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let refused = tokio_tungstenite::connect_async(request).await;
    assert!(refused.is_err(), "foreign origin must not upgrade");
    drop(home);
}

#[tokio::test]
async fn mux_streams_carry_baselines_and_stream_errors() {
    let (home, _client, base, _fake, _host, _ws) = spawn_stack("muxbase").await;
    // A parked stream answers its baseline and stays open; the signed-out
    // account shape is one of the static baselines.
    let mut ws = open_mux_stream(&base, "s1", "account/watch", json!({})).await;
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["type"], json!("item"));
    assert_eq!(frame["streamId"], json!("s1"));
    assert_eq!(frame["value"]["status"], json!("signed-out"));
    // An unimplemented stream fails its own stream with the client-visible
    // error shape; the socket itself stays up.
    ws.send(Message::Text(
        json!({ "type": "open", "streamId": "s2", "endpoint": "bogus/stream", "payload": {} })
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let failure = next_frame(&mut ws).await;
    assert_eq!(failure["type"], json!("error"));
    assert_eq!(failure["streamId"], json!("s2"));
    assert_eq!(
        failure["error"]["code"],
        json!("gateway/method-unavailable")
    );
    drop(home);
}

#[tokio::test]
async fn mux_duplicate_stream_id_and_binary_frames_close_the_socket() {
    let (home, _client, base, _fake, _host, _ws) = spawn_stack("muxclose").await;
    // Duplicate stream id → 1008.
    let mut ws = open_mux_stream(&base, "dup", "$events", json!({ "args": {} })).await;
    let _ = next_frame(&mut ws).await; // ready
    ws.send(Message::Text(
        json!({ "type": "open", "streamId": "dup", "endpoint": "$events", "payload": {} })
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(close_code(ws).await, 1008);

    // A binary frame → 1003.
    let mut ws = open_mux_stream(&base, "bin", "$events", json!({ "args": {} })).await;
    let _ = next_frame(&mut ws).await; // ready
    ws.send(Message::Binary(vec![0u8].into())).await.unwrap();
    assert_eq!(close_code(ws).await, 1003);
    drop(home);
}

#[tokio::test]
async fn oversized_body_is_refused() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("toolarge").await;
    let body = vec![b'{'; 17 * 1024 * 1024];
    let response = client
        .post(format!("{base}/api/session/list"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    drop(home);
}

#[tokio::test]
async fn upload_route_stages_image_bytes_and_prompt_consumes_them() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("upload").await;
    // The binary route takes images only (plain files ride the base64 unary
    // route) — feed it a real 1×1 PNG.
    const PNG_1X1: [u8; 67] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let envelope: Value = client
        .post(format!(
            "{base}/api/session/uploadFileBinary?sessionId=thr_a&name=shot.png&mediaType=image/png"
        ))
        .body(PNG_1X1.to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(envelope["ok"], json!(true), "upload envelope: {envelope}");
    let receipt = envelope["value"]["receiptId"].as_str().unwrap().to_string();

    // A prompt naming the receipt delivers it as a runtime attachment.
    let value = rpc(
        &client,
        &base,
        "session/prompt",
        json!({ "request": {
            "sessionId": "thr_a",
            "content": [
                { "type": "text", "text": "look at this" },
                { "type": "file", "receiptId": receipt },
            ],
            "requestId": "rpc-77",
        }}),
    )
    .await;
    assert_eq!(value["ok"], json!(true), "prompt envelope: {value}");
    let posts = fake.queue_posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["prompt"], json!("look at this"));
    assert_eq!(posts[0]["rpc_id"], json!("rpc-77"));
    assert!(
        posts[0]["attachments"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "the staged receipt must ride the queue POST: {posts:?}"
    );
    drop(home);
}

#[tokio::test]
async fn drop_bytes_route_serves_exactly_once() {
    let (home, client, base, _fake, host, _ws) = spawn_stack("drop").await;
    let id = uuid::Uuid::new_v4().to_string();
    host.drop_bytes
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(id.clone(), vec![1, 2, 3, 0xff]);
    let served = client
        .get(format!("{base}/api/host/dropBytes/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(served.status(), reqwest::StatusCode::OK);
    assert_eq!(served.bytes().await.unwrap().to_vec(), vec![1, 2, 3, 0xff]);
    let spent = client
        .get(format!("{base}/api/host/dropBytes/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(spent.status(), reqwest::StatusCode::NOT_FOUND);
    drop(home);
}

#[tokio::test]
async fn read_bytes_answers_a_multipart_binary_response() {
    let (home, client, base, _fake, _host, workspace) = spawn_stack("readbytes").await;
    std::fs::write(
        workspace.join("blob.bin"),
        [0x89, b'P', b'N', b'G', 0x00, 0xfe],
    )
    .unwrap();
    let response = client
        .post(format!("{base}/api/workspaceFiles/readBytes"))
        .json(&json!({ "args": { "workspaceFileScopeId": "thr_a", "path": "blob.bin" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("multipart/form-data; boundary="));
    let body = response.bytes().await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("name=\"metadata\""),
        "metadata part missing: {body}"
    );
    assert!(
        body.contains("name=\"bytes-0\"; filename=\"bytes-0.bin\""),
        "binary file part missing: {body}"
    );
    let metadata = body
        .split("\r\n--")
        .next()
        .unwrap()
        .split("\r\n\r\n")
        .nth(1)
        .unwrap();
    let metadata: Value = serde_json::from_str(metadata).unwrap();
    assert_eq!(metadata["attachments"][0]["part"], json!("bytes-0"));
    drop(home);
}

#[tokio::test]
async fn workspace_files_changes_stream_reports_writes() {
    let (home, _client, base, _fake, _host, workspace) = spawn_stack("wfchanges").await;
    std::fs::write(workspace.join("watched.txt"), "v1\n").unwrap();
    let mut ws = open_mux_stream(
        &base,
        "s1",
        "workspaceFiles/changes",
        json!({ "args": { "workspaceFileScopeId": "thr_a", "path": "watched.txt" } }),
    )
    .await;
    let ready = next_frame(&mut ws).await;
    assert_eq!(ready["value"]["kind"], json!("ready"));
    tokio::fs::write(workspace.join("watched.txt"), "v2 with more bytes\n")
        .await
        .unwrap();
    let change = next_frame(&mut ws).await;
    assert_eq!(change["value"]["kind"], json!("change"));
    assert!(
        change["value"]["change"]["absolutePath"]
            .as_str()
            .is_some_and(|path| path.ends_with("watched.txt")),
        "change frame: {change}"
    );
    drop(home);
}

// === Lane B: behavior of the zero-coverage endpoints, over the transport ===

/// Drive one approval round end to end and assert the runtime decision.
async fn approval_round(
    client: &reqwest::Client,
    base: &str,
    fake: &FakeRuntime,
    outcome: &str,
    want_decision: &str,
) {
    // Arm the watcher via a prompt, then raise the approval on the feed.
    let value = rpc(
        client,
        base,
        "session/prompt",
        json!({ "request": { "sessionId": "thr_a", "content": [{ "type": "text", "text": "写文件" }] } }),
    )
    .await;
    assert_eq!(value["ok"], json!(true), "prompt: {value}");
    fake.await_watcher().await;

    let mut ws = open_mux_stream(base, "ev", "$events", json!({ "args": {} })).await;
    let ready = next_frame(&mut ws).await;
    assert_eq!(ready["value"]["type"], json!("ready"));
    let client_id = ready["value"]["clientId"].as_str().unwrap().to_string();

    fake.emit(json!({
        "schema_version": 1, "seq": 7, "thread_id": "thr_a",
        "event": "approval.required",
        "payload": { "id": "appr_1", "tool_name": "write_file", "description": "write out.txt" },
    }));
    let waterfall = next_frame(&mut ws).await;
    assert_eq!(waterfall["type"], json!("item"));
    assert_eq!(waterfall["value"]["type"], json!("waterfall"));
    assert_eq!(waterfall["value"]["eventId"], json!("appr_1"));
    assert_eq!(
        waterfall["value"]["request"]["toolName"],
        json!("write_file")
    );

    let answered = rpc(
        client,
        base,
        "$events/result",
        json!({
            "clientId": client_id,
            "eventId": "appr_1",
            "outcome": { "kind": "result", "value": outcome },
        }),
    )
    .await;
    assert_eq!(answered["ok"], json!(true), "result envelope: {answered}");
    assert_eq!(answered["value"]["delivered"], json!(true));
    assert_eq!(answered["value"]["decision"], json!(want_decision));
    let decisions = fake.approvals.lock().unwrap().clone();
    assert_eq!(
        decisions.last(),
        Some(&("appr_1".to_string(), want_decision.to_string())),
        "runtime decisions: {decisions:?}"
    );
    // The deciding client's card retires: the cancel frame lands on the
    // still-open $events stream.
    let cancel = next_frame(&mut ws).await;
    assert_eq!(cancel["value"]["type"], json!("cancel"));
    assert_eq!(cancel["value"]["eventId"], json!("appr_1"));
}

#[tokio::test]
async fn approval_roundtrip_allow_deny_and_unclaimed_reach_the_runtime() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("approve").await;
    approval_round(&client, &base, &fake, "allowed-once", "allow").await;
    approval_round(&client, &base, &fake, "rejected", "deny").await;
    approval_round(&client, &base, &fake, "next", "deny").await;
    drop(home);
}

#[tokio::test]
async fn events_result_for_unknown_event_is_an_idempotent_noop() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("noop").await;
    let value = rpc(
        &client,
        &base,
        "$events/result",
        json!({
            "clientId": "any",
            "eventId": "never-seen",
            "outcome": { "kind": "result", "value": "allowed-once" },
        }),
    )
    .await;
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["value"]["delivered"], json!(false));
    // A missing clientId is an argument error, not a silent pass.
    let missing = rpc(
        &client,
        &base,
        "$events/result",
        json!({ "eventId": "x", "outcome": null }),
    )
    .await;
    assert_eq!(missing["error"]["code"], json!("gateway/arguments-invalid"));
    drop(home);
}

#[tokio::test]
async fn pending_waterfall_is_redelivered_to_a_late_stream_and_settled_by_the_runtime() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("redeliver").await;
    let value = rpc(
        &client,
        &base,
        "session/prompt",
        json!({ "request": { "sessionId": "thr_a", "content": [{ "type": "text", "text": "hi" }] } }),
    )
    .await;
    assert_eq!(value["ok"], json!(true));
    fake.await_watcher().await;
    // The approval fires while NO $events stream is open.
    fake.emit(json!({
        "schema_version": 1, "seq": 9, "thread_id": "thr_a",
        "event": "approval.required",
        "payload": { "id": "appr_late", "tool_name": "bash", "description": "ls" },
    }));
    // Give the watcher a beat to register the pending input.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // A stream connecting afterwards still sees it (upstream re-delivers
    // pending waterfalls to every connecting client).
    let mut ws = open_mux_stream(&base, "ev", "$events", json!({ "args": {} })).await;
    let _ready = next_frame(&mut ws).await;
    let waterfall = next_frame(&mut ws).await;
    assert_eq!(waterfall["value"]["type"], json!("waterfall"));
    assert_eq!(waterfall["value"]["eventId"], json!("appr_late"));
    // The runtime settling it (decided elsewhere / timeout) cancels the
    // card on the live stream.
    fake.emit(json!({
        "schema_version": 1, "seq": 10, "thread_id": "thr_a",
        "event": "approval.timeout",
        "payload": { "id": "appr_late" },
    }));
    let cancel = next_frame(&mut ws).await;
    assert_eq!(cancel["value"]["type"], json!("cancel"));
    assert_eq!(cancel["value"]["eventId"], json!("appr_late"));
    drop(home);
}

#[tokio::test]
async fn jobs_roster_follow_and_kill_map_onto_runtime_tasks() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("jobs").await;
    fake.add_task(json!({
        "id": "task_run", "thread_id": "thr_a", "status": "running",
        "prompt": "long job\nsecond line", "created_at": "2026-11-15T08:00:00Z",
        "timeline": [
            { "kind": "message", "summary": "partial " },
            { "kind": "message", "summary": "answer" },
            { "kind": "tool_progress", "summary": "bash: ls" },
        ],
    }));
    fake.add_task(json!({
        "id": "task_done", "thread_id": "thr_a", "status": "completed",
        "prompt_summary": "quick job", "created_at": "2026-11-15T08:01:00Z",
        "ended_at": "2026-11-15T08:01:05Z", "result_summary": "done",
        "timeline": [{ "kind": "message", "summary": "hello" }],
    }));
    fake.add_task(json!({
        "id": "task_foreign", "thread_id": "thr_other", "status": "running",
        "prompt": "not mine", "created_at": "2026-11-15T08:02:00Z",
    }));

    // The roster lists only this session's tasks, as whole-set rows.
    let mut roster = open_mux_stream(
        &base,
        "roster",
        "job/list",
        json!({ "args": { "request": { "sessionId": "thr_a" } } }),
    )
    .await;
    let rows = next_frame(&mut roster).await;
    assert_eq!(rows["value"]["type"], json!("rows"), "roster frame: {rows}");
    let jobs = rows["value"]["jobs"].as_array().expect("jobs");
    assert_eq!(jobs.len(), 2, "foreign task must be excluded: {jobs:?}");
    let running = jobs.iter().find(|j| j["id"] == json!("task_run")).unwrap();
    assert_eq!(running["status"], json!("running"));
    assert_eq!(running["progress"], json!("bash: ls"));
    let done = jobs.iter().find(|j| j["id"] == json!("task_done")).unwrap();
    assert_eq!(done["status"], json!("completed"));
    assert_eq!(done["label"], json!("quick job"), "a summary's label field");
    assert_eq!(done["detail"], json!("done"));
    assert!(done["finishedAt"].as_i64().unwrap() > 0);

    // Following a settled task ends with the anchor→output→status shape.
    let mut follow = open_mux_stream(
        &base,
        "tail",
        "job/follow",
        json!({ "args": { "request": { "jobId": "task_done" } } }),
    )
    .await;
    let opened = next_frame(&mut follow).await;
    assert_eq!(opened["value"]["type"], json!("opened"));
    let output = next_frame(&mut follow).await;
    assert_eq!(output["value"]["type"], json!("output"));
    assert_eq!(output["value"]["chunks"][0]["text"], json!("hello\n"));
    let status = next_frame(&mut follow).await;
    assert_eq!(status["value"]["type"], json!("status"));
    assert_eq!(status["value"]["job"]["status"], json!("completed"));

    // Kill a running task; a second kill reads already-finished.
    let killed = rpc(
        &client,
        &base,
        "job/kill",
        json!({ "request": { "jobId": "task_run" } }),
    )
    .await;
    assert_eq!(killed["value"]["outcome"], json!("requested"));
    let again = rpc(
        &client,
        &base,
        "job/kill",
        json!({ "request": { "jobId": "task_run" } }),
    )
    .await;
    assert_eq!(again["value"]["outcome"], json!("already-finished"));
    let unknown = rpc(
        &client,
        &base,
        "job/kill",
        json!({ "request": { "jobId": "task_nope" } }),
    )
    .await;
    assert_eq!(unknown["error"]["code"], json!("job/not-found"));
    drop(home);
}

#[tokio::test]
async fn model_catalog_groups_by_runtime_provider() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("catalog").await;
    let value = rpc(&client, &base, "session/modelCatalog", json!({})).await;
    assert_eq!(value["ok"], json!(true), "catalog: {value}");
    let catalog = &value["value"];
    assert_eq!(catalog["default"]["provider"], json!("p1"));
    assert_eq!(catalog["default"]["model"], json!("m1"));
    assert_eq!(catalog["routableProviders"], json!(["p1", "p2"]));
    let groups = catalog["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["id"], json!("p1"));
    assert_eq!(groups[0]["models"].as_array().unwrap().len(), 2);
    drop(home);
}

#[tokio::test]
async fn prompt_queues_steers_and_interrupts_through_the_runtime() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("prompt").await;
    // No active turn: a queue prompt lands on the queue route.
    let value = rpc(
        &client,
        &base,
        "session/prompt",
        json!({ "request": {
            "sessionId": "thr_a",
            "content": [{ "type": "text", "text": "first" }],
            "requestId": "rpc-1",
        }}),
    )
    .await;
    assert_eq!(value["value"]["accepted"], json!(true));
    assert_eq!(
        fake.queue_posts.lock().unwrap().last().unwrap()["prompt"],
        json!("first")
    );

    // With a running turn, steer rides the active turn, cancel interrupts it.
    *fake.turns.lock().unwrap() = vec![json!({ "id": "turn_9", "status": "in_progress" })];
    let value = rpc(
        &client,
        &base,
        "session/prompt",
        json!({ "request": {
            "sessionId": "thr_a",
            "mode": "steer",
            "content": [{ "type": "text", "text": "nudge" }],
        }}),
    )
    .await;
    assert_eq!(value["value"]["accepted"], json!(true));
    let posts = fake.turn_posts.lock().unwrap().clone();
    assert!(
        posts
            .iter()
            .any(|(kind, body)| *kind == "steer" && body["prompt"] == json!("nudge")),
        "steer posts: {posts:?}"
    );

    let value = rpc(
        &client,
        &base,
        "session/cancel",
        json!({ "sessionId": "thr_a" }),
    )
    .await;
    assert_eq!(value["ok"], json!(true), "cancel: {value}");
    let posts = fake.turn_posts.lock().unwrap().clone();
    assert!(
        posts.iter().any(|(kind, _)| *kind == "interrupt"),
        "interrupt posts: {posts:?}"
    );
    drop(home);
}

#[tokio::test]
async fn session_fork_and_search_shape() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("forksearch").await;
    // The client files a fork child as a hidden blank placeholder until the
    // Host's `api-session/added` carries its real row (found in the desktop:
    // the sidebar only listed the child after a reload).
    let mut events_ws = open_mux_stream(&base, "e1", "$events", json!({ "args": {} })).await;
    let _ready = next_frame(&mut events_ws).await; // ready frame
    let forked = rpc(
        &client,
        &base,
        "session/fork",
        json!({ "sessionId": "thr_a" }),
    )
    .await;
    assert_eq!(forked["ok"], json!(true), "fork: {forked}");
    assert_eq!(forked["value"]["sessionId"], json!("thr_fork"));
    let added = wait_emit(&mut events_ws, "api-session/added").await;
    let row = &added["value"]["args"][0];
    assert_eq!(row["sessionId"], json!("thr_fork"), "{added}");
    assert_eq!(
        row["cwd"],
        json!(fake.workspace.to_string_lossy()),
        "the child lists under its workspace: {added}"
    );
    let found = rpc(
        &client,
        &base,
        "session/search",
        json!({ "query": "anything" }),
    )
    .await;
    assert_eq!(found["ok"], json!(true), "search: {found}");
    // The fake's summary search lists every thread it knows, the forked
    // child included; each row is the summary's id plus its preview snippet.
    let rows = found["value"]["items"].as_array().expect("search rows");
    let mut ids: Vec<&str> = rows
        .iter()
        .map(|row| row["sessionId"].as_str().expect("session id"))
        .collect();
    ids.sort();
    assert_eq!(ids, ["thr_a", "thr_fork"], "search rows: {found}");
    assert!(
        rows.iter().all(|row| row.get("snippet").is_some()),
        "search rows: {found}"
    );
    // Found in the desktop: the term was interpolated before it was
    // percent-encoded, so a query holding `&` reached the runtime cut at
    // that character (`R&D` searched for `r`).
    let found = rpc(&client, &base, "session/search", json!({ "query": "R&D" })).await;
    assert_eq!(found["ok"], json!(true), "{found}");
    let queries = fake.summary_queries.lock().unwrap().clone();
    assert_eq!(
        queries
            .iter()
            .filter(|query| query.starts_with("search="))
            .cloned()
            .collect::<Vec<_>>(),
        ["search=anything&limit=50", "search=R%26D&limit=50"],
        "the term is one query component"
    );
    drop(home);
}

#[tokio::test]
async fn schedule_run_and_history_map_onto_automations() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("schedule").await;
    let run = rpc(
        &client,
        &base,
        "schedule/run",
        json!({ "request": { "id": "auto_1" } }),
    )
    .await;
    assert_eq!(run["value"], json!({ "id": "auto_1", "triggered": true }));
    assert_eq!(fake.automation_runs.lock().unwrap().as_slice(), ["auto_1"]);
    let history = rpc(
        &client,
        &base,
        "schedule/history",
        json!({ "request": { "id": "auto_1" } }),
    )
    .await;
    assert_eq!(history["ok"], json!(true), "history: {history}");
    let records = history["value"]["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["messageId"], json!("run_1"));
    assert_eq!(
        records[0]["targetSessionId"],
        json!("thr_a"),
        "the run's session"
    );
    assert!(
        records[0].get("prompt").is_none(),
        "no prompt to show is an absent field, never null: {records:?}"
    );
    drop(home);
}

/// Found in the desktop: every task detail warned "could not load the run
/// interval limit" — the bridge answered retention numbers where the page's
/// schema requires `newSessionIntervalSeconds`, a number.
#[tokio::test]
async fn schedule_delivery_limits_carry_the_page_field() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("schedule-limits").await;
    let limits = rpc(&client, &base, "schedule/deliveryLimits", json!({})).await;
    assert_eq!(
        limits["value"]["newSessionIntervalSeconds"],
        json!(0),
        "{limits}"
    );
    drop(home);
}

/// Found in the desktop: the task page's pause, resume, edit and delete all
/// answered "edit without a change" — the page nests its change in `patch`
/// and states its intent in `action`, and the bridge read neither.
#[tokio::test]
async fn schedule_edit_applies_the_page_patch_and_delete() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("schedule-edit").await;
    let other = fake.workspace.join("other");
    std::fs::create_dir_all(&other).expect("other project dir");
    fake.add_automation(json!({
        "id": "auto_1", "name": "Tidy", "prompt": "run tidy",
        "rrule": "FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=0",
        "cwds": [fake.workspace.to_string_lossy()],
        "status": "active",
        "created_at": "2026-11-15T00:00:00Z", "updated_at": "2026-11-15T00:00:01Z",
        "next_run_at": "2026-11-16T09:00:00Z",
    }));
    let edit = |request: Value| {
        rpc(
            &client,
            &base,
            "schedule/edit",
            json!({ "request": request }),
        )
    };
    // Pause and resume carry the page's `patch.active` and the revision it read.
    let paused = edit(json!({
        "id": "auto_1", "action": "update", "patch": { "active": false },
        "expectedUpdatedAt": "2026-11-15T00:00:01Z",
    }))
    .await;
    assert_eq!(
        paused["value"]["status"],
        json!("paused"),
        "pause: {paused}"
    );
    let resumed = edit(json!({
        "id": "auto_1", "action": "update", "patch": { "active": true },
        "expectedUpdatedAt": "2026-11-15T00:00:02Z",
    }))
    .await;
    assert_eq!(
        resumed["value"]["status"],
        json!("active"),
        "resume: {resumed}"
    );
    // `expectedUpdatedAt: null` — the editor read no revision — skips the check.
    let renamed = edit(json!({
        "id": "auto_1", "action": "update", "patch": { "title": "Tidy up" },
        "expectedUpdatedAt": null,
    }))
    .await;
    assert_eq!(
        renamed["value"]["title"],
        json!("Tidy up"),
        "rename: {renamed}"
    );
    // A timing edit recompiles and a target's created-session cwd remaps in
    // the same change.
    let retimed = edit(json!({
        "id": "auto_1", "action": "update",
        "patch": {
            "timing": { "kind": "daily", "daily": { "time": "09:30" } },
            "target": { "kind": "new", "create": { "cwd": other.to_string_lossy() } },
        },
    }))
    .await;
    // The entry's rule is the page's shape — a DTSTART line, then the rule —
    // so only the rule line is compared here.
    let entry_rrule = retimed["value"]["rrule"].as_str().expect("entry rrule");
    assert_eq!(
        entry_rrule
            .lines()
            .next_back()
            .unwrap_or_default()
            .trim_start_matches("RRULE:"),
        "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=9;BYMINUTE=30",
        "retime: {retimed}"
    );
    assert_eq!(
        retimed["value"]["target"]["create"]["cwd"],
        json!(other.to_string_lossy()),
        "retime: {retimed}"
    );
    // A revision the editor did read is still enforced.
    let stale = edit(json!({
        "id": "auto_1", "action": "update", "patch": { "title": "Stale" },
        "expectedUpdatedAt": "2026-11-15T00:00:01Z",
    }))
    .await;
    assert_eq!(
        stale["error"]["code"],
        json!("schedule_conflict"),
        "stale: {stale}"
    );
    // Delete is the same verb with `action: "delete"`.
    let deleted = edit(json!({ "id": "auto_1", "action": "delete" })).await;
    assert_eq!(
        deleted["value"],
        json!({ "id": "auto_1", "deleted": true }),
        "delete: {deleted}"
    );
    let catalog = rpc(&client, &base, "schedule/catalog", json!({})).await;
    assert_eq!(
        catalog["value"],
        json!([]),
        "deleted task still listed: {catalog}"
    );
    // What the runtime received, in order.
    let edits = fake.automation_edits.lock().unwrap().clone();
    assert_eq!(
        edits,
        vec![
            ("PATCH", "auto_1".to_string(), json!({ "status": "paused" })),
            ("PATCH", "auto_1".to_string(), json!({ "status": "active" })),
            ("PATCH", "auto_1".to_string(), json!({ "name": "Tidy up" })),
            (
                "PATCH",
                "auto_1".to_string(),
                json!({
                    "rrule": "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=9;BYMINUTE=30",
                    "cwds": [other.to_string_lossy()],
                })
            ),
            ("DELETE", "auto_1".to_string(), Value::Null),
        ],
        "runtime edits: {edits:?}"
    );
    drop(home);
}

#[tokio::test]
async fn session_follow_snapshots_the_journal_and_streams_live_status() {
    let (home, _client, base, fake, _host, _ws) = spawn_stack("follow").await;
    let mut ws = open_mux_stream(
        &base,
        "f1",
        "session/follow",
        json!({ "args": { "address": { "sessionId": "thr_a" } } }),
    )
    .await;
    // Baseline: an empty journal opens at cursor -1 with the thread's own
    // header and workspace.
    let snapshot = next_frame(&mut ws).await;
    let value = &snapshot["value"];
    assert_eq!(value["type"], json!("snapshot"), "follow frame: {snapshot}");
    assert_eq!(value["header"]["sessionId"], json!("thr_a"));
    assert_eq!(value["cursor"], json!(-1));
    assert!(
        value["header"]["cwd"]
            .as_str()
            .is_some_and(|cwd| cwd.ends_with(
                fake.workspace
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )),
        "header cwd must be the thread workspace: {snapshot}"
    );
    // The live tail subscribes AFTER the snapshot: wait for the follow's own
    // SSE subscription before emitting (broadcast has no replay). The follow
    // stream carries the journal's turn records; the session's `running` flip
    // rides the $events stream as an `api-session/status` emit — both sides
    // are asserted.
    fake.await_watcher().await;
    let mut events_ws = open_mux_stream(&base, "e1", "$events", json!({ "args": {} })).await;
    let _ready = next_frame(&mut events_ws).await; // ready frame

    fake.emit(
        json!({ "schema_version": 1, "seq": 1, "event": "turn.started", "thread_id": "thr_a" }),
    );
    // Follow: the mapped journal record for the turn start.
    let record = next_frame(&mut ws).await;
    assert_eq!(
        record["value"]["type"],
        json!("event"),
        "follow must carry the turn-start journal record: {record}"
    );
    // $events: the running flip.
    let started = wait_emit(&mut events_ws, "api-session/status").await;
    assert_eq!(started["value"]["args"], json!(["thr_a", true]));
    fake.emit(
        json!({ "schema_version": 1, "seq": 2, "event": "turn.completed", "thread_id": "thr_a" }),
    );
    let ended = wait_emit(&mut events_ws, "api-session/status").await;
    assert_eq!(ended["value"]["args"], json!(["thr_a", false]));
    drop(home);
}

/// Found in the desktop: a parked stream registered its live sink only
/// after its baseline was drawn, so an increment pushed in between (a
/// settled turn, a model switch, a queue update) reached every other stream
/// but this one, and the client held the baseline's values until the next
/// push.
#[tokio::test]
async fn a_push_during_a_streams_baseline_follows_that_baseline() {
    let (home, _client, base, fake, host, _ws) = spawn_stack("sink-race").await;
    fake.gated.store(true, Ordering::SeqCst);
    let mut ws = open_mux_stream(&base, "w1", "workspace/follow", json!({})).await;
    // The baseline is parked on the fake's thread list.
    fake.entered.notified().await;
    host.push_live(
        "workspace/follow",
        json!({ "type": "replace", "mark": "during" }),
    )
    .await;
    fake.release.notify_one();
    let baseline = next_frame(&mut ws).await;
    assert_eq!(baseline["value"]["type"], json!("baseline"), "{baseline}");
    // A second push lands with the sink registered in either ordering: the
    // baseline must be followed by the first one, not skipped to the second.
    host.push_live(
        "workspace/follow",
        json!({ "type": "replace", "mark": "after" }),
    )
    .await;
    let next = next_frame(&mut ws).await;
    assert_eq!(
        next["value"]["mark"],
        json!("during"),
        "the push made during the baseline: {next}"
    );
    drop(home);
}

/// Read mux frames until one is an `emit` frame of `event` (journal-event
/// frames interleaved with it are skipped, not asserted away).
async fn wait_emit(ws: &mut Ws, event: &str) -> Value {
    for _ in 0..8 {
        let frame = next_frame(ws).await;
        if frame["value"]["type"] == json!("emit") && frame["value"]["event"] == json!(event) {
            return frame;
        }
    }
    panic!("no {event} emit frame within 8 frames");
}

#[tokio::test]
async fn subagents_prompt_and_interrupt_map_onto_the_child_seam() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("subagents").await;
    let value = rpc(
        &client,
        &base,
        "subagents/prompt",
        json!({ "request": {
            "childSessionId": "thr_child",
            "content": [{ "type": "text", "text": "continue the plan" }],
            "requestId": "msg-42",
        }}),
    )
    .await;
    assert_eq!(value["ok"], json!(true), "subagent prompt: {value}");
    assert_eq!(value["value"]["messageId"], json!("msg-42"));
    let posts = fake.subagent_posts.lock().unwrap().clone();
    assert!(
        posts
            .iter()
            .any(|(kind, body)| *kind == "input" && body.contains("continue the plan")),
        "input posts: {posts:?}"
    );

    // A file part is refused loudly — the child seam is text-only.
    let refused = rpc(
        &client,
        &base,
        "subagents/prompt",
        json!({ "request": {
            "childSessionId": "thr_child",
            "content": [{ "type": "file", "receiptId": "r" }],
        }}),
    )
    .await;
    assert_eq!(
        refused["error"]["code"],
        json!("subagent/attachment-invalid"),
        "{refused}"
    );

    let stopped = rpc(
        &client,
        &base,
        "subagents/interruptByUser",
        json!({ "childSessionId": "thr_child" }),
    )
    .await;
    assert_eq!(stopped["value"]["accepted"], json!(true));
    let posts = fake.subagent_posts.lock().unwrap().clone();
    assert!(
        posts.iter().any(|(kind, _)| *kind == "cancel"),
        "cancel posts: {posts:?}"
    );
    drop(home);
}

#[tokio::test]
async fn open_in_app_open_fences_paths_outside_every_workspace() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("openinapp").await;
    // The fence: a path outside every session workspace is refused before
    // any application resolves — no launcher side effects in the test.
    let response = client
        .post(format!("{base}/open-in-app/open"))
        .json(&json!({ "app": "Finder", "path": "/etc" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], json!("bad-request"), "{body}");
    // A missing body member is a loud 400, not a launch attempt.
    let response = client
        .post(format!("{base}/open-in-app/open"))
        .json(&json!({ "app": "Finder" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    // An unknown app id has no icon: 404, and a foreign origin never even
    // reaches the icon route.
    let icon = client
        .get(format!("{base}/open-in-app/icon/no-such-app"))
        .send()
        .await
        .unwrap();
    assert_eq!(icon.status(), reqwest::StatusCode::NOT_FOUND);
    drop(home);
}

#[tokio::test]
async fn agent_presets_describe_forwards_to_runtime() {
    let (home, client, base, _fake, _host, _ws) = spawn_stack("presets-describe").await;
    let envelope = rpc(&client, &base, "agentPresets/describe", json!({})).await;
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["value"]["selection"]["selected"], json!("middle"));
    drop(home);
}

#[tokio::test]
async fn agent_presets_apply_posts_ops_verbatim() {
    let (home, client, base, fake, _host, _ws) = spawn_stack("presets-apply").await;
    let envelope = rpc(
        &client,
        &base,
        "agentPresets/apply",
        json!({ "ops": [{ "op": "selectPreset", "name": "all" }] }),
    )
    .await;
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    // The ops array rides to the runtime untouched — the bridge adds no
    // interpretation of its own.
    let posts = fake.preset_applies.lock().unwrap().clone();
    assert_eq!(
        posts,
        vec![json!([{ "op": "selectPreset", "name": "all" }])]
    );
    drop(home);
}
