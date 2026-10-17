//! Offline end-to-end receipt for the headless self-provider and the
//! `[runtime] max_spawn_depth` config key, run against the REAL `codesmith`
//! binary. TODO-31 left this gap: `HeadlessSelfProvider` was verified with
//! recorded shell scripts faking the child; here the child is the actual
//! binary speaking the actual provider line.
//!
//! Fake model, ponytail ladder: wiremock — an existing dev-dependency —
//! stands up a loopback OpenAI-compatible streaming endpoint and the real
//! binary is pointed at it through explicit env opt-in
//! (`CODESMITH_PROVIDER=openai`, `CODESMITH_BASE_URL`, `OPENAI_API_KEY`).
//! Zero product changes, zero new dependencies, nothing reachable from a
//! user build: the endpoint lives only inside this test process and the env
//! vars are set only on spawned children. The alternative — a cargo feature
//! compiling the `mock` provider into the CLI plus a `validate()` carve-out
//! for `provider = "mock"` — adds a trust-boundary surface and buys no
//! additional proof (it would not exercise the real provider stack at all).
//!
//! Depth observability: `exec --json` output does not expose the engine's
//! resolved `max_spawn_depth`, so enforcement is observed where it is cheap
//! and real — the depth-gate rejection text flows back to the model as the
//! tool error, so it appears verbatim in the second captured
//! chat/completions request body. What is NOT covered here: the REPL and
//! automation-thread engine sites use the same `config.max_spawn_depth()`
//! getter (pinned by the `config::` unit tests) but have no direct
//! engine-level test.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The real binary under test (built by cargo before integration tests).
const BIN: &str = env!("CARGO_BIN_EXE_codesmith");

const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

// ───────────────────────────── fake OpenAI endpoint ──────────────────────────

/// One OpenAI Chat Completions streaming response body: the given frames as
/// `data:` SSE events, terminated by the `[DONE]` sentinel.
fn sse_response(frames: &[serde_json::Value]) -> ResponseTemplate {
    let mut body = String::new();
    for frame in frames {
        body.push_str("data: ");
        body.push_str(&serde_json::to_string(frame).expect("serialize SSE frame"));
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    // set_body_string forces mime text/plain over any header; raw bytes +
    // an explicit header leave the SSE content type in place.
    ResponseTemplate::new(200)
        .set_body_bytes(body.into_bytes())
        .insert_header("content-type", "text/event-stream")
}

fn chunk(delta: serde_json::Value, finish_reason: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-e2e",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000,
        "model": "e2e-test-model",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    })
}

fn usage_frame() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-e2e",
        "object": "chat.completion.chunk",
        "created": 1_700_000,
        "model": "e2e-test-model",
        "choices": [],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
}

/// A streaming response whose final answer is plain text.
fn text_answer(text: &str) -> ResponseTemplate {
    sse_response(&[
        chunk(
            serde_json::json!({"role": "assistant", "content": text}),
            None,
        ),
        chunk(serde_json::json!({}), Some("stop")),
        usage_frame(),
    ])
}

/// A streaming response that emits one complete function tool call.
fn tool_call_answer(call_id: &str, name: &str, arguments: &str) -> ResponseTemplate {
    sse_response(&[
        chunk(
            serde_json::json!({"role": "assistant", "tool_calls": [{
                "index": 0,
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": arguments}
            }]}),
            None,
        ),
        chunk(serde_json::json!({}), Some("tool_calls")),
        usage_frame(),
    ])
}

// ────────────────────────────── process plumbing ──────────────────────────────

/// Scratch `CODESMITH_HOME` (state isolation, same idiom as `dispatch.rs`)
/// with the config file pinned via `CODESMITH_CONFIG_PATH`, a scratch
/// workspace, and the loopback fake model endpoint.
struct HeadlessE2E {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    server: MockServer,
}

impl HeadlessE2E {
    async fn new(config_toml: &str) -> Self {
        let home = tempfile::tempdir().expect("create scratch CODESMITH_HOME");
        std::fs::write(home.path().join("config.toml"), config_toml)
            .expect("write scratch config.toml");
        let workspace = tempfile::tempdir().expect("create scratch workspace");
        let server = MockServer::start().await;
        Self {
            home,
            workspace,
            server,
        }
    }

    /// The real binary pointed at the loopback fake endpoint. The provider
    /// env vars are the only channel — nothing here is reachable from a user
    /// build. `CODESMITH_CONFIG_PATH` isolates the config file (the
    /// `CODESMITH_HOME` knob relocates state but not this config path);
    /// a developer shell must not leak its own OpenAI/proxy settings into
    /// the children's provider resolution.
    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env("CODESMITH_HOME", self.home.path())
            .env(
                "CODESMITH_CONFIG_PATH",
                self.home.path().join("config.toml"),
            )
            .env("CODESMITH_PROVIDER", "openai")
            .env("CODESMITH_BASE_URL", format!("{}/v1", self.server.uri()))
            .env("OPENAI_API_KEY", "e2e-test-key")
            .env_remove("OPENAI_BASE_URL")
            .env_remove("OPENAI_MODEL")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .current_dir(self.workspace.path());
        cmd
    }

    fn mock(matcher: impl wiremock::Match + 'static) -> wiremock::MockBuilder {
        Mock::given(method("POST"))
            .and(path(CHAT_COMPLETIONS_PATH))
            .and(matcher)
    }

    fn fallback_mock() -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(CHAT_COMPLETIONS_PATH))
    }
}

/// Spawn with piped stdio and a deadline, so a regression that hangs the
/// binary (e.g. waiting for a human approval) fails fast with the captured
/// stderr instead of stalling CI (same idiom as `dispatch.rs`).
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> (std::process::ExitStatus, String, String) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn codesmith");
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                child.kill().expect("kill runaway codesmith");
                child.wait().expect("reap codesmith");
                let stderr = read_pipe(child.stderr.take());
                panic!("codesmith did not exit within {timeout:?}; stderr so far:\n{stderr}");
            }
            Err(error) => panic!("failed to wait for codesmith: {error}"),
        }
    };
    let stdout = read_pipe(child.stdout.take());
    let stderr = read_pipe(child.stderr.take());
    (status, stdout, stderr)
}

fn read_pipe(mut pipe: Option<impl Read>) -> String {
    let mut out = String::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_string(&mut out);
    }
    out
}

/// The `exec --json` summary: the last JSON document on stdout, parsed the
/// same way `HeadlessSelfProvider::parse_exec_summary` does (whole stdout
/// first, then re-parse from the final `\n{` boundary).
fn parse_summary(stdout: &str) -> serde_json::Value {
    let text = stdout.trim();
    serde_json::from_str(text).unwrap_or_else(|_| {
        let index = text
            .rfind("\n{")
            .expect("an exec --json summary line on stdout");
        let tail = text[index + 1..].trim();
        serde_json::from_str(tail)
            .unwrap_or_else(|error| panic!("unparseable exec --json summary ({error}):\n{text}"))
    })
}

async fn captured_bodies(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("wiremock records requests")
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

fn tool_entries(summary: &serde_json::Value) -> &Vec<serde_json::Value> {
    summary["tools"]
        .as_array()
        .expect("tools array in the exec summary")
}

// ─────────────────────────────────── tests ───────────────────────────────────

/// The provider-contract round trip against the real binary: the exact
/// argv/env `HeadlessSelfProvider::command` builds, answered by the fake
/// endpoint over the real OpenAI provider stack.
///
/// The child runs with `CODESMITH_SUBAGENT_DEPTH=1` /
/// `CODESMITH_MAX_SPAWN_DEPTH=1` (what a parent with cap 1 passes), so its
/// engine must derive `max_spawn_depth = 0` and reject the second-level
/// spawn; the rejection text the model then sees is the observable proof
/// that the budget crossed the process boundary and was enforced.
#[tokio::test]
async fn provider_contract_round_trip_against_real_binary() {
    let e2e = HeadlessE2E::new("").await;
    HeadlessE2E::mock(body_string_contains("E2E-CHILD-OBJECTIVE"))
        .respond_with(tool_call_answer(
            "call_e2e_child",
            "agent_open",
            r#"{"objective": "try to spawn one level deeper"}"#,
        ))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    HeadlessE2E::fallback_mock()
        .respond_with(text_answer("child done"))
        .mount(&e2e.server)
        .await;

    let mut cmd = e2e.command();
    cmd.arg("exec")
        .arg("--json")
        .arg("--auto")
        .arg("--model")
        .arg("e2e-test-model")
        .arg("--append-system-prompt")
        .arg("HEADLESS ROLE PROMPT e2e")
        .arg("E2E-CHILD-OBJECTIVE: map the workspace")
        .env("CODESMITH_SUBAGENT_DEPTH", "1")
        .env("CODESMITH_MAX_SPAWN_DEPTH", "1")
        .env("CODESMITH_SUBAGENT_ID", "agent_headless_e2e");
    let (status, stdout, stderr) = run_with_timeout(cmd, Duration::from_secs(120));

    assert_eq!(status.code(), Some(0), "stderr:\n{stderr}");
    let summary = parse_summary(&stdout);
    assert_eq!(summary["model"], "e2e-test-model", "summary:\n{summary}");
    assert_eq!(summary["output"], "child done");
    assert_eq!(summary["status"], "completed");
    let agent_open = tool_entries(&summary)
        .iter()
        .find(|entry| entry["name"] == "agent_open")
        .expect("agent_open dispatched in the child engine");
    assert_eq!(
        agent_open["success"], false,
        "budget 1 - 1 = 0 must reject the second-level spawn"
    );

    let bodies = captured_bodies(&e2e.server).await;
    assert!(
        bodies.len() >= 2,
        "expected at least two model requests (tool call, then rejection feedback);\n{}",
        bodies.join("\n---\n")
    );
    assert!(
        bodies[0].contains("HEADLESS ROLE PROMPT e2e"),
        "--append-system-prompt must reach the model-visible system message;\n{}",
        bodies[0]
    );
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("depth limit reached") && body.contains("max 0")),
        "the real child must derive max_spawn_depth = 1 - 1 = 0 from the env pair \
         and reject the spawn; bodies:\n{}",
        bodies.join("\n---\n")
    );
}

/// Full engine-level dispatch: `[subagents] external_provider = "headless"`
/// makes the outer engine spawn the REAL binary as its child, and
/// `[runtime] max_spawn_depth = 1` must exhaust the budget across the
/// process hop — the child inherits `depth = 1 / max = 1` (budget 0) and its
/// own spawn attempt is rejected.
///
/// Mock routing is content-distinguished, not order-distinguished: the outer
/// engine's requests carry the outer objective, the child's carry the
/// explore role prompt the provider forwards via `--append-system-prompt`
/// (the outer conversation never contains it), and only the outer's second
/// turn contains the session name.
#[tokio::test]
async fn external_provider_dispatches_real_child_and_enforces_configured_depth() {
    let e2e = HeadlessE2E::new(
        "[subagents]\nexternal_provider = \"headless\"\n\n[runtime]\nmax_spawn_depth = 1\n",
    )
    .await;
    // Outer turn 1: delegate to a named session.
    HeadlessE2E::mock(body_string_contains("E2E-OUTER-OBJECTIVE"))
        .respond_with(tool_call_answer(
            "call_e2e_open",
            "agent_open",
            r#"{"name": "e2e-scout", "objective": "E2E-CHILD-OBJECTIVE: inspect the tree", "type": "explore"}"#,
        ))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    // Child turn 1: the child tries one level deeper (must be rejected).
    HeadlessE2E::mock(body_string_contains("exploration sub-agent"))
        .respond_with(tool_call_answer(
            "call_e2e_grandchild",
            "agent_open",
            r#"{"objective": "try one level deeper"}"#,
        ))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    // Outer turn 2: wait for the named session's terminal result.
    HeadlessE2E::mock(body_string_contains("e2e-scout"))
        .respond_with(tool_call_answer(
            "call_e2e_eval",
            "agent_eval",
            r#"{"name": "e2e-scout"}"#,
        ))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    // Everything else (outer turn 3, child turn 2): final plain answer.
    HeadlessE2E::fallback_mock()
        .respond_with(text_answer("outer done"))
        .mount(&e2e.server)
        .await;

    let mut cmd = e2e.command();
    cmd.arg("exec")
        .arg("--json")
        .arg("--auto")
        .arg("--model")
        .arg("e2e-test-model")
        .arg("E2E-OUTER-OBJECTIVE: delegate one inspection");
    let (status, stdout, stderr) = run_with_timeout(cmd, Duration::from_secs(180));

    assert_eq!(status.code(), Some(0), "stderr:\n{stderr}");
    let summary = parse_summary(&stdout);
    assert_eq!(summary["status"], "completed", "summary:\n{summary}");
    // Not exact-equality: a completed external child also wakes the parent
    // (parent-completion sentinel), which answers once more — the summary's
    // output concatenates those final answers.
    assert!(
        summary["output"]
            .as_str()
            .is_some_and(|output| output.contains("outer done")),
        "summary:\n{summary}"
    );
    for expected in ["agent_open", "agent_eval"] {
        assert!(
            tool_entries(&summary)
                .iter()
                .any(|entry| entry["name"] == expected && entry["success"] == true),
            "expected a successful {expected} dispatch in the outer summary:\n{summary}"
        );
    }

    let bodies = captured_bodies(&e2e.server).await;
    // A headless child's system prompt is the exec BASE prompt with the role
    // prompt appended; an in-process child would carry only the role prompt
    // (no exec constitution), and the outer engine carries no role prompt.
    // Requiring BOTH markers is what proves the dispatch really crossed the
    // process boundary.
    let child_bodies: Vec<&str> = bodies
        .iter()
        .map(String::as_str)
        .filter(|body| {
            body.contains("exploration sub-agent") && body.contains("CONSTITUTION OF CODESMITH")
        })
        .collect();
    assert!(
        !child_bodies.is_empty(),
        "the headless provider must spawn the real binary as the child (no request \
         carries BOTH the role prompt and the exec base prompt); bodies:\n{}",
        bodies.join("\n---\n")
    );
    assert!(
        child_bodies
            .iter()
            .any(|body| body.contains("E2E-CHILD-OBJECTIVE")),
        "the child must run the assignment objective"
    );
    assert!(
        child_bodies
            .iter()
            .any(|body| body.contains("depth limit reached") && body.contains("max 0")),
        "configured max_spawn_depth = 1 must exhaust the budget across the process hop \
         (env 1/1 → child budget 0) and reject the second level; child bodies:\n{}",
        child_bodies.join("\n---\n")
    );
}

/// Regression for the `[runtime] max_spawn_depth` wiring: the depth-gate
/// error message has advertised this key since TODO-31, but nothing read it
/// — a user who set it got the default (3) and deeper trees silently. With
/// `max_spawn_depth = 0` the FIRST spawn must be rejected by the real
/// binary; before the wiring this test fails because the config key is
/// ignored and the spawn succeeds.
#[tokio::test]
async fn runtime_max_spawn_depth_config_reaches_the_depth_gate() {
    let e2e = HeadlessE2E::new("[runtime]\nmax_spawn_depth = 0\n").await;
    HeadlessE2E::mock(body_string_contains("E2E-OUTER-OBJECTIVE"))
        .respond_with(tool_call_answer(
            "call_e2e_open",
            "agent_open",
            r#"{"objective": "delegate something"}"#,
        ))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    HeadlessE2E::fallback_mock()
        .respond_with(text_answer("outer done"))
        .mount(&e2e.server)
        .await;

    let mut cmd = e2e.command();
    cmd.arg("exec")
        .arg("--json")
        .arg("--auto")
        .arg("--model")
        .arg("e2e-test-model")
        .arg("E2E-OUTER-OBJECTIVE: try to delegate");
    let (status, stdout, stderr) = run_with_timeout(cmd, Duration::from_secs(120));

    assert_eq!(status.code(), Some(0), "stderr:\n{stderr}");
    let summary = parse_summary(&stdout);
    assert_eq!(summary["status"], "completed", "summary:\n{summary}");
    let agent_open = tool_entries(&summary)
        .iter()
        .find(|entry| entry["name"] == "agent_open")
        .expect("agent_open dispatched");
    assert_eq!(
        agent_open["success"], false,
        "max_spawn_depth = 0 must reject the first spawn (config key silently ignored?)"
    );
    let bodies = captured_bodies(&e2e.server).await;
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("depth limit reached") && body.contains("max 0")),
        "the configured cap must drive the rejection text the model sees; bodies:\n{}",
        bodies.join("\n---\n")
    );
}
