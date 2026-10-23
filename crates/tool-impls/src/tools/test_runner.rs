//! Cargo test runner tool: `run_tests`.
//!
//! `cargo test` runs workspace code, so this tool follows the same explicit
//! approval policy as the other code-executing tools.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use codesmith_agent_runtime::tools::cargo_failure_summary::summarize_cargo_failure;
use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str,
};
use codesmith_agent_runtime::tools::tool_catalog::RUN_TESTS_NAME;

use codesmith_agent_runtime::dependencies::ExternalTool;

const MAX_OUTPUT_CHARS: usize = 40_000;

/// Wall-clock cap for one `cargo test` invocation — a hung test binary
/// (deadlock, network wait, infinite loop) must not wedge the worker
/// forever. Mirrors the shell-tool timeout clamp (600_000 ms).
const CARGO_TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Tool for running `cargo test` in the workspace root.
pub struct RunTestsTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunTestsOutput {
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
    command: String,
}

#[async_trait]
impl ToolSpec for RunTestsTool {
    fn name(&self) -> &'static str {
        RUN_TESTS_NAME
    }

    fn description(&self) -> &'static str {
        "Run `cargo test` in the workspace root with optional extra arguments."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "args": {
                    "type": "string",
                    "description": "Optional extra arguments to pass to `cargo test` (shell-style)."
                },
                "all_features": {
                    "type": "boolean",
                    "description": "When true, include `--all-features`."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExecutesCode, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // `run_tests` declares `ToolCapability::ExecutesCode` — match the
        // default approval policy for code-executing tools.
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let all_features = optional_bool(&input, "all_features", false);
        let extra_args = optional_str(&input, "args")
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut args = vec!["test".to_string()];
        if all_features {
            args.push("--all-features".to_string());
        }
        if let Some(extra) = extra_args {
            let split = shlex::split(extra).ok_or_else(|| {
                ToolError::invalid_input("Failed to parse 'args' as shell-style tokens")
            })?;
            // Model-supplied args run under an approval prompt rendered as
            // "cargo test" — cargo flags themselves can widen the blast
            // radius far beyond the workspace (`--manifest-path` runs tests
            // from an arbitrary project; `--config` can set
            // `target.<triple>.runner`, an arbitrary command executed for
            // every test binary; `--target-dir`/`--target` redirect the
            // build). Reject them before a `--` separator; tokens after
            // `--` go to the test binary and are the intended use case.
            const BLOCKED_FLAGS: &[&str] =
                &["--manifest-path", "--config", "--target-dir", "--target"];
            for token in split.iter().take_while(|t| t.as_str() != "--") {
                let flag = token.split('=').next().unwrap_or("");
                if BLOCKED_FLAGS.contains(&flag) {
                    return Err(ToolError::invalid_input(format!(
                        "args flag `{flag}` redirects the build outside the workspace"
                    )));
                }
            }
            args.extend(split);
        }

        let command_str = format_command(&context.workspace, &args);
        let output = run_cargo(&context.workspace, &args).await?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout_raw = String::from_utf8_lossy(&output.stdout);
        let stderr_raw = String::from_utf8_lossy(&output.stderr);
        let stdout = truncate_with_note(&stdout_raw, MAX_OUTPUT_CHARS);
        let stderr = truncate_with_note(&stderr_raw, MAX_OUTPUT_CHARS);

        let result = RunTestsOutput {
            success: output.status.success(),
            exit_code,
            stdout,
            stderr,
            command: command_str,
        };

        let mut tool_result =
            ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))?;
        // Summarize from the FULL buffers, not the head-truncated views:
        // cargo prints compile chatter first and the actionable failure
        // details (the `failures:` listing, `panicked at`, `test result:`)
        // near the END — on a big build the truncated head holds only
        // compiler noise and the summary is silently dropped.
        if let Some(summary) = summarize_cargo_failure(
            &result.command,
            &stdout_raw,
            &stderr_raw,
            Some(result.exit_code),
        ) {
            tool_result = tool_result.with_metadata(json!({
                "summary": summary.summary,
                "cargo_failure_summary": summary.to_metadata_value(),
            }));
        }
        Ok(tool_result)
    }
}

// === Helpers ===

/// Per-stream capture cap for cargo output: builds can spray compiler
/// chatter; the actionable failure detail lives at the END, so the tail is
/// what gets kept. Bounds memory instead of buffering the whole stream.
const CARGO_OUTPUT_CAP_BYTES: usize = 32 * 1024 * 1024;

async fn run_cargo(workspace: &Path, args: &[String]) -> Result<std::process::Output, ToolError> {
    let Some(mut cmd) = codesmith_agent_runtime::dependencies::Cargo::tokio_command() else {
        return Err(ToolError::not_available(
            "cargo is not installed or not in PATH",
        ));
    };
    cmd.args(args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Own process group: a timed-out cargo run must take rustc / test
    // binaries with it, not orphan them.
    codesmith_agent_runtime::shell_manager::tokio_own_process_group(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::not_available("cargo is not installed or not in PATH")
        } else {
            ToolError::execution_failed(format!("Failed to run cargo: {e}"))
        }
    })?;
    let mut stdout = child.stdout.take().expect("stdout piped above");
    let mut stderr = child.stderr.take().expect("stderr piped above");
    let stdout_task = tokio::spawn(async move {
        codesmith_agent_runtime::utils::read_capped_tail(&mut stdout, CARGO_OUTPUT_CAP_BYTES).await
    });
    let stderr_task = tokio::spawn(async move {
        codesmith_agent_runtime::utils::read_capped_tail(&mut stderr, CARGO_OUTPUT_CAP_BYTES).await
    });
    let status = match tokio::time::timeout(CARGO_TEST_TIMEOUT, child.wait()).await {
        Ok(result) => {
            result.map_err(|e| ToolError::execution_failed(format!("Failed to run cargo: {e}")))?
        }
        Err(_) => {
            codesmith_agent_runtime::shell_manager::kill_process_group_of_opt(child.id());
            let _ = child.start_kill();
            return Err(ToolError::Timeout {
                seconds: CARGO_TEST_TIMEOUT.as_secs(),
            });
        }
    };
    let (stdout, stdout_dropped) = stdout_task.await.unwrap_or_default();
    let (stderr, stderr_dropped) = stderr_task.await.unwrap_or_default();
    let mut stdout = stdout;
    if stdout_dropped > 0 {
        let note = format!("[… {stdout_dropped} earlier bytes of output not captured]\n");
        stdout.splice(..0, note.into_bytes());
    }
    let mut stderr = stderr;
    if stderr_dropped > 0 {
        let note = format!("[… {stderr_dropped} earlier bytes of output not captured]\n");
        stderr.splice(..0, note.into_bytes());
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn format_command(workspace: &Path, args: &[String]) -> String {
    // Quote args via shlex so the recorded command round-trips through
    // `shlex::split` — `summarize_cargo_failure` runs that split on this
    // string, and a plain space join with a quoted arg returns None,
    // silently suppressing the whole failure summary.
    let joined =
        shlex::try_join(args.iter().map(String::as_str)).unwrap_or_else(|_| args.join(" "));
    format!("(cd {} && cargo {})", workspace.display(), joined)
}

fn truncate_with_note(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = char_boundary_index(text, max_chars);
    let truncated = &text[..end];
    let omitted_chars = text
        .chars()
        .count()
        .saturating_sub(truncated.chars().count());
    let note = format!(
        "\n\n[output truncated to {max_chars} characters; {omitted_chars} characters omitted]"
    );
    format!("{truncated}{note}")
}

fn char_boundary_index(text: &str, max_chars: usize) -> usize {
    if max_chars == 0 {
        return 0;
    }
    for (count, (idx, _)) in text.char_indices().enumerate() {
        if count == max_chars {
            return idx;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn cargo_available() -> bool {
        Command::new("cargo")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn init_cargo_project(root: &Path) -> std::path::PathBuf {
        let project_dir = root.join("project");
        fs::create_dir_all(&project_dir).expect("create project dir");
        let status = codesmith_agent_runtime::dependencies::Cargo::command()
            .expect("cargo not found")
            .args([
                "init",
                "--lib",
                "--vcs",
                "none",
                "-q",
                "--name",
                "eval_project",
            ])
            .current_dir(&project_dir)
            .status()
            .expect("cargo should spawn");
        assert!(status.success(), "cargo init failed");
        project_dir
    }

    /// `run_tests` is `ToolCapability::ExecutesCode`, so it must follow the
    /// explicit-approval policy that applies to other code-executing tools.
    #[test]
    fn run_tests_requires_user_approval() {
        let tool = RunTestsTool;
        assert_eq!(
            tool.approval_requirement(),
            ApprovalRequirement::Required,
            "run_tests must gate cargo test behind user approval"
        );
    }

    #[tokio::test]
    async fn run_tests_succeeds_on_fresh_project() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let project_dir = init_cargo_project(tmp.path());

        let ctx = ToolContext::new(&project_dir);
        let tool = RunTestsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);

        let parsed: RunTestsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(parsed.success);
        assert_eq!(parsed.exit_code, 0);
        assert!(parsed.command.contains("cargo test"));
    }

    #[tokio::test]
    async fn run_tests_reports_failures_without_hard_error() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let project_dir = init_cargo_project(tmp.path());

        let lib_rs = project_dir.join("src/lib.rs");
        let failing = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn fails() {
        assert_eq!(2 + 2, 5);
    }
}
"#;
        fs::write(&lib_rs, failing).expect("write failing test");

        let ctx = ToolContext::new(&project_dir);
        let tool = RunTestsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);

        let parsed: RunTestsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(!parsed.success);
        assert_ne!(parsed.exit_code, 0);
        let metadata = result.metadata.expect("metadata");
        assert_eq!(
            metadata["cargo_failure_summary"]["kind"],
            json!("test_failure")
        );
        assert!(
            metadata["cargo_failure_summary"]["summary"]
                .as_str()
                .unwrap()
                .contains("Failing tests:")
        );
    }

    #[test]
    fn truncation_adds_note() {
        let long = "x".repeat(MAX_OUTPUT_CHARS + 128);
        let truncated = truncate_with_note(&long, MAX_OUTPUT_CHARS);
        assert!(truncated.contains("output truncated"));
    }
}
