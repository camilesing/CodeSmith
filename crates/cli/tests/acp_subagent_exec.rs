//! Offline end-to-end receipt for the ACP external sub-agent provider,
//! run against the REAL `codesmith` binary on BOTH sides of the seam: the
//! outer `exec` engine spawns the real `codesmith serve --acp` server as
//! its child, over the same `[subagents] external_provider = "acp"`
//! wiring a user configures. The unit tests in `tools/subagent/acp.rs`
//! script a `/bin/sh` child with hand-written JSON-RPC lines — they
//! cannot catch a disagreement between this repo's own ACP client and
//! its own ACP server (the first such bug: the server stringifies
//! numeric JSON-RPC ids for Zed compat, the client matched only numeric
//! ids — the run hung to the provider timeout until the id matcher
//! accepted both spellings).
//!
//! Fake model, same posture as `headless_exec.rs` (fixtures are
//! per-file by local idiom): wiremock stands up a loopback
//! OpenAI-compatible endpoint and both processes are pointed at it
//! through explicit env opt-in. One shape difference: the ACP server
//! issues its LLM call with `stream: false`, so the child's mock
//! answers a plain `chat.completion` JSON body while the outer engine's
//! turns keep the SSE streaming fixtures.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The real binary under test (built by cargo before integration tests).
/// Spawned twice: as the outer `exec` process and — via the acp table —
/// as the `--acp` child server.
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

/// One NON-streaming Chat Completions body — the shape the `--acp`
/// server requests (`stream: false` in `run_prompt`) and parses; an SSE
/// body here would fail the child's reply extraction and fail the run.
fn json_answer(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "id": "chatcmpl-acp-e2e",
        "object": "chat.completion",
        "created": 1_700_000_000,
        "model": "e2e-test-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    }))
}

// ────────────────────────────── process plumbing ──────────────────────────────

/// Scratch `CODESMITH_HOME` + config file + workspace + loopback endpoint
/// (same idiom as `headless_exec.rs`; the child `--acp` server inherits
/// every provider env var from the outer process, so no acp-table env is
/// needed — the ACP provider layers its own additions on top of the
/// inherited environment).
struct AcpE2E {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    server: MockServer,
}

impl AcpE2E {
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

    /// The real outer binary pointed at the loopback fake endpoint.
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
}

/// Spawn with piped stdio and a deadline, so a regression that hangs the
/// binary fails fast with the captured stderr instead of stalling CI.
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

/// The `exec --json` summary: the last JSON document on stdout.
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

/// The ACP seam round trip against the real binary on both ends: the
/// outer engine's `agent_open` goes through
/// `[subagents] external_provider = "acp"`, the provider speaks
/// initialize → session/new → session/prompt to the real `--acp` server,
/// and the child's reply ("acp child done") must flow back through the
/// `agent_message_chunk` notification into the parent's transcript.
///
/// Mock routing is content-distinguished: only the ACP child's model
/// request carries the `<assignment-instructions>` fold (the provider
/// embeds the role system prompt in the prompt text — ACP baseline has
/// no system-prompt field), and only the `--acp` server emits its
/// "ACP-compatible editor" system prompt. Requiring BOTH markers is what
/// proves the run really crossed the process boundary through the
/// protocol, not an in-process shortcut.
#[tokio::test]
async fn acp_external_provider_round_trips_with_real_acp_server_child() {
    assert!(
        !BIN.contains('\''),
        "TOML literal string cannot carry the binary path {BIN:?}"
    );
    let config_toml = format!(
        "[subagents]\nexternal_provider = \"acp\"\n\n[subagents.acp]\ncommand = '{BIN}'\nargs = \
         [\"--model\", \"e2e-test-model\", \"serve\", \"--acp\"]\ntimeout_secs = 30\n"
    );
    let e2e = AcpE2E::new(&config_toml).await;

    // Outer turn 1: delegate to a named session.
    Mock::given(method("POST"))
        .and(path(CHAT_COMPLETIONS_PATH))
        .and(body_string_contains("E2E-OUTER-OBJECTIVE"))
        .respond_with(tool_call_answer(
            "call_e2e_acp_open",
            "agent_open",
            r#"{"name": "e2e-acp-scout", "objective": "E2E-CHILD-OBJECTIVE: inspect via acp", "type": "explore"}"#,
        ))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    // ACP child: non-streaming JSON (the --acp server asks stream:false).
    Mock::given(method("POST"))
        .and(path(CHAT_COMPLETIONS_PATH))
        .and(body_string_contains("assignment-instructions"))
        .respond_with(json_answer("acp child done"))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    // Outer turn 2: wait for the named session's terminal result.
    Mock::given(method("POST"))
        .and(path(CHAT_COMPLETIONS_PATH))
        .and(body_string_contains("e2e-acp-scout"))
        .respond_with(tool_call_answer(
            "call_e2e_acp_eval",
            "agent_eval",
            r#"{"name": "e2e-acp-scout"}"#,
        ))
        .up_to_n_times(1)
        .mount(&e2e.server)
        .await;
    // Everything else (outer turn 3): final plain answer.
    Mock::given(method("POST"))
        .and(path(CHAT_COMPLETIONS_PATH))
        .respond_with(text_answer("outer done"))
        .mount(&e2e.server)
        .await;

    let mut cmd = e2e.command();
    cmd.arg("exec")
        .arg("--json")
        .arg("--auto")
        .arg("--model")
        .arg("e2e-test-model")
        .arg("E2E-OUTER-OBJECTIVE: delegate one inspection over acp");
    let (status, stdout, stderr) = run_with_timeout(cmd, Duration::from_secs(180));

    assert_eq!(status.code(), Some(0), "stderr:\n{stderr}");
    let summary = parse_summary(&stdout);
    assert_eq!(summary["status"], "completed", "summary:\n{summary}");
    for expected in ["agent_open", "agent_eval"] {
        assert!(
            tool_entries(&summary)
                .iter()
                .any(|entry| entry["name"] == expected && entry["success"] == true),
            "expected a successful {expected} dispatch in the outer summary:\n{summary}"
        );
    }

    let bodies = captured_bodies(&e2e.server).await;
    let child_bodies: Vec<&str> = bodies
        .iter()
        .map(String::as_str)
        .filter(|body| body.contains("ACP-compatible editor"))
        .collect();
    assert!(
        !child_bodies.is_empty(),
        "the acp provider must spawn the real --acp server child (no request carries the \
         --acp system prompt); bodies:\n{}",
        bodies.join("\n---\n")
    );
    assert!(
        child_bodies
            .iter()
            .any(|body| body.contains("assignment-instructions")
                && body.contains("E2E-CHILD-OBJECTIVE")),
        "the child must receive the role prompt fold + the assignment objective over the \
         protocol; child bodies:\n{}",
        child_bodies.join("\n---\n")
    );
    // The child's reply must flow back: the outer conversation (turn 2+)
    // contains the child output text, not just a status code.
    assert!(
        bodies
            .iter()
            .any(|body| !body.contains("ACP-compatible editor") && body.contains("acp child done")),
        "the child's agent_message_chunk output must reach the parent transcript; \
         bodies:\n{}",
        bodies.join("\n---\n")
    );

    // Round-8 TODO-51: an ACP session is a saved session. The child's turn
    // must be on disk in the scratch home under mode "acp", prompt and
    // reply included — the record `session/list` and `session/resume` read.
    let sessions_dir = e2e.home.path().join("sessions");
    let acp_sessions: Vec<serde_json::Value> = std::fs::read_dir(&sessions_dir)
        .expect("the --acp child opened the session store")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?).ok()
        })
        .filter(|session| session["metadata"]["mode"] == "acp")
        .collect();
    assert_eq!(
        acp_sessions.len(),
        1,
        "exactly one ACP child session saved under {}",
        sessions_dir.display()
    );
    let saved_messages = acp_sessions[0]["messages"].to_string();
    assert!(
        saved_messages.contains("E2E-CHILD-OBJECTIVE") && saved_messages.contains("acp child done"),
        "the saved ACP session carries the child's prompt and reply:\n{saved_messages}"
    );
}
