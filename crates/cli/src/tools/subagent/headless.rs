//! Headless self-provider — the first implementation of the external
//! sub-agent seam ([`ExternalSubagentProvider`], defined in
//! `codesmith_agent_runtime::subagent`).
//!
//! A child runs as a **separate `codesmith exec --json` process** of the same
//! binary (the "out-of-process own SDK" shape): the parent passes the
//! role instruction as `--append-system-prompt`, the rendered objective as
//! the prompt, the resolved model, and the spawn-depth budget through the
//! [`SPAWN_DEPTH_ENV`] / [`MAX_SPAWN_DEPTH_ENV`] environment variables, then
//! parses the `ExecSummary` JSON the child prints on stdout. `--auto`
//! (tool-backed agent mode with auto-approvals) is passed only when the
//! parent itself runs auto-approved — the parent's posture is a ceiling.
//!
//! Depth budget: the child's `exec` reads the two variables and runs its own
//! engine with `max_spawn_depth = max - depth` (see [`inherited_spawn_budget`]),
//! so recursion stays bounded globally across process hops exactly as it is
//! in-process.
//!
//! Known limitations (beyond those documented on the seam definition):
//! - A parent *without* auto-approval gets a tool-less child: plain `exec`
//!   is a one-shot model response; the headless CLI has no "read-only tools
//!   without approval" posture yet.
//! - The run timeout is fixed ([`DEFAULT_EXTERNAL_TIMEOUT`], 30 minutes), not
//!   configurable; a child exceeding it is killed and reported as failed.
//! - Streaming progress is not surfaced: the parent sees only the final
//!   `--json` summary, not `stream-json` events.
//! - The child inherits the parent's environment (API keys, config paths) —
//!   it is our own binary, not a third-party server, so no sanitization is
//!   applied.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;

use super::{
    ExternalSubagentOutcome, ExternalSubagentProvider, ExternalSubagentRequest, SubAgentStatus,
};
use crate::config::Config;

/// Child's depth in the global spawn tree, set by [`HeadlessSelfProvider`].
pub const SPAWN_DEPTH_ENV: &str = "CODESMITH_SUBAGENT_DEPTH";
/// Global spawn-depth cap the child must enforce, set by
/// [`HeadlessSelfProvider`].
pub const MAX_SPAWN_DEPTH_ENV: &str = "CODESMITH_MAX_SPAWN_DEPTH";
/// Informational: which sub-agent slot this process runs (set by
/// [`HeadlessSelfProvider`], read for logging/diagnostics).
pub const AGENT_ID_ENV: &str = "CODESMITH_SUBAGENT_ID";
/// Hard wall-clock budget for one headless child run.
pub const DEFAULT_EXTERNAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub const HEADLESS_PROVIDER_NAME: &str = "headless";
/// The `[subagents] external_provider` values this build knows how to build.
pub const KNOWN_EXTERNAL_PROVIDERS: &[&str] =
    &[HEADLESS_PROVIDER_NAME, super::acp::ACP_PROVIDER_NAME];

/// Whether `name` is a provider this build can construct.
#[must_use]
pub fn is_known_external_provider(name: &str) -> bool {
    KNOWN_EXTERNAL_PROVIDERS.contains(&name.trim())
}

/// Build the configured external provider, or `None` for the in-process
/// default. An unknown name fails loud (config validation rejects it earlier;
/// this is the enforcement in depth).
pub fn provider_from_config(config: &Config) -> Result<Option<Arc<dyn ExternalSubagentProvider>>> {
    let Some(name) = config.subagent_external_provider() else {
        return Ok(None);
    };
    match name.trim() {
        HEADLESS_PROVIDER_NAME => Ok(Some(Arc::new(HeadlessSelfProvider::from_current_exe()?))),
        super::acp::ACP_PROVIDER_NAME => Ok(Some(Arc::new(
            super::acp::AcpSubagentProvider::from_config(config)?,
        ))),
        other => bail!(
            "Unknown [subagents] external_provider '{other}'; supported: {} (unset = in-process sub-agents)",
            KNOWN_EXTERNAL_PROVIDERS.join(", ")
        ),
    }
}

/// Spawn-depth budget a headless child inherits from its parent, read from
/// the environment: `Some(max - depth)` when both variables are present, or
/// `None` when this process is not a headless child (run with the configured
/// default). Malformed or half-present values fail loud rather than silently
/// widening the budget.
pub fn inherited_spawn_budget() -> Result<Option<u32>> {
    spawn_budget_from(
        std::env::var(SPAWN_DEPTH_ENV).ok().as_deref(),
        std::env::var(MAX_SPAWN_DEPTH_ENV).ok().as_deref(),
    )
}

fn spawn_budget_from(depth: Option<&str>, max: Option<&str>) -> Result<Option<u32>> {
    match (depth, max) {
        (None, None) => Ok(None),
        (Some(depth), Some(max)) => {
            let depth: u32 = depth.trim().parse().with_context(|| {
                format!("{SPAWN_DEPTH_ENV}={depth:?} is not a non-negative integer")
            })?;
            let max: u32 = max.trim().parse().with_context(|| {
                format!("{MAX_SPAWN_DEPTH_ENV}={max:?} is not a non-negative integer")
            })?;
            Ok(Some(max.saturating_sub(depth)))
        }
        _ => bail!("{SPAWN_DEPTH_ENV} and {MAX_SPAWN_DEPTH_ENV} must be set together"),
    }
}

/// Runs one sub-agent as a `codesmith exec --json` child process of the same
/// binary. `program` is `std::env::current_exe()` in production and a
/// recorded script in tests.
pub struct HeadlessSelfProvider {
    program: PathBuf,
    timeout: Duration,
}

impl HeadlessSelfProvider {
    pub fn from_current_exe() -> Result<Self> {
        let program = std::env::current_exe()
            .context("resolve current executable for the headless sub-agent provider")?;
        Ok(Self::with_program(program))
    }

    /// Point the provider at an explicit binary (tests use a recorded shell
    /// script to fake the headless protocol).
    #[must_use]
    pub fn with_program(program: PathBuf) -> Self {
        Self {
            program,
            timeout: DEFAULT_EXTERNAL_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn command(&self, request: &ExternalSubagentRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.arg("exec").arg("--json");
        if request.auto_approve {
            cmd.arg("--auto");
        }
        if let Some(model) = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        {
            cmd.arg("--model").arg(model);
        }
        cmd.arg("--append-system-prompt")
            .arg(&request.system_prompt);
        cmd.arg(&request.prompt);
        cmd.current_dir(&request.workspace)
            .env(SPAWN_DEPTH_ENV, request.spawn_depth.to_string())
            .env(MAX_SPAWN_DEPTH_ENV, request.max_spawn_depth.to_string())
            .env(AGENT_ID_ENV, &request.agent_id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Cancellation path: dropping the `run` future (manager abort)
            // kills the child instead of leaking it.
            .kill_on_drop(true);
        cmd
    }
}

#[async_trait]
impl ExternalSubagentProvider for HeadlessSelfProvider {
    fn name(&self) -> &'static str {
        HEADLESS_PROVIDER_NAME
    }

    async fn run(&self, request: ExternalSubagentRequest) -> Result<ExternalSubagentOutcome> {
        let mut cmd = self.command(&request);
        let output = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| {
                anyhow!(
                    "headless sub-agent '{}' exceeded {}s; the child process was killed",
                    request.agent_id,
                    self.timeout.as_secs()
                )
            })?
            .with_context(|| {
                format!(
                    "failed to run headless sub-agent via {}",
                    self.program.display()
                )
            })?;
        parse_exec_summary(&output.stdout, &output.stderr, output.status.code())
    }
}

/// Wire shape of the summary document `codesmith exec --json` prints as its
/// last stdout line (`ExecSummary` in `main_root.rs`).
#[derive(Deserialize, Default)]
struct ExecSummaryWire {
    #[serde(default)]
    model: String,
    #[serde(default)]
    output: String,
    #[serde(default)]
    tools: Vec<serde_json::Value>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Parse a child's stdout into an outcome. The summary is the last JSON
/// document on stdout; text-mode noise before it is tolerated by re-parsing
/// from the final `\n{` boundary. Anything unparseable fails with the exit
/// code and a stderr tail.
fn parse_exec_summary(
    stdout: &[u8],
    stderr: &[u8],
    exit_code: Option<i32>,
) -> Result<ExternalSubagentOutcome> {
    let text = String::from_utf8_lossy(stdout);
    let candidate = text.trim();
    let summary = match serde_json::from_str::<ExecSummaryWire>(candidate) {
        Ok(summary) => summary,
        Err(primary) => {
            let tail = candidate.rfind("\n{").map(|idx| &candidate[idx + 1..]);
            match tail.and_then(|tail| serde_json::from_str::<ExecSummaryWire>(tail).ok()) {
                Some(summary) => summary,
                None => bail!(
                    "headless sub-agent produced no parseable `exec --json` summary \
                     (exit code {}): {primary}; stderr: {}",
                    exit_code.map_or_else(|| "signal".to_string(), |code| code.to_string()),
                    stderr_tail(stderr),
                ),
            }
        }
    };

    let exit_note = exit_code.map_or_else(
        || "terminated by signal".to_string(),
        |code| format!("exit code {code}"),
    );
    let status = match (
        summary
            .error
            .as_deref()
            .map(str::trim)
            .filter(|error| !error.is_empty()),
        summary.status.as_deref(),
    ) {
        (Some(error), _) => SubAgentStatus::Failed(error.to_string()),
        (None, Some("failed")) => {
            SubAgentStatus::Failed(format!("exec turn ended with status failed ({exit_note})"))
        }
        (None, Some("canceled")) => SubAgentStatus::Cancelled,
        (None, Some("interrupted")) => SubAgentStatus::Interrupted("exec turn interrupted".into()),
        (None, _) if exit_code.is_some_and(|code| code != 0) => SubAgentStatus::Failed(format!(
            "exec exited abnormally ({exit_note}); stderr: {}",
            stderr_tail(stderr),
        )),
        (None, _) => SubAgentStatus::Completed,
    };

    Ok(ExternalSubagentOutcome {
        status,
        output: summary.output,
        steps_taken: u32::try_from(summary.tools.len()).unwrap_or(u32::MAX),
        model: Some(summary.model)
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty()),
    })
}

/// Last few stderr lines, one line, for an error message.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut lines: Vec<&str> = text.lines().rev().take(8).collect();
    lines.reverse();
    let joined = lines.join(" | ");
    if joined.trim().is_empty() {
        "<empty>".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::super::{SubAgentAssignment, SubAgentType};
    use super::*;

    fn request(workspace: PathBuf, auto_approve: bool) -> ExternalSubagentRequest {
        ExternalSubagentRequest {
            agent_id: "agent_test".to_string(),
            agent_type: SubAgentType::Explore,
            assignment: SubAgentAssignment::new(
                "map the repo".to_string(),
                Some("scout".to_string()),
            ),
            system_prompt: "ROLE PROMPT".to_string(),
            prompt: "OBJECTIVE TEXT".to_string(),
            model: Some("deepseek-v4-flash".to_string()),
            workspace,
            spawn_depth: 2,
            max_spawn_depth: 3,
            auto_approve,
        }
    }

    #[cfg(unix)]
    fn write_script(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("mock-codesmith.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn record_line(record: &std::path::Path) -> String {
        std::fs::read_to_string(record).unwrap_or_default()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_provider_round_trips_exec_json_summary() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record.txt");
        let script = write_script(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$@\" > {record}\n\
                 printf 'cwd=%s\\n' \"$PWD\" >> {record}\n\
                 printf 'depth=%s max=%s id=%s\\n' \"$CODESMITH_SUBAGENT_DEPTH\" \
                 \"$CODESMITH_MAX_SPAWN_DEPTH\" \"$CODESMITH_SUBAGENT_ID\" >> {record}\n\
                 cat <<'EOF'\n\
                 {{\n\
                   \"mode\": \"agent\",\n\
                   \"model\": \"deepseek-v4-flash\",\n\
                   \"prompt\": \"x\",\n\
                   \"output\": \"found 3 crates\",\n\
                   \"tools\": [{{\"name\": \"list_dir\"}}, {{\"name\": \"read_file\"}}],\n\
                   \"status\": \"completed\",\n\
                   \"error\": null\n\
                 }}\n\
                 EOF\n",
                record = record.display(),
            ),
        );

        let provider = HeadlessSelfProvider::with_program(script);
        let outcome = provider
            .run(request(dir.path().to_path_buf(), true))
            .await
            .unwrap();

        assert_eq!(outcome.status, SubAgentStatus::Completed);
        assert_eq!(outcome.output, "found 3 crates");
        assert_eq!(outcome.steps_taken, 2);
        assert_eq!(outcome.model.as_deref(), Some("deepseek-v4-flash"));

        let recorded = record_line(&record);
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            &lines[..8],
            &[
                "exec",
                "--json",
                "--auto",
                "--model",
                "deepseek-v4-flash",
                "--append-system-prompt",
                "ROLE PROMPT",
                "OBJECTIVE TEXT",
            ],
            "argv the child saw:\n{recorded}"
        );
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            recorded.contains(&format!("cwd={}", cwd.display())),
            "child must run in the workspace; recorded:\n{recorded}"
        );
        assert!(
            recorded.contains("depth=2 max=3 id=agent_test"),
            "spawn-depth budget must cross the process boundary; recorded:\n{recorded}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_provider_without_auto_approve_omits_auto_flag() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record.txt");
        let script = write_script(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$@\" > {record}\n\
                 printf '{{\"output\": \"ok\", \"tools\": []}}'\n",
                record = record.display(),
            ),
        );

        let provider = HeadlessSelfProvider::with_program(script);
        let outcome = provider
            .run(request(dir.path().to_path_buf(), false))
            .await
            .unwrap();
        assert_eq!(outcome.status, SubAgentStatus::Completed);

        let recorded = record_line(&record);
        assert!(
            !recorded.contains("--auto"),
            "a non-auto parent must not grant its child auto-approval; recorded:\n{recorded}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_provider_maps_exec_error_to_failed() {
        let dir = tempfile::tempdir().unwrap();
        // Mirrors the real exec: the JSON summary goes to stdout, then the
        // process exits non-zero because the turn failed.
        let script = write_script(
            dir.path(),
            "cat <<'EOF'\n\
             {\n\
               \"model\": \"deepseek-v4-flash\",\n\
               \"output\": \"\",\n\
               \"tools\": [{\"name\": \"exec_shell\"}],\n\
               \"status\": \"failed\",\n\
               \"error\": \"exec turn failed: model refused\"\n\
             }\n\
             EOF\n\
             exit 1\n",
        );

        let provider = HeadlessSelfProvider::with_program(script);
        let outcome = provider
            .run(request(dir.path().to_path_buf(), true))
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            SubAgentStatus::Failed("exec turn failed: model refused".to_string())
        );
        assert_eq!(outcome.steps_taken, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_provider_reports_unparseable_output_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "echo 'not json'\necho 'config load failed: bad toml' >&2\nexit 2\n",
        );

        let provider = HeadlessSelfProvider::with_program(script);
        let err = provider
            .run(request(dir.path().to_path_buf(), true))
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("no parseable"), "{message}");
        assert!(message.contains("exit code 2"), "{message}");
        assert!(message.contains("bad toml"), "{message}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_provider_times_out_and_kills_child() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(dir.path(), "sleep 30\necho '{\"output\": \"late\"}'\n");

        let started = std::time::Instant::now();
        let provider =
            HeadlessSelfProvider::with_program(script).with_timeout(Duration::from_millis(300));
        let err = provider
            .run(request(dir.path().to_path_buf(), true))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("exceeded"), "{err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timeout must fire promptly, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn spawn_budget_from_env_pair() {
        assert_eq!(spawn_budget_from(None, None).unwrap(), None);
        assert_eq!(spawn_budget_from(Some("1"), Some("3")).unwrap(), Some(2));
        assert_eq!(spawn_budget_from(Some("3"), Some("3")).unwrap(), Some(0));
        // Over-depth (parent already at the cap): saturate to zero, never wrap.
        assert_eq!(spawn_budget_from(Some("5"), Some("3")).unwrap(), Some(0));
        assert!(spawn_budget_from(Some("x"), Some("3")).is_err());
        assert!(spawn_budget_from(Some("1"), None).is_err());
        assert!(spawn_budget_from(None, Some("3")).is_err());
    }

    #[test]
    fn parse_exec_summary_status_mapping() {
        let mk =
            |body: &str, exit: Option<i32>| parse_exec_summary(body.as_bytes(), b"", exit).unwrap();

        assert_eq!(
            mk(
                "{\"output\": \"x\", \"tools\": [], \"status\": \"completed\"}",
                Some(0)
            )
            .status,
            SubAgentStatus::Completed
        );
        assert_eq!(
            mk("{\"output\": \"\", \"status\": \"canceled\"}", Some(0)).status,
            SubAgentStatus::Cancelled
        );
        assert_eq!(
            mk("{\"output\": \"\", \"status\": \"interrupted\"}", Some(0)).status,
            SubAgentStatus::Interrupted("exec turn interrupted".into())
        );
        // No error/status fields but a non-zero exit is still a failure.
        assert!(matches!(
            mk("{\"output\": \"x\", \"tools\": []}", Some(1)).status,
            SubAgentStatus::Failed(_)
        ));
        // Text-mode noise before the JSON document is tolerated.
        assert_eq!(
            mk(
                "stderr chatter\n{\n  \"output\": \"y\",\n  \"tools\": [{}]\n}",
                Some(0)
            )
            .output,
            "y"
        );
        assert_eq!(
            mk("noise\n{\"output\": \"z\", \"tools\": []}", Some(0)).steps_taken,
            0
        );
        // Noise glued to the document on the same line is not rescued.
        assert!(parse_exec_summary(b"noise {\"output\": \"z\"}", b"", Some(0)).is_err());
    }

    #[test]
    fn provider_from_config_maps_known_names() {
        let config = |name: Option<&str>| crate::config::Config {
            subagents: Some(crate::config::SubagentsConfig {
                external_provider: name.map(str::to_string),
                ..crate::config::SubagentsConfig::default()
            }),
            ..crate::config::Config::default()
        };

        assert!(provider_from_config(&config(None)).unwrap().is_none());

        let unknown = match provider_from_config(&config(Some("nonsense"))) {
            Err(err) => err,
            Ok(_) => panic!("an unknown provider name must be rejected"),
        };
        assert!(
            format!("{unknown:#}").contains("Unknown [subagents] external_provider 'nonsense'"),
            "{unknown:#}"
        );

        let headless = provider_from_config(&config(Some("headless")))
            .unwrap()
            .expect("headless resolves to a provider");
        assert_eq!(headless.name(), "headless");

        // `acp` resolves once the [subagents.acp] command is configured…
        let mut acp_config = config(Some("acp"));
        acp_config.subagents.as_mut().unwrap().acp = Some(crate::config::AcpSubagentConfig {
            command: "/bin/echo".to_string(),
            ..crate::config::AcpSubagentConfig::default()
        });
        let acp = provider_from_config(&acp_config)
            .unwrap()
            .expect("acp resolves to a provider with a command");
        assert_eq!(acp.name(), "acp");

        // …and fails loud without one.
        let missing = match provider_from_config(&config(Some("acp"))) {
            Err(err) => err,
            Ok(_) => panic!("acp without an [subagents.acp] table must be rejected"),
        };
        assert!(
            format!("{missing:#}").contains("requires an [subagents.acp]"),
            "{missing:#}"
        );
    }
}
