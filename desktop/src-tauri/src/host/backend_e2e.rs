//! Real-backend main-path E2E: the actual `codesmith serve --http` binary
//! against a scripted loopback OpenAI-compatible model, driven through the
//! real bridge transport the way the vendored UI drives it.
//!
//! The lane is `#[ignore]`-gated because it needs a built backend binary —
//! run it with:
//!
//! ```sh
//! cargo build --locked -p codesmith-cli
//! cargo test --manifest-path desktop/src-tauri/Cargo.toml -- --ignored
//! ```
//!
//! The scripted model is an axum route answering `POST /v1/chat/completions`
//! with OpenAI streaming chunks from a FIFO script (one response per request,
//! the same content-distinguished posture as `crates/cli`'s wiremock E2E);
//! it can stream plain text or a structured `tool_calls` delta, and hold a
//! response to keep a turn running. Everything else — the engine loop, the
//! journal, approvals, the queue — is the real production code.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

use super::codesmith::CodesmithProcess;
use super::transport_tests::{next_frame, open_mux_stream, rpc};
use super::{bridge_router, HostState, SharedHost};

// === The scripted model ===

enum LlmAnswer {
    /// Plain assistant text, streamed as two content chunks.
    Text(&'static str),
    /// One structured tool call, streamed as a single tool_calls delta.
    ToolCall {
        name: &'static str,
        arguments: String,
    },
    /// Hold the response (keep the turn running) before answering with text.
    Held {
        hold: Duration,
        answer: &'static str,
    },
}

/// One scripted response, claimed by the request whose LAST user message
/// contains `trigger` (an empty trigger claims any request — the FIFO tail).
/// Content matching, not plain FIFO: an interrupted turn's request may never
/// reach the model, which would silently shift a FIFO script onto the wrong
/// turn.
struct ScriptedEntry {
    trigger: &'static str,
    answer: LlmAnswer,
}

#[derive(Clone, Default)]
struct LlmScript {
    entries: Arc<StdMutex<Vec<ScriptedEntry>>>,
}

/// The last user-role message content of one chat/completions body.
fn last_user_message(body: &Value) -> String {
    body.get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
                .and_then(|message| message.get("content"))
                .map(|content| content.to_string())
        })
        .unwrap_or_default()
}

impl LlmScript {
    /// Claim the answer for one request body; `None` when nothing matches.
    fn claim(&self, body: &Value) -> Option<LlmAnswer> {
        let last_user = last_user_message(body);
        let mut entries = self.entries.lock().unwrap();
        let index = entries
            .iter()
            .position(|entry| entry.trigger.is_empty() || last_user.contains(entry.trigger))?;
        Some(entries.remove(index).answer)
    }
}

/// The OpenAI streaming body for one scripted answer.
fn sse_body(answer: &LlmAnswer) -> String {
    let chunk = |delta: Value, finish: Option<&str>| {
        json!({
            "id": "chatcmpl-e2e",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "e2e-model",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        })
        .to_string()
    };
    let mut body = String::new();
    match answer {
        LlmAnswer::Text(text) => {
            let text = *text;
            let split = text.len() / 2;
            let (head, tail) = text.split_at(split);
            for piece in [head, tail] {
                body.push_str(&format!(
                    "data: {}\n\n",
                    chunk(json!({ "role": "assistant", "content": piece }), None)
                ));
            }
            body.push_str(&format!("data: {}\n\n", chunk(json!({}), Some("stop"))));
        }
        LlmAnswer::ToolCall { name, arguments } => {
            body.push_str(&format!(
                "data: {}\n\n",
                chunk(
                    json!({ "tool_calls": [{
                        "index": 0,
                        "id": "call_e2e",
                        "type": "function",
                        "function": { "name": name, "arguments": arguments },
                    }]}),
                    None,
                )
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                chunk(json!({}), Some("tool_calls"))
            ));
        }
        LlmAnswer::Held { answer, .. } => {
            body.push_str(&sse_body(&LlmAnswer::Text(answer)));
        }
    }
    body.push_str("data: [DONE]\n\n");
    body
}

async fn llm_completions(State(script): State<LlmScript>, body: axum::body::Bytes) -> Response {
    let parsed: Result<Value, _> = serde_json::from_slice(&body);
    let Some(answer) = parsed.ok().and_then(|body| script.claim(&body)) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "script exhausted: no answer matches this request",
        )
            .into_response();
    };
    if let LlmAnswer::Held { hold, .. } = &answer {
        tokio::time::sleep(*hold).await;
    }
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream"),
        )],
        sse_body(&answer),
    )
        .into_response()
}

// === The environment guard ===

/// Process-global env for one real-backend test (the spawned backend
/// inherits it). Holds the shared TEST_HOME lock for its whole life, the
/// same discipline the dispatch and transport tests use.
struct BackendEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
    home: PathBuf,
    vars: Vec<(&'static str, Option<String>)>,
}

impl Drop for BackendEnv {
    fn drop(&mut self) {
        for (name, previous) in self.vars.drain(..) {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn set_var(name: &'static str, vars: &mut Vec<(&'static str, Option<String>)>, value: &str) {
    vars.push((name, std::env::var(name).ok()));
    std::env::set_var(name, value);
}

// === The harness ===

fn codesmith_binary() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/codesmith");
    assert!(
        path.is_file(),
        "the real-backend E2E lane needs {} — build it first: cargo build --locked -p codesmith-cli",
        path.display()
    );
    path
}

/// One scripted model + one real `codesmith serve --http` child + one real
/// bridge on ephemeral ports.
// The env lock is a std guard held across the stack's awaits BY DESIGN: the
// process-global env it protects is set for the whole test (the child
// inherits it), exactly like the dispatch tests' TestHome.
#[allow(clippy::await_holding_lock)]
async fn real_stack(
    tag: &str,
    entries: Vec<(&'static str, LlmAnswer)>,
) -> (
    BackendEnv,
    reqwest::Client,
    String,
    Arc<LlmScript>,
    PathBuf,
    CodesmithProcess,
) {
    let guard = super::tests::TEST_HOME_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let home = std::env::temp_dir().join(format!(
        "cds-backend-e2e-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&home).expect("backend home");
    let workspace = home.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut vars = Vec::new();
    let home_str = home.to_string_lossy().to_string();
    let config_path = home.join("config.toml").to_string_lossy().to_string();
    set_var("CODESMITH_HOME", &mut vars, &home_str);
    set_var("CODESMITH_CONFIG_PATH", &mut vars, &config_path);

    let script = Arc::new(LlmScript::default());
    for (trigger, answer) in entries {
        script
            .entries
            .lock()
            .unwrap()
            .push(ScriptedEntry { trigger, answer });
    }
    let llm_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("llm bind");
    let llm_addr = llm_listener.local_addr().expect("llm addr");
    let llm_app = Router::new()
        .route("/v1/chat/completions", post(llm_completions))
        .with_state((*script).clone());
    tokio::spawn(async move {
        let _ = axum::serve(llm_listener, llm_app).await;
    });

    std::fs::write(
        home.join("config.toml"),
        format!(
            "provider = \"openai\"\n\n[providers.openai]\nbase_url = \"http://{llm_addr}/v1\"\nmodel = \"e2e-model\"\napi_key = \"e2e-key\"\n"
        ),
    )
    .expect("config.toml");

    let env = BackendEnv {
        _guard: guard,
        home,
        vars,
    };
    let process = CodesmithProcess::spawn(Some(&codesmith_binary()))
        .await
        .expect("spawn real backend");
    let host: SharedHost = Arc::new(HostState {
        backend: CodesmithProcess::for_test(process.base_url.clone(), process.token.clone()),
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
    tokio::spawn(async move {
        let _ = axum::serve(bridge_listener, bridge_router(host)).await;
    });

    (
        env,
        reqwest::Client::new(),
        format!("http://{bridge_addr}"),
        script,
        workspace,
        process,
    )
}

// === Shared drivers ===

/// GET one runtime path with the backend's bearer token.
async fn runtime_get(client: &reqwest::Client, process: &CodesmithProcess, path: &str) -> Value {
    let response = client
        .get(format!("{}{path}", process.base_url))
        .bearer_auth(&process.token)
        .send()
        .await
        .expect("runtime request");
    assert!(
        response.status().is_success(),
        "GET {path} -> {}",
        response.status()
    );
    response.json().await.expect("runtime json")
}

/// POST one runtime path with the backend's bearer token.
async fn runtime_post(
    client: &reqwest::Client,
    process: &CodesmithProcess,
    path: &str,
    body: Value,
) -> reqwest::Response {
    client
        .post(format!("{}{path}", process.base_url))
        .bearer_auth(&process.token)
        .json(&body)
        .send()
        .await
        .expect("runtime request")
}

/// Wait until the thread's latest turn settles (or panic past `secs`).
async fn wait_settled(
    client: &reqwest::Client,
    process: &CodesmithProcess,
    thread: &str,
    secs: u64,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let detail = runtime_get(client, process, &format!("/v1/threads/{thread}")).await;
        let settled = detail
            .pointer("/turns/0/status")
            .and_then(Value::as_str)
            .map(|status| !matches!(status, "in_progress" | "queued" | "interrupting"))
            .unwrap_or(true);
        if settled {
            return detail;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "turn of {thread} did not settle in {secs}s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Create a session in the stack's workspace through the bridge.
async fn create_session(client: &reqwest::Client, base: &str, workspace: &Path) -> String {
    let value = rpc(
        client,
        base,
        "session/create",
        json!({ "request": { "cwd": workspace.to_string_lossy() } }),
    )
    .await;
    assert_eq!(value["ok"], json!(true), "session/create: {value}");
    value["value"]["sessionId"].as_str().unwrap().to_string()
}

/// Send a plain text prompt through the bridge.
async fn send_prompt(client: &reqwest::Client, base: &str, session: &str, text: &str) {
    let value = rpc(
        client,
        base,
        "session/prompt",
        json!({ "request": { "sessionId": session, "content": [{ "type": "text", "text": text }] } }),
    )
    .await;
    assert_eq!(value["ok"], json!(true), "session/prompt: {value}");
}

// === C1: a chat turn settles and replays identically across a bridge restart ===

#[tokio::test]
#[ignore = "needs target/debug/codesmith — cargo build --locked -p codesmith-cli, then run with --ignored"]
async fn chat_turn_settles_and_replays_identically_across_a_bridge_restart() {
    let (env, client, base, _script, workspace, process) = real_stack(
        "c1",
        vec![("", LlmAnswer::Text("hello from the scripted model"))],
    )
    .await;
    let session = create_session(&client, &base, &workspace).await;
    send_prompt(&client, &base, &session, "say the thing").await;

    let detail = wait_settled(&client, &process, &session, 30).await;
    assert_eq!(
        detail.pointer("/turns/0/status"),
        Some(&json!("completed")),
        "turn status: {detail}"
    );

    // The replayed journal carries the scripted answer verbatim.
    let page = rpc(
        &client,
        &base,
        "session/page",
        json!({ "request": { "address": { "sessionId": session } } }),
    )
    .await;
    assert_eq!(page["ok"], json!(true), "page: {page}");
    let records = serde_json::to_string(&page["value"]["records"]).unwrap();
    assert!(
        records.contains("hello from the scripted model"),
        "replay records must carry the answer: {records}"
    );

    // A brand-new host over the same runtime (a bridge restart) replays the
    // byte-identical journal.
    let host: SharedHost = Arc::new(HostState {
        backend: CodesmithProcess::for_test(process.base_url.clone(), process.token.clone()),
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, bridge_router(host)).await;
    });
    let restarted = format!("http://{addr}");
    let page2 = rpc(
        &client,
        &restarted,
        "session/page",
        json!({ "request": { "address": { "sessionId": session } } }),
    )
    .await;
    assert_eq!(
        page2["value"]["records"], page["value"]["records"],
        "a bridge restart must replay the identical journal"
    );

    // Compaction keeps the session pageable through the bridge.
    let compact = runtime_post(
        &client,
        &process,
        &format!("/v1/threads/{session}/compact"),
        json!({}),
    )
    .await;
    assert!(
        compact.status().is_success(),
        "compact -> {}",
        compact.status()
    );
    let after = rpc(
        &client,
        &restarted,
        "session/page",
        json!({ "request": { "address": { "sessionId": session } } }),
    )
    .await;
    assert_eq!(after["ok"], json!(true), "page after compact: {after}");
    drop(env);
}

/// Drive one approval round with `decision` against a scripted write_file
/// call; both open clients must see the waterfall and its cancel. The tool
/// round is followed by a plain answer (the engine's continuation request).
async fn approval_round(
    env_tag: &str,
    decision: &str,
    file: &str,
    expect_written: bool,
) -> CodesmithProcess {
    let (env, client, base, _script, workspace, process) = real_stack(
        env_tag,
        vec![
            (
                "写文件",
                LlmAnswer::ToolCall {
                    name: "write_file",
                    arguments: format!("{{\"path\":\"{file}\",\"content\":\"approved bytes\\n\"}}"),
                },
            ),
            ("", LlmAnswer::Text("the tool round is settled")),
        ],
    )
    .await;
    let session = create_session(&client, &base, &workspace).await;

    // Two clients hold $events streams open.
    let mut watcher_a = open_mux_stream(&base, "a", "$events", json!({ "args": {} })).await;
    let ready_a = next_frame(&mut watcher_a).await;
    let client_a = ready_a["value"]["clientId"].as_str().unwrap().to_string();
    let mut watcher_b = open_mux_stream(&base, "b", "$events", json!({ "args": {} })).await;
    let ready_b = next_frame(&mut watcher_b).await;
    let _client_b = ready_b["value"]["clientId"].as_str().unwrap().to_string();

    send_prompt(&client, &base, &session, "写文件").await;

    // Both clients see the waterfall.
    let mut event_id = String::new();
    for watcher in [&mut watcher_a, &mut watcher_b] {
        loop {
            let frame = next_frame(watcher).await;
            if frame["value"]["type"] == json!("waterfall") {
                assert_eq!(frame["value"]["request"]["toolName"], json!("write_file"));
                assert_eq!(frame["value"]["agentId"], json!(session));
                event_id = frame["value"]["eventId"].as_str().unwrap().to_string();
                break;
            }
        }
    }

    // Client A answers; both cards retire.
    let answered = rpc(
        &client,
        &base,
        "$events/result",
        json!({
            "clientId": client_a,
            "eventId": event_id,
            "outcome": { "kind": "result", "value": decision },
        }),
    )
    .await;
    assert_eq!(answered["value"]["delivered"], json!(true), "{answered}");
    for watcher in [&mut watcher_a, &mut watcher_b] {
        loop {
            let frame = next_frame(watcher).await;
            if frame["value"]["type"] == json!("cancel") {
                assert_eq!(frame["value"]["eventId"], json!(event_id));
                break;
            }
        }
    }

    let detail = wait_settled(&client, &process, &session, 30).await;
    assert_eq!(detail.pointer("/turns/0/status"), Some(&json!("completed")));

    let written = std::fs::read_to_string(workspace.join(file)).unwrap_or_default();
    if expect_written {
        assert_eq!(written, "approved bytes\n", "the allowed write must land");
    } else {
        assert!(
            !workspace.join(file).is_file(),
            "the denied write must leave no file"
        );
    }
    drop(env);
    process
}

#[tokio::test]
#[ignore = "needs target/debug/codesmith — cargo build --locked -p codesmith-cli, then run with --ignored"]
async fn approval_allow_writes_the_file_and_retires_every_card() {
    let _ = approval_round("c2", "allowed-once", "out-allow.txt", true).await;
}

#[tokio::test]
#[ignore = "needs target/debug/codesmith — cargo build --locked -p codesmith-cli, then run with --ignored"]
async fn approval_deny_leaves_the_file_unwritten() {
    let _ = approval_round("c3", "rejected", "out-deny.txt", false).await;
}

// === C4: a queued prompt parks behind a running turn and runs after interrupt ===

#[tokio::test]
#[ignore = "needs target/debug/codesmith — cargo build --locked -p codesmith-cli, then run with --ignored"]
async fn queued_prompt_parks_and_runs_after_the_running_turn_ends() {
    // Trigger order matters: the drained queue item's request folds BOTH
    // prompts into one user message (history + queue drain), so the
    // second-turn trigger must be scanned before the first's.
    let (env, client, base, _script, workspace, process) = real_stack(
        "c4",
        vec![
            ("bravo-unique", LlmAnswer::Text("second turn answer")),
            (
                "alpha-unique",
                LlmAnswer::Held {
                    hold: Duration::from_secs(4),
                    answer: "first turn answer",
                },
            ),
        ],
    )
    .await;
    let session = create_session(&client, &base, &workspace).await;

    // Turn one starts and holds (the scripted model sleeps).
    send_prompt(&client, &base, &session, "alpha-unique first").await;
    // The second prompt parks on the queue.
    send_prompt(&client, &base, &session, "bravo-unique second").await;
    let mut parked = false;
    for _ in 0..50 {
        let detail = runtime_get(&client, &process, &format!("/v1/threads/{session}")).await;
        if detail["queue"].as_array().is_some_and(|q| q.len() == 1) {
            parked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(parked, "the second prompt must park on the queue");

    // Interrupt the held turn; the queued prompt then runs to completion.
    let cancelled = rpc(
        &client,
        &base,
        "session/cancel",
        json!({ "sessionId": session }),
    )
    .await;
    assert_eq!(cancelled["ok"], json!(true), "cancel: {cancelled}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let detail = runtime_get(&client, &process, &format!("/v1/threads/{session}")).await;
        let turns = detail["turns"].as_array().cloned().unwrap_or_default();
        let last_settled = turns
            .last()
            .and_then(|turn| turn.get("status"))
            .and_then(Value::as_str)
            .map(|status| !matches!(status, "in_progress" | "queued" | "interrupting"))
            .unwrap_or(false);
        let queue_empty = detail["queue"].as_array().is_none_or(|q| q.is_empty());
        if turns.len() >= 2 && last_settled && queue_empty {
            let journal = serde_json::to_string(&detail).unwrap();
            assert!(
                journal.contains("second turn answer"),
                "the queued prompt's answer must be journaled: {journal}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "queued turn did not settle: {detail}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    drop(env);
}
