//! ACP external sub-agent provider — delegates one child run to any
//! Agent-Client-Protocol stdio agent.
//!
//! The second implementation of the [`ExternalSubagentProvider`] seam (the
//! first is the headless self-provider): the parent spawns the configured
//! command, speaks the ACP baseline over JSON-RPC 2.0 stdin/stdout lines
//! (`initialize` → `session/new` → `session/prompt`, notifications
//! `session/update` in between), and maps the final `stopReason` onto
//! [`SubAgentStatus`]. The wire shapes mirror this repo's own `--acp`
//! server adapter (`crates/cli/src/acp_server.rs`) — hand-rolled framing,
//! zero new dependencies.
//!
//! # Known limitations
//!
//! - The seam returns one terminal outcome; mid-run progress does not
//!   stream back to the parent's mailbox (streaming would need an event
//!   channel extension on the trait — a separate change).
//! - A foreign agent cannot inherit the CodeSmith spawn-depth budget; the
//!   depth env vars are still exported (a `codesmith` child enforces
//!   them), but an opaque agent is bounded only by this provider's
//!   timeout.
//! - ACP baseline has no system-prompt field, so the assignment's system
//!   prompt is folded into the prompt text as a leading block; the child
//!   agent treats it as ordinary context.
//! - No model negotiation: `request.model` is not forwarded (the child
//!   uses its own default); `steps_taken` is unobservable and reported
//!   as 0.
//!
//! # Cancellation
//!
//! Dropping the `run` future is the seam's cancellation path: the child is
//! spawned `kill_on_drop`, and this driver additionally sends a best-effort
//! `session/cancel` before tearing down on timeout.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use codesmith_agent_runtime::subagent::{
    ExternalSubagentOutcome, ExternalSubagentProvider, ExternalSubagentRequest, SubAgentStatus,
};

use crate::config::Config;

pub const ACP_PROVIDER_NAME: &str = "acp";

/// Default child-run timeout, aligned with the headless provider's 30 min.
const DEFAULT_ACP_TIMEOUT_SECS: u64 = 1800;

/// The ACP protocol version this client speaks (the repo's server
/// baseline).
const ACP_PROTOCOL_VERSION: u64 = 1;

pub struct AcpSubagentProvider {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    timeout: Duration,
}

impl AcpSubagentProvider {
    /// Build from `[subagents] external_provider = "acp"` +
    /// `[subagents.acp]`. The command must be present — a configured
    /// provider without a command fails loud here (config validation also
    /// rejects it at load; this is the enforcement in depth).
    pub fn from_config(config: &Config) -> Result<Self> {
        let acp = config
            .subagents
            .as_ref()
            .and_then(|subagents| subagents.acp.as_ref())
            .ok_or_else(|| {
                anyhow!(
                    "[subagents] external_provider = \"acp\" requires an [subagents.acp] \
                     table with a command"
                )
            })?;
        Self::new(
            acp.command.clone(),
            acp.args.clone(),
            acp.env.clone(),
            acp.timeout_secs,
        )
    }

    /// Construct directly (tests, embeds). `timeout_secs` falls back to
    /// [`DEFAULT_ACP_TIMEOUT_SECS`] and clamps to at least one second.
    pub fn new(
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        timeout_secs: Option<u64>,
    ) -> Result<Self> {
        if command.trim().is_empty() {
            bail!("ACP sub-agent command must not be empty");
        }
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_ACP_TIMEOUT_SECS).max(1));
        Ok(Self {
            command,
            args,
            env,
            timeout,
        })
    }
}

#[async_trait::async_trait]
impl ExternalSubagentProvider for AcpSubagentProvider {
    fn name(&self) -> &'static str {
        ACP_PROVIDER_NAME
    }

    async fn run(&self, request: ExternalSubagentRequest) -> Result<ExternalSubagentOutcome> {
        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.args)
            .current_dir(&request.workspace)
            // Exported for a `codesmith` child to enforce; an opaque agent
            // ignores them (see the module's known limitations).
            .env(
                "CODESMITH_SUBAGENT_DEPTH",
                request.spawn_depth.saturating_add(1).to_string(),
            )
            .env(
                "CODESMITH_MAX_SPAWN_DEPTH",
                request.max_spawn_depth.to_string(),
            )
            .env("CODESMITH_SUBAGENT_ID", &request.agent_id)
            .envs(&self.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("ACP sub-agent spawn '{}': {e}", self.command))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("ACP sub-agent stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ACP sub-agent stdout unavailable"))?;
        let mut reader = BufReader::new(stdout).lines();

        let drive = async {
            // --- initialize ---
            send_request(
                &mut stdin,
                1,
                "initialize",
                json!({
                    "protocolVersion": ACP_PROTOCOL_VERSION,
                    "clientCapabilities": {}
                }),
            )
            .await?;
            let init = await_response(&mut reader, 1).await?;
            if init.get("error").is_some() {
                bail!("ACP initialize failed: {init}");
            }

            // --- session/new ---
            send_request(
                &mut stdin,
                2,
                "session/new",
                json!({ "cwd": request.workspace }),
            )
            .await?;
            let new_session = await_response(&mut reader, 2).await?;
            if let Some(error) = new_session.get("error") {
                bail!("ACP session/new failed: {error}");
            }
            let session_id = new_session
                .pointer("/result/sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("ACP session/new returned no sessionId: {new_session}"))?
                .to_string();

            // --- session/prompt (collect agent_message_chunk updates) ---
            let mut prompt_text = String::new();
            if !request.system_prompt.trim().is_empty() {
                prompt_text.push_str(&format!(
                    "<assignment-instructions>\n{}\n</assignment-instructions>\n\n",
                    request.system_prompt.trim()
                ));
            }
            prompt_text.push_str(&request.prompt);
            send_request(
                &mut stdin,
                3,
                "session/prompt",
                json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": prompt_text }]
                }),
            )
            .await?;

            let mut output = String::new();
            let prompt_result = loop {
                let line = reader.next_line().await?.ok_or_else(|| {
                    anyhow!("ACP sub-agent closed stdout before the prompt result")
                })?;
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(&line)
                    .map_err(|e| anyhow!("ACP sub-agent sent a non-JSON line: {e}"))?;
                if is_response_for(&value, 3) {
                    break value;
                }
                if let Some(chunk) = agent_message_chunk(&value, &session_id) {
                    output.push_str(&chunk);
                }
                // Other notifications (logs, tool updates) are ignored.
            };

            if let Some(error) = prompt_result.get("error") {
                bail!("ACP session/prompt failed: {error}");
            }
            let stop_reason = prompt_result
                .pointer("/result/stopReason")
                .and_then(Value::as_str)
                .unwrap_or("end_turn");
            let status = match stop_reason {
                "end_turn" | "stop" | "max_tokens" | "max_tokens_reached" => {
                    SubAgentStatus::Completed
                }
                "cancelled" | "canceled" | "aborted" => SubAgentStatus::Cancelled,
                other => SubAgentStatus::Failed(format!("ACP child stopped with reason '{other}'")),
            };
            Ok(ExternalSubagentOutcome {
                status,
                output,
                steps_taken: 0,
                model: request.model.clone(),
            })
        };

        match tokio::time::timeout(self.timeout, drive).await {
            Ok(outcome) => outcome,
            Err(_) => {
                // Best-effort cancel before kill_on_drop tears the child
                // down with the dropped future.
                let _ = send_notification(&mut stdin, "session/cancel", json!({})).await;
                Ok(ExternalSubagentOutcome {
                    status: SubAgentStatus::Failed(format!(
                        "ACP sub-agent timed out after {}s",
                        self.timeout.as_secs()
                    )),
                    output: String::new(),
                    steps_taken: 0,
                    model: request.model.clone(),
                })
            }
        }
    }
}

/// Write one JSON-RPC request line.
async fn send_request<W>(stdin: &mut W, id: u64, method: &str, params: Value) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let line = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    stdin
        .write_all(line.to_string().as_bytes())
        .await
        .map_err(|e| anyhow!("ACP write {method}: {e}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|e| anyhow!("ACP write {method} newline: {e}"))?;
    stdin.flush().await.map_err(|e| anyhow!("ACP flush: {e}"))
}

/// Write one JSON-RPC notification line (no id, never answered).
async fn send_notification<W>(stdin: &mut W, method: &str, params: Value) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let line = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    stdin
        .write_all(line.to_string().as_bytes())
        .await
        .map_err(|e| anyhow!("ACP write {method}: {e}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|e| anyhow!("ACP write {method} newline: {e}"))?;
    stdin.flush().await.map_err(|e| anyhow!("ACP flush: {e}"))
}

/// Read lines until the response for `id` arrives; notifications in
/// between are skipped (the prompt phase handles them itself).
type ChildLines = tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>;

async fn await_response(reader: &mut ChildLines, id: u64) -> Result<Value> {
    loop {
        let line = reader
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("ACP sub-agent closed stdout before answering request {id}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .map_err(|e| anyhow!("ACP sub-agent sent a non-JSON line: {e}"))?;
        if is_response_for(&value, id) {
            return Ok(value);
        }
    }
}

fn is_response_for(value: &Value, id: u64) -> bool {
    if value.get("method").is_some() {
        return false;
    }
    match value.get("id") {
        Some(Value::Number(n)) => n.as_u64() == Some(id),
        // This repo's own --acp server stringifies numeric ids (Zed
        // compat, `acp_server::jsonrpc_response_id`); accept both the
        // raw and the stringified spelling so a codesmith child answers
        // as readily as any other ACP agent. Caught by the real-binary
        // e2e (`tests/acp_subagent_exec.rs`), which hung to the
        // provider timeout before this branch existed.
        Some(Value::String(s)) => s == &id.to_string(),
        _ => false,
    }
}

/// Extract an `agent_message_chunk` text from a `session/update`
/// notification for `session_id`.
fn agent_message_chunk(value: &Value, session_id: &str) -> Option<String> {
    if value.get("method").and_then(Value::as_str) != Some("session/update") {
        return None;
    }
    if value.pointer("/params/sessionId").and_then(Value::as_str) != Some(session_id) {
        return None;
    }
    if value
        .pointer("/params/update/sessionUpdate")
        .and_then(Value::as_str)
        != Some("agent_message_chunk")
    {
        return None;
    }
    value
        .pointer("/params/update/content/text")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::subagent::SubAgentAssignment;
    use std::path::PathBuf;

    fn request() -> ExternalSubagentRequest {
        ExternalSubagentRequest {
            agent_id: "agent_acp_1".to_string(),
            agent_type: Default::default(),
            assignment: SubAgentAssignment::new("Summarize the repo.".to_string(), None),
            system_prompt: "Be terse.".to_string(),
            prompt: "Summarize the repo.".to_string(),
            model: Some("some-model".to_string()),
            workspace: PathBuf::from("."),
            spawn_depth: 1,
            max_spawn_depth: 3,
            auto_approve: false,
        }
    }

    /// Scripted ACP child: emits the given JSON lines up front (printf per
    /// line, shell-quoted). The client's id-keyed response matching makes
    /// a static script valid regardless of write/read interleaving.
    fn scripted_acp_child(lines: &[serde_json::Value]) -> (String, Vec<String>) {
        let script = lines
            .iter()
            .map(|value| format!("printf %s {}", shell_quote(&value.to_string())))
            .collect::<Vec<_>>()
            .join("; printf '\\n'; ");
        (
            "/bin/sh".to_string(),
            vec![
                "-c".to_string(),
                format!("{script}; printf '\\n'; sleep 10"),
            ],
        )
    }

    /// Single-quote for `/bin/sh -c` argument safety.
    fn shell_quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }

    fn provider_for(command: String, args: Vec<String>) -> AcpSubagentProvider {
        AcpSubagentProvider::new(command, args, HashMap::new(), Some(30)).unwrap()
    }

    #[tokio::test]
    async fn happy_path_collects_updates_and_maps_end_turn_to_completed() {
        let (command, args) = scripted_acp_child(&[
            json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1}}),
            json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s1"}}),
            json!({
                "jsonrpc": "2.0", "method": "session/update",
                "params": {"sessionId": "s1", "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "ACP says "}
                }}
            }),
            json!({
                "jsonrpc": "2.0", "method": "session/update",
                "params": {"sessionId": "s1", "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "hi."}
                }}
            }),
            json!({"jsonrpc": "2.0", "id": 3, "result": {"stopReason": "end_turn"}}),
        ]);
        let provider = provider_for(command, args);
        let outcome = provider.run(request()).await.expect("run completes");
        assert_eq!(outcome.status, SubAgentStatus::Completed);
        assert_eq!(outcome.output, "ACP says hi.");
        assert_eq!(outcome.model.as_deref(), Some("some-model"));
    }

    #[tokio::test]
    async fn prompt_error_fails_the_outcome() {
        let (command, args) = scripted_acp_child(&[
            json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1}}),
            json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s1"}}),
            json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32603, "message": "model on fire"}}),
        ]);
        let provider = provider_for(command, args);
        let err = provider
            .run(request())
            .await
            .expect_err("error response must fail the run");
        assert!(err.to_string().contains("model on fire"), "{err}");
    }

    #[tokio::test]
    async fn hung_child_times_out_with_failed_outcome() {
        let provider = AcpSubagentProvider::new(
            "/bin/sh".to_string(),
            vec!["-c".to_string(), "sleep 60".to_string()],
            HashMap::new(),
            Some(2),
        )
        .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(30), provider.run(request()))
            .await
            .expect("provider-level timeout fires first")
            .expect("timeout maps to an outcome, not an error");
        assert!(
            matches!(&outcome.status, SubAgentStatus::Failed(reason) if reason.contains("timed out")),
            "{:?}",
            outcome.status
        );
    }

    #[tokio::test]
    async fn empty_command_is_rejected_loud() {
        assert!(
            AcpSubagentProvider::new("  ".to_string(), Vec::new(), HashMap::new(), None).is_err()
        );
    }

    /// The repo's own --acp server stringifies numeric ids (Zed compat);
    /// the client must match both spellings or every response is missed
    /// and the run hangs to the timeout (pinned live by
    /// `tests/acp_subagent_exec.rs`).
    #[test]
    fn response_matching_accepts_raw_and_stringified_ids() {
        let numeric = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
        let stringified = json!({"jsonrpc": "2.0", "id": "1", "result": {}});
        let other_id = json!({"jsonrpc": "2.0", "id": "2", "result": {}});
        let notification = json!({"jsonrpc": "2.0", "method": "session/update"});
        assert!(is_response_for(&numeric, 1));
        assert!(is_response_for(&stringified, 1));
        assert!(!is_response_for(&other_id, 1));
        assert!(!is_response_for(&notification, 1));
    }

    #[test]
    fn update_notification_extraction_ignores_other_kinds() {
        let update = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "chunk"}
                }
            }
        });
        assert_eq!(agent_message_chunk(&update, "s1").as_deref(), Some("chunk"));
        assert_eq!(agent_message_chunk(&update, "other"), None);

        let tool_update = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "title": "grep"
                }
            }
        });
        assert_eq!(agent_message_chunk(&tool_update, "s1"), None);
    }
}
