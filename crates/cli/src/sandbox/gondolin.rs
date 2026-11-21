//! Gondolin microVM sandbox backend ([earendil-works/gondolin]).
//!
//! Gondolin runs commands inside a local Linux microVM (QEMU by default;
//! macOS works too — no KVM required) with host-side network egress and
//! filesystem policy. Integration shape:
//!
//! - **Sessions.** One `gondolin bash -- <keeper>` process per compiled
//!   policy shape (mount set + network allowlist) keeps a VM alive and
//!   registered under `~/.cache/gondolin/sessions/`; its id is discovered
//!   from `gondolin list` by owner PID. Sessions boot lazily on the first
//!   matching command — the first command pays the VM boot (seconds; the
//!   very first run ever also downloads ~200 MB of guest assets, handled
//!   by gondolin itself).
//! - **Commands.** `gondolin exec --sock <session> --env K=V -- sh -lc
//!   "<cmd>"` runs the command inside that VM as a local subprocess of
//!   ours — its stdout/stderr pipes ARE the task's output streams (no
//!   control-plane polling for output at all), and its exit code is the
//!   command's.
//! - **Policy compilation** mirrors the container family: writable roots
//!   from `SandboxPolicy::get_writable_roots` become `--mount-hostfs
//!   host:guest` pairs (same path on both sides, `:ro` for protected
//!   subpaths and read-only policies); `--allow-host` carries the network
//!   allowlist.
//!
//! Known limitations:
//! - interactive use (tty/stdin) is not supported;
//! - a killed command's local CLI process dies, but whether the guest
//!   process dies with it is gondolin's socket-mode behavior to guarantee,
//!   not ours — a survivor stops when the session VM stops;
//! - `--allow-host '*'` stands in for "network on": gondolin's model is an
//!   allowlist, and CodeSmith's policy vocabulary is only on/off today
//!   (mapping `[sandbox.network] allow` hosts onto the allowlist is a
//!   follow-up);
//! - killing the CLI does not stop session VMs: `stop()` kills the keeper
//!   processes at engine shutdown, and gondolin's own `list` GC reaps
//!   registry entries whose owner is gone;
//! - the workspace mount happens at session boot, so the first request for
//!   a policy shape defines its mounts for the engine's lifetime (shapes
//!   are keyed by the compiled spec, so differing shapes get their own
//!   VM).
//!
//! [earendil-works/gondolin]: https://github.com/earendil-works/gondolin

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;

use super::backend::{
    FsIsolation, NetworkControl, SandboxBackend, SandboxBackendCapabilities, SandboxExecRequest,
    SandboxOutput,
};
use codesmith_sandbox::runtime::{
    SandboxPolicy, SandboxTaskPlane, SandboxTaskSpawn, SandboxTaskStatus,
};

/// The command keeping each session VM alive: runs forever, registers the
/// session, owns the mounts and the network policy.
const SESSION_KEEPER_CMD: &str = "sleep infinity";

/// How long to wait for a freshly booted session to register.
const SESSION_BOOT_TIMEOUT: Duration = Duration::from_secs(120);

/// A booted session VM: its id (for `exec --sock`) plus the keeper process
/// whose lifetime is the VM's.
struct SessionVm {
    session_id: String,
    keeper: Child,
}

impl std::fmt::Debug for SessionVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionVm")
            .field("session_id", &self.session_id)
            .field("keeper_pid", &self.keeper.id())
            .finish()
    }
}

/// The compiled session shape: identical shapes share one VM.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
struct SessionSpec {
    /// `--mount-hostfs` values (`host:guest[:ro]`).
    mounts: Vec<String>,
    /// `--allow-host` values; empty = network off (gondolin allowlist model).
    allow_hosts: Vec<String>,
}

/// Gondolin backend: persistent microVM sessions + socket-mode execs.
pub struct GondolinBackend {
    inner: std::sync::Arc<GondolinInner>,
}

impl std::fmt::Debug for GondolinBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GondolinBackend")
            .field("sessions", &self.inner.sessions.lock().map(|m| m.len()))
            .field("tasks", &self.inner.tasks.lock().map(|m| m.len()))
            .finish()
    }
}

impl Default for GondolinBackend {
    fn default() -> Self {
        Self {
            inner: std::sync::Arc::new(GondolinInner::default()),
        }
    }
}

impl GondolinBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// The shared state: `Arc` so the async `exec` can run its blocking combo
/// on `spawn_blocking` with an owned handle.
#[derive(Debug, Default)]
struct GondolinInner {
    /// Lazily booted sessions by compiled spec.
    sessions: Mutex<HashMap<SessionSpec, SessionVm>>,
    /// Live command subprocesses by task id.
    tasks: Mutex<HashMap<String, Child>>,
}

impl GondolinInner {
    fn gondolin(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("gondolin");
        cmd.args(args);
        cmd
    }

    /// Run a short-lived gondolin command and return its output.
    fn control(&self, args: &[&str]) -> Result<std::process::Output> {
        self.gondolin(args)
            .output()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    anyhow!(
                        "gondolin CLI not found on PATH; install it (npm install -g \
                         @earendil-works/gondolin — QEMU required, see gondolin docs) or set \
                         sandbox_backend = \"none\""
                    )
                } else {
                    anyhow!("failed to run gondolin: {err}")
                }
            })
            .with_context(|| format!("gondolin {}", args.first().copied().unwrap_or("")))
    }

    /// The session id of a freshly booted keeper: scan `gondolin list` for
    /// the entry owned by the keeper's PID. `gondolin list` prints
    /// whitespace-separated columns starting with `ID PID …`.
    fn discover_session_id(&self, keeper_pid: u32) -> Result<String> {
        let needle = format!("{keeper_pid} ");
        let deadline = Instant::now() + SESSION_BOOT_TIMEOUT;
        loop {
            let out = self.control(&["list"])?;
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let mut fields = line.split_whitespace();
                    if let (Some(id), Some(pid)) = (fields.next(), fields.next())
                        && pid.parse::<u32>().is_ok_and(|parsed| parsed == keeper_pid)
                        && line.contains(&needle)
                    {
                        return Ok(id.to_string());
                    }
                }
            }
            if Instant::now() >= deadline {
                bail!(
                    "gondolin session owned by pid {keeper_pid} did not register within \
                     {}s (first boot downloads ~200MB of guest assets — retry once the \
                     download finishes)",
                    SESSION_BOOT_TIMEOUT.as_secs()
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Boot (or return the existing) session for a compiled spec.
    fn session_for(&self, spec: &SessionSpec) -> Result<String> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = sessions.get(spec) {
            return Ok(session.session_id.clone());
        }
        let mut args: Vec<String> = vec!["bash".to_string()];
        for mount in &spec.mounts {
            args.push("--mount-hostfs".into());
            args.push(mount.clone());
        }
        for host in &spec.allow_hosts {
            args.push("--allow-host".into());
            args.push(host.clone());
        }
        args.push("--".into());
        args.push(SESSION_KEEPER_CMD.to_string());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        #[cfg(unix)]
        let mut cmd = {
            use std::os::unix::process::CommandExt;
            let mut cmd = self.gondolin(&arg_refs);
            cmd.process_group(0);
            cmd
        };
        #[cfg(not(unix))]
        let mut cmd = self.gondolin(&arg_refs);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut keeper = cmd
            .spawn()
            .context("failed to boot gondolin session VM (is QEMU installed?)")?;
        let keeper_pid = keeper.id();
        let session_id = match self.discover_session_id(keeper_pid) {
            Ok(id) => id,
            Err(err) => {
                // The VM never registered: dropping a `Child` does not stop
                // it, and the keeper (`sleep infinity`) would leak the
                // process and its VM for good — every retry would leak
                // another. Kill it before propagating.
                let _ = keeper.kill();
                let _ = keeper.wait();
                return Err(err);
            }
        };
        sessions.insert(
            spec.clone(),
            SessionVm {
                session_id: session_id.clone(),
                keeper,
            },
        );
        Ok(session_id)
    }
}

#[async_trait]
impl SandboxBackend for GondolinBackend {
    fn id(&self) -> &'static str {
        "gondolin"
    }

    fn capabilities(&self) -> SandboxBackendCapabilities {
        SandboxBackendCapabilities {
            fs_isolation: FsIsolation::MicroVm,
            // Gondolin's host-side HTTP/HTTPS allowlist is real per-host
            // policy — the only backend in the set with this grade.
            network: NetworkControl::HostPolicy,
            // QEMU (or libkrun on macOS): works wherever gondolin does.
            requires_linux_host: false,
            interactive: false,
        }
    }

    fn prepare(&self) -> Result<()> {
        // Functional probe only: `--version` catches a broken node install
        // (the CLI is a node package). The VM boot happens lazily per
        // session shape, and guest assets are gondolin's own concern.
        let out = self.inner.control(&["--version"]).context(
            "gondolin CLI probe failed — install it (npm install -g \
                      @earendil-works/gondolin) or set sandbox_backend = \"none\"",
        )?;
        if !out.status.success() {
            bail!(
                "gondolin --version exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.inner.stop()
    }

    fn classify_denial(&self, _exit_code: i32, stderr: &str) -> Option<String> {
        // Guest read-only mounts deny with the same EROFS dialect the
        // container family sees.
        stderr
            .to_ascii_lowercase()
            .contains("read-only file system")
            .then(|| "gondolin mount policy denied the write (read-only mount)".to_string())
    }

    fn task_plane(&self) -> Option<&dyn SandboxTaskPlane> {
        Some(self)
    }

    async fn exec(&self, request: SandboxExecRequest) -> Result<SandboxOutput> {
        if request.stdin.is_some() {
            bail!("the gondolin backend cannot feed stdin; pipe input inside the command instead");
        }
        let inner = std::sync::Arc::clone(&self.inner);
        let spec = compile_session_spec(&request);
        let timeout = Duration::from_millis(request.timeout_ms);
        let result: Result<SandboxOutput> = tokio::task::spawn_blocking(move || {
            let session = inner.session_for(&spec)?;
            let task = inner.spawn_in_session(&session, &request)?;
            let (stdout, stderr, exit_code) = inner.wait_collect(task, Instant::now() + timeout)?;
            Ok(SandboxOutput {
                stdout,
                stderr,
                exit_code: exit_code.unwrap_or(-1),
            })
        })
        .await
        .context("gondolin exec task failed")?;
        result
    }
}

impl GondolinInner {
    fn stop(&self) -> Result<()> {
        // Killing the keeper processes stops their VMs; gondolin's `list`
        // GC reaps the registry entries afterwards.
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        for (_, mut session) in sessions.drain() {
            let _ = session.keeper.kill();
            let _ = session.keeper.wait();
        }
        Ok(())
    }
}

/// Compile the session shape for a request: the writable-root legislation
/// (`get_writable_roots`) becomes hostfs mounts at identical paths (`:ro`
/// for protected subpaths and read-only policies), and the network flag
/// becomes the allowlist (`'*'` stands in for on — gondolin's model IS an
/// allowlist, and CodeSmith's vocabulary is only on/off today).
fn compile_session_spec(request: &SandboxExecRequest) -> SessionSpec {
    let cwd = request.cwd.to_string_lossy().into_owned();
    let mut mounts = Vec::new();
    match &request.policy {
        SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. } => {
            mounts.push(format!("{cwd}:{cwd}"));
        }
        SandboxPolicy::ReadOnly => {
            mounts.push(format!("{cwd}:{cwd}:ro"));
        }
        SandboxPolicy::WorkspaceWrite { .. } => {
            for root in request.policy.get_writable_roots(&request.cwd) {
                let root_display = root.root.to_string_lossy().into_owned();
                mounts.push(format!("{root_display}:{root_display}"));
                for subpath in root.read_only_subpaths {
                    let sub_display = subpath.to_string_lossy().into_owned();
                    mounts.push(format!("{sub_display}:{sub_display}:ro"));
                }
            }
        }
    }
    let allow_hosts = if request.policy.has_network_access() {
        vec!["*".to_string()]
    } else {
        Vec::new()
    };
    SessionSpec {
        mounts,
        allow_hosts,
    }
}

/// Single-quote a shell word for the `sh -lc` payload.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

impl SandboxTaskPlane for GondolinBackend {
    fn spawn(&self, request: SandboxExecRequest) -> Result<SandboxTaskSpawn> {
        if request.stdin.is_some() {
            bail!("the gondolin backend cannot feed stdin; pipe input inside the command instead");
        }
        let spec = compile_session_spec(&request);
        let session = self.inner.session_for(&spec)?;
        let task = self.inner.spawn_in_session(&session, &request)?;
        Ok(task)
    }

    fn status(&self, task: &str) -> Result<SandboxTaskStatus> {
        let mut tasks = self.inner.tasks.lock().unwrap_or_else(|e| e.into_inner());
        match tasks.get_mut(task) {
            None => Ok(SandboxTaskStatus::Gone),
            Some(child) => match child.try_wait()? {
                None => Ok(SandboxTaskStatus::Running),
                Some(status) => Ok(SandboxTaskStatus::Exited(status.code().unwrap_or(-1))),
            },
        }
    }

    fn kill(&self, task: &str) -> Result<()> {
        let mut tasks = self.inner.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut child) = tasks.remove(task) {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    fn cleanup(&self, task: &str) -> Result<()> {
        let mut tasks = self.inner.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut child) = tasks.remove(task) {
            let _ = child.wait();
        }
        Ok(())
    }
}

impl GondolinInner {
    /// Spawn a socket-mode exec for `request` in `session`. The subprocess
    /// pipes are the task's output streams; its exit code is the command's.
    fn spawn_in_session(
        &self,
        session: &str,
        request: &SandboxExecRequest,
    ) -> Result<SandboxTaskSpawn> {
        let task_id = format!("gondolin-shell-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let cwd_display = request.cwd.to_string_lossy().into_owned();
        // cwd rides inside the shell payload: socket-mode exec has no
        // documented --cwd flag.
        let payload = format!("cd {} && {}", shell_quote(&cwd_display), request.cmd);
        let mut args: Vec<String> = vec!["exec".to_string(), "--sock".into(), session.to_string()];
        for (key, value) in &request.env {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
        args.push("--".into());
        args.push("sh".into());
        args.push("-lc".into());
        args.push(payload);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        #[cfg(unix)]
        let mut cmd = {
            use std::os::unix::process::CommandExt;
            let mut cmd = self.gondolin(&arg_refs);
            cmd.process_group(0);
            cmd
        };
        #[cfg(not(unix))]
        let mut cmd = self.gondolin(&arg_refs);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("gondolin exec failed in session {session}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("gondolin exec had no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("gondolin exec had no stderr"))?;
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task_id.clone(), child);
        Ok(SandboxTaskSpawn {
            task: task_id,
            stdout: Box::new(stdout),
            stderr: Some(Box::new(stderr)),
        })
    }

    /// Wait for a spawned task to finish (or the deadline), collecting its
    /// output via reader threads, and reap the subprocess. Used by the
    /// one-shot `exec` combo.
    fn wait_collect(
        &self,
        task: SandboxTaskSpawn,
        deadline: Instant,
    ) -> Result<(String, String, Option<i32>)> {
        // Drain both streams on reader threads (EOF when the CLI exits).
        let stdout = spawn_reader(task.stdout);
        let stderr = task.stderr.map(spawn_reader);
        let finish = |stdout: ReaderJoin, stderr: Option<ReaderJoin>| {
            (
                stdout.join().unwrap_or_default(),
                stderr
                    .map(|handle| handle.join().unwrap_or_default())
                    .unwrap_or_default(),
            )
        };
        loop {
            let state = {
                let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
                match tasks.get_mut(&task.task) {
                    None => None,
                    Some(child) => match child.try_wait()? {
                        None => Some(SandboxTaskStatus::Running),
                        Some(status) => {
                            let code = status.code();
                            tasks.remove(&task.task);
                            Some(SandboxTaskStatus::Exited(code.unwrap_or(-1)))
                        }
                    },
                }
            };
            match state {
                Some(SandboxTaskStatus::Exited(code)) => {
                    let (stdout_str, stderr_str) = finish(stdout, stderr);
                    return Ok((stdout_str, stderr_str, Some(code)));
                }
                None => {
                    let (stdout_str, stderr_str) = finish(stdout, stderr);
                    return Ok((stdout_str, stderr_str, None));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                self.kill_task(&task.task);
                let (stdout_str, stderr_str) = finish(stdout, stderr);
                return Ok((stdout_str, stderr_str, None));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Kill and reap a task subprocess (internal form used by
    /// `wait_collect`'s deadline path).
    fn kill_task(&self, task: &str) {
        if let Some(mut child) = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task)
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Reader thread collecting a stream to a String (lossy UTF-8).
type ReaderJoin = std::thread::JoinHandle<String>;
fn spawn_reader(mut stream: Box<dyn Read + Send>) -> ReaderJoin {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn request(policy: SandboxPolicy, cmd: &str, cwd: &str) -> SandboxExecRequest {
        SandboxExecRequest {
            cmd: cmd.to_string(),
            env: HashMap::new(),
            cwd: PathBuf::from(cwd),
            timeout_ms: 10_000,
            stdin: None,
            policy,
        }
    }

    fn workspace_write() -> SandboxPolicy {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: false,
            exclude_tmpdir: true,
            exclude_slash_tmp: true,
        }
    }

    #[test]
    fn workspace_write_compiles_to_rw_mounts_with_ro_subpaths() {
        let spec = compile_session_spec(&request(workspace_write(), "cargo build", "/w/p"));
        assert!(spec.mounts.contains(&"/w/p:/w/p".to_string()));
        assert!(spec.allow_hosts.is_empty(), "network off by default");
    }

    #[test]
    fn read_only_policy_compiles_to_ro_mount() {
        let spec = compile_session_spec(&request(SandboxPolicy::ReadOnly, "ls", "/w/p"));
        assert_eq!(spec.mounts, vec!["/w/p:/w/p:ro".to_string()]);
    }

    #[test]
    fn danger_full_access_mounts_rw_and_opens_network() {
        let spec = compile_session_spec(&request(
            SandboxPolicy::DangerFullAccess,
            "curl example.com",
            "/w",
        ));
        assert_eq!(spec.mounts, vec!["/w:/w".to_string()]);
        assert_eq!(spec.allow_hosts, vec!["*".to_string()]);
    }

    #[test]
    fn differing_specs_do_not_share_sessions() {
        let a = compile_session_spec(&request(workspace_write(), "true", "/a"));
        let b = compile_session_spec(&request(SandboxPolicy::ReadOnly, "true", "/a"));
        assert_ne!(a, b, "read-only and workspace-write are distinct VMs");
        assert_eq!(
            a,
            compile_session_spec(&request(workspace_write(), "other", "/a"))
        );
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn capabilities_report_microvm_and_host_policy_network() {
        let backend = GondolinBackend::new();
        assert_eq!(backend.id(), "gondolin");
        assert_eq!(backend.capabilities().fs_isolation, FsIsolation::MicroVm);
        assert_eq!(backend.capabilities().network, NetworkControl::HostPolicy);
        assert!(!backend.capabilities().requires_linux_host);
        assert!(!backend.capabilities().interactive);
        assert!(backend.task_plane().is_some());
    }

    #[test]
    fn denial_classification_matches_read_only_dialect() {
        let backend = GondolinBackend::new();
        assert!(
            backend
                .classify_denial(1, "touch: /etc/x: Read-only file system")
                .is_some()
        );
        assert!(backend.classify_denial(1, "Permission denied").is_none());
    }

    /// End-to-end through a real gondolin install, gated on the CLI (and
    /// skipped loudly otherwise — the VM boot also implies the ~200MB
    /// first-use asset download).
    #[test]
    fn gondolin_exec_roundtrip_when_cli_available() {
        let backend = GondolinBackend::new();
        if backend.prepare().is_err() {
            eprintln!("skipping: gondolin CLI not available on PATH");
            return;
        }
        let out = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime")
            .block_on(backend.exec(request(workspace_write(), "printf vm-hello", "/tmp")))
            .expect("exec");
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert!(out.stdout.contains("vm-hello"), "stdout: {}", out.stdout);
        backend.stop().ok();
    }
}
