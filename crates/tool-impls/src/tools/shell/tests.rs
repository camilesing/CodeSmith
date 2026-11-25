#![allow(unsafe_code)]
// Tests mutate the process environment (`std::env::set_var`) which is
// `unsafe` in edition 2024. Env-mutating tests are serialized by `env_lock`
// and run single-threaded; see SAFETY notes at each call site.
use super::*;

use codesmith_agent_runtime::tools::spec::ToolContext;
use serde_json::{Value, json};
use std::process::Command;
use tempfile::tempdir;

// `env_lock` exists only to serialize Unix-only env-mutating tests.
// Windows builds gate that test out, so the helper would be dead code
// under `-Dwarnings` if the import + helper were unconditional.
#[cfg(unix)]
use std::sync::{Mutex, OnceLock};

#[cfg(unix)]
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Snapshot the named env vars and restore them on drop — including during
/// unwind, so a panicking test can't leak its scrubbed/overridden env into
/// the shared test process.
#[cfg(unix)]
struct EnvRestore {
    snapshots: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

#[cfg(unix)]
impl EnvRestore {
    fn new(names: &[&'static str]) -> Self {
        Self {
            snapshots: names.iter().map(|n| (*n, std::env::var_os(n))).collect(),
        }
    }
}

#[cfg(unix)]
impl Drop for EnvRestore {
    fn drop(&mut self) {
        // SAFETY: EnvRestore values are always created while the env_lock
        // guard is held and drop before it (reverse declaration order), so
        // env access stays serialized.
        for (name, value) in &self.snapshots {
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// Construct a shared shell manager backed by the runtime's no-op terminal
/// controller. Tests don't need crossterm raw-mode save/restore; the
/// crossterm-backed constructor lives in the CLI binary.
fn make_shared_shell_manager(workspace: std::path::PathBuf) -> SharedShellManager {
    use codesmith_agent_runtime::shell_manager::default_terminal_control;
    use std::sync::{Arc, Mutex};
    Arc::new(Mutex::new(ShellManager::with_terminal_control(
        workspace,
        default_terminal_control(),
    )))
}

/// Minimal [`SandboxBackend`] stub for approval-path tests that only need
/// `context.sandbox_backend` to be `Some` — `exec` is never called.
/// (The real `OpenSandboxBackend` lives in the CLI binary's `sandbox`
/// submodule and depends on `crate::config`, so it can't cross into the
/// tool-impls crate.)
#[derive(Debug)]
struct StubSandboxBackend;

/// Shared capability table for the stub backends: a one-shot, on/off-network
/// remote service shape.
fn stub_capabilities() -> codesmith_sandbox::runtime::SandboxBackendCapabilities {
    use codesmith_sandbox::runtime::{FsIsolation, NetworkControl};
    codesmith_sandbox::runtime::SandboxBackendCapabilities {
        fs_isolation: FsIsolation::RemoteService,
        network: NetworkControl::OnOff,
        requires_linux_host: false,
        interactive: false,
    }
}

#[async_trait::async_trait]
impl codesmith_sandbox::runtime::SandboxBackend for StubSandboxBackend {
    fn id(&self) -> &'static str {
        "stub"
    }

    fn capabilities(&self) -> codesmith_sandbox::runtime::SandboxBackendCapabilities {
        stub_capabilities()
    }

    async fn exec(
        &self,
        _request: codesmith_sandbox::runtime::SandboxExecRequest,
    ) -> anyhow::Result<codesmith_sandbox::runtime::SandboxOutput> {
        unimplemented!("approval_requirement_for_input does not exec")
    }
}

fn echo_command(message: &str) -> String {
    format!("echo {message}")
}

fn sleep_command(seconds: u64) -> String {
    let dispatcher = codesmith_agent_runtime::shell_dispatcher::global_dispatcher();
    if dispatcher.kind().is_powershell() {
        return format!("Start-Sleep -Seconds {seconds}");
    }
    #[cfg(windows)]
    {
        let ping_count = seconds.saturating_add(1);
        format!("ping 127.0.0.1 -n {ping_count} > NUL")
    }
    #[cfg(not(windows))]
    {
        format!("sleep {seconds}")
    }
}

fn sleep_then_echo_command(seconds: u64, message: &str) -> String {
    let dispatcher = codesmith_agent_runtime::shell_dispatcher::global_dispatcher();
    if dispatcher.kind().is_powershell() {
        return format!("Start-Sleep -Seconds {seconds}; echo {message}");
    }
    #[cfg(windows)]
    {
        let ping_count = seconds.saturating_add(1);
        format!("ping 127.0.0.1 -n {ping_count} > NUL && echo {message}")
    }
    #[cfg(not(windows))]
    {
        format!("sleep {seconds} && echo {message}")
    }
}

fn echo_stdin_command() -> String {
    let dispatcher = codesmith_agent_runtime::shell_dispatcher::global_dispatcher();
    if dispatcher.kind().is_powershell() {
        return "[Console]::In.ReadToEnd()".to_string();
    }
    #[cfg(windows)]
    {
        "more".to_string()
    }
    #[cfg(not(windows))]
    {
        "cat".to_string()
    }
}

fn network_restricted_context(tmp: &std::path::Path) -> ToolContext {
    ToolContext::new(tmp)
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::WorkspaceWrite {
            writable_roots: vec![tmp.to_path_buf()],
            network_access: false,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        })
        .with_shell_network_denied_hint(
            "Shell command blocked: Plan mode runs shell commands in a network-restricted sandbox.",
        )
        .with_sandbox_runtime(SandboxRuntimeConfig {
            enabled: true,
            fail_if_unavailable: false,
            auto_allow_bash_if_sandboxed: true,
            ..SandboxRuntimeConfig::default()
        })
}

fn failed_network_shell_result(stdout: &str, stderr: &str) -> ShellResult {
    ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(6),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        duration_ms: 25,
        stdout_len: stdout.len(),
        stderr_len: stderr.len(),
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        sandboxed: true,
        sandbox_type: Some("seatbelt".to_string()),
        sandbox_denied: false,
        sandbox_requested: true,
        sandbox_effective: true,
        sandbox_backend: Some("seatbelt".to_string()),
        sandbox_unavailable_reason: None,
        sandbox_fallback_allowed: false,
        sandbox_excluded_command: None,
        sandbox_fail_closed: false,
    }
}

#[test]
fn exec_shell_auto_allows_only_when_sandbox_would_enforce() {
    let tmp = tempdir().expect("tempdir");
    let tool = ExecShellTool;
    let input = json!({ "command": "printf ok" });

    let unsandboxed = ToolContext::new(tmp.path()).with_sandbox_runtime(SandboxRuntimeConfig {
        enabled: false,
        auto_allow_bash_if_sandboxed: true,
        ..SandboxRuntimeConfig::default()
    });
    assert_eq!(
        tool.approval_requirement_for_input(&input, &unsandboxed),
        ApprovalRequirement::Required
    );

    let external = ToolContext::new(tmp.path())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::ExternalSandbox {
            network_access: true,
        })
        .with_sandbox_backend(std::sync::Arc::new(StubSandboxBackend))
        .with_sandbox_runtime(SandboxRuntimeConfig {
            auto_allow_bash_if_sandboxed: true,
            ..SandboxRuntimeConfig::default()
        });
    assert_eq!(
        tool.approval_requirement_for_input(&input, &external),
        ApprovalRequirement::Auto
    );

    // DangerFullAccess must NOT ride the sandbox auto-allow even with a
    // backend configured: the backend runs it unconfined, so a predicted
    // "sandboxed" would be an over-claim granting approval the sandbox
    // never justifies.
    let full_access = ToolContext::new(tmp.path())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::DangerFullAccess)
        .with_sandbox_backend(std::sync::Arc::new(StubSandboxBackend))
        .with_sandbox_runtime(SandboxRuntimeConfig {
            auto_allow_bash_if_sandboxed: true,
            ..SandboxRuntimeConfig::default()
        });
    assert_eq!(
        tool.approval_requirement_for_input(&input, &full_access),
        ApprovalRequirement::Required
    );
}

/// An execpolicy `AskUser` decision must reach the user: the predictive
/// sandbox Auto path defers to the policy, and an unattended
/// (auto-approve) run is blocked instead of silently executing (OCR
/// review 2026-11-03).
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn execpolicy_ask_user_is_enforced_not_recorded() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore = EnvRestore::new(&["CODESMITH_HOME"]);
    let home = tempdir().expect("tempdir");
    // SAFETY: held under env_lock; restored by EnvRestore on drop.
    unsafe { std::env::set_var("CODESMITH_HOME", home.path()) };
    // No allow rules: every command evaluates to AskUser.
    std::fs::write(
        home.path().join("execpolicy.toml"),
        "[rules.default]\nallow = []\ndeny = []\n",
    )
    .expect("write policy");

    let tmp = tempdir().expect("tempdir");
    let tool = ExecShellTool;
    let input = json!({ "command": "printf ok" });

    // Predicted-sandbox Auto must not override a policy that says ask.
    let sandboxed = ToolContext::new(tmp.path())
        .with_elevated_sandbox_policy(ExecutionSandboxPolicy::ExternalSandbox {
            network_access: true,
        })
        .with_sandbox_backend(std::sync::Arc::new(StubSandboxBackend))
        .with_sandbox_runtime(SandboxRuntimeConfig {
            auto_allow_bash_if_sandboxed: true,
            ..SandboxRuntimeConfig::default()
        });
    assert_eq!(
        tool.approval_requirement_for_input(&input, &sandboxed),
        ApprovalRequirement::Required,
        "execpolicy AskUser must force approval"
    );

    // Unattended (auto-approve) runs are blocked outright.
    let unattended = ToolContext::with_auto_approve(tmp.path(), true, "notes.md", "mcp.json", true);
    let result = tool.execute(input, &unattended).await.expect("execute");
    assert!(
        result.content.contains("execpolicy"),
        "expected an execpolicy block, got: {}",
        result.content
    );
}

#[tokio::test]
async fn shell_result_metadata_reports_sandbox_effective_state() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let mut result = failed_network_shell_result("000", "");
    result.sandbox_fallback_allowed = true;
    result.sandbox_unavailable_reason = Some("sandbox backend missing".to_string());
    result.sandbox_effective = false;
    result.sandboxed = false;
    result.sandbox_type = None;
    result.sandbox_backend = None;

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "curl https://api.github.com".to_string(),
            result,
            stdout_total_len: 3,
            stderr_total_len: 0,
        },
        &ctx,
    )
    .await;

    let metadata = tool_result.metadata.expect("metadata");
    assert_eq!(metadata["sandbox_requested"], json!(true));
    assert_eq!(metadata["sandbox_effective"], json!(false));
    assert_eq!(metadata["sandbox_fallback_allowed"], json!(true));
    assert_eq!(
        metadata["sandbox_unavailable_reason"],
        json!("sandbox backend missing")
    );
}

#[test]
#[cfg(unix)]
fn shell_execution_scrubs_git_fsmonitor_and_hookspath() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::new(&[
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG_KEY_1",
        "GIT_CONFIG_VALUE_1",
    ]);
    // SAFETY: the env_lock guard serializes env access; no other test reads
    // these vars while held.
    unsafe {
        std::env::remove_var("GIT_CONFIG_COUNT");
        std::env::remove_var("GIT_CONFIG_KEY_0");
        std::env::remove_var("GIT_CONFIG_VALUE_0");
        std::env::remove_var("GIT_CONFIG_KEY_1");
        std::env::remove_var("GIT_CONFIG_VALUE_1");
    }

    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let result = manager
        .execute_with_options_env(
            "printf '%s\\n%s\\n%s\\n%s\\n%s\\n' \"${GIT_CONFIG_COUNT-unset}\" \"${GIT_CONFIG_KEY_0-unset}\" \"${GIT_CONFIG_VALUE_0-unset}\" \"${GIT_CONFIG_KEY_1-unset}\" \"${GIT_CONFIG_VALUE_1-unset}\"",
            None,
            5000,
            false,
            None,
            false,
            None,
            std::collections::HashMap::new(),
        )
        .expect("execute");

    assert_eq!(result.status, ShellStatus::Completed);
    let lines: Vec<&str> = result.stdout.trim_end_matches('\n').split('\n').collect();
    assert_eq!(lines[0], "2");
    assert_eq!(lines[1], "core.fsmonitor");
    // Explicit boolean disable; the empty value only worked because git
    // parses "" as false.
    assert_eq!(lines[2], "false");
    assert_eq!(lines[3], "core.hooksPath");
    // hooksPath has no "off" spelling — it points at the (never created)
    // owner-only disabled dir so the hook lookup deterministically fails.
    assert!(
        lines[4].ends_with("hooks-disabled") && lines[4].contains(".codesmith"),
        "hooksPath points at the disabled dir: {}",
        lines[4]
    );
}

#[test]
#[cfg(unix)]
fn shell_execution_scrubs_parent_env_and_keeps_explicit_env() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::new(&["DEEPSEEK_CHILD_ENV_SHELL_SECRET"]);
    // SAFETY: the env_lock guard serializes env access; no other test reads
    // this var while held.
    unsafe {
        std::env::set_var("DEEPSEEK_CHILD_ENV_SHELL_SECRET", "parent-secret");
    }

    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());
    let mut extra = std::collections::HashMap::new();
    extra.insert(
        "DEEPSEEK_CHILD_ENV_EXPLICIT".to_string(),
        "explicit-value".to_string(),
    );

    let result = manager
        .execute_with_options_env(
            "printf '%s\\n%s\\n' \"${DEEPSEEK_CHILD_ENV_SHELL_SECRET-unset}\" \"${DEEPSEEK_CHILD_ENV_EXPLICIT-unset}\"",
            None,
            5000,
            false,
            None,
            false,
            None,
            extra,
        )
        .expect("execute");

    assert_eq!(result.status, ShellStatus::Completed);
    assert_eq!(result.stdout, "unset\nexplicit-value\n");
}

#[test]
fn test_sync_execution() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute(&echo_command("hello"), None, 5000, false)
        .expect("execute");

    assert_eq!(result.status, ShellStatus::Completed);
    assert!(result.stdout.contains("hello"));
    assert!(result.task_id.is_none());
}

#[test]
fn test_background_execution() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute(&sleep_then_echo_command(1, "done"), None, 5000, true)
        .expect("execute");

    assert_eq!(result.status, ShellStatus::Running);
    assert!(result.task_id.is_some());

    let task_id = result
        .task_id
        .expect("background execution should return task_id");

    // Wait for completion
    let final_result = manager
        .get_output(&task_id, true, 5000)
        .expect("get_output");

    assert_eq!(final_result.status, ShellStatus::Completed);
    assert!(final_result.stdout.contains("done"));
}

#[test]
fn test_timeout() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute(&sleep_command(10), None, 1000, false)
        .expect("execute");

    assert_eq!(result.status, ShellStatus::TimedOut);
}

#[test]
fn test_kill() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute(&sleep_command(60), None, 5000, true)
        .expect("execute");

    let task_id = result
        .task_id
        .expect("background execution should return task_id");

    // Kill it
    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
}

#[test]
fn test_write_stdin_streams_output() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute_with_options(&echo_stdin_command(), None, 5000, true, None, false, None)
        .expect("execute");

    let task_id = result
        .task_id
        .expect("background execution should return task_id");

    manager
        .write_stdin(&task_id, "hello\n", true)
        .expect("write stdin");

    let delta = manager
        .get_output_delta(&task_id, true, 5000)
        .expect("get_output_delta");

    assert!(delta.result.stdout.contains("hello"));

    let delta2 = manager
        .get_output_delta(&task_id, false, 0)
        .expect("get_output_delta");
    assert!(delta2.result.stdout.is_empty());
}

#[test]
#[cfg(unix)]
fn background_tty_command_has_controlling_terminal() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let result = manager
        .execute_with_options(
            "sh -c 'exec 3<>/dev/tty && printf tty-ok && exec 3>&-'",
            None,
            5000,
            true,
            None,
            true,
            Some(ExecutionSandboxPolicy::DangerFullAccess),
        )
        .expect("execute tty command");

    let task_id = result
        .task_id
        .expect("background tty execution should return task_id");

    let done = manager
        .get_output(&task_id, true, 10_000)
        .expect("get tty command output");

    assert_eq!(done.status, ShellStatus::Completed);
    assert_eq!(done.exit_code, Some(0));
    assert!(
        done.stdout.contains("tty-ok"),
        "tty output should confirm /dev/tty opened; got {done:?}"
    );
}

#[test]
fn test_job_list_poll_cancel_and_stale_snapshot() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let started = manager
        .execute(&sleep_then_echo_command(1, "done"), None, 5000, true)
        .expect("execute");
    let task_id = started.task_id.expect("task id");
    manager
        .tag_linked_task(&task_id, Some("task_123".to_string()))
        .expect("tag linked task");

    let running = manager.list_jobs();
    let job = running
        .iter()
        .find(|job| job.id == task_id)
        .expect("running job");
    assert_eq!(job.status, ShellStatus::Running);
    assert_eq!(job.linked_task_id.as_deref(), Some("task_123"));
    assert!(job.command.contains("done"));
    assert_eq!(job.cwd, tmp.path());

    let completed = manager
        .poll_delta(&task_id, true, 5000)
        .expect("poll delta");
    assert_eq!(completed.result.status, ShellStatus::Completed);
    assert!(completed.result.stdout.contains("done"));

    let detail = manager.inspect_job(&task_id).expect("inspect");
    assert!(detail.stdout.contains("done"));
    assert_eq!(detail.snapshot.status, ShellStatus::Completed);

    manager.remember_stale_job(
        "shell_stale",
        "cargo test",
        tmp.path().to_path_buf(),
        Some("task_old".to_string()),
    );
    let stale = manager
        .list_jobs()
        .into_iter()
        .find(|job| job.id == "shell_stale")
        .expect("stale job");
    assert!(stale.stale);
    assert_eq!(stale.linked_task_id.as_deref(), Some("task_old"));
}

#[test]
fn test_job_cancel_updates_completion_state() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let started = manager
        .execute(&sleep_command(60), None, 5000, true)
        .expect("execute");
    let task_id = started.task_id.expect("task id");

    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
    let job = manager.inspect_job(&task_id).expect("inspect");
    assert_eq!(job.snapshot.status, ShellStatus::Killed);
    assert!(!job.snapshot.stdin_available);
}

#[test]
fn test_output_truncation() {
    let long_output = "x".repeat(50_000);
    let (truncated, _meta) = truncate_with_meta(&long_output);

    assert!(truncated.len() < long_output.len());
    assert!(truncated.contains("truncated"));
}

#[test]
fn test_truncate_with_meta_reports_omission_counts() {
    let long_output = format!("line1\nline2\n{}", "x".repeat(60_000));
    let (truncated, meta) = truncate_with_meta(&long_output);

    assert!(meta.truncated);
    assert!(meta.original_len >= long_output.len());
    assert!(meta.omitted > 0);
    assert!(truncated.contains("bytes omitted"));
}

#[test]
fn network_restricted_hint_detects_silent_curl_failure() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("000", "");

    let hint = shell_network_restricted_hint(
        &ctx,
        "curl -s -o /dev/null -w '%{http_code}' https://api.github.com",
        &result,
    )
    .expect("network-restricted hint");

    assert!(hint.contains("Plan mode"));
}

#[test]
fn network_restricted_hint_ignores_local_failures() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("", "No such file or directory");

    assert!(shell_network_restricted_hint(&ctx, "cat missing.txt", &result).is_none());
}

#[test]
fn network_restricted_hint_requires_corroborating_failure_symptom() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    // A sandboxed failure with no output and a plain exit code could be a
    // crash or a silent `exit 1`; that alone must not claim network denial.
    let result = failed_network_shell_result("", "");

    assert!(
        shell_network_restricted_hint(&ctx, "curl -s https://api.github.com", &result).is_none()
    );
}

#[test]
fn network_restricted_hint_matches_subcommand_not_argument_text() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("", "couldn't connect to host");

    assert!(
        shell_network_restricted_hint(&ctx, "git clone https://example.com/repo", &result)
            .is_some()
    );
    // "clone" appears only inside the commit message argument.
    assert!(
        shell_network_restricted_hint(&ctx, "git commit -m \"fix clone bug\"", &result).is_none()
    );
}

#[tokio::test]
async fn shell_delta_result_surfaces_network_restricted_hint() {
    let tmp = tempdir().expect("tempdir");
    let ctx = network_restricted_context(tmp.path());
    let result = failed_network_shell_result("000", "");

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "gh issue list".to_string(),
            result,
            stdout_total_len: 3,
            stderr_total_len: 0,
        },
        &ctx,
    )
    .await;

    assert!(!tool_result.success);
    assert!(tool_result.content.starts_with("Shell command blocked"));
    let metadata = tool_result.metadata.expect("metadata");
    assert_eq!(
        metadata
            .get("sandbox_network_restricted")
            .and_then(Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn shell_delta_result_includes_cargo_failure_summary() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(101),
        stdout: "running 1 test\ntest tests::fails ... FAILED\n\nfailures:\n\n---- tests::fails stdout ----\nthread 'tests::fails' panicked at src/lib.rs:7:9:\nboom\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; finished in 0.00s\n".to_string(),
        stderr: "error: test failed, to rerun pass `--lib`".to_string(),
        duration_ms: 12,
        stdout_len: 0,
        stderr_len: 0,
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        sandboxed: false,
        sandbox_type: None,
        sandbox_denied: false,
        sandbox_requested: false,
        sandbox_effective: false,
        sandbox_backend: None,
        sandbox_unavailable_reason: None,
        sandbox_fallback_allowed: false,
        sandbox_excluded_command: None,
        sandbox_fail_closed: false,
    };

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "cargo test".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    )
    .await;

    let metadata = tool_result.metadata.expect("metadata");
    assert_eq!(
        metadata["cargo_failure_summary"]["kind"],
        json!("test_failure")
    );
    assert!(
        metadata["cargo_failure_summary"]["summary"]
            .as_str()
            .unwrap()
            .contains("Failing tests: tests::fails")
    );
    assert!(
        metadata["summary"]
            .as_str()
            .unwrap()
            .contains("error: test failed")
    );
}

#[tokio::test]
async fn shell_delta_result_keeps_existing_summary_for_generic_cargo_failure() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(1),
        stdout: "build failed".to_string(),
        stderr: "command failed without structured cargo diagnostics".to_string(),
        duration_ms: 12,
        stdout_len: 0,
        stderr_len: 0,
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        sandboxed: false,
        sandbox_type: None,
        sandbox_denied: false,
        sandbox_requested: false,
        sandbox_effective: false,
        sandbox_backend: None,
        sandbox_unavailable_reason: None,
        sandbox_fallback_allowed: false,
        sandbox_excluded_command: None,
        sandbox_fail_closed: false,
    };

    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "cargo test".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    )
    .await;

    let metadata = tool_result.metadata.expect("metadata");
    assert!(metadata.get("cargo_failure_summary").is_none());
    assert_eq!(
        metadata["summary"],
        json!("command failed without structured cargo diagnostics")
    );
}

#[test]
fn test_summarize_output_strips_truncation_note() {
    let long_output = "x".repeat(60_000);
    let (truncated, _meta) = truncate_with_meta(&long_output);
    let summary = summarize_output(&truncated);
    assert!(!summary.contains("Output truncated at"));
}

#[tokio::test]
async fn test_exec_shell_metadata_includes_summaries() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let tool = ExecShellTool;

    let result = tool
        .execute(json!({"command": echo_command("hello")}), &ctx)
        .await
        .expect("execute");
    assert!(result.success);

    let meta = result.metadata.expect("metadata");
    let summary = meta
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(summary.contains("hello"));
    assert!(meta.get("stdout_len").is_some());
    assert!(meta.get("stdout_truncated").is_some());
}

#[cfg(not(windows))]
#[tokio::test]
async fn test_exec_shell_combined_output_uses_single_stream() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let tool = ExecShellTool;
    let command = "printf 'out\\n'; printf 'err\\n' >&2";

    let result = tool
        .execute(json!({"command": command, "combined_output": true}), &ctx)
        .await
        .expect("execute");
    assert!(result.success, "{}", result.content);
    assert!(result.content.contains("out"), "{}", result.content);
    assert!(result.content.contains("err"), "{}", result.content);

    let meta = result.metadata.expect("metadata");
    assert_eq!(
        meta.get("combined_output").and_then(Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn test_exec_shell_foreground_timeout_guides_background_rerun() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let tool = ExecShellTool;

    let result = tool
        .execute(
            json!({
                "command": sleep_command(10),
                "timeout_ms": 1000
            }),
            &ctx,
        )
        .await
        .expect("execute");

    assert!(!result.success);
    assert!(result.content.contains("task_shell_start"));
    assert!(result.content.contains("background: true"));
    assert!(result.content.contains("process killed"));
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("TimedOut"));
    let recovery = meta
        .get("foreground_timeout_recovery")
        .expect("timeout recovery metadata");
    assert_eq!(
        recovery
            .get("exec_shell_background")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        recovery
            .get("hint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("exec_shell_wait")
    );
}

/// Output first, then keep running — so the foreground poll loop sees new
/// output while the command is still `Running`.
fn echo_then_sleep_command(message: &str, seconds: u64) -> String {
    let dispatcher = codesmith_agent_runtime::shell_dispatcher::global_dispatcher();
    if dispatcher.kind().is_powershell() {
        return format!("echo {message}; Start-Sleep -Seconds {seconds}");
    }
    format!("{} && {}", echo_command(message), sleep_command(seconds))
}

/// Records every checkpoint a tool reports (`tool-execution-update`).
struct RecordingSink(std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>);
#[async_trait::async_trait]
impl codesmith_agent_runtime::tools::spec::ToolProgressSink for RecordingSink {
    async fn progress(&self, id: &str, name: &str, message: &str) {
        self.0
            .lock()
            .unwrap()
            .push((id.to_string(), name.to_string(), message.to_string()));
    }
}

/// A long-running foreground command reports its new output through the
/// context's progress sink while it runs: the tool name rides along, the
/// id is the shell task id, and the first checkpoint carries the output
/// produced so far. No sink (`ToolContext::new`) ⇒ the loop is inert —
/// every other shell test runs that way.
#[tokio::test]
async fn test_exec_shell_foreground_reports_progress_checkpoints() {
    let tmp = tempdir().expect("tempdir");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ctx = ToolContext::new(tmp.path())
        .with_progress_sink(Some(std::sync::Arc::new(RecordingSink(seen.clone()))));
    let tool = ExecShellTool;

    let result = tool
        .execute(
            json!({
                "command": echo_then_sleep_command("first", 2),
                "timeout_ms": 20000
            }),
            &ctx,
        )
        .await
        .expect("execute");
    assert!(result.success, "{}", result.content);

    let got = seen.lock().unwrap().clone();
    assert!(
        !got.is_empty(),
        "at least one checkpoint while the command ran"
    );
    assert!(
        got.iter()
            .all(|(id, name, _)| name == "exec_shell" && !id.is_empty()),
        "{got:?}"
    );
    assert!(
        got.iter().any(|(_, _, message)| message.contains("first")),
        "the checkpoint carries the output produced so far: {got:?}"
    );
}

#[test]
fn tail_capped_respects_char_boundaries() {
    assert_eq!(tail_capped("abc", 10), "abc");
    assert_eq!(tail_capped("abcdef", 3), "def");
    // A cut that would land inside a multi-byte char moves forward to the
    // next boundary instead of panicking.
    let text = "ab€cd"; // '€' is 3 bytes at [2..5]
    assert_eq!(tail_capped(text, 4), "cd");
    assert_eq!(tail_capped(text, 5), "€cd");
}

/// A foreground `exec_shell` run is a shell-manager job too (the tool polls a
/// background spawn). It must carry the foreground mark — the engine's
/// shell-job watcher does not bridge its outcome a second time — while a
/// `background: true` run must not.
#[tokio::test]
async fn foreground_run_is_marked_and_background_run_is_not() {
    let tmp = tempdir().expect("tempdir");
    let shell_manager = make_shared_shell_manager(tmp.path().to_path_buf());
    let ctx =
        ToolContext::new(tmp.path()).with_shell_manager(wrap_shell_manager(shell_manager.clone()));

    ExecShellTool
        .execute(json!({"command": echo_command("fg-marker")}), &ctx)
        .await
        .expect("foreground run");
    ExecShellTool
        .execute(
            json!({"command": echo_command("bg-marker"), "background": true}),
            &ctx,
        )
        .await
        .expect("background run");

    let jobs = shell_manager
        .lock()
        .expect("shell manager lock")
        .list_jobs();
    assert_eq!(jobs.len(), 2, "one job per run");
    let foreground = jobs.iter().find(|job| job.command.contains("fg-marker"));
    let background = jobs.iter().find(|job| job.command.contains("bg-marker"));
    assert!(
        foreground.is_some_and(|job| job.foreground),
        "the foreground run is marked: {jobs:?}"
    );
    assert!(
        background.is_some_and(|job| !job.foreground),
        "the background run is not marked: {jobs:?}"
    );
}

#[tokio::test]
async fn test_exec_shell_foreground_cancel_kills_process() {
    let tmp = tempdir().expect("tempdir");
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let ctx = ToolContext::new(tmp.path()).with_cancel_token(cancel_token.clone());
    let command = sleep_command(30);

    let task = tokio::spawn(async move {
        ExecShellTool
            .execute(
                json!({
                    "command": command,
                    "timeout_ms": 600_000
                }),
                &ctx,
            )
            .await
            .expect("execute")
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel_token.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("foreground shell should observe cancellation")
        .expect("task should not panic");

    assert!(!result.success);
    assert!(result.content.contains("Command canceled"));
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Killed"));
    assert_eq!(meta.get("canceled").and_then(Value::as_bool), Some(true));
}

#[tokio::test]
async fn test_exec_shell_foreground_can_move_to_background() {
    let tmp = tempdir().expect("tempdir");
    let shell_manager = make_shared_shell_manager(tmp.path().to_path_buf());
    let ctx =
        ToolContext::new(tmp.path()).with_shell_manager(wrap_shell_manager(shell_manager.clone()));
    let command = sleep_command(30);
    let task_ctx = ctx.clone();

    let task = tokio::spawn(async move {
        ExecShellTool
            .execute(
                json!({
                    "command": command,
                    "timeout_ms": 600_000
                }),
                &task_ctx,
            )
            .await
            .expect("execute")
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    shell_manager
        .lock()
        .expect("shell manager lock")
        .request_foreground_background();

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("foreground shell should detach")
        .expect("task should not panic");

    assert!(result.success);
    assert!(result.content.contains("Command moved to background"));
    // A detached job is a background job again: no foreground mark remains,
    // so the engine's shell-job watcher may bridge its completion.
    assert!(
        !shell_manager
            .lock()
            .expect("shell manager lock")
            .list_jobs()
            .iter()
            .any(|job| job.foreground),
        "no job stays marked as foreground-owned after the detach"
    );
    assert!(result.content.contains("exec_shell_cancel"));

    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Running"));
    assert_eq!(
        meta.get("backgrounded").and_then(Value::as_bool),
        Some(true)
    );
    let task_id = meta
        .get("task_id")
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let mut manager = shell_manager.lock().expect("shell manager lock");
    let job = manager.inspect_job(&task_id).expect("inspect job");
    assert_eq!(job.snapshot.status, ShellStatus::Running);
    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
}

#[tokio::test]
async fn test_exec_shell_wait_cancel_leaves_background_process_running() {
    let tmp = tempdir().expect("tempdir");
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let shell_manager = make_shared_shell_manager(tmp.path().to_path_buf());
    let ctx = ToolContext::new(tmp.path())
        .with_cancel_token(cancel_token.clone())
        .with_shell_manager(wrap_shell_manager(shell_manager.clone()));
    let started = shell_manager
        .lock()
        .expect("shell manager lock")
        .execute(&sleep_command(30), None, 600_000, true)
        .expect("execute");
    let task_id = started.task_id.expect("task id");
    let wait_task_id = task_id.clone();
    let task_ctx = ctx.clone();

    let task = tokio::spawn(async move {
        ShellWaitTool::new("exec_shell_wait")
            .execute(
                json!({
                    "task_id": wait_task_id,
                    "wait": true,
                    "timeout_ms": 600_000
                }),
                &task_ctx,
            )
            .await
            .expect("wait")
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel_token.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("wait should observe cancellation")
        .expect("task should not panic");

    assert!(result.success);
    assert!(result.content.contains("still running"));
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Running"));
    assert_eq!(
        meta.get("wait_canceled").and_then(Value::as_bool),
        Some(true)
    );

    let mut manager = shell_manager.lock().expect("shell manager lock");
    let job = manager.inspect_job(&task_id).expect("inspect job");
    assert_eq!(job.snapshot.status, ShellStatus::Running);
    let killed = manager.kill(&task_id).expect("kill");
    assert_eq!(killed.status, ShellStatus::Killed);
}

#[tokio::test]
async fn test_completed_background_shell_releases_process_handles() {
    let tmp = tempdir().expect("tempdir");
    let shell_manager = make_shared_shell_manager(tmp.path().to_path_buf());
    let ctx =
        ToolContext::new(tmp.path()).with_shell_manager(wrap_shell_manager(shell_manager.clone()));
    let started = shell_manager
        .lock()
        .expect("shell manager lock")
        .execute(&echo_command("done"), None, 600_000, true)
        .expect("execute");
    let task_id = started.task_id.expect("task id");

    let result = ShellWaitTool::new("exec_shell_wait")
        .execute(
            json!({
                "task_id": task_id.clone(),
                "wait": true,
                "timeout_ms": 5_000
            }),
            &ctx,
        )
        .await
        .expect("wait");

    assert!(result.success);
    let mut manager = shell_manager.lock().expect("shell manager lock");
    let shell = manager.processes.get_mut(&task_id).expect("tracked shell");
    shell.poll();
    assert_eq!(shell.status, ShellStatus::Completed);
    assert!(shell.stdin.is_none());
    assert!(shell.child.is_none());
    assert!(shell.stdout_thread.is_none());
    assert!(shell.stderr_thread.is_none());
}

#[tokio::test]
async fn test_exec_shell_cancel_tool_kills_background_process() {
    let tmp = tempdir().expect("tempdir");
    let shell_manager = make_shared_shell_manager(tmp.path().to_path_buf());
    let ctx =
        ToolContext::new(tmp.path()).with_shell_manager(wrap_shell_manager(shell_manager.clone()));
    let started = shell_manager
        .lock()
        .expect("shell manager lock")
        .execute(&sleep_command(30), None, 600_000, true)
        .expect("execute");
    let task_id = started.task_id.expect("task id");

    let result = ShellCancelTool
        .execute(json!({ "task_id": task_id }), &ctx)
        .await
        .expect("cancel");

    assert!(result.success);
    assert!(result.content.contains("Canceled background command"));
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Killed"));

    let task_id = meta
        .get("task_id")
        .and_then(Value::as_str)
        .expect("task id");
    let mut manager = shell_manager.lock().expect("shell manager lock");
    let job = manager.inspect_job(task_id).expect("inspect job");
    assert_eq!(job.snapshot.status, ShellStatus::Killed);
}

#[tokio::test]
async fn test_exec_shell_cancel_tool_can_kill_all_running_processes() {
    let tmp = tempdir().expect("tempdir");
    let shell_manager = make_shared_shell_manager(tmp.path().to_path_buf());
    let ctx =
        ToolContext::new(tmp.path()).with_shell_manager(wrap_shell_manager(shell_manager.clone()));
    let first = shell_manager
        .lock()
        .expect("shell manager lock")
        .execute(&sleep_command(30), None, 600_000, true)
        .expect("execute first")
        .task_id
        .expect("first task id");
    let second = shell_manager
        .lock()
        .expect("shell manager lock")
        .execute(&sleep_command(30), None, 600_000, true)
        .expect("execute second")
        .task_id
        .expect("second task id");

    let result = ShellCancelTool
        .execute(json!({ "all": true }), &ctx)
        .await
        .expect("cancel all");

    assert!(result.success);
    let meta = result.metadata.expect("metadata");
    assert_eq!(meta.get("status").and_then(Value::as_str), Some("Killed"));
    assert_eq!(meta.get("canceled").and_then(Value::as_u64), Some(2));

    let mut manager = shell_manager.lock().expect("shell manager lock");
    let first_job = manager.inspect_job(&first).expect("inspect first");
    let second_job = manager.inspect_job(&second).expect("inspect second");
    assert_eq!(first_job.snapshot.status, ShellStatus::Killed);
    assert_eq!(second_job.snapshot.status, ShellStatus::Killed);
}

fn make_failed_result(stderr: &str) -> ShellResult {
    ShellResult {
        task_id: None,
        status: ShellStatus::Failed,
        exit_code: Some(1),
        stdout: String::new(),
        stderr: stderr.to_string(),
        duration_ms: 0,
        stdout_len: 0,
        stderr_len: stderr.len(),
        stdout_omitted: 0,
        stderr_omitted: 0,
        stdout_truncated: false,
        sandboxed: false,
        sandbox_type: None,
        sandbox_denied: false,
        stderr_truncated: false,
        sandbox_requested: false,
        sandbox_effective: false,
        sandbox_backend: None,
        sandbox_unavailable_reason: None,
        sandbox_fallback_allowed: false,
        sandbox_excluded_command: None,
        sandbox_fail_closed: false,
    }
}

#[test]
fn test_macos_provenance_detected_by_activity_time_message() {
    let result = make_failed_result(
        "failed to update builder last activity time: open \
         /Users/user/.docker/buildx/activity/.tmp-abc: operation not permitted",
    );
    assert!(looks_like_macos_provenance_failure(&result));
}

#[test]
fn test_macos_provenance_detected_by_activity_path_and_eperm() {
    let result = make_failed_result(
        "error: open /home/user/.docker/buildx/activity/foo: operation not permitted",
    );
    assert!(looks_like_macos_provenance_failure(&result));
}

#[test]
fn test_macos_provenance_not_triggered_on_success() {
    let mut result = make_failed_result(
        "failed to update builder last activity time: open \
         /Users/user/.docker/buildx/activity/.tmp-abc: operation not permitted",
    );
    result.status = ShellStatus::Completed;
    result.exit_code = Some(0);
    assert!(!looks_like_macos_provenance_failure(&result));
}

#[test]
fn test_macos_provenance_not_triggered_on_unrelated_eperm() {
    let result = make_failed_result("open /some/other/path: operation not permitted");
    assert!(!looks_like_macos_provenance_failure(&result));
}

// Regression test for #828: shell spawns an orphaned background subprocess
// (simulating `nohup curl`) that keeps the pipe write-end open after the shell
// exits. collect_output() must not block indefinitely — it kills the whole
// process group first, allowing reader threads to get EOF and exit.
#[cfg(unix)]
#[test]
fn test_orphaned_subprocess_does_not_block_collect_output() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    // sh spawns `sleep 100 &` and exits; the sleep subprocess inherits the
    // pipe write-ends and would keep reader threads blocked without the fix.
    let result = manager
        .execute("sh -c 'sleep 100 &'", None, 5000, true)
        .expect("execute");
    let task_id = result.task_id.expect("task id");

    // Drive to completion with a tight timeout — must not hang.
    let done = manager
        .get_output(&task_id, true, 3000)
        .expect("get_output must complete, not hang");
    assert_eq!(done.status, ShellStatus::Completed);
}

#[test]
fn test_list_jobs_cleans_up_completed_old_processes() {
    let tmp = tempdir().expect("tempdir");
    let mut manager = ShellManager::new(tmp.path().to_path_buf());

    let bg = manager
        .execute(&echo_command("bg"), None, 5000, true)
        .expect("execute bg");
    let bg_id = bg.task_id.expect("bg task id");
    manager.get_output(&bg_id, true, 3000).expect("bg done");

    // Both the completed job and any tracking state should be present.
    assert!(!manager.processes.is_empty());

    // cleanup(ZERO) removes all completed processes immediately.
    manager.cleanup(Duration::ZERO);
    assert!(
        manager.processes.is_empty(),
        "completed processes should be evicted by cleanup"
    );
}

/// Regression for #1691: a `git commit -m "feat: complete sub-pages"` shell
/// command must reach the OS shell with its quoted message intact (one argv
/// slot), never split into `feat:` / `complete` / `sub-pages"`.
#[test]
fn issue_1691_quoted_commit_message_round_trips() {
    let cmd = r#"git commit -m "feat: complete sub-pages""#;
    let spec = CommandSpec::shell(
        cmd,
        std::path::PathBuf::from("/tmp"),
        Duration::from_secs(5),
    );

    let dispatcher = codesmith_agent_runtime::shell_dispatcher::global_dispatcher();
    // The whole command (with quotes) is a single argv entry. The actual
    // shell binary can vary by platform, but the payload itself must stay
    // intact in one shell arg. We never split the command string ourselves.
    assert_eq!(spec.program, dispatcher.kind().binary());
    if dispatcher.kind().is_powershell() {
        assert_eq!(
            spec.args,
            [
                dispatcher.kind().command_flag().to_string(),
                "-Command".to_string(),
                format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {cmd}")
            ]
        );
    } else if matches!(
        dispatcher.kind(),
        codesmith_agent_runtime::shell_dispatcher::ShellKind::Cmd
    ) {
        assert_eq!(
            spec.args,
            ["/C".to_string(), format!("chcp 65001 >NUL & {cmd}")]
        );
    } else {
        assert_eq!(
            spec.args,
            [
                dispatcher.kind().command_flag().to_string(),
                cmd.to_string()
            ]
        );
    }
    assert_eq!(
        spec.args.len(),
        if dispatcher.kind().is_powershell() {
            3
        } else {
            2
        }
    );

    let mut built = Command::new(&spec.program);
    push_shell_args(&mut built, &spec.program, &spec.args);
    let got: Vec<String> = built
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(got, spec.args);
}

// === P2-6: session-aware cwd ==============================================

#[cfg(unix)]
#[test]
fn extract_cwd_marker_strips_and_returns_last_path() {
    let mut stdout = String::from("before\n");
    let mut stderr = format!("noise\n{CWD_MARKER_PREFIX}/definitely/not/real\n");
    // A path that doesn't canonicalize (a partial/malformed line, e.g. a
    // kill mid-printf) yields no capture and is kept as honest output.
    assert_eq!(extract_cwd_marker(&mut stdout, &mut stderr), None);
    assert_eq!(
        stderr,
        format!("noise\n{CWD_MARKER_PREFIX}/definitely/not/real\n")
    );
    assert_eq!(stdout, "before\n");

    let real = std::env::temp_dir();
    let mut stdout = format!("a\n{CWD_MARKER_PREFIX}{}\nb\n", real.display());
    let mut stderr = String::new();
    assert_eq!(
        extract_cwd_marker(&mut stdout, &mut stderr),
        Some(real.canonicalize().unwrap())
    );
    assert_eq!(stdout, "a\nb\n");
}

#[cfg(unix)]
#[test]
fn session_cwd_capture_rejects_outside_workspace() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));
    let outside = std::env::temp_dir().join("codesmith-p2-6-reject-probe");
    std::fs::create_dir_all(&outside).expect("mkdir");

    let mut stdout = format!("{CWD_MARKER_PREFIX}{}\n", outside.display());
    let mut stderr = String::new();
    assert!(
        apply_session_cwd_capture(&ctx, &mut stdout, &mut stderr, tmp.path()).is_none(),
        "a cwd outside the workspace must be dropped"
    );
    assert!(ctx.session_cwd_override().is_none());
    assert!(
        !stdout.contains(CWD_MARKER_PREFIX),
        "marker stripped even when rejected"
    );
    let _ = std::fs::remove_dir(&outside);
}

#[cfg(unix)]
#[test]
fn session_cwd_capture_accepts_workspace_subdir() {
    let tmp = tempdir().expect("tempdir");
    let sub = tmp.path().join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir");
    let ctx = ToolContext::new(tmp.path())
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));

    let mut stdout = format!("{}{}\n", CWD_MARKER_PREFIX, sub.display());
    let mut stderr = String::new();
    let applied =
        apply_session_cwd_capture(&ctx, &mut stdout, &mut stderr, tmp.path()).expect("applied");
    assert_eq!(applied.canonicalize().unwrap(), sub.canonicalize().unwrap());
    assert_eq!(
        ctx.session_cwd_override().unwrap().canonicalize().unwrap(),
        sub.canonicalize().unwrap()
    );
}

/// Only jobs the harness itself wrapped (a foreground command demoted to
/// background — the wrapper's `_cs_rc=$?` bytes are in the job's command)
/// may steer the session cwd through a wait/interact delta; a plain
/// `background: true` task emitting a marker-shaped line must not.
#[cfg(unix)]
#[tokio::test]
async fn delta_cwd_capture_only_for_harness_wrapped_jobs() {
    let tmp = tempdir().expect("tempdir");
    let sub = tmp.path().join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir");

    let delta_result = |command: &str| {
        let mut result = failed_network_shell_result("", "");
        result.status = ShellStatus::Completed;
        result.exit_code = Some(0);
        result.stderr = format!("{}{}\n", CWD_MARKER_PREFIX, sub.display());
        ShellDeltaResult {
            command: command.to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        }
    };

    // Unwrapped background task: the marker line stays in the output and
    // the session cwd is untouched.
    let ctx = ToolContext::new(tmp.path())
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));
    let tool_result =
        build_shell_delta_tool_result(delta_result("long-running --daemon"), &ctx).await;
    assert!(
        tool_result.content.contains(CWD_MARKER_PREFIX),
        "marker must not be consumed for an unwrapped task"
    );
    assert!(ctx.session_cwd_override().is_none());

    // Wrapped (demoted foreground) job: the marker is stripped and applied.
    let ctx = ToolContext::new(tmp.path())
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));
    let wrapped_cmd = wrap_with_cwd_capture("cd sub && build");
    let tool_result = build_shell_delta_tool_result(delta_result(&wrapped_cmd), &ctx).await;
    assert!(
        !tool_result.content.contains(CWD_MARKER_PREFIX),
        "marker stripped for the wrapped job"
    );
    assert_eq!(
        ctx.session_cwd_override().unwrap().canonicalize().unwrap(),
        sub.canonicalize().unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exec_shell_cd_persists_across_calls() {
    let tmp = tempdir().expect("tempdir");
    let sub = tmp.path().join("pkg");
    std::fs::create_dir_all(&sub).expect("mkdir");
    let ctx = ToolContext::new(tmp.path())
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));
    let tool = ExecShellTool;

    // First call: cd into the subdir. The final cwd is captured and surfaced.
    let first = tool
        .execute(json!({"command": "cd pkg"}), &ctx)
        .await
        .expect("execute");
    assert!(first.success, "{}", first.content);
    assert!(
        first.content.contains("working directory is now"),
        "cwd change must be surfaced: {}",
        first.content
    );
    assert!(
        !first.content.contains(CWD_MARKER_PREFIX),
        "the marker must never leak into model-visible output: {}",
        first.content
    );
    let meta = first.metadata.expect("metadata");
    assert_eq!(
        meta.get("session_cwd").and_then(Value::as_str),
        Some(sub.canonicalize().unwrap().to_str().unwrap())
    );

    // Second call: runs in the captured directory, no explicit cwd needed.
    let second = tool
        .execute(json!({"command": "pwd"}), &ctx)
        .await
        .expect("execute");
    assert!(second.success, "{}", second.content);
    assert!(
        second.content.contains("pkg"),
        "second call should run in the persisted cwd: {}",
        second.content
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exec_shell_cd_outside_workspace_does_not_persist() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path())
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));
    let tool = ExecShellTool;

    let first = tool
        .execute(json!({"command": "cd /"}), &ctx)
        .await
        .expect("execute");
    assert!(first.success, "{}", first.content);
    assert!(
        !first.content.contains("working directory is now"),
        "an outside-workspace cd must not persist: {}",
        first.content
    );

    let second = tool
        .execute(json!({"command": "pwd"}), &ctx)
        .await
        .expect("execute");
    assert!(
        !second.content.trim_end().eq("/"),
        "cwd should still be the workspace: {}",
        second.content
    );
    assert!(
        second
            .content
            .contains(tmp.path().file_name().unwrap().to_string_lossy().as_ref())
    );
}

// === P0-2: truncated shell output spills to disk for retrieval ============

/// Point the process-global spillover root at a temp dir for the duration
/// of a test, so spill tests never write into the real
/// `~/.codesmith/tool_outputs/`. Holds the runtime's spill guard for the
/// whole test body — without that, two concurrent spill tests would
/// interleave their root swaps and drop-restore chains.
struct TestSpillRoot {
    prior: Option<std::path::PathBuf>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl TestSpillRoot {
    fn new(tmp: &std::path::Path) -> Self {
        use codesmith_session::spillover as spill_store;
        let guard = spill_store::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = spill_store::set_test_spillover_root(Some(tmp.join("tool_outputs")));
        Self {
            prior,
            _guard: guard,
        }
    }
}

impl Drop for TestSpillRoot {
    fn drop(&mut self) {
        use codesmith_session::spillover as spill_store;
        // Guard is still held here (struct fields drop after the Drop body).
        let _ = spill_store::set_test_spillover_root(self.prior.take());
    }
}

/// A foreground command whose stdout exceeds the 30KB truncation budget
/// spills its FULL output to disk: the tool result carries a retrieval
/// footer and `spillover_*` metadata, and the spilled file contains the
/// middle bytes the truncated view elides.
#[cfg(not(windows))]
#[tokio::test]
async fn test_exec_shell_spills_truncated_output_for_retrieval() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let tool = ExecShellTool;
    let _spill_root = TestSpillRoot::new(tmp.path());

    // ~108KB of stdout — truncation keeps head+tail, elides the middle.
    let result = tool
        .execute(json!({"command": "seq 1 20000"}), &ctx)
        .await
        .expect("execute");
    assert!(result.success, "{}", result.content);
    assert!(
        result.content.contains("[Output truncated"),
        "expected truncation marker: {}",
        result.content
    );

    let meta = result.metadata.expect("metadata");
    let spill_ref = meta
        .get("spillover_ref")
        .and_then(Value::as_str)
        .expect("spillover_ref")
        .to_string();
    let spill_path = std::path::PathBuf::from(
        meta.get("spillover_path")
            .and_then(Value::as_str)
            .expect("spillover_path"),
    );
    assert!(
        result
            .content
            .contains(&format!("retrieve_tool_result ref={spill_ref}")),
        "footer must name the retrieval ref: {}",
        result.content
    );
    assert!(spill_path.exists(), "spill file missing: {spill_path:?}");

    // The spill holds the FULL output — including the elided middle.
    let spilled = std::fs::read_to_string(&spill_path).expect("read spill");
    assert!(
        spilled.len() > 30_000,
        "spill should hold the full output, got {} bytes",
        spilled.len()
    );
    assert!(
        spilled.contains("\n15000\n"),
        "the elided middle must be retrievable from the spill"
    );
}

/// The background wait path spills too, keyed by the task's full-output
/// totals — even when the delta slice a single poll renders is small.
#[cfg(not(windows))]
#[tokio::test]
async fn test_exec_shell_wait_spills_full_output() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let _spill_root = TestSpillRoot::new(tmp.path());

    let start = ExecShellTool
        .execute(json!({"command": "seq 1 20000", "background": true}), &ctx)
        .await
        .expect("start");
    let task_id = start
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    let wait = ShellWaitTool::new("exec_shell_wait")
        .execute(json!({"task_id": task_id, "timeout_ms": 30_000}), &ctx)
        .await
        .expect("wait");
    assert!(wait.success, "{}", wait.content);

    let meta = wait.metadata.expect("metadata");
    assert_eq!(
        meta.get("spillover_ref").and_then(Value::as_str),
        Some(task_id.as_str()),
        "wait-path spills are keyed by the task id"
    );
    let spill_path = std::path::PathBuf::from(
        meta.get("spillover_path")
            .and_then(Value::as_str)
            .expect("spillover_path"),
    );
    let spilled = std::fs::read_to_string(&spill_path).expect("read spill");
    assert!(
        spilled.contains("\n15000\n"),
        "elided middle must be in the wait-path spill"
    );
}

/// While a task is still Running, polls must NOT spill the full output on
/// every call — each poll would otherwise re-clone and re-write the FULL
/// accumulated buffers (O(polls × bytes) of disk churn on the async path).
/// The poll that observes the terminal status spills the final view, the
/// same rule the foreground path applies.
#[cfg(not(windows))]
#[tokio::test]
async fn test_exec_shell_wait_running_poll_does_not_spill() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let _spill_root = TestSpillRoot::new(tmp.path());

    // Emit >30KB quickly, then linger in Running so the first poll observes
    // large totals while the task is still alive.
    let start = ExecShellTool
        .execute(
            json!({"command": "seq 1 20000 && sleep 5", "background": true}),
            &ctx,
        )
        .await
        .expect("start");
    let task_id = start
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("task_id"))
        .and_then(Value::as_str)
        .expect("task id")
        .to_string();

    // Give seq a moment to flush its ~108KB before the Running poll.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let poll = ShellWaitTool::new("exec_shell_wait")
        .execute(
            json!({"task_id": task_id, "wait": false, "timeout_ms": 1000}),
            &ctx,
        )
        .await
        .expect("poll");
    let meta = poll.metadata.clone().expect("metadata");
    assert_eq!(
        meta.get("status").and_then(Value::as_str),
        Some("Running"),
        "task must still be running at the first poll: {:?}",
        poll.content
    );
    assert!(
        meta.get("stdout_total_len")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 30_000,
        "poll totals must already exceed the spill threshold for this test to be meaningful"
    );
    assert!(
        meta.get("spillover_ref").is_none(),
        "no spill while Running: {:?}",
        poll.metadata
    );
    assert!(
        !poll.content.contains("retrieve_tool_result"),
        "no retrieval footer while Running: {}",
        poll.content
    );

    // The terminal poll spills the final view under the task id.
    let done = ShellWaitTool::new("exec_shell_wait")
        .execute(json!({"task_id": task_id, "timeout_ms": 30_000}), &ctx)
        .await
        .expect("wait");
    assert!(done.success, "{}", done.content);
    let meta = done.metadata.expect("metadata");
    assert_eq!(
        meta.get("spillover_ref").and_then(Value::as_str),
        Some(task_id.as_str()),
        "terminal poll must spill under the task id"
    );
}

/// Minimal [`ShellManagerApi`] stub whose `inspect_job` reports a stale
/// (evicted / restart-orphanned) job — only the spill path is exercised, so
/// every other method panics if reached.
struct StaleInspectManager;

impl codesmith_agent_runtime::host_services::ShellManagerApi for StaleInspectManager {
    fn clear_foreground_background_request(&self) {
        unimplemented!("spill test only")
    }
    fn set_sandbox_runtime(&self, _runtime: codesmith_sandbox::runtime::SandboxRuntimeConfig) {
        unimplemented!("spill test only")
    }
    fn sandbox_backend_id(&self) -> Option<String> {
        unimplemented!("spill test only")
    }
    fn execute_with_options_env(
        &self,
        _command: &str,
        _working_dir: Option<&str>,
        _timeout_ms: u64,
        _background: bool,
        _stdin_data: Option<&str>,
        _tty: bool,
        _policy_override: Option<codesmith_sandbox::runtime::SandboxPolicy>,
        _extra_env: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<ShellResult> {
        unimplemented!("spill test only")
    }
    fn execute_interactive_with_policy_env(
        &self,
        _command: &str,
        _working_dir: Option<&str>,
        _timeout_ms: u64,
        _policy_override: Option<codesmith_sandbox::runtime::SandboxPolicy>,
        _extra_env: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<ShellResult> {
        unimplemented!("spill test only")
    }
    fn write_stdin(&self, _task_id: &str, _input: &str, _close: bool) -> anyhow::Result<()> {
        unimplemented!("spill test only")
    }
    fn kill(&self, _task_id: &str) -> anyhow::Result<ShellResult> {
        unimplemented!("spill test only")
    }
    fn kill_running(&self) -> anyhow::Result<Vec<ShellResult>> {
        unimplemented!("spill test only")
    }
    fn take_foreground_background_request(&self) -> bool {
        unimplemented!("spill test only")
    }
    fn get_output(
        &self,
        _task_id: &str,
        _block: bool,
        _timeout_ms: u64,
    ) -> anyhow::Result<ShellResult> {
        unimplemented!("spill test only")
    }
    fn get_output_delta(
        &self,
        _task_id: &str,
        _wait: bool,
        _timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult> {
        unimplemented!("spill test only")
    }
    fn tag_linked_task(
        &self,
        _task_id: &str,
        _linked_task_id: Option<String>,
    ) -> anyhow::Result<()> {
        unimplemented!("spill test only")
    }
    fn list_jobs(&self) -> Vec<ShellJobSnapshot> {
        unimplemented!("spill test only")
    }
    fn set_foreground_wait(&self, _task_id: &str, _foreground: bool) {
        unimplemented!("spill test only")
    }
    fn inspect_job(&self, _task_id: &str) -> anyhow::Result<ShellJobDetail> {
        Ok(ShellJobDetail {
            snapshot: ShellJobSnapshot {
                id: "stale-task".to_string(),
                job_id: "stale-task".to_string(),
                command: "cargo test".to_string(),
                cwd: std::path::PathBuf::from("/tmp"),
                status: ShellStatus::Killed,
                exit_code: None,
                elapsed_ms: 0,
                // The stored tail is all a stale detail carries — a strictly
                // worse view than a previously spilled FULL output.
                stdout_tail: "…last lines only…".to_string(),
                stderr_tail: String::new(),
                stdout_len: 20,
                stderr_len: 0,
                stdin_available: false,
                stale: true,
                linked_task_id: None,
                foreground: false,
            },
            stdout: "…last lines only…".to_string(),
            stderr: String::new(),
        })
    }
    fn poll_delta(
        &self,
        _task_id: &str,
        _wait: bool,
        _timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult> {
        unimplemented!("spill test only")
    }
    fn request_foreground_background(&self) {
        unimplemented!("spill test only")
    }
}

/// A stale (evicted / restart-orphanned) job's `inspect_job` detail carries
/// only the stored tail — spilling it under the task id would overwrite a
/// previously spilled FULL view with a strictly worse one. The spill must be
/// skipped so the fuller file survives (no footer rather than a degraded
/// spill).
#[tokio::test]
async fn test_spill_task_full_output_skips_stale_detail() {
    let tmp = tempdir().expect("tempdir");
    let _spill_root = TestSpillRoot::new(tmp.path());
    let mut ctx = ToolContext::new(tmp.path());
    ctx.shell_manager = std::sync::Arc::new(StaleInspectManager);

    // Prime a FULL spill under the task id — what an earlier live poll wrote.
    let full = "x".repeat(40_000);
    let path = codesmith_session::spillover::write_spillover("stale-task", &full)
        .expect("prime full spill");

    let spill = spill_task_full_output(&ctx, "stale-task").await;
    assert!(
        spill.is_none(),
        "stale detail must not spill over the full file"
    );
    assert_eq!(
        std::fs::read_to_string(&path)
            .expect("full spill survives")
            .len(),
        full.len(),
        "the previously spilled FULL view must be intact"
    );
}

/// The spill id embeds the pid: the spillover root is shared across
/// concurrent CodeSmith processes, and millis+seq alone can collide between
/// two processes that start their counters in the same millisecond.
#[test]
fn test_fresh_shell_spill_id_is_process_unique() {
    let a = fresh_shell_spill_id();
    let b = fresh_shell_spill_id();
    assert_ne!(a, b, "consecutive ids must differ");
    assert!(
        a.ends_with(&format!("-{}", std::process::id())),
        "id must embed the pid for cross-process uniqueness: {a}"
    );
}

#[test]
fn exit_code_semantics_translates_shared_codes() {
    assert_eq!(
        exit_code_semantics(Some(127), None).as_deref(),
        Some(
            "[command not found — check the spelling and that the binary is on PATH; NEVER retry the same command verbatim]"
        )
    );
    assert_eq!(
        exit_code_semantics(Some(126), None).as_deref(),
        Some(
            "[found but not executable — check the file mode (chmod +x) or invoke it through its interpreter]"
        )
    );
    // Search-family exit 1 is a result; anything else stays untranslated.
    assert!(
        exit_code_semantics(Some(1), Some("grep -r foo ."))
            .is_some_and(|note| note.contains("NO MATCHES"))
    );
    assert!(exit_code_semantics(Some(1), Some("cargo build")).is_none());
    assert_eq!(exit_code_semantics(Some(0), Some("grep x")), None);
    assert_eq!(exit_code_semantics(None, Some("grep x")), None);
}

#[test]
fn command_program_skips_env_prefixes_and_paths() {
    assert_eq!(command_program("grep -rn foo ."), "grep");
    assert_eq!(command_program("env RG=1 /usr/bin/rg foo"), "rg");
    assert_eq!(command_program(""), "");
}

/// A classified sandbox denial must carry the dialect marker; an
/// unclassified failure must not gain one (a wrong marker would send the
/// model chasing sandbox policy instead of the real error).
#[test]
fn sandbox_denied_hint_marks_only_classified_denials() {
    let denied = {
        let mut result = failed_network_shell_result("", "Permission denied");
        result.sandbox_denied = true;
        result
    };
    let hint = sandbox_denied_hint(&denied).expect("classified denial carries the marker");
    assert!(
        hint.starts_with("[sandbox:"),
        "marker must be greppable: {hint}"
    );
    assert!(hint.contains("Do NOT chmod"));

    let plain = failed_network_shell_result("", "Permission denied");
    assert_eq!(
        sandbox_denied_hint(&plain),
        None,
        "an unclassified permission failure must not claim the sandbox"
    );
}

// === stub shell manager for tool-level error paths ==========================

/// One configurable [`ShellManagerApi`] stub for the tool-level error-path
/// tests below. Only the fields a test sets are answered; any other call
/// panics with "not scripted" so an unexpected reach fails the test loudly
/// (same posture as [`StaleInspectManager`]).
#[derive(Default)]
struct ScriptedManager {
    execute_result: Option<Result<ShellResult, String>>,
    write_stdin_result: Option<Result<(), String>>,
    kill_result: Option<Result<ShellResult, String>>,
    kill_running_result: Option<Result<Vec<ShellResult>, String>>,
    delta_result: Option<Result<ShellDeltaResult, String>>,
}

impl codesmith_agent_runtime::host_services::ShellManagerApi for ScriptedManager {
    fn clear_foreground_background_request(&self) {}
    fn set_sandbox_runtime(&self, _runtime: codesmith_sandbox::runtime::SandboxRuntimeConfig) {}
    fn sandbox_backend_id(&self) -> Option<String> {
        None
    }
    fn execute_with_options_env(
        &self,
        _command: &str,
        _working_dir: Option<&str>,
        _timeout_ms: u64,
        _background: bool,
        _stdin_data: Option<&str>,
        _tty: bool,
        _policy_override: Option<codesmith_sandbox::runtime::SandboxPolicy>,
        _extra_env: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<ShellResult> {
        match &self.execute_result {
            Some(Ok(result)) => Ok(result.clone()),
            Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
            None => panic!("execute_with_options_env not scripted"),
        }
    }
    fn execute_interactive_with_policy_env(
        &self,
        _command: &str,
        _working_dir: Option<&str>,
        _timeout_ms: u64,
        _policy_override: Option<codesmith_sandbox::runtime::SandboxPolicy>,
        _extra_env: std::collections::HashMap<String, String>,
    ) -> anyhow::Result<ShellResult> {
        panic!("execute_interactive not scripted")
    }
    fn write_stdin(&self, _task_id: &str, _input: &str, _close: bool) -> anyhow::Result<()> {
        match &self.write_stdin_result {
            Some(Ok(())) => Ok(()),
            Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
            None => panic!("write_stdin not scripted"),
        }
    }
    fn kill(&self, _task_id: &str) -> anyhow::Result<ShellResult> {
        match &self.kill_result {
            Some(Ok(result)) => Ok(result.clone()),
            Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
            None => panic!("kill not scripted"),
        }
    }
    fn kill_running(&self) -> anyhow::Result<Vec<ShellResult>> {
        match &self.kill_running_result {
            Some(Ok(results)) => Ok(results.clone()),
            Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
            None => panic!("kill_running not scripted"),
        }
    }
    fn take_foreground_background_request(&self) -> bool {
        false
    }
    fn get_output(
        &self,
        _task_id: &str,
        _block: bool,
        _timeout_ms: u64,
    ) -> anyhow::Result<ShellResult> {
        panic!("get_output not scripted")
    }
    fn get_output_delta(
        &self,
        _task_id: &str,
        _wait: bool,
        _timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult> {
        match &self.delta_result {
            Some(Ok(delta)) => Ok(delta.clone()),
            Some(Err(msg)) => Err(anyhow::anyhow!("{msg}")),
            None => panic!("get_output_delta not scripted"),
        }
    }
    fn tag_linked_task(
        &self,
        _task_id: &str,
        _linked_task_id: Option<String>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn list_jobs(&self) -> Vec<ShellJobSnapshot> {
        Vec::new()
    }
    fn inspect_job(&self, _task_id: &str) -> anyhow::Result<ShellJobDetail> {
        panic!("inspect_job not scripted")
    }
    fn poll_delta(
        &self,
        _task_id: &str,
        _wait: bool,
        _timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult> {
        panic!("poll_delta not scripted")
    }
    fn set_foreground_wait(&self, _task_id: &str, _foreground: bool) {}
    fn request_foreground_background(&self) {}
}

fn killed_result() -> ShellResult {
    let mut result = failed_network_shell_result("", "");
    result.status = ShellStatus::Killed;
    result.exit_code = None;
    result
}

fn completed_delta(stdout: &str) -> ShellDeltaResult {
    let mut result = failed_network_shell_result(stdout, "");
    result.status = ShellStatus::Completed;
    result.exit_code = Some(0);
    result.task_id = Some("stub-task".to_string());
    ShellDeltaResult {
        command: "stub command".to_string(),
        result,
        stdout_total_len: stdout.len(),
        stderr_total_len: 0,
    }
}

#[tokio::test]
async fn wait_delta_maps_manager_errors_from_both_branches() {
    let tmp = tempdir().expect("tempdir");
    let stub = std::sync::Arc::new(ScriptedManager {
        delta_result: Some(Err("stub delta failure".to_string())),
        ..ScriptedManager::default()
    });

    let mut plain = ToolContext::new(tmp.path());
    plain.shell_manager = stub.clone();
    let err = wait_for_shell_delta_cancellable(&plain, "t1", 1000, "exec_shell_wait")
        .await
        .expect_err("poll error must surface");
    assert!(format!("{err}").contains("stub delta failure"));

    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let mut cancelled = ToolContext::new(tmp.path()).with_cancel_token(token);
    cancelled.shell_manager = stub;
    let err = wait_for_shell_delta_cancellable(&cancelled, "t1", 1000, "exec_shell_wait")
        .await
        .expect_err("cancelled-branch poll error must surface");
    assert!(format!("{err}").contains("stub delta failure"));
}

#[tokio::test]
async fn cancel_tool_maps_manager_failures_and_requires_a_task_id() {
    let tmp = tempdir().expect("tempdir");
    let mut ctx = ToolContext::new(tmp.path());
    ctx.shell_manager = std::sync::Arc::new(ScriptedManager {
        kill_result: Some(Err("stub kill failure".to_string())),
        kill_running_result: Some(Err("stub kill_running failure".to_string())),
        ..ScriptedManager::default()
    });

    let err = ShellCancelTool
        .execute(json!({ "task_id": "t9" }), &ctx)
        .await
        .expect_err("kill failure must surface");
    assert!(format!("{err}").contains("stub kill failure"));

    let err = ShellCancelTool
        .execute(json!({ "all": true }), &ctx)
        .await
        .expect_err("kill_running failure must surface");
    assert!(format!("{err}").contains("stub kill_running failure"));

    let err = ShellCancelTool
        .execute(json!({ "all": false }), &ctx)
        .await
        .expect_err("task_id is required");
    assert!(format!("{err}").contains("task_id"));
}

#[tokio::test]
async fn cancel_tool_falls_back_to_the_requested_id_and_honors_the_alias() {
    let tmp = tempdir().expect("tempdir");
    let mut ctx = ToolContext::new(tmp.path());
    ctx.shell_manager = std::sync::Arc::new(ScriptedManager {
        kill_result: Some(Ok(killed_result())),
        ..ScriptedManager::default()
    });
    // The `id` alias resolves; the kill result carries no task id, so the
    // requested id is echoed back.
    let ok = ShellCancelTool
        .execute(json!({ "id": "alias-id" }), &ctx)
        .await
        .expect("cancel via alias");
    assert!(ok.content.contains("Canceled background command: alias-id"));
    let meta = ok.metadata.expect("metadata");
    assert_eq!(
        meta.get("task_id").and_then(Value::as_str),
        Some("alias-id")
    );
}

#[tokio::test]
async fn interact_tool_reports_delta_output_and_maps_failures() {
    let tmp = tempdir().expect("tempdir");

    // Success: the first poll already carries output.
    let mut ok_ctx = ToolContext::new(tmp.path());
    ok_ctx.shell_manager = std::sync::Arc::new(ScriptedManager {
        delta_result: Some(Ok(completed_delta("stub-out"))),
        ..ScriptedManager::default()
    });
    let result = ShellInteractTool::new("exec_shell_interact")
        .execute(json!({ "task_id": "t1", "timeout_ms": 500 }), &ok_ctx)
        .await
        .expect("interact delta");
    assert!(result.content.contains("stub-out"), "{}", result.content);

    // write_stdin failure surfaces as a tool error.
    let mut stdin_err_ctx = ToolContext::new(tmp.path());
    stdin_err_ctx.shell_manager = std::sync::Arc::new(ScriptedManager {
        write_stdin_result: Some(Err("stub stdin failure".to_string())),
        ..ScriptedManager::default()
    });
    let err = ShellInteractTool::new("exec_shell_interact")
        .execute(json!({ "task_id": "t1", "input": "x" }), &stdin_err_ctx)
        .await
        .expect_err("stdin failure must surface");
    assert!(format!("{err}").contains("stub stdin failure"));

    // get_output_delta failure in the polling loop.
    let mut delta_err_ctx = ToolContext::new(tmp.path());
    delta_err_ctx.shell_manager = std::sync::Arc::new(ScriptedManager {
        delta_result: Some(Err("stub delta failure".to_string())),
        ..ScriptedManager::default()
    });
    let err = ShellInteractTool::new("exec_shell_interact")
        .execute(
            json!({ "task_id": "t1", "timeout_ms": 500 }),
            &delta_err_ctx,
        )
        .await
        .expect_err("delta failure must surface");
    assert!(format!("{err}").contains("stub delta failure"));

    // A cancelled token: the cancel branch still reports the delta and
    // stamps wait_canceled.
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let mut cancelled = ToolContext::new(tmp.path()).with_cancel_token(token);
    cancelled.shell_manager = std::sync::Arc::new(ScriptedManager {
        delta_result: Some(Ok(completed_delta("late-out"))),
        ..ScriptedManager::default()
    });
    let result = ShellInteractTool::new("exec_shell_interact")
        .execute(json!({ "task_id": "t1" }), &cancelled)
        .await
        .expect("cancelled interact still reports the delta");
    assert!(result.content.contains("late-out"));
    assert_eq!(
        result
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("wait_canceled"))
            .and_then(Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn interact_tool_sends_input_to_a_real_background_task() {
    let tmp = tempdir().expect("tempdir");
    let shell_manager = make_shared_shell_manager(tmp.path().to_path_buf());
    let ctx =
        ToolContext::new(tmp.path()).with_shell_manager(wrap_shell_manager(shell_manager.clone()));
    let started = shell_manager
        .lock()
        .expect("shell manager lock")
        .execute(&echo_stdin_command(), None, 600_000, true)
        .expect("start stdin echo task");
    let task_id = started.task_id.expect("task id");

    let result = ShellInteractTool::new("exec_shell_interact")
        .execute(
            json!({
                "task_id": task_id,
                "input": "hello interact\n",
                "close_stdin": true,
                "timeout_ms": 3000
            }),
            &ctx,
        )
        .await
        .expect("interact");
    assert!(
        result.content.contains("hello interact"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn foreground_via_background_requires_a_process_id() {
    let tmp = tempdir().expect("tempdir");
    let mut no_id = failed_network_shell_result("", "");
    no_id.status = ShellStatus::Completed;
    no_id.task_id = None;
    let mut ctx = ToolContext::new(tmp.path());
    ctx.shell_manager = std::sync::Arc::new(ScriptedManager {
        execute_result: Some(Ok(no_id)),
        ..ScriptedManager::default()
    });
    let err = execute_foreground_via_background(
        &ctx,
        "echo hi",
        None,
        1000,
        None,
        false,
        None,
        std::collections::HashMap::new(),
        SandboxRuntimeConfig::default(),
    )
    .await
    .expect_err("a spawn without a task id must fail");
    assert!(
        format!("{err}").contains("did not return a process id"),
        "got `{err}`"
    );
}

// === progress checkpoints ================================================

#[tokio::test]
async fn shell_progress_emits_throttles_and_tails_excerpts() {
    let tmp = tempdir().expect("tempdir");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ctx = ToolContext::new(tmp.path())
        .with_progress_sink(Some(std::sync::Arc::new(RecordingSink(seen.clone()))));
    let mut progress = ShellProgress::new("task-9", "exec_shell");

    // First checkpoint with new output emits immediately.
    progress.checkpoint(&ctx, "out1\n", 5, "", 0, false).await;
    assert_eq!(seen.lock().unwrap().len(), 1);

    // New output within the interval is throttled...
    progress
        .checkpoint(&ctx, "out1\nmore\n", 10, "", 0, false)
        .await;
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "within-interval output waits"
    );
    // ...and unchanged lengths are a no-op.
    progress
        .checkpoint(&ctx, "out1\nmore\n", 10, "", 0, false)
        .await;
    assert_eq!(seen.lock().unwrap().len(), 1);

    // Once the interval elapsed, the new slice (not the whole stream) emits.
    progress.last_emit = Some(Instant::now() - Duration::from_secs(2));
    progress
        .checkpoint(&ctx, "out1\nmore\n", 10, "", 0, false)
        .await;
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 2);
    assert!(
        got[1].2.contains("more") && !got[1].2.contains("out1\n"),
        "message carries only the new output: {got:?}"
    );

    // A truncated excerpt reports the tail of the non-empty stream.
    progress.last_emit = Some(Instant::now() - Duration::from_secs(2));
    progress
        .checkpoint(
            &ctx,
            &"h".repeat(PROGRESS_MESSAGE_MAX_BYTES + 10),
            5000,
            &"e".repeat(64),
            64,
            true,
        )
        .await;
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 3);
    assert!(
        got[2].2.ends_with(&"e".repeat(64)),
        "truncated excerpts prefer the non-empty (stderr) tail, capped: {got:?}"
    );
    assert!(got[2].2.len() <= PROGRESS_MESSAGE_MAX_BYTES);
}

// === session cwd capture edge paths ======================================

#[cfg(unix)]
#[test]
fn session_cwd_capture_tolerates_missing_base_directories() {
    let tmp = tempdir().expect("tempdir");
    let sub = tmp.path().join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir sub");

    // Workspace missing: the fallback closure keeps the raw path, and the
    // capture is still accepted via the existing ran_in directory.
    let ctx = ToolContext::new(tmp.path().join("gone-workspace"))
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));
    let mut stdout = format!("{}{}\n", CWD_MARKER_PREFIX, sub.display());
    let mut stderr = String::new();
    let applied =
        apply_session_cwd_capture(&ctx, &mut stdout, &mut stderr, tmp.path()).expect("applied");
    assert_eq!(applied, sub.canonicalize().unwrap());

    // ran_in missing: same posture, accepted via the workspace instead.
    let ctx = ToolContext::new(tmp.path())
        .with_session_cwd(std::sync::Arc::new(std::sync::Mutex::new(None)));
    let mut stdout = format!("{}{}\n", CWD_MARKER_PREFIX, sub.display());
    let mut stderr = String::new();
    let applied = apply_session_cwd_capture(
        &ctx,
        &mut stdout,
        &mut stderr,
        &tmp.path().join("gone-ran-in"),
    )
    .expect("applied");
    assert_eq!(applied, sub.canonicalize().unwrap());
}

// === spill attachment with fresh metadata ================================

#[tokio::test]
async fn attach_spill_adds_footer_and_metadata_to_a_result_without_any() {
    let tmp = tempdir().expect("tempdir");
    let _spill_root = TestSpillRoot::new(tmp.path());
    let spill = spill_full_shell_output("spill-ref-1", "AAAA", "BBBB").expect("spill to temp root");
    let mut result = ToolResult {
        canonical: None,
        content: "base".to_string(),
        success: true,
        metadata: None,
    };
    attach_spill_to_result(&mut result, &spill);
    assert!(
        result
            .content
            .contains("retrieve_tool_result ref=spill-ref-1"),
        "{}",
        result.content
    );
    let meta = result.metadata.expect("metadata inserted from scratch");
    assert_eq!(
        meta.get("spillover_ref").and_then(Value::as_str),
        Some("spill-ref-1")
    );
    assert!(meta.get("spillover_path").and_then(Value::as_str).is_some());
}

#[tokio::test]
async fn shell_delta_failed_empty_output_carries_the_exit_note() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let mut result = failed_network_shell_result("", "");
    result.status = ShellStatus::Failed;
    result.exit_code = Some(127);
    let tool_result = build_shell_delta_tool_result(
        ShellDeltaResult {
            command: "plain background task".to_string(),
            result,
            stdout_total_len: 0,
            stderr_total_len: 0,
        },
        &ctx,
    )
    .await;
    assert!(
        tool_result.content.contains("command not found"),
        "{}",
        tool_result.content
    );
    assert!(tool_result.content.contains("127"));
}

// === exec_shell failure / execpolicy paths ===============================

/// Exit-code 127 renders as the command-not-found note. Scoped like its
/// execpolicy siblings: the tool loads `codesmith_home()/execpolicy.toml`,
/// so an unscoped run would consult (or die on) the developer's real policy
/// file. Unix-gated: 127 is the POSIX "not found" exit code.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn exec_shell_failure_surfaces_exit_code_semantics() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::new(&["CODESMITH_HOME"]);
    let home = tempdir().expect("tempdir");
    // SAFETY: held under env_lock; restored by EnvRestore on drop.
    unsafe { std::env::set_var("CODESMITH_HOME", home.path()) };

    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path());
    let result = ExecShellTool
        .execute(
            json!({"command": "codesmith-missing-cmd-xyz", "timeout_ms": 5000}),
            &ctx,
        )
        .await
        .expect("execute");
    assert!(!result.success);
    assert!(
        result.content.contains("command not found"),
        "{}",
        result.content
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn exec_shell_surfaces_execpolicy_load_failure() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::new(&["CODESMITH_HOME"]);
    let home = tempdir().expect("tempdir");
    // SAFETY: held under env_lock; restored by EnvRestore on drop.
    unsafe { std::env::set_var("CODESMITH_HOME", home.path()) };
    std::fs::write(home.path().join("execpolicy.toml"), "not valid toml [[[")
        .expect("write broken policy");

    let tmp = tempdir().expect("tempdir");
    let err = ExecShellTool
        .execute(
            json!({"command": "printf ok"}),
            &ToolContext::new(tmp.path()),
        )
        .await
        .expect_err("a broken policy must fail loud, not run unpoliced");
    assert!(
        format!("{err}").contains("execpolicy load failed"),
        "got `{err}`"
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn exec_shell_records_execpolicy_decision_in_metadata() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::new(&["CODESMITH_HOME"]);
    let home = tempdir().expect("tempdir");
    // SAFETY: held under env_lock; restored by EnvRestore on drop.
    unsafe { std::env::set_var("CODESMITH_HOME", home.path()) };
    // No allow rules: every command evaluates to AskUser, which (attended)
    // is recorded in the result metadata rather than blocking the run.
    std::fs::write(
        home.path().join("execpolicy.toml"),
        "[rules.default]\nallow = []\ndeny = []\n",
    )
    .expect("write policy");

    let tmp = tempdir().expect("tempdir");
    let result = ExecShellTool
        .execute(
            json!({"command": "printf ok"}),
            &ToolContext::new(tmp.path()),
        )
        .await
        .expect("attended run proceeds under an ask policy");
    assert!(result.success, "{}", result.content);
    let meta = result.metadata.expect("metadata");
    assert_eq!(
        meta.get("execpolicy")
            .and_then(|e| e.get("decision"))
            .and_then(Value::as_str),
        Some("ask_user")
    );
}

// === external sandbox backend path =======================================

/// Stub backend answering `exec` with a fixed output (or an error) — the
/// external-sandbox rendering path needs a result without a real sandbox.
/// `denial` makes `classify_denial` claim a policy denial on non-zero exits.
#[derive(Debug)]
struct StubExecBackend {
    stdout: String,
    stderr: String,
    exit_code: i32,
    fail: bool,
    denial: bool,
}

#[async_trait::async_trait]
impl codesmith_sandbox::runtime::SandboxBackend for StubExecBackend {
    fn id(&self) -> &'static str {
        "stub-exec"
    }

    fn capabilities(&self) -> codesmith_sandbox::runtime::SandboxBackendCapabilities {
        stub_capabilities()
    }

    fn classify_denial(&self, exit_code: i32, stderr: &str) -> Option<String> {
        let _ = stderr;
        (self.denial && exit_code != 0).then(|| "stub policy denied".to_string())
    }

    async fn exec(
        &self,
        _request: codesmith_sandbox::runtime::SandboxExecRequest,
    ) -> anyhow::Result<codesmith_sandbox::runtime::SandboxOutput> {
        if self.fail {
            Err(anyhow::anyhow!("stub backend blew up"))
        } else {
            Ok(codesmith_sandbox::runtime::SandboxOutput {
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
                exit_code: self.exit_code,
            })
        }
    }
}

#[tokio::test]
async fn sandbox_backend_result_renders_and_spills_large_output() {
    let tmp = tempdir().expect("tempdir");
    let _spill_root = TestSpillRoot::new(tmp.path());
    let ctx =
        ToolContext::new(tmp.path()).with_sandbox_backend(std::sync::Arc::new(StubExecBackend {
            stdout: "x".repeat(40_000),
            stderr: String::new(),
            exit_code: 0,
            fail: false,
            denial: false,
        }));
    let result = ExecShellTool
        .execute(json!({"command": "printf big"}), &ctx)
        .await
        .expect("backend run");
    assert!(result.success, "{}", result.content);
    assert!(
        result.content.contains("[Output truncated"),
        "truncated view: {}",
        result.content
    );
    assert!(
        result.content.contains("retrieve_tool_result"),
        "spill footer: {}",
        result.content
    );
    let meta = result.metadata.expect("metadata");
    assert_eq!(
        meta.get("sandbox_backend").and_then(Value::as_str),
        Some("stub-exec"),
        "backend id must come from the backend, not a hardcoded string"
    );
    assert!(meta.get("spillover_ref").is_some());
}

#[tokio::test]
async fn sandbox_backend_errors_and_mode_limits_surface() {
    let tmp = tempdir().expect("tempdir");
    let tool = ExecShellTool;

    // Backend failure is a tool error, not a panic.
    let ctx =
        ToolContext::new(tmp.path()).with_sandbox_backend(std::sync::Arc::new(StubExecBackend {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            fail: true,
            denial: false,
        }));
    let result = tool
        .execute(json!({"command": "printf x"}), &ctx)
        .await
        .expect("error result");
    assert!(!result.success);
    assert!(
        result
            .content
            .contains("Sandbox backend error: stub backend blew up"),
        "{}",
        result.content
    );

    // Interactive / background / tty modes are rejected with a backend set.
    for (input, needle) in [
        (
            json!({"command": "printf x", "interactive": true}),
            "Interactive mode",
        ),
        (
            json!({"command": "printf x", "background": true}),
            "Background mode",
        ),
        // tty shares the background rejection message (the product folds
        // both into "Background mode is not supported…").
        (
            json!({"command": "printf x", "tty": true}),
            "Background mode",
        ),
    ] {
        let desc = input.clone();
        let result = tool.execute(input, &ctx).await.expect("mode error result");
        assert!(!result.success, "{desc}");
        assert!(
            result.content.contains(needle),
            "{desc}: {}",
            result.content
        );
    }
}

/// A backend-classified denial must surface as the cross-tool hint prefix
/// (prepended, never replacing the raw stderr) plus the metadata flag.
#[tokio::test]
async fn sandbox_backend_denial_prefixes_hint_and_keeps_stderr() {
    let tmp = tempdir().expect("tempdir");
    // No env isolation needed: the ExecPolicy feature is disabled, so
    // `execute` never resolves CODESMITH_HOME (that env read is what races
    // env-mutating neighbors).
    let mut features = codesmith_agent_runtime::features::Features::with_defaults();
    features.disable(codesmith_agent_runtime::features::Feature::ExecPolicy);
    let ctx = ToolContext::new(tmp.path())
        .with_features(features)
        .with_sandbox_backend(std::sync::Arc::new(StubExecBackend {
            stdout: String::new(),
            stderr: "touch: /etc/x: Read-only file system".to_string(),
            exit_code: 1,
            fail: false,
            denial: true,
        }));
    let result = ExecShellTool
        .execute(json!({"command": "touch /etc/x"}), &ctx)
        .await
        .expect("denied run");
    assert!(!result.success);
    assert!(
        result.content.starts_with("[sandbox:"),
        "denial hint must lead the output: {}",
        result.content
    );
    assert!(
        result.content.contains("Read-only file system"),
        "raw stderr must stay attached: {}",
        result.content
    );
    let meta = result.metadata.expect("metadata");
    assert_eq!(
        meta.get("sandbox_denied").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        meta.get("sandbox_backend").and_then(Value::as_str),
        Some("stub-exec")
    );
}

/// Task-plane stub: declares a task plane so the tool takes the
/// manager-routing fall-through. The wiring guard fires before anything
/// spawns — the default manager carries no backend.
#[derive(Debug)]
struct StubTaskPlaneBackend;

#[async_trait::async_trait]
impl codesmith_sandbox::runtime::SandboxBackend for StubTaskPlaneBackend {
    fn id(&self) -> &'static str {
        "stub-plane"
    }

    fn capabilities(&self) -> codesmith_sandbox::runtime::SandboxBackendCapabilities {
        stub_capabilities()
    }

    fn task_plane(&self) -> Option<&dyn codesmith_sandbox::runtime::SandboxTaskPlane> {
        Some(self)
    }

    async fn exec(
        &self,
        _request: codesmith_sandbox::runtime::SandboxExecRequest,
    ) -> anyhow::Result<codesmith_sandbox::runtime::SandboxOutput> {
        unimplemented!("routing goes through the manager task plane")
    }
}

impl codesmith_sandbox::runtime::SandboxTaskPlane for StubTaskPlaneBackend {
    fn spawn(
        &self,
        _request: codesmith_sandbox::runtime::SandboxExecRequest,
    ) -> anyhow::Result<codesmith_sandbox::runtime::SandboxTaskSpawn> {
        unimplemented!("wiring guard fires before spawn")
    }

    fn status(&self, _task: &str) -> anyhow::Result<codesmith_sandbox::runtime::SandboxTaskStatus> {
        unimplemented!("wiring guard fires before status")
    }

    fn kill(&self, _task: &str) -> anyhow::Result<()> {
        unimplemented!("wiring guard fires before kill")
    }

    fn cleanup(&self, _task: &str) -> anyhow::Result<()> {
        unimplemented!("wiring guard fires before cleanup")
    }
}

/// A task-plane backend on an unwired manager must refuse, never run the
/// command locally while a backend is configured.
#[tokio::test]
async fn task_plane_backend_with_unwired_manager_refuses() {
    let tmp = tempdir().expect("tempdir");
    let mut features = codesmith_agent_runtime::features::Features::with_defaults();
    features.disable(codesmith_agent_runtime::features::Feature::ExecPolicy);
    let ctx = ToolContext::new(tmp.path())
        .with_features(features)
        .with_sandbox_backend(std::sync::Arc::new(StubTaskPlaneBackend));
    let result = ExecShellTool
        .execute(json!({"command": "printf x"}), &ctx)
        .await
        .expect("guard error result");
    assert!(!result.success);
    assert!(
        result
            .content
            .contains("'stub-plane' is configured but the shell manager is not wired to it"),
        "{}",
        result.content
    );
}

// === note tool ============================================================

#[tokio::test]
async fn note_tool_appends_and_reports_the_path() {
    let tmp = tempdir().expect("tempdir");
    let notes = tmp.path().join("notes.md");
    let mut ctx = ToolContext::new(tmp.path());
    ctx.notes_path = notes.clone();
    let result = NoteTool
        .execute(json!({"content": "remember this"}), &ctx)
        .await
        .expect("note");
    assert!(result.success);
    let body = std::fs::read_to_string(&notes).expect("notes file");
    assert!(body.contains("remember this"), "{body}");
}

#[tokio::test]
async fn note_tool_surfaces_directory_and_open_failures() {
    let tmp = tempdir().expect("tempdir");

    // Parent path blocked by a regular file: create_dir_all fails.
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, "x").expect("write blocker");
    let mut ctx = ToolContext::new(tmp.path());
    ctx.notes_path = tmp.path().join("blocker").join("notes.md");
    let err = NoteTool
        .execute(json!({"content": "x"}), &ctx)
        .await
        .expect_err("unusable notes directory");
    assert!(
        format!("{err}").contains("Failed to create notes directory"),
        "got `{err}`"
    );

    // Notes path IS a directory: open fails.
    let mut ctx = ToolContext::new(tmp.path());
    ctx.notes_path = tmp.path().to_path_buf();
    let err = NoteTool
        .execute(json!({"content": "x"}), &ctx)
        .await
        .expect_err("notes path is a directory");
    assert!(
        format!("{err}").contains("Failed to open notes file"),
        "got `{err}`"
    );
}

// === declared approval requirements ======================================

#[test]
fn shell_tool_approval_requirements_are_declared() {
    assert_eq!(
        ShellCancelTool.approval_requirement(),
        ApprovalRequirement::Required
    );
    assert_eq!(
        ShellWaitTool::new("exec_shell_wait").approval_requirement(),
        ApprovalRequirement::Auto
    );
    assert_eq!(
        ShellInteractTool::new("exec_shell_interact").approval_requirement(),
        ApprovalRequirement::Auto
    );
    assert_eq!(NoteTool.approval_requirement(), ApprovalRequirement::Auto);
    assert_eq!(ShellCancelTool.name(), "exec_shell_cancel");
    assert!(NoteTool.input_schema().get("properties").is_some());
}
