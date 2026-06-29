//! Concrete shell-process manager (pty / process / sandbox plumbing).
//!
//! This module hosts the terminal-agnostic `ShellManager` — background
//! process management, sandboxed sync/interactive execution, output
//! truncation — plus its supporting types (`BackgroundShell`, decision
//! helpers, and the `ShellManagerHost` bridge onto `ShellManagerApi`).
//!
//! ## Terminal raw-mode trait-erasure
//!
//! `ShellManager`'s sandboxed sync/interactive exec paths must briefly
//! release the host terminal from ratatui raw mode so the child can own the
//! tty (issue #1690). `crossterm` is a terminal-coupled dependency that this
//! runtime crate deliberately does NOT pull in. To keep `ShellManager`
//! portable while preserving that save/restore, the raw-mode toggle is
//! abstracted behind the [`ShellTerminalControl`] trait: the TUI supplies a
//! crossterm-backed implementation; non-TUI hosts (tests, app-server) use
//! [`NoopTerminalControl`] (raw mode is never enabled there, so the toggle
//! is a no-op).

#![allow(unsafe_code)] // libc::kill / libc::prctl in kill_child_process_group / install_parent_death_signal

// Concrete shell-process manager (pty / process / sandbox plumbing), background
// process management, the `ShellManagerHost` bridge onto `ShellManagerApi` / `ShellApi`,
// and supporting decision helpers. Physically migrated from the TUI's `tools::shell`
// (C4-3b.7a.4.c) so the engine core can host shell execution without depending on the
// terminal-coupled binary. crossterm raw-mode is trait-erased via `ShellTerminalControl`;
// the TUI injects a crossterm impl, non-TUI hosts get `NoopTerminalControl`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use uuid::Uuid;
use wait_timeout::ChildExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::child_env;
use crate::host_services::{ShellApi, ShellExecResult, ShellExecStatus, ShellManagerApi};
use crate::sandbox::{
    CommandSpec, ExecEnv, SandboxDecision, SandboxExecRequest, SandboxManager,
    SandboxPolicy as ExecutionSandboxPolicy, SandboxRuntimeConfig, SandboxType,
};
use crate::tools::git_env::merge_git_scrub_env;
use crate::tools::shell_output::{summarize_output, truncate_with_meta};
use crate::tools::shell_types::{
    ShellDeltaResult, ShellJobDetail, ShellJobSnapshot, ShellResult, ShellStatus,
};

/// Terminal raw-mode control for shell execution.
///
/// Implemented by the TUI (crossterm-backed) and by
/// [`NoopTerminalControl`] for non-terminal hosts. See the module docs for
/// the rationale.
pub trait ShellTerminalControl: Send + Sync {
    /// Whether the host currently has terminal raw mode enabled.
    fn raw_mode_enabled(&self) -> bool;
    /// Disable terminal raw mode (best-effort; errors are swallowed by the
    /// caller).
    fn disable_raw_mode(&self);
    /// Re-enable terminal raw mode (best-effort).
    fn enable_raw_mode(&self);
}

/// No-op [`ShellTerminalControl`] for non-terminal hosts.
///
/// Raw mode is never enabled outside the TUI, so every method is a no-op and
/// [`raw_mode_enabled`](ShellTerminalControl::raw_mode_enabled) returns
/// `false`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTerminalControl;

impl ShellTerminalControl for NoopTerminalControl {
    fn raw_mode_enabled(&self) -> bool {
        false
    }
    fn disable_raw_mode(&self) {}
    fn enable_raw_mode(&self) {}
}

/// Convenience: the default terminal control (no-op) used when a host does
/// not inject a crossterm-backed implementation.
pub fn default_terminal_control() -> Arc<dyn ShellTerminalControl> {
    Arc::new(NoopTerminalControl)
}

/// RAII guard that restores terminal raw mode on drop when `restore` is set.
///
/// Holds an `Arc<dyn ShellTerminalControl>` so it can be dropped without
/// borrowing `ShellManager` — the guard outlives the `&self` borrow taken by
/// the surrounding spawn call.
struct RawModeGuard {
    restore: bool,
    terminal: Arc<dyn ShellTerminalControl>,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.restore {
            self.terminal.enable_raw_mode();
        }
    }
}

pub enum ShellChild {
    Process(Child),
    Pty(Box<dyn portable_pty::Child + Send>),
}

#[cfg(unix)]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    let pgid = child.id() as libc::pid_t;
    if pgid <= 0 {
        return child.kill();
    }

    let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            child.kill()
        }
    }
}

/// Configure parent-death signaling so shell-spawned children are reaped when
/// the TUI dies abnormally (#421). On Linux this installs
/// `PR_SET_PDEATHSIG(SIGTERM)` via `pre_exec` — the kernel then sends SIGTERM
/// to the child the moment the parent process exits, even on SIGKILL of the
/// TUI. The cancellation path already SIGKILLs the whole process group, so
/// this only fires when the parent dies without running its drop / cleanup
/// code (panic during shutdown, OOM, hardware crash, etc.).
///
/// On macOS / Windows there's no kernel equivalent. The existing graceful
/// path (`kill_child_process_group` from the cancellation token) still
/// handles normal shutdown; abnormal exit can leak children — tracked as a
/// follow-up watchdog item per the original issue's acceptance criteria.
#[cfg(target_os = "linux")]
fn install_parent_death_signal(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` runs in the child between fork and exec. The closure
    // only calls `libc::prctl` with stack-allocated constant arguments and
    // does not touch heap memory or the parent's locks. Both requirements
    // (async-signal-safe + no allocation in the post-fork window) are met.
    unsafe {
        cmd.pre_exec(|| {
            let result = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
            if result == -1 {
                // Surface the errno but do not abort the spawn — the child
                // will simply lose the parent-death cleanup safety net.
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// Attach `args` to a `std::process::Command`, honoring shell-quoting on
/// Windows.
///
/// Issue #1691: on Windows the shell command is invoked as
/// `cmd /C "chcp 65001 >NUL & <command>"`. Rust's `Command::arg` applies
/// MSVCRT (`CommandLineToArgvW`) escaping, turning the embedded `"` in a
/// quoted argument (e.g. `git commit -m "feat: complete sub-pages"`) into
/// `\"`. `cmd.exe` does NOT use MSVCRT parsing — it treats `\` literally and
/// `"` as a bare quote toggle — so the escaped payload is mis-tokenized and
/// `git` receives `feat:`, `complete`, `sub-pages"` as separate pathspecs
/// (the reported `pathspec 'sub-pages"' did not match` symptom). Passing the
/// `cmd /C` payload through `CommandExt::raw_arg` suppresses std's escaping so
/// the string reaches `cmd.exe` verbatim, exactly as a terminal would.
#[cfg(windows)]
pub fn push_shell_args(cmd: &mut Command, program: &str, args: &[String]) {
    use std::os::windows::process::CommandExt;
    // The `cmd /C <payload>` shape is the only place std's per-arg escaping
    // corrupts a quoted command. Pass `/C` and the payload raw so the quotes
    // survive; any other program keeps normal (correct) escaping. Match `cmd`
    // by file stem so a full path (`C:\Windows\System32\cmd.exe`) or `.exe`
    // suffix still triggers the raw-arg path.
    let is_cmd = std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("cmd"))
        .unwrap_or(false);
    if is_cmd && args.len() == 2 && args[0].eq_ignore_ascii_case("/C") {
        cmd.raw_arg(&args[0]);
        cmd.raw_arg(&args[1]);
    } else {
        cmd.args(args);
    }
}

#[cfg(not(windows))]
pub fn push_shell_args(cmd: &mut Command, _program: &str, args: &[String]) {
    // Unix delegates tokenization entirely to `sh -c <command>`; the command
    // string is passed as a single argv entry and never split by us.
    cmd.args(args);
}

#[cfg(not(target_os = "linux"))]
fn install_parent_death_signal(_cmd: &mut Command) {
    // No kernel-level equivalent on macOS / Windows. The cooperative
    // cancellation + process_group SIGKILL path covers normal shutdown;
    // abnormal exit (panic without unwind, SIGKILL of the TUI) can still
    // leak children on those platforms — tracked as a follow-up.
}

#[derive(Clone, Copy, Debug)]
struct ShellExitStatus {
    code: Option<i32>,
    success: bool,
}

impl ShellExitStatus {
    fn from_std(status: std::process::ExitStatus) -> Self {
        Self {
            code: status.code(),
            success: status.success(),
        }
    }

    fn from_pty(status: portable_pty::ExitStatus) -> Self {
        let code = i32::try_from(status.exit_code()).unwrap_or(i32::MAX);
        Self {
            code: Some(code),
            success: status.success(),
        }
    }
}

impl ShellChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ShellExitStatus>> {
        match self {
            ShellChild::Process(child) => child
                .try_wait()
                .map(|status| status.map(ShellExitStatus::from_std)),
            ShellChild::Pty(child) => child
                .try_wait()
                .map(|status| status.map(ShellExitStatus::from_pty)),
        }
    }

    fn wait(&mut self) -> std::io::Result<ShellExitStatus> {
        match self {
            ShellChild::Process(child) => child.wait().map(ShellExitStatus::from_std),
            ShellChild::Pty(child) => child.wait().map(ShellExitStatus::from_pty),
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            ShellChild::Process(child) => kill_child_process_group(child),
            #[cfg(not(unix))]
            ShellChild::Process(child) => child.kill(),
            ShellChild::Pty(child) => child.kill(),
        }
    }
}

pub enum StdinWriter {
    Pipe(ChildStdin),
    Pty(Box<dyn Write + Send>),
}

impl StdinWriter {
    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            StdinWriter::Pipe(stdin) => stdin.write_all(data),
            StdinWriter::Pty(writer) => writer.write_all(data),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            StdinWriter::Pipe(stdin) => stdin.flush(),
            StdinWriter::Pty(writer) => writer.flush(),
        }
    }
}

fn spawn_reader_thread<R: Read + Send + 'static>(
    mut reader: R,
    buffer: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut guard) = buffer.lock() {
                        guard.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn result_sandbox_fields(
    sandbox_type: SandboxType,
) -> (bool, Option<String>, bool, bool, Option<String>) {
    let sandboxed = !matches!(sandbox_type, SandboxType::None);
    let backend = if sandboxed {
        Some(sandbox_type.to_string())
    } else {
        None
    };
    (sandboxed, backend.clone(), sandboxed, sandboxed, backend)
}

fn apply_shell_result_sandbox_metadata(result: &mut ShellResult, sandbox_type: SandboxType) {
    let (sandboxed, sandbox_type_str, requested, effective, backend) =
        result_sandbox_fields(sandbox_type);
    result.sandboxed = sandboxed;
    result.sandbox_type = sandbox_type_str;
    result.sandbox_requested = requested;
    result.sandbox_effective = effective;
    result.sandbox_backend = backend;
}

fn apply_shell_result_decision_metadata(result: &mut ShellResult, decision: &SandboxDecision) {
    result.sandbox_requested = decision.sandbox_requested;
    result.sandbox_effective = decision.sandbox_effective;
    result.sandboxed = decision.sandbox_effective;
    result.sandbox_type = decision.sandbox_backend.clone();
    result.sandbox_backend = decision.sandbox_backend.clone();
    result.sandbox_unavailable_reason = decision.sandbox_unavailable_reason.clone();
    result.sandbox_fallback_allowed = decision.sandbox_fallback_allowed;
    result.sandbox_excluded_command = decision.sandbox_excluded_command.clone();
    result.sandbox_fail_closed = decision.sandbox_fail_closed;
}

/// A background shell process being tracked
pub struct BackgroundShell {
    pub id: String,
    pub command: String,
    pub working_dir: PathBuf,
    pub status: ShellStatus,
    pub exit_code: Option<i32>,
    pub started_at: Instant,
    pub sandbox_type: SandboxType,
    pub sandbox_decision: SandboxDecision,
    pub linked_task_id: Option<String>,
    stdout_buffer: Arc<Mutex<Vec<u8>>>,
    stderr_buffer: Option<Arc<Mutex<Vec<u8>>>>,
    stdout_cursor: usize,
    stderr_cursor: usize,
    pub stdin: Option<StdinWriter>,
    pub child: Option<ShellChild>,
    pub stdout_thread: Option<std::thread::JoinHandle<()>>,
    pub stderr_thread: Option<std::thread::JoinHandle<()>>,
}

impl BackgroundShell {
    /// Check if the process has completed and update status
    pub fn poll(&mut self) -> bool {
        if self.status != ShellStatus::Running {
            return true;
        }

        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.exit_code = status.code;
                    self.status = if status.success {
                        ShellStatus::Completed
                    } else {
                        ShellStatus::Failed
                    };
                    self.collect_output();
                    true
                }
                Ok(None) => false, // Still running
                Err(_) => {
                    self.status = ShellStatus::Failed;
                    self.collect_output();
                    true
                }
            }
        } else {
            true
        }
    }

    /// Collect output from the background threads
    fn collect_output(&mut self) {
        // Kill the whole process group before joining reader threads.
        // When the shell spawned persistent background jobs (e.g. `nohup curl`),
        // those subprocesses keep the pipe write-ends open after the shell exits.
        // Without this kill, handle.join() blocks indefinitely, freezing the UI
        // event loop that calls list_jobs() → poll() → collect_output().
        #[cfg(unix)]
        if let Some(ShellChild::Process(ref mut proc)) = self.child {
            let _ = kill_child_process_group(proc);
        }
        if let Some(handle) = self.stdout_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
        self.stdin = None;
        self.child = None;
    }

    fn write_stdin(&mut self, input: &str, close: bool) -> Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            if !input.is_empty() {
                stdin
                    .write_all(input.as_bytes())
                    .context("Failed to write to stdin")?;
                stdin.flush().ok();
            }
            if close {
                self.stdin = None;
            }
            return Ok(());
        }

        if input.is_empty() && close {
            return Ok(());
        }

        Err(anyhow!("stdin is not available for task {}", self.id))
    }

    fn full_output(&self) -> (String, String, usize, usize) {
        let stdout_bytes = self
            .stdout_buffer
            .lock()
            .map(|data| data.clone())
            .unwrap_or_default();
        let stderr_bytes = self
            .stderr_buffer
            .as_ref()
            .and_then(|buffer| buffer.lock().ok().map(|data| data.clone()))
            .unwrap_or_default();

        let stdout_len = stdout_bytes.len();
        let stderr_len = stderr_bytes.len();

        (
            String::from_utf8_lossy(&stdout_bytes).to_string(),
            String::from_utf8_lossy(&stderr_bytes).to_string(),
            stdout_len,
            stderr_len,
        )
    }

    fn take_delta(&mut self) -> (String, String, usize, usize, usize, usize) {
        let (stdout_delta, stdout_total) =
            take_delta_from_buffer(&self.stdout_buffer, &mut self.stdout_cursor);
        let (stderr_delta, stderr_total) = if let Some(buffer) = self.stderr_buffer.as_ref() {
            take_delta_from_buffer(buffer, &mut self.stderr_cursor)
        } else {
            (Vec::new(), 0)
        };

        let stdout_delta_len = stdout_delta.len();
        let stderr_delta_len = stderr_delta.len();

        (
            String::from_utf8_lossy(&stdout_delta).to_string(),
            String::from_utf8_lossy(&stderr_delta).to_string(),
            stdout_delta_len,
            stderr_delta_len,
            stdout_total,
            stderr_total,
        )
    }

    fn sandbox_denied(&self) -> bool {
        if matches!(self.status, ShellStatus::Running) {
            return false;
        }
        let (_, stderr_full, _, _) = self.full_output();
        SandboxManager::was_denied(
            self.sandbox_type,
            self.exit_code.unwrap_or(-1),
            &stderr_full,
        )
    }

    /// Kill the process
    fn kill(&mut self) -> Result<()> {
        if let Some(ref mut child) = self.child {
            child.kill().context("Failed to kill process")?;
            let _ = child.wait();
        }
        self.status = ShellStatus::Killed;
        self.collect_output();
        Ok(())
    }

    /// Get a snapshot of the current state
    #[allow(dead_code)]
    pub fn snapshot(&self) -> ShellResult {
        let sandboxed = self.sandbox_decision.sandbox_effective;
        let (stdout_full, stderr_full, _, _) = self.full_output();
        let (stdout, stdout_meta) = truncate_with_meta(&stdout_full);
        let (stderr, stderr_meta) = truncate_with_meta(&stderr_full);
        let mut result = ShellResult {
            task_id: Some(self.id.clone()),
            status: self.status.clone(),
            exit_code: self.exit_code,
            stdout,
            stderr,
            duration_ms: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            stdout_len: stdout_meta.original_len,
            stderr_len: stderr_meta.original_len,
            stdout_omitted: stdout_meta.omitted,
            stderr_omitted: stderr_meta.omitted,
            stdout_truncated: stdout_meta.truncated,
            stderr_truncated: stderr_meta.truncated,
            sandboxed,
            sandbox_type: if sandboxed {
                Some(self.sandbox_type.to_string())
            } else {
                None
            },
            sandbox_denied: self.sandbox_denied(),
            sandbox_requested: false,
            sandbox_effective: false,
            sandbox_backend: None,
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: false,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        };
        apply_shell_result_decision_metadata(&mut result, &self.sandbox_decision);
        result
    }

    fn job_snapshot(&self) -> ShellJobSnapshot {
        // Use tail_from_buffer instead of full_output so we never clone the
        // entire accumulated stdout/stderr for display purposes.  full_output
        // is O(total_bytes_written), which caused the ShellManager mutex to be
        // held for an arbitrarily long time during list_jobs() calls from the
        // TUI event loop — freezing input handling on long automation runs.
        let (stdout_len, stdout_tail) = tail_from_buffer(&self.stdout_buffer, 1200);
        let (stderr_len, stderr_tail) = self
            .stderr_buffer
            .as_ref()
            .map(|buf| tail_from_buffer(buf, 1200))
            .unwrap_or((0, String::new()));
        ShellJobSnapshot {
            id: self.id.clone(),
            job_id: self.id.clone(),
            command: self.command.clone(),
            cwd: self.working_dir.clone(),
            status: self.status.clone(),
            exit_code: self.exit_code,
            elapsed_ms: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            stdout_tail,
            stderr_tail,
            stdout_len,
            stderr_len,
            stdin_available: self.stdin.is_some() && self.status == ShellStatus::Running,
            stale: false,
            linked_task_id: self.linked_task_id.clone(),
        }
    }

    fn job_detail(&self) -> ShellJobDetail {
        let (stdout, stderr, _, _) = self.full_output();
        ShellJobDetail {
            snapshot: self.job_snapshot(),
            stdout,
            stderr,
        }
    }
}

impl Drop for BackgroundShell {
    fn drop(&mut self) {
        if self.status == ShellStatus::Running
            && let Some(ref mut child) = self.child
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn apply_runtime_filesystem_policy(
    policy: &mut ExecutionSandboxPolicy,
    runtime: &SandboxRuntimeConfig,
) {
    if let Some(mode) = runtime.filesystem.mode.as_deref() {
        let normalized = mode.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "read-only" | "readonly" => {
                *policy = ExecutionSandboxPolicy::ReadOnly;
                return;
            }
            "danger-full-access" | "danger_full_access" | "none" => {
                *policy = ExecutionSandboxPolicy::DangerFullAccess;
                return;
            }
            "external-sandbox" | "external_sandbox" => {
                *policy = ExecutionSandboxPolicy::ExternalSandbox {
                    network_access: runtime.network.enabled.unwrap_or(true),
                };
                return;
            }
            "workspace-write" | "workspace_write" | "workspace" => {}
            _ => {}
        }
    }

    let ExecutionSandboxPolicy::WorkspaceWrite {
        writable_roots,
        network_access,
        exclude_tmpdir,
        exclude_slash_tmp,
        ..
    } = policy
    else {
        return;
    };

    if let Some(enabled) = runtime.network.enabled {
        *network_access = enabled;
    }
    if runtime.network.allow_managed_domains_only {
        // Local OS sandboxes cannot enforce host allow-lists; keep the command
        // network-restricted unless a backend with domain policy support runs it.
        *network_access = false;
    }

    for root in &runtime.filesystem.writable_roots {
        if !writable_roots.iter().any(|existing| existing == root) {
            writable_roots.push(root.clone());
        }
    }
    for root in &runtime.filesystem.allow_write {
        if !writable_roots.iter().any(|existing| existing == root) {
            writable_roots.push(root.clone());
        }
    }
    if let Some(value) = runtime.filesystem.exclude_tmpdir {
        *exclude_tmpdir = value;
    }
    if let Some(value) = runtime.filesystem.exclude_slash_tmp {
        *exclude_slash_tmp = value;
    }
}

fn sandbox_unavailable_reason(prefer_bwrap: bool) -> String {
    #[cfg(target_os = "macos")]
    {
        let _ = prefer_bwrap;
        if !crate::sandbox::seatbelt::is_available() {
            return "macOS sandbox-exec is unavailable".to_string();
        }
    }
    #[cfg(target_os = "linux")]
    {
        if prefer_bwrap && !crate::sandbox::bwrap::is_available() {
            return "bubblewrap was requested but /usr/bin/bwrap is unavailable".to_string();
        }
        if !crate::sandbox::landlock::is_available() && !crate::sandbox::bwrap::is_available() {
            return "no Linux sandbox backend is available (Landlock or bubblewrap)".to_string();
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = prefer_bwrap;
        if !crate::sandbox::windows::is_available() {
            return "Windows sandbox helper is unavailable".to_string();
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = prefer_bwrap;
        return "no sandbox backend is available on this platform".to_string();
    }
    "sandbox backend is unavailable".to_string()
}

pub fn decide_sandbox(
    manager: &SandboxManager,
    runtime: &SandboxRuntimeConfig,
    policy: &ExecutionSandboxPolicy,
    command: &str,
    program: &str,
) -> crate::sandbox::SandboxDecision {
    if !policy.should_sandbox() {
        return crate::sandbox::SandboxDecision::unsandboxed(policy);
    }
    if !runtime.enabled {
        return crate::sandbox::SandboxDecision::disabled(
            policy,
            "sandbox disabled by configuration",
        );
    }
    if !runtime.platform_enabled() {
        return crate::sandbox::SandboxDecision::disabled(
            policy,
            format!(
                "sandbox disabled for platform {}",
                crate::sandbox::current_platform()
            ),
        );
    }
    if runtime.command_is_excluded(program, command) {
        return crate::sandbox::SandboxDecision::excluded(policy, program.to_string());
    }

    let sandbox_type = manager.select_sandbox(policy);
    if matches!(sandbox_type, SandboxType::None) {
        return crate::sandbox::SandboxDecision::unavailable(
            policy,
            sandbox_unavailable_reason(runtime.prefer_bwrap),
            runtime.fail_if_unavailable,
        );
    }

    #[cfg(target_os = "linux")]
    if matches!(sandbox_type, SandboxType::LinuxLandlock) {
        return crate::sandbox::SandboxDecision::unavailable(
            policy,
            "Linux Landlock helper is not wired for child-process enforcement; enable prefer_bwrap or configure an external sandbox backend",
            runtime.fail_if_unavailable,
        );
    }

    crate::sandbox::SandboxDecision::enforcing(policy, sandbox_type)
}

/// Manages background shell processes with optional sandboxing.
pub struct ShellManager {
    pub processes: HashMap<String, BackgroundShell>,
    stale_jobs: HashMap<String, ShellJobSnapshot>,
    default_workspace: PathBuf,
    sandbox_manager: SandboxManager,
    sandbox_policy: ExecutionSandboxPolicy,
    sandbox_runtime: SandboxRuntimeConfig,
    foreground_background_requested: bool,
    /// Terminal raw-mode controller. The TUI injects a crossterm-backed
    /// implementation; non-TUI hosts (tests) get a no-op. Trait-erased so
    /// `ShellManager` can live in the terminal-agnostic runtime crate.
    terminal: Arc<dyn ShellTerminalControl>,
}

impl std::fmt::Debug for ShellManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellManager")
            .field("processes", &self.processes.len())
            .field("stale_jobs", &self.stale_jobs.len())
            .field("default_workspace", &self.default_workspace)
            .field("sandbox_policy", &self.sandbox_policy)
            .field(
                "foreground_background_requested",
                &self.foreground_background_requested,
            )
            .finish()
    }
}

impl ShellManager {
    /// Create a new `ShellManager` with default (no sandbox) policy and a
    /// no-op terminal controller (raw mode is never enabled outside the TUI).
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            processes: HashMap::new(),
            stale_jobs: HashMap::new(),
            default_workspace: workspace,
            sandbox_manager: SandboxManager::new(),
            sandbox_policy: ExecutionSandboxPolicy::default(),
            sandbox_runtime: SandboxRuntimeConfig::default(),
            foreground_background_requested: false,
            terminal: default_terminal_control(),
        }
    }

    /// Create a new `ShellManager` with a specific sandbox policy.
    #[allow(dead_code)]
    pub fn with_sandbox(workspace: PathBuf, policy: ExecutionSandboxPolicy) -> Self {
        Self {
            processes: HashMap::new(),
            stale_jobs: HashMap::new(),
            default_workspace: workspace,
            sandbox_manager: SandboxManager::new(),
            sandbox_policy: policy,
            sandbox_runtime: SandboxRuntimeConfig::default(),
            foreground_background_requested: false,
            terminal: default_terminal_control(),
        }
    }

    /// Create a new `ShellManager` with an explicit terminal raw-mode
    /// controller. The TUI uses this to inject its crossterm-backed
    /// implementation so sandboxed sync/interactive exec can save/restore
    /// raw mode around child spawn.
    pub fn with_terminal_control(
        workspace: PathBuf,
        terminal: Arc<dyn ShellTerminalControl>,
    ) -> Self {
        Self {
            processes: HashMap::new(),
            stale_jobs: HashMap::new(),
            default_workspace: workspace,
            sandbox_manager: SandboxManager::new(),
            sandbox_policy: ExecutionSandboxPolicy::default(),
            sandbox_runtime: SandboxRuntimeConfig::default(),
            foreground_background_requested: false,
            terminal,
        }
    }

    /// Set the sandbox policy for future commands.
    #[allow(dead_code)]
    pub fn set_sandbox_policy(&mut self, policy: ExecutionSandboxPolicy) {
        self.sandbox_policy = policy;
    }

    /// Get the current sandbox policy.
    #[allow(dead_code)]
    pub fn sandbox_policy(&self) -> &ExecutionSandboxPolicy {
        &self.sandbox_policy
    }

    /// Enable or disable bubblewrap passthrough (#2184).
    ///
    /// When enabled and `/usr/bin/bwrap` is present on Linux, exec_shell
    /// commands are routed through bubblewrap for filesystem isolation.
    #[allow(dead_code)] // Wired from EngineConfig in follow-up PR
    pub fn set_prefer_bwrap(&mut self, prefer: bool) {
        self.sandbox_manager.set_prefer_bwrap(prefer);
        self.sandbox_runtime.prefer_bwrap = prefer;
    }

    pub fn set_sandbox_runtime(&mut self, runtime: SandboxRuntimeConfig) {
        self.sandbox_manager.set_prefer_bwrap(runtime.prefer_bwrap);
        self.sandbox_runtime = runtime;
    }

    /// Request that the active foreground shell wait detach and leave its
    /// process running in the background job table.
    pub fn request_foreground_background(&mut self) {
        self.foreground_background_requested = true;
    }

    fn clear_foreground_background_request(&mut self) {
        self.foreground_background_requested = false;
    }

    fn take_foreground_background_request(&mut self) -> bool {
        let requested = self.foreground_background_requested;
        self.foreground_background_requested = false;
        requested
    }

    /// Check if sandboxing is available on this platform.
    #[allow(dead_code)]
    pub fn is_sandbox_available(&mut self) -> bool {
        self.sandbox_manager.is_available()
    }

    #[allow(dead_code)]
    pub fn default_workspace(&self) -> &Path {
        &self.default_workspace
    }

    /// Execute a shell command with the configured sandbox policy.
    #[allow(dead_code)]
    pub fn execute(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
    ) -> Result<ShellResult> {
        self.execute_with_policy(command, working_dir, timeout_ms, background, None)
    }

    /// Execute a shell command with a specific sandbox policy (overrides default).
    #[allow(dead_code)]
    pub fn execute_with_policy(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
    ) -> Result<ShellResult> {
        self.execute_with_options(
            command,
            working_dir,
            timeout_ms,
            background,
            None,
            false,
            policy_override,
        )
    }

    /// Execute a shell command with stdin/TTY options.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_options(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
    ) -> Result<ShellResult> {
        self.execute_with_options_env(
            command,
            working_dir,
            timeout_ms,
            background,
            stdin_data,
            tty,
            policy_override,
            HashMap::new(),
        )
    }

    /// Same as `execute_with_options`, plus an extra env-var map that is
    /// merged into the spawned process environment. Used by the `shell_env`
    /// hook injection path (#456); other callers should use the simpler
    /// wrapper above.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_options_env(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> Result<ShellResult> {
        // Log execution via ShellDispatcher when SHELL_DISPATCHER_LOG is set.
        crate::shell_dispatcher::ShellDispatcher::log_exec(command);

        let work_dir = working_dir.map_or_else(|| self.default_workspace.clone(), PathBuf::from);

        // Clamp timeout to max 10 minutes (600000ms)
        let timeout_ms = timeout_ms.clamp(1000, 600_000);

        // Use override policy if provided, otherwise use the manager's policy
        let mut policy = policy_override.unwrap_or_else(|| self.sandbox_policy.clone());
        apply_runtime_filesystem_policy(&mut policy, &self.sandbox_runtime);

        let mut env = extra_env;
        merge_git_scrub_env(&mut env);

        // Create command spec and prepare sandboxed environment
        let spec = CommandSpec::shell(command, work_dir.clone(), Duration::from_millis(timeout_ms))
            .with_policy(policy.clone())
            .with_env(env);
        let decision = decide_sandbox(
            &self.sandbox_manager,
            &self.sandbox_runtime,
            &policy,
            command,
            &spec.program,
        );
        if !decision.allows_execution() {
            return Ok(ShellResult {
                task_id: None,
                status: ShellStatus::Failed,
                exit_code: None,
                stdout: String::new(),
                stderr: decision
                    .sandbox_unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "sandbox unavailable".to_string()),
                duration_ms: 0,
                stdout_len: 0,
                stderr_len: decision
                    .sandbox_unavailable_reason
                    .as_ref()
                    .map_or(0, String::len),
                stdout_omitted: 0,
                stderr_omitted: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                sandboxed: false,
                sandbox_type: None,
                sandbox_denied: true,
                sandbox_requested: decision.sandbox_requested,
                sandbox_effective: decision.sandbox_effective,
                sandbox_backend: decision.sandbox_backend.clone(),
                sandbox_unavailable_reason: decision.sandbox_unavailable_reason.clone(),
                sandbox_fallback_allowed: decision.sandbox_fallback_allowed,
                sandbox_excluded_command: decision.sandbox_excluded_command.clone(),
                sandbox_fail_closed: decision.sandbox_fail_closed,
            });
        }
        let exec_env = if decision.sandbox_effective {
            self.sandbox_manager.prepare(&spec)
        } else {
            SandboxManager::prepare_unsandboxed_for_fallback(&spec)
        };

        if background {
            self.spawn_background_sandboxed(
                command, &work_dir, &exec_env, stdin_data, tty, &decision,
            )
        } else {
            if tty {
                return Err(anyhow!(
                    "TTY mode requires background execution (set background: true)."
                ));
            }
            Self::execute_sync_sandboxed(
                command,
                &work_dir,
                timeout_ms,
                stdin_data,
                &exec_env,
                &decision,
                &self.terminal,
            )
        }
    }

    /// Execute a shell command interactively (stdin/stdout/stderr inherit from terminal).
    #[allow(dead_code)]
    pub fn execute_interactive(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ShellResult> {
        self.execute_interactive_with_policy(command, working_dir, timeout_ms, None)
    }

    /// Execute a shell command interactively with a specific sandbox policy override.
    pub fn execute_interactive_with_policy(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        policy_override: Option<ExecutionSandboxPolicy>,
    ) -> Result<ShellResult> {
        self.execute_interactive_with_policy_env(
            command,
            working_dir,
            timeout_ms,
            policy_override,
            HashMap::new(),
        )
    }

    /// Interactive variant that accepts extra env vars (#456 shell_env hook).
    pub fn execute_interactive_with_policy_env(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> Result<ShellResult> {
        crate::shell_dispatcher::ShellDispatcher::log_exec(command);

        let work_dir = working_dir.map_or_else(|| self.default_workspace.clone(), PathBuf::from);

        let timeout_ms = timeout_ms.clamp(1000, 600_000);
        let mut policy = policy_override.unwrap_or_else(|| self.sandbox_policy.clone());
        apply_runtime_filesystem_policy(&mut policy, &self.sandbox_runtime);

        let mut env = extra_env;
        merge_git_scrub_env(&mut env);

        let spec = CommandSpec::shell(command, work_dir.clone(), Duration::from_millis(timeout_ms))
            .with_policy(policy.clone())
            .with_env(env);
        let decision = decide_sandbox(
            &self.sandbox_manager,
            &self.sandbox_runtime,
            &policy,
            command,
            &spec.program,
        );
        if !decision.allows_execution() {
            return Ok(ShellResult {
                task_id: None,
                status: ShellStatus::Failed,
                exit_code: None,
                stdout: String::new(),
                stderr: decision
                    .sandbox_unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "sandbox unavailable".to_string()),
                duration_ms: 0,
                stdout_len: 0,
                stderr_len: decision
                    .sandbox_unavailable_reason
                    .as_ref()
                    .map_or(0, String::len),
                stdout_omitted: 0,
                stderr_omitted: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                sandboxed: false,
                sandbox_type: None,
                sandbox_denied: true,
                sandbox_requested: decision.sandbox_requested,
                sandbox_effective: decision.sandbox_effective,
                sandbox_backend: decision.sandbox_backend.clone(),
                sandbox_unavailable_reason: decision.sandbox_unavailable_reason.clone(),
                sandbox_fallback_allowed: decision.sandbox_fallback_allowed,
                sandbox_excluded_command: decision.sandbox_excluded_command.clone(),
                sandbox_fail_closed: decision.sandbox_fail_closed,
            });
        }
        let exec_env = if decision.sandbox_effective {
            self.sandbox_manager.prepare(&spec)
        } else {
            SandboxManager::prepare_unsandboxed_for_fallback(&spec)
        };

        Self::execute_interactive_sandboxed(
            command,
            &work_dir,
            timeout_ms,
            &exec_env,
            &decision,
            &self.terminal,
        )
    }

    /// Execute command synchronously with timeout (sandboxed).
    fn execute_sync_sandboxed(
        original_command: &str,
        working_dir: &std::path::Path,
        timeout_ms: u64,
        stdin_data: Option<&str>,
        exec_env: &ExecEnv,
        decision: &crate::sandbox::SandboxDecision,
        terminal: &Arc<dyn ShellTerminalControl>,
    ) -> Result<ShellResult> {
        let started = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let sandbox_type = exec_env.sandbox_type;
        let sandboxed = decision.sandbox_effective;

        // Build the command from ExecEnv
        let program = exec_env.program();
        let args = exec_env.args();

        let mut cmd = Command::new(program);
        push_shell_args(&mut cmd, program, args);
        cmd.current_dir(working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        install_parent_death_signal(&mut cmd);

        if stdin_data.is_some() {
            cmd.stdin(Stdio::piped());
        }

        child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));

        // Disable raw mode before spawn; restore only if raw mode was active
        // on entry (issue #1690). Trait-erased so `ShellManager` can live in
        // the terminal-agnostic runtime crate; the TUI injects a crossterm
        // implementation, non-TUI hosts get a no-op.
        let raw_mode_was_enabled = terminal.raw_mode_enabled();
        if raw_mode_was_enabled {
            terminal.disable_raw_mode();
        }
        let _guard = RawModeGuard {
            restore: raw_mode_was_enabled,
            terminal: Arc::clone(terminal),
        };

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to execute: {original_command}"))?;

        if let Some(input) = stdin_data
            && let Some(mut stdin) = child.stdin.take()
        {
            stdin
                .write_all(input.as_bytes())
                .context("Failed to write to stdin")?;
            stdin.flush().ok();
        }

        let stdout_handle = child.stdout.take().context("Failed to capture stdout")?;
        let stderr_handle = child.stderr.take().context("Failed to capture stderr")?;

        // Spawn threads to read output
        let stdout_thread = std::thread::spawn(move || {
            let mut reader = stdout_handle;
            let mut buf = Vec::new();
            let _ = reader.read_to_end(&mut buf);
            buf
        });

        let stderr_thread = std::thread::spawn(move || {
            let mut reader = stderr_handle;
            let mut buf = Vec::new();
            let _ = reader.read_to_end(&mut buf);
            buf
        });

        // Wait with timeout
        if let Some(status) = child.wait_timeout(timeout)? {
            let stdout = stdout_thread.join().unwrap_or_default();
            let stderr = stderr_thread.join().unwrap_or_default();
            let stdout_str = String::from_utf8_lossy(&stdout).to_string();
            let stderr_str = String::from_utf8_lossy(&stderr).to_string();
            let exit_code = status.code().unwrap_or(-1);

            // Check if sandbox denied the operation
            let sandbox_denied = SandboxManager::was_denied(sandbox_type, exit_code, &stderr_str);
            let (stdout, stdout_meta) = truncate_with_meta(&stdout_str);
            let (stderr, stderr_meta) = truncate_with_meta(&stderr_str);

            let mut result = ShellResult {
                task_id: None,
                status: if status.success() {
                    ShellStatus::Completed
                } else {
                    ShellStatus::Failed
                },
                exit_code: status.code(),
                stdout,
                stderr,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: stdout_meta.original_len,
                stderr_len: stderr_meta.original_len,
                stdout_omitted: stdout_meta.omitted,
                stderr_omitted: stderr_meta.omitted,
                stdout_truncated: stdout_meta.truncated,
                stderr_truncated: stderr_meta.truncated,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied,
                sandbox_requested: false,
                sandbox_effective: false,
                sandbox_backend: None,
                sandbox_unavailable_reason: None,
                sandbox_fallback_allowed: false,
                sandbox_excluded_command: None,
                sandbox_fail_closed: false,
            };
            apply_shell_result_decision_metadata(&mut result, decision);
            Ok(result)
        } else {
            // Timeout - kill the process
            #[cfg(unix)]
            let _ = kill_child_process_group(&mut child);
            #[cfg(not(unix))]
            let _ = child.kill();
            let status = child.wait().ok();
            let stdout = stdout_thread.join().unwrap_or_default();
            let stderr = stderr_thread.join().unwrap_or_default();
            let stdout_str = String::from_utf8_lossy(&stdout).to_string();
            let stderr_str = String::from_utf8_lossy(&stderr).to_string();
            let (stdout, stdout_meta) = truncate_with_meta(&stdout_str);
            let (stderr, stderr_meta) = truncate_with_meta(&stderr_str);

            let mut result = ShellResult {
                task_id: None,
                status: ShellStatus::TimedOut,
                exit_code: status.and_then(|s| s.code()),
                stdout,
                stderr,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: stdout_meta.original_len,
                stderr_len: stderr_meta.original_len,
                stdout_omitted: stdout_meta.omitted,
                stderr_omitted: stderr_meta.omitted,
                stdout_truncated: stdout_meta.truncated,
                stderr_truncated: stderr_meta.truncated,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied: false,
                sandbox_requested: false,
                sandbox_effective: false,
                sandbox_backend: None,
                sandbox_unavailable_reason: None,
                sandbox_fallback_allowed: false,
                sandbox_excluded_command: None,
                sandbox_fail_closed: false,
            };
            apply_shell_result_decision_metadata(&mut result, decision);
            Ok(result)
        }
    }

    /// Execute command interactively with timeout (sandboxed).
    fn execute_interactive_sandboxed(
        original_command: &str,
        working_dir: &std::path::Path,
        timeout_ms: u64,
        exec_env: &ExecEnv,
        decision: &crate::sandbox::SandboxDecision,
        terminal: &Arc<dyn ShellTerminalControl>,
    ) -> Result<ShellResult> {
        let started = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let sandbox_type = exec_env.sandbox_type;
        let sandboxed = decision.sandbox_effective;

        let program = exec_env.program();
        let args = exec_env.args();

        let mut cmd = Command::new(program);
        push_shell_args(&mut cmd, program, args);
        cmd.current_dir(working_dir)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        install_parent_death_signal(&mut cmd);

        // Disable raw mode before spawn; restore only if raw mode was active
        // on entry (issue #1690). Trait-erased so `ShellManager` can live in
        // the terminal-agnostic runtime crate; the TUI injects a crossterm
        // implementation, non-TUI hosts get a no-op.
        let raw_mode_was_enabled = terminal.raw_mode_enabled();
        if raw_mode_was_enabled {
            terminal.disable_raw_mode();
        }
        let _guard = RawModeGuard {
            restore: raw_mode_was_enabled,
            terminal: Arc::clone(terminal),
        };

        child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to execute: {original_command}"))?;

        if let Some(status) = child.wait_timeout(timeout)? {
            let mut result = ShellResult {
                task_id: None,
                status: if status.success() {
                    ShellStatus::Completed
                } else {
                    ShellStatus::Failed
                },
                exit_code: status.code(),
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: 0,
                stderr_len: 0,
                stdout_omitted: 0,
                stderr_omitted: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied: false,
                sandbox_requested: false,
                sandbox_effective: false,
                sandbox_backend: None,
                sandbox_unavailable_reason: None,
                sandbox_fallback_allowed: false,
                sandbox_excluded_command: None,
                sandbox_fail_closed: false,
            };
            apply_shell_result_decision_metadata(&mut result, decision);
            Ok(result)
        } else {
            #[cfg(unix)]
            let _ = kill_child_process_group(&mut child);
            #[cfg(not(unix))]
            let _ = child.kill();
            let status = child.wait().ok();

            let mut result = ShellResult {
                task_id: None,
                status: ShellStatus::TimedOut,
                exit_code: status.and_then(|s| s.code()),
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: 0,
                stderr_len: 0,
                stdout_omitted: 0,
                stderr_omitted: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied: false,
                sandbox_requested: false,
                sandbox_effective: false,
                sandbox_backend: None,
                sandbox_unavailable_reason: None,
                sandbox_fallback_allowed: false,
                sandbox_excluded_command: None,
                sandbox_fail_closed: false,
            };
            apply_shell_result_decision_metadata(&mut result, decision);
            Ok(result)
        }
    }

    /// Spawn a background process (sandboxed).
    fn spawn_background_sandboxed(
        &mut self,
        original_command: &str,
        working_dir: &std::path::Path,
        exec_env: &ExecEnv,
        stdin_data: Option<&str>,
        tty: bool,
        decision: &SandboxDecision,
    ) -> Result<ShellResult> {
        let task_id = format!("shell_{}", &Uuid::new_v4().to_string()[..8]);
        let started = Instant::now();
        let sandbox_type = exec_env.sandbox_type;
        let sandboxed = decision.sandbox_effective;

        // Build the command from ExecEnv
        let program = exec_env.program();
        let args = exec_env.args();

        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer = if tty {
            None
        } else {
            Some(Arc::new(Mutex::new(Vec::new())))
        };

        let (child, stdin, stdout_thread, stderr_thread) = if tty {
            let pty_system = native_pty_system();
            let pair = pty_system
                .openpty(PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .context("Failed to open PTY")?;

            let mut cmd = CommandBuilder::new(program);
            for arg in args {
                cmd.arg(arg);
            }
            cmd.cwd(working_dir);
            child_env::apply_to_pty_command(&mut cmd, child_env::string_map_env(&exec_env.env));

            let child = pair
                .slave
                .spawn_command(cmd)
                .with_context(|| format!("Failed to spawn PTY command: {original_command}"))?;
            drop(pair.slave);

            let reader = pair
                .master
                .try_clone_reader()
                .context("Failed to clone PTY reader")?;
            let stdout_thread = Some(spawn_reader_thread(reader, Arc::clone(&stdout_buffer)));
            let writer = pair
                .master
                .take_writer()
                .context("Failed to take PTY writer")?;

            (
                ShellChild::Pty(child),
                Some(StdinWriter::Pty(writer)),
                stdout_thread,
                None,
            )
        } else {
            let mut cmd = Command::new(program);
            push_shell_args(&mut cmd, program, args);
            cmd.current_dir(working_dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            #[cfg(unix)]
            {
                cmd.process_group(0);
            }

            child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));

            let mut child = cmd
                .spawn()
                .with_context(|| format!("Failed to spawn background: {original_command}"))?;

            let stdout_handle = child.stdout.take().context("Failed to capture stdout")?;
            let stderr_handle = child.stderr.take().context("Failed to capture stderr")?;
            let stdin_handle = child.stdin.take().map(StdinWriter::Pipe);

            let stdout_thread = Some(spawn_reader_thread(
                stdout_handle,
                Arc::clone(&stdout_buffer),
            ));
            let stderr_thread = stderr_buffer
                .as_ref()
                .map(|buffer| spawn_reader_thread(stderr_handle, Arc::clone(buffer)));

            (
                ShellChild::Process(child),
                stdin_handle,
                stdout_thread,
                stderr_thread,
            )
        };

        let mut bg_shell = BackgroundShell {
            id: task_id.clone(),
            command: original_command.to_string(),
            working_dir: working_dir.to_path_buf(),
            status: ShellStatus::Running,
            exit_code: None,
            started_at: started,
            sandbox_type,
            sandbox_decision: decision.clone(),
            linked_task_id: None,
            stdout_buffer,
            stderr_buffer,
            stdout_cursor: 0,
            stderr_cursor: 0,
            stdin,
            child: Some(child),
            stdout_thread,
            stderr_thread,
        };

        if let Some(input) = stdin_data {
            bg_shell.write_stdin(input, false)?;
        }

        self.processes.insert(task_id.clone(), bg_shell);

        let mut result = ShellResult {
            task_id: Some(task_id),
            status: ShellStatus::Running,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            stdout_len: 0,
            stderr_len: 0,
            stdout_omitted: 0,
            stderr_omitted: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            sandboxed,
            sandbox_type: if sandboxed {
                Some(sandbox_type.to_string())
            } else {
                None
            },
            sandbox_denied: false,
            sandbox_requested: false,
            sandbox_effective: false,
            sandbox_backend: None,
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: false,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        };
        apply_shell_result_decision_metadata(&mut result, decision);
        Ok(result)
    }

    /// Get output from a background process
    #[allow(dead_code)]
    pub fn get_output(
        &mut self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> Result<ShellResult> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;

        if block && shell.status == ShellStatus::Running {
            let timeout = Duration::from_millis(timeout_ms.clamp(1000, 600_000));
            let deadline = Instant::now() + timeout;

            while shell.status == ShellStatus::Running && Instant::now() < deadline {
                if shell.poll() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            // If still running after timeout
            if shell.status == ShellStatus::Running {
                return Ok(shell.snapshot());
            }
        } else {
            shell.poll();
        }

        Ok(shell.snapshot())
    }

    /// Write data to stdin of a background process.
    pub fn write_stdin(&mut self, task_id: &str, input: &str, close: bool) -> Result<()> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;
        shell.write_stdin(input, close)?;
        Ok(())
    }

    /// Get incremental output from a background process, consuming any new output.
    pub fn get_output_delta(
        &mut self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> Result<ShellDeltaResult> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;

        if wait && shell.status == ShellStatus::Running {
            let timeout = Duration::from_millis(timeout_ms.clamp(1000, 600_000));
            let deadline = Instant::now() + timeout;

            while shell.status == ShellStatus::Running && Instant::now() < deadline {
                if shell.poll() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        } else {
            shell.poll();
        }

        let (
            stdout_delta,
            stderr_delta,
            stdout_delta_len,
            stderr_delta_len,
            stdout_total,
            stderr_total,
        ) = shell.take_delta();
        let (stdout, stdout_meta) = truncate_with_meta(&stdout_delta);
        let (stderr, stderr_meta) = truncate_with_meta(&stderr_delta);
        let sandboxed = shell.sandbox_decision.sandbox_effective;

        let command = shell.command.clone();
        let mut result = ShellResult {
            task_id: Some(shell.id.clone()),
            status: shell.status.clone(),
            exit_code: shell.exit_code,
            stdout,
            stderr,
            duration_ms: u64::try_from(shell.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            stdout_len: stdout_meta.original_len.max(stdout_delta_len),
            stderr_len: stderr_meta.original_len.max(stderr_delta_len),
            stdout_omitted: stdout_meta.omitted,
            stderr_omitted: stderr_meta.omitted,
            stdout_truncated: stdout_meta.truncated,
            stderr_truncated: stderr_meta.truncated,
            sandboxed,
            sandbox_type: if sandboxed {
                Some(shell.sandbox_type.to_string())
            } else {
                None
            },
            sandbox_denied: shell.sandbox_denied(),
            sandbox_requested: false,
            sandbox_effective: false,
            sandbox_backend: None,
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: false,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        };
        apply_shell_result_decision_metadata(&mut result, &shell.sandbox_decision);

        Ok(ShellDeltaResult {
            command,
            result,
            stdout_total_len: stdout_total,
            stderr_total_len: stderr_total,
        })
    }

    /// Kill a running background process
    pub fn kill(&mut self, task_id: &str) -> Result<ShellResult> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;

        shell.kill()?;
        Ok(shell.snapshot())
    }

    /// Kill every currently running background shell process.
    pub fn kill_running(&mut self) -> Result<Vec<ShellResult>> {
        let ids = self
            .processes
            .iter()
            .filter(|(_, shell)| shell.status == ShellStatus::Running)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();

        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            results.push(self.kill(&id)?);
        }
        Ok(results)
    }

    /// Poll a background process and return incremental output.
    pub fn poll_delta(
        &mut self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> Result<ShellDeltaResult> {
        self.get_output_delta(task_id, wait, timeout_ms)
    }

    /// Attach durable task context to a live shell job.
    pub fn tag_linked_task(&mut self, task_id: &str, linked_task_id: Option<String>) -> Result<()> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;
        shell.linked_task_id = linked_task_id;
        Ok(())
    }

    /// Inspect full output for a live or stale job.
    pub fn inspect_job(&mut self, task_id: &str) -> Result<ShellJobDetail> {
        if let Some(shell) = self.processes.get_mut(task_id) {
            shell.poll();
            return Ok(shell.job_detail());
        }
        if let Some(snapshot) = self.stale_jobs.get(task_id) {
            return Ok(ShellJobDetail {
                snapshot: snapshot.clone(),
                stdout: snapshot.stdout_tail.clone(),
                stderr: snapshot.stderr_tail.clone(),
            });
        }
        Err(anyhow!("Task {task_id} not found"))
    }

    /// List all live and known-stale background shell jobs for the TUI.
    pub fn list_jobs(&mut self) -> Vec<ShellJobSnapshot> {
        for shell in self.processes.values_mut() {
            shell.poll();
        }
        // Evict completed processes older than 1 hour to bound memory growth.
        self.cleanup(Duration::from_secs(3600));

        let mut jobs = self
            .processes
            .values()
            .map(BackgroundShell::job_snapshot)
            .collect::<Vec<_>>();
        jobs.extend(self.stale_jobs.values().cloned());
        jobs.sort_by(|a, b| {
            job_status_rank(&a.status, a.stale)
                .cmp(&job_status_rank(&b.status, b.stale))
                .then_with(|| a.id.cmp(&b.id))
        });
        jobs
    }

    /// Remember a restart-stale job so the UI can show it instead of hiding it.
    #[allow(dead_code)]
    pub fn remember_stale_job(
        &mut self,
        id: impl Into<String>,
        command: impl Into<String>,
        cwd: PathBuf,
        linked_task_id: Option<String>,
    ) {
        let id = id.into();
        self.stale_jobs.insert(
            id.clone(),
            ShellJobSnapshot {
                id: id.clone(),
                job_id: id,
                command: command.into(),
                cwd,
                status: ShellStatus::Killed,
                exit_code: None,
                elapsed_ms: 0,
                stdout_tail: String::new(),
                stderr_tail: "Process is no longer attached to this TUI session.".to_string(),
                stdout_len: 0,
                stderr_len: 0,
                stdin_available: false,
                stale: true,
                linked_task_id,
            },
        );
    }

    /// Clean up completed processes older than the given duration
    pub fn cleanup(&mut self, max_age: Duration) {
        let _now = Instant::now();
        self.processes.retain(|_, shell| {
            if shell.status == ShellStatus::Running {
                true
            } else {
                shell.started_at.elapsed() < max_age
            }
        });
    }
}

fn take_delta_from_buffer(buffer: &Arc<Mutex<Vec<u8>>>, cursor: &mut usize) -> (Vec<u8>, usize) {
    let guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let total = guard.len();
    let start = (*cursor).min(total);
    // Clone only the unread portion (the delta), not the entire accumulated buffer.
    // Long-running processes can produce megabytes of output; cloning the full
    // buffer on every poll held the ShellManager mutex for O(total_bytes) time.
    let delta = guard[start..].to_vec();
    *cursor = total;
    (delta, total)
}

/// Read only the tail of a byte buffer and return (total_len, tail_string).
///
/// Avoids cloning the full buffer when only a trailing excerpt is needed
/// (e.g. for the job-panel display).  `max_tail_chars` is in Unicode scalar
/// values; we read at most `max_tail_chars * 4` bytes from the end to account
/// for multi-byte UTF-8 sequences.
fn tail_from_buffer(buffer: &Arc<Mutex<Vec<u8>>>, max_tail_chars: usize) -> (usize, String) {
    let guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let total = guard.len();
    // Over-estimate byte count (4 bytes per char worst case for UTF-8).
    let mut tail_start = total.saturating_sub(max_tail_chars.saturating_mul(4));
    // Snap forward to the next valid UTF-8 codepoint boundary so we don't
    // pass a slice beginning with continuation bytes (0x80–0xBF) to
    // from_utf8_lossy, which would emit a leading U+FFFD replacement char.
    while tail_start < total && (guard[tail_start] & 0xC0) == 0x80 {
        tail_start += 1;
    }
    let tail_str = String::from_utf8_lossy(&guard[tail_start..]).into_owned();
    (total, tail_text(&tail_str, max_tail_chars))
}

fn tail_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let tail = text
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}

fn job_status_rank(status: &ShellStatus, stale: bool) -> u8 {
    if stale {
        return 4;
    }
    match status {
        ShellStatus::Running => 0,
        ShellStatus::Failed | ShellStatus::TimedOut => 1,
        ShellStatus::Killed => 2,
        ShellStatus::Completed => 3,
    }
}

/// Thread-safe wrapper for `ShellManager`
pub type SharedShellManager = Arc<Mutex<ShellManager>>;

/// Bridge the TUI's [`SharedShellManager`] onto the portable
/// [`ShellManagerApi`] trait. A newtype is required (orphan rule); each
/// method locks the inner `std::sync::Mutex` synchronously and delegates to
/// the inherent [`ShellManager`] method — the pre-trait call sites did the
/// same under a single `.lock()` guard; the host mutex serializes concurrent
/// callers. The engine-core [`ShellApi`] impl (returning the reduced
/// [`ShellExecResult`]) lives in this module (`shell_manager`).
pub struct ShellManagerHost(pub SharedShellManager);

/// Tool-facing rich shell surface. Delegates to the inherent
/// `ShellManager` methods (private ones like `get_output_delta` are visible
/// because this impl shares the module).
impl ShellManagerApi for ShellManagerHost {
    fn clear_foreground_background_request(&self) {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.clear_foreground_background_request();
    }

    fn set_sandbox_runtime(&self, runtime: SandboxRuntimeConfig) {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.set_sandbox_runtime(runtime);
    }

    fn execute_with_options_env(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> anyhow::Result<ShellResult> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.execute_with_options_env(
            command,
            working_dir,
            timeout_ms,
            background,
            stdin_data,
            tty,
            policy_override,
            extra_env,
        )
    }

    fn execute_interactive_with_policy_env(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> anyhow::Result<ShellResult> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.execute_interactive_with_policy_env(
            command,
            working_dir,
            timeout_ms,
            policy_override,
            extra_env,
        )
    }

    fn write_stdin(&self, task_id: &str, input: &str, close: bool) -> anyhow::Result<()> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.write_stdin(task_id, input, close)
    }

    fn kill(&self, task_id: &str) -> anyhow::Result<ShellResult> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.kill(task_id)
    }

    fn kill_running(&self) -> anyhow::Result<Vec<ShellResult>> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.kill_running()
    }

    fn take_foreground_background_request(&self) -> bool {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.take_foreground_background_request()
    }

    fn get_output(
        &self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> anyhow::Result<ShellResult> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.get_output(task_id, block, timeout_ms)
    }

    fn get_output_delta(
        &self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.get_output_delta(task_id, wait, timeout_ms)
    }

    fn tag_linked_task(&self, task_id: &str, linked_task_id: Option<String>) -> anyhow::Result<()> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.tag_linked_task(task_id, linked_task_id)
    }

    fn list_jobs(&self) -> Vec<ShellJobSnapshot> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.list_jobs()
    }

    fn inspect_job(&self, task_id: &str) -> anyhow::Result<ShellJobDetail> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.inspect_job(task_id)
    }

    fn poll_delta(
        &self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.poll_delta(task_id, wait, timeout_ms)
    }

    fn request_foreground_background(&self) {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        guard.request_foreground_background();
    }
}

/// Wrap a concrete [`SharedShellManager`] behind the portable
/// [`ShellManagerApi`] trait object. Used at the host→runtime boundary
/// (engine tool setup, `RuntimeToolServices` population, test contexts).
pub fn wrap_shell_manager(sm: SharedShellManager) -> Arc<dyn ShellManagerApi> {
    Arc::new(ShellManagerHost(sm))
}

/// Construct a new shared shell manager with default (no sandbox) policy and
/// the no-op terminal controller (raw mode is never enabled outside the TUI).
///
/// This is the portable default used by `ToolContext::new` in `spec.rs`. The
/// TUI overrides it with a crossterm-backed controller via
/// [`ShellManager::with_terminal_control`] wherever interactive raw-mode
/// save/restore around child spawn is required.
pub fn new_shared_shell_manager(workspace: PathBuf) -> SharedShellManager {
    Arc::new(Mutex::new(ShellManager::new(workspace)))
}

impl ShellApi for ShellManagerHost {
    fn execute(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
    ) -> anyhow::Result<ShellExecResult> {
        let mut shell = self.0.lock().unwrap_or_else(|p| p.into_inner());
        ShellManager::execute(&mut shell, command, working_dir, timeout_ms, background)
            .map(shell_exec_result_from)
    }
}

/// Convert the TUI-local [`ShellResult`] into the portable
/// [`ShellExecResult`]. A free function (not `impl From`) because the orphan
/// rule forbids `impl From<ShellResult> for ShellExecResult` from the TUI
/// crate (both the trait and the target type are foreign).
fn shell_exec_result_from(r: ShellResult) -> ShellExecResult {
    ShellExecResult {
        task_id: r.task_id,
        status: match r.status {
            ShellStatus::Running => ShellExecStatus::Running,
            ShellStatus::Completed => ShellExecStatus::Completed,
            ShellStatus::Failed => ShellExecStatus::Failed,
            ShellStatus::Killed => ShellExecStatus::Killed,
            ShellStatus::TimedOut => ShellExecStatus::TimedOut,
        },
        exit_code: r.exit_code,
        stdout: r.stdout,
        stderr: r.stderr,
        duration_ms: r.duration_ms,
    }
}
