//! Container-family sandbox backend: one engine, three ids.
//!
//! `docker` (OCI default runtime), `gvisor` (docker `--runtime runsc`), and
//! `firecracker` (Kata Containers' `kata-fc` runtime handler, which boots a
//! real Firecracker microVM per container) share this implementation — the
//! argv shape is identical, only the runtime (and the honesty table)
//! differs. Separate ids exist so capability claims and configuration
//! errors name the isolation technology truthfully.
//!
//! Policy compilation reuses the local plane's single-source legislation:
//! [`SandboxPolicy::get_writable_roots`] decides every host-visible mount.
//! The container runs `--read-only`, so host effects are exactly the
//! writable bind mounts (writes elsewhere fail with `Read-only file
//! system`, the container dialect of a sandbox denial). Container-internal
//! writes never exist under confinement; under `danger-full-access` the
//! container filesystem is writable and ephemeral (discarded at exit).
//!
//! Background tasks run `docker run -d` under a `codesmith.engine` label;
//! output streams through a `docker logs -f` follower (separate stdout and
//! stderr readers, like a piped local child), terminal state comes from
//! `docker inspect`, and kill/cleanup are `docker kill` / `docker rm -f`.
//!
//! Known limitations:
//! - network control is all-or-nothing (`--network none`); no per-host
//!   policy, so `[sandbox.network] allow_managed_domains_only` keeps
//!   network off (the shared bridge in `apply_runtime_filesystem_policy`);
//! - interactive use (tty allocation, stdin writes) is not supported —
//!   `docker run -d` cannot accept stdin after detach;
//! - containers have no parent-death signal: a SIGKILLed CLI leaves them
//!   running. `prepare()` sweeps this engine label's leftovers at startup
//!   and `stop()` at engine shutdown; the manager's drop path kills each
//!   live task individually.
//! - denial classification recognizes only `Read-only file system`; a bare
//!   `Permission denied` inside a container may be a real permission error
//!   (image user/ownership) and is deliberately not classified.
//! - the `gvisor` and `firecracker` runtimes require a Linux host with the
//!   runtime registered (Docker Desktop on macOS cannot install custom
//!   runtimes — `prepare()` fails loud there).
//! - `bash` must exist in the image (`bash -lc` runs the command); the
//!   default image is chosen for it.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;

use super::backend::{
    FsIsolation, NetworkControl, SandboxBackend, SandboxBackendCapabilities, SandboxExecRequest,
    SandboxOutput,
};
use codesmith_sandbox::runtime::{
    SandboxPolicy, SandboxTaskPlane, SandboxTaskSpawn, SandboxTaskStatus,
};

/// Default image: needs `bash` for the command shell; users override via
/// `[sandbox.docker] image` for a toolchain-carrying image.
pub const DEFAULT_IMAGE: &str = "ubuntu:24.04";

/// Docker label marking this engine's task containers (orphan sweep).
const ENGINE_LABEL_KEY: &str = "codesmith.engine";

/// Exit code docker reports for a SIGKILLed container (`docker kill`).
const DOCKER_KILL_EXIT_CODE: i32 = 137;

/// The engine shared by the three container ids.
struct ContainerEngine {
    /// OCI runtime override (`None` = docker default, `runc`).
    runtime: Option<&'static str>,
    image: String,
    /// Unique per engine build; labels task containers for orphan sweeping.
    engine_label: String,
    /// Live `docker logs -f` follower processes by task name, so cleanup
    /// can reap them (a dropped `Child` never reaped would zombie).
    followers: Mutex<HashMap<String, Child>>,
}

impl std::fmt::Debug for ContainerEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerEngine")
            .field("runtime", &self.runtime)
            .field("image", &self.image)
            .field("engine_label", &self.engine_label)
            .field("followers", &self.followers.lock().map(|m| m.len()))
            .finish()
    }
}

impl ContainerEngine {
    fn docker_output(&self, args: &[String]) -> Result<std::process::Output> {
        Command::new("docker")
            .args(args)
            .output()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    anyhow!("docker CLI not found on PATH; install Docker or set sandbox_backend = \"none\"")
                } else {
                    anyhow!("failed to run docker: {err}")
                }
            })
            .with_context(|| format!("docker {}", args.first().map(String::as_str).unwrap_or("")))
    }

    /// Functional startup probes (a version check would miss "installed
    /// but the daemon refuses"): daemon reachable, runtime registered,
    /// image present. Then sweep this engine label's leftovers —
    /// containers have no parent-death signal, so a crashed CLI leaves
    /// them behind.
    fn prepare(&self) -> Result<()> {
        let info = self
            .docker_output(&["info".to_string(), "--format".into(), "{{json .}}".into()])
            .context("docker daemon unreachable — start Docker (or install it) or set sandbox_backend = \"none\"")?;
        if !info.status.success() {
            bail!(
                "docker daemon refused `docker info` (exit {}): {} — start Docker or set sandbox_backend = \"none\"",
                info.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&info.stderr).trim()
            );
        }
        if let Some(runtime) = self.runtime {
            let runtimes = self
                .docker_output(&[
                    "info".to_string(),
                    "--format".into(),
                    "{{json .Runtimes}}".into(),
                ])
                .context("failed to query docker runtimes")?;
            let body = String::from_utf8_lossy(&runtimes.stdout);
            if !body.contains(runtime) {
                bail!(
                    "runtime {runtime:?} is not registered with the docker daemon. {RUNTIME_INSTALL_HINT}"
                );
            }
        }
        let inspect = self
            .docker_output(&["image".into(), "inspect".into(), self.image.clone()])
            .with_context(|| {
                format!(
                    "sandbox image {:?} is not present locally; run `docker pull {}` or set \
                     [sandbox.docker] image (images are never pulled implicitly)",
                    self.image, self.image
                )
            })?;
        if !inspect.status.success() {
            bail!(
                "sandbox image {:?} is not present locally; run `docker pull {}` or set \
                 [sandbox.docker] image (images are never pulled implicitly)",
                self.image,
                self.image
            );
        }
        self.sweep_labelled_containers();
        Ok(())
    }

    /// `docker rm -f` every container carrying this engine's label — used
    /// both as the crash-orphan sweep (`prepare`) and the engine-shutdown
    /// teardown (`stop`).
    fn sweep_labelled_containers(&self) {
        let filter = format!("{ENGINE_LABEL_KEY}={}", self.engine_label);
        let listed =
            self.docker_output(&["ps".to_string(), "-aq".into(), "--filter".into(), filter]);
        let Ok(out) = listed else {
            return; // sweep is best-effort; exec failures surface at prepare
        };
        if !out.status.success() {
            return;
        }
        for id in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            let _ = self.docker_output(&["rm".into(), "-f".into(), id.to_string()]);
        }
    }

    /// Full task output (no follow) — used by the one-shot `exec` combo.
    fn task_output(&self, task: &str) -> Result<(String, String)> {
        let mut child = Command::new("docker")
            .args(["logs", task])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn docker logs")?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            let _ = pipe.read_to_string(&mut stdout);
        }
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        let _ = child.wait();
        Ok((stdout, stderr))
    }

    /// One-shot execution on the task plane: spawn, wait out the deadline,
    /// collect, clean up. The single source of argv compilation — the
    /// async `exec` wraps this under `spawn_blocking`.
    fn exec_sync(&self, request: SandboxExecRequest) -> Result<SandboxOutput> {
        if request.stdin.is_some() {
            bail!(
                "the container backend cannot feed stdin to a detached task; pipe input inside the command instead"
            );
        }
        let timeout_ms = request.timeout_ms;
        let spawned = self.spawn(request)?;
        let task = spawned.task;
        // The follower streams are dropped here — output is fetched once
        // below via `docker logs` (full, no follow).
        drop(spawned.stdout);
        drop(spawned.stderr);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            match self.status(&task)? {
                SandboxTaskStatus::Exited(code) => {
                    let (stdout, stderr) = self.task_output(&task)?;
                    self.cleanup(&task).ok();
                    return Ok(SandboxOutput {
                        stdout,
                        stderr,
                        exit_code: code,
                    });
                }
                SandboxTaskStatus::Gone => {
                    bail!("container task {task} vanished before exiting");
                }
                SandboxTaskStatus::Running => {
                    if std::time::Instant::now() >= deadline {
                        let _ = self.kill(&task);
                        let (stdout, stderr) = self.task_output(&task)?;
                        self.cleanup(&task).ok();
                        return Ok(SandboxOutput {
                            stdout,
                            stderr,
                            exit_code: DOCKER_KILL_EXIT_CODE,
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
}

const RUNTIME_INSTALL_HINT: &str = "gVisor (runsc) and Kata (kata-fc) need a Linux host with the \
     runtime installed (Docker Desktop on macOS cannot register custom runtimes) — install the \
     runtime, use sandbox_backend = \"docker\", or set sandbox_backend = \"none\"";

/// Build the `docker run` argument vector for a request. Pure — the unit
/// tests pin the policy compilation.
///
/// Mount legislation (single-sourced with the local plane via
/// `get_writable_roots`): the container is `--read-only`; every writable
/// root from the policy is an rw bind at its own path (host and container
/// see the same path, so host file tools and the container agree), and
/// protected control-plane subpaths are re-mounted `:ro` AFTER their root
/// — docker applies binds in order, a later deeper mount shadows (the
/// bwrap lesson: hardened mounts after the writable binds).
fn build_run_args(
    runtime: Option<&'static str>,
    image: &str,
    engine_label: &str,
    task_name: Option<&str>,
    request: &SandboxExecRequest,
) -> Vec<String> {
    let cwd_display = request.cwd.to_string_lossy().into_owned();
    let mut args: Vec<String> = vec!["run".to_string()];
    if let Some(runtime) = runtime {
        args.push("--runtime".into());
        args.push(runtime.to_string());
    }
    if let Some(name) = task_name {
        // Detached, named, labelled background task (no --rm: the logs stay
        // readable until cleanup removes the container).
        args.push("-d".into());
        args.push("--name".into());
        args.push(name.to_string());
        args.push("--label".into());
        args.push(format!("{ENGINE_LABEL_KEY}={engine_label}"));
    } else {
        // One-shot foreground run: auto-remove at exit.
        args.push("--rm".into());
    }
    match &request.policy {
        // Full access: writable (ephemeral) container fs, default network.
        SandboxPolicy::DangerFullAccess => {
            args.push("-v".into());
            args.push(format!("{cwd_display}:{cwd_display}:rw"));
        }
        // Delegated confinement: the backend only controls the network
        // flag; the workspace is mounted rw as the execution substrate.
        SandboxPolicy::ExternalSandbox { .. } => {
            args.push("-v".into());
            args.push(format!("{cwd_display}:{cwd_display}:rw"));
        }
        // Read-only: whole container fs read-only, workspace mounted for
        // reads, network off.
        SandboxPolicy::ReadOnly => {
            args.push("--read-only".into());
            args.push("-v".into());
            args.push(format!("{cwd_display}:{cwd_display}:ro"));
        }
        SandboxPolicy::WorkspaceWrite { .. } => {
            args.push("--read-only".into());
            for root in request.policy.get_writable_roots(&request.cwd) {
                let root_display = root.root.to_string_lossy().into_owned();
                args.push("-v".into());
                args.push(format!("{root_display}:{root_display}:rw"));
                for subpath in root.read_only_subpaths {
                    let sub_display = subpath.to_string_lossy().into_owned();
                    args.push("-v".into());
                    args.push(format!("{sub_display}:{sub_display}:ro"));
                }
            }
        }
    }
    if !request.policy.has_network_access() {
        args.push("--network".into());
        args.push("none".into());
    }
    args.push("-w".into());
    args.push(cwd_display);
    for (key, value) in &request.env {
        args.push("-e".into());
        args.push(format!("{key}={value}"));
    }
    args.push("--".into());
    args.push(image.to_string());
    args.push("bash".into());
    args.push("-lc".into());
    args.push(request.cmd.clone());
    args
}

/// One container backend: `id` names the isolation technology, `runtime`
/// is the OCI runtime it pins.
pub struct ContainerCliBackend {
    id: &'static str,
    runtime: Option<&'static str>,
    fs_isolation: FsIsolation,
    requires_linux_host: bool,
    engine: Arc<ContainerEngine>,
}

impl std::fmt::Debug for ContainerCliBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerCliBackend")
            .field("id", &self.id)
            .field("runtime", &self.runtime)
            .field("image", &self.engine.image)
            .field("engine_label", &self.engine.engine_label)
            .finish()
    }
}

impl ContainerCliBackend {
    /// `docker`: OS containers on the daemon's default runtime. Works
    /// wherever the docker CLI and daemon do (including Docker Desktop on
    /// macOS — the "host" is the daemon VM).
    #[must_use]
    pub fn docker(image: String) -> Self {
        Self::new("docker", None, FsIsolation::Container, false, image)
    }

    /// `gvisor`: gVisor's userspace kernel intercepts the container's
    /// syscalls (`--runtime runsc`). Linux host only.
    #[must_use]
    pub fn gvisor(image: String) -> Self {
        Self::new(
            "gvisor",
            Some("runsc"),
            FsIsolation::UserSpaceKernel,
            true,
            image,
        )
    }

    /// `firecracker`: Kata Containers' `kata-fc` handler boots a Firecracker
    /// microVM per container (`--runtime kata-fc`). Linux host with Kata
    /// installed.
    #[must_use]
    pub fn firecracker(image: String) -> Self {
        Self::new(
            "firecracker",
            Some("kata-fc"),
            FsIsolation::MicroVm,
            true,
            image,
        )
    }

    fn new(
        id: &'static str,
        runtime: Option<&'static str>,
        fs_isolation: FsIsolation,
        requires_linux_host: bool,
        image: String,
    ) -> Self {
        Self {
            id,
            runtime,
            fs_isolation,
            requires_linux_host,
            engine: Arc::new(ContainerEngine {
                runtime,
                image,
                engine_label: uuid::Uuid::new_v4().to_string()[..8].to_string(),
                followers: Mutex::new(HashMap::new()),
            }),
        }
    }
}

#[async_trait]
impl SandboxBackend for ContainerCliBackend {
    fn id(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> SandboxBackendCapabilities {
        SandboxBackendCapabilities {
            fs_isolation: self.fs_isolation,
            network: NetworkControl::OnOff,
            requires_linux_host: self.requires_linux_host,
            interactive: false,
        }
    }

    fn prepare(&self) -> Result<()> {
        if self.requires_linux_host && !cfg!(target_os = "linux") {
            bail!(
                "sandbox backend '{}' requires a Linux host. {RUNTIME_INSTALL_HINT}",
                self.id
            );
        }
        self.engine.prepare()
    }

    fn stop(&self) -> Result<()> {
        self.engine.sweep_labelled_containers();
        Ok(())
    }

    fn classify_denial(&self, _exit_code: i32, stderr: &str) -> Option<String> {
        // Conservative by design: only the denial that is unambiguous in a
        // container. A bare `Permission denied` can be a real image
        // ownership error and is deliberately not classified (same
        // discipline as the Landlock dialect note).
        stderr
            .to_ascii_lowercase()
            .contains("read-only file system")
            .then(|| "container policy denied the write (read-only filesystem)".to_string())
    }

    fn task_plane(&self) -> Option<&dyn SandboxTaskPlane> {
        Some(self.engine.as_ref())
    }

    async fn exec(&self, request: SandboxExecRequest) -> Result<SandboxOutput> {
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || engine.exec_sync(request))
            .await
            .context("container exec task failed")?
    }
}

impl SandboxTaskPlane for ContainerEngine {
    fn spawn(&self, request: SandboxExecRequest) -> Result<SandboxTaskSpawn> {
        if request.stdin.is_some() {
            bail!(
                "the container backend cannot feed stdin to a detached task; pipe input inside the command instead"
            );
        }
        let name = format!("codesmith-shell-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let args = build_run_args(
            self.runtime,
            &self.image,
            &self.engine_label,
            Some(&name),
            &request,
        );
        let out = self
            .docker_output(&args)
            .with_context(|| format!("docker run failed for task {name}"))?;
        if !out.status.success() {
            bail!(
                "docker run failed (exit {}): {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        // Follow the task's output. `docker logs -f` routes container
        // stdout/stderr to the CLI's stdout/stderr separately — two reader
        // streams, like a piped local child. It exits when the container
        // stops, which doubles as a completion signal for the poll loop.
        let mut follower = Command::new("docker")
            .args(["logs", "-f", &name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn docker logs follower")?;
        let stdout = follower
            .stdout
            .take()
            .ok_or_else(|| anyhow!("docker logs follower had no stdout"))?;
        let stderr = follower
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
        self.followers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.clone(), follower);
        Ok(SandboxTaskSpawn {
            task: name,
            stdout: Box::new(stdout),
            stderr,
        })
    }

    fn status(&self, task: &str) -> Result<SandboxTaskStatus> {
        let out = self.docker_output(&[
            "inspect".to_string(),
            "-f".into(),
            "{{.State.Status}}:{{if .State.Running}}-{{else}}{{.State.ExitCode}}{{end}}".into(),
            task.to_string(),
        ])?;
        if !out.status.success() {
            let body = String::from_utf8_lossy(&out.stderr);
            if body.contains("No such object") {
                return Ok(SandboxTaskStatus::Gone);
            }
            bail!("docker inspect failed for {task}: {}", body.trim());
        }
        let line = String::from_utf8_lossy(&out.stdout);
        let line = line.trim();
        match line.split_once(':') {
            Some(("running" | "created" | "paused" | "restarting", _)) => {
                Ok(SandboxTaskStatus::Running)
            }
            Some(("exited" | "dead", code)) => {
                Ok(SandboxTaskStatus::Exited(code.parse::<i32>().unwrap_or(-1)))
            }
            _ => bail!("unrecognized docker inspect output: {line:?}"),
        }
    }

    fn kill(&self, task: &str) -> Result<()> {
        let out = self.docker_output(&["kill".to_string(), task.to_string()])?;
        if !out.status.success() {
            let body = String::from_utf8_lossy(&out.stderr);
            // Idempotent: killing a finished task succeeds by contract.
            if body.contains("is not running") || body.contains("No such object") {
                return Ok(());
            }
            bail!("docker kill failed for {task}: {}", body.trim());
        }
        Ok(())
    }

    fn cleanup(&self, task: &str) -> Result<()> {
        // Reap the logs follower (it exits once the container stops; force
        // for the kill path) so it never zombies.
        if let Some(mut follower) = self
            .followers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task)
        {
            let _ = follower.kill();
            let _ = follower.wait();
        }
        let out = self.docker_output(&["rm".to_string(), "-f".into(), task.to_string()])?;
        if !out.status.success() {
            let body = String::from_utf8_lossy(&out.stderr);
            if body.contains("No such object") {
                return Ok(());
            }
            bail!("docker rm failed for {task}: {}", body.trim());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

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

    fn args_contains(args: &[String], needle: &str) -> bool {
        args.iter().any(|arg| arg == needle)
    }

    fn find_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.windows(2)
            .find(|w| w[0] == flag)
            .map(|w| w[1].as_str())
    }

    fn mounts(args: &[String]) -> Vec<&str> {
        args.windows(2)
            .filter(|w| w[0] == "-v")
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn workspace_write_compiles_to_read_only_plus_rw_mounts() {
        let req = request(workspace_write(), "cargo build", "/work/proj");
        let args = build_run_args(
            None,
            "ubuntu:24.04",
            "eng1",
            Some("codesmith-shell-1"),
            &req,
        );
        assert!(args_contains(&args, "--read-only"));
        // cwd mounts at its own path, rw — host file tools and container agree.
        assert!(mounts(&args).contains(&"/work/proj:/work/proj:rw"));
        // network off by default posture
        assert_eq!(find_flag_value(&args, "--network"), Some("none"));
        assert_eq!(find_flag_value(&args, "--runtime"), None);
        assert_eq!(find_flag_value(&args, "--name"), Some("codesmith-shell-1"));
        // label ties the container to this engine for orphan sweeping
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--label" && w[1] == "codesmith.engine=eng1")
        );
        // command tail
        let tail: Vec<&str> = args.iter().map(String::as_str).collect();
        let pos = tail.iter().position(|a| *a == "--").expect("-- separator");
        assert_eq!(
            &tail[pos + 1..],
            &["ubuntu:24.04", "bash", "-lc", "cargo build"]
        );
    }

    #[test]
    fn read_only_mounts_workspace_ro_without_network_flag_duplication() {
        let req = request(SandboxPolicy::ReadOnly, "ls", "/work/proj");
        let args = build_run_args(None, "ubuntu:24.04", "eng1", Some("t"), &req);
        assert!(args_contains(&args, "--read-only"));
        assert!(mounts(&args).contains(&"/work/proj:/work/proj:ro"));
        assert!(!mounts(&args).contains(&"/work/proj:/work/proj:rw"));
        assert_eq!(find_flag_value(&args, "--network"), Some("none"));
    }

    #[test]
    fn danger_full_access_skips_read_only_and_network_none() {
        let req = request(SandboxPolicy::DangerFullAccess, "curl example.com", "/w");
        let args = build_run_args(None, "ubuntu:24.04", "eng1", Some("t"), &req);
        assert!(!args_contains(&args, "--read-only"));
        assert!(find_flag_value(&args, "--network").is_none());
        assert!(mounts(&args).contains(&"/w:/w:rw"));
    }

    #[test]
    fn network_access_grants_default_network() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir: true,
            exclude_slash_tmp: true,
        };
        let req = request(policy, "cargo publish --dry-run", "/w");
        let args = build_run_args(None, "ubuntu:24.04", "eng1", Some("t"), &req);
        assert!(find_flag_value(&args, "--network").is_none());
    }

    #[test]
    fn runtime_presets_pin_their_flag() {
        let req = request(workspace_write(), "true", "/w");
        let gvisor = build_run_args(Some("runsc"), "ubuntu:24.04", "e", Some("t"), &req);
        assert_eq!(find_flag_value(&gvisor, "--runtime"), Some("runsc"));
        let kata = build_run_args(Some("kata-fc"), "ubuntu:24.04", "e", Some("t"), &req);
        assert_eq!(find_flag_value(&kata, "--runtime"), Some("kata-fc"));
    }

    #[test]
    fn env_vars_ride_as_dash_e() {
        let mut req = request(workspace_write(), "true", "/w");
        req.env
            .insert("CARGO_TERM_COLOR".to_string(), "always".to_string());
        let args = build_run_args(None, "ubuntu:24.04", "e", Some("t"), &req);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "CARGO_TERM_COLOR=always")
        );
    }

    #[test]
    fn one_shot_run_uses_rm_instead_of_name() {
        let req = request(workspace_write(), "true", "/w");
        let args = build_run_args(None, "ubuntu:24.04", "e", None, &req);
        assert!(args_contains(&args, "--rm"));
        assert!(find_flag_value(&args, "--name").is_none());
    }

    #[test]
    fn capabilities_are_honest_per_id() {
        let docker = ContainerCliBackend::docker("ubuntu:24.04".into());
        assert_eq!(docker.id(), "docker");
        assert_eq!(docker.capabilities().fs_isolation, FsIsolation::Container);
        assert!(!docker.capabilities().requires_linux_host);
        assert!(docker.task_plane().is_some());

        let gvisor = ContainerCliBackend::gvisor("ubuntu:24.04".into());
        assert_eq!(gvisor.id(), "gvisor");
        assert_eq!(
            gvisor.capabilities().fs_isolation,
            FsIsolation::UserSpaceKernel
        );
        assert!(gvisor.capabilities().requires_linux_host);

        let firecracker = ContainerCliBackend::firecracker("ubuntu:24.04".into());
        assert_eq!(firecracker.id(), "firecracker");
        assert_eq!(
            firecracker.capabilities().fs_isolation,
            FsIsolation::MicroVm
        );
        assert!(firecracker.capabilities().requires_linux_host);

        // All three deny network per-policy only (OnOff), none interactive.
        for backend in [&docker, &gvisor, &firecracker] {
            assert_eq!(backend.capabilities().network, NetworkControl::OnOff);
            assert!(!backend.capabilities().interactive);
        }
    }

    #[test]
    fn denial_classification_is_conservative() {
        let backend = ContainerCliBackend::docker("ubuntu:24.04".into());
        assert!(
            backend
                .classify_denial(1, "touch: /etc/x: Read-only file system")
                .is_some()
        );
        // Bare permission denied stays unclassified: it may be a real
        // image ownership error inside the container.
        assert!(backend.classify_denial(1, "Permission denied").is_none());
    }

    /// End-to-end through the real docker daemon, gated on the daemon
    /// being reachable AND the image being present locally — a missing
    /// image must not auto-pull (and the test says so when it skips).
    #[test]
    fn docker_exec_roundtrip_when_daemon_available() {
        let backend = ContainerCliBackend::docker(DEFAULT_IMAGE.to_string());
        if backend.prepare().is_err() {
            eprintln!("skipping: docker daemon or image {DEFAULT_IMAGE} not available locally");
            return;
        }
        let out = futures_lite_block(backend.exec(request(
            workspace_write(),
            "printf container-hello",
            "/tmp",
        )))
        .expect("exec");
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert!(
            out.stdout.contains("container-hello"),
            "stdout: {}",
            out.stdout
        );
        // Sandbox denial dialect: a write outside the writable roots is
        // denied by the read-only container fs.
        let denied = futures_lite_block(backend.exec(request(
            workspace_write(),
            "touch /etc/probe-denied",
            "/tmp",
        )))
        .expect("exec");
        assert_ne!(denied.exit_code, 0);
        assert!(denied.stderr.contains("Read-only file system"));
        assert!(
            backend
                .classify_denial(denied.exit_code, &denied.stderr)
                .is_some()
        );
        // No labelled containers survive the runs.
        assert!(labelled_containers(&backend).is_empty());
    }

    fn labelled_containers(backend: &ContainerCliBackend) -> Vec<String> {
        let out = backend
            .engine
            .docker_output(&[
                "ps".to_string(),
                "-aq".into(),
                "--filter".into(),
                format!("{ENGINE_LABEL_KEY}={}", backend.engine.engine_label),
            ])
            .expect("docker ps");
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// exec() is async; in a sync test block on a minimal one-shot runtime.
    fn futures_lite_block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    /// Full wiring E2E through the real `ShellManager`: backend-routed
    /// background jobs, settled results, denial classification, and the
    /// kill path — the same path the engine wires per turn.
    #[test]
    fn docker_backend_joins_shell_manager_jobs_end_to_end() {
        use codesmith_agent_runtime::shell_manager::ShellManager;
        use codesmith_agent_runtime::tools::shell_types::ShellStatus;

        let backend = Arc::new(ContainerCliBackend::docker(DEFAULT_IMAGE.to_string()));
        if backend.prepare().is_err() {
            eprintln!("skipping: docker daemon or image {DEFAULT_IMAGE} not available locally");
            return;
        }
        let mut manager = ShellManager::new(std::env::temp_dir());
        manager.set_sandbox_backend(Some(backend));

        // Background job settles with output and the backend id stamped.
        let spawned = manager
            .execute_with_options_env(
                "printf mgr-hello",
                None,
                5_000,
                true,
                None,
                false,
                None,
                HashMap::new(),
            )
            .expect("spawn");
        let task_id = spawned.task_id.expect("task id");
        assert_eq!(spawned.status, ShellStatus::Running);
        assert_eq!(spawned.sandbox_backend.as_deref(), Some("docker"));
        let mut settled = None;
        for _ in 0..100 {
            if !manager.poll_task_running(&task_id).unwrap_or(false) {
                settled = Some(manager.get_output(&task_id, false, 0).expect("settled"));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let settled = settled.expect("job settled");
        assert_eq!(settled.status, ShellStatus::Completed);
        assert_eq!(settled.exit_code, Some(0));
        assert!(settled.stdout.contains("mgr-hello"), "{}", settled.stdout);

        // A write outside the writable roots is denied and classified.
        let denied = manager
            .execute_with_options_env(
                "touch /etc/probe",
                None,
                5_000,
                true,
                None,
                false,
                None,
                HashMap::new(),
            )
            .expect("spawn");
        let denied_id = denied.task_id.expect("task id");
        let mut denied_settled = None;
        for _ in 0..100 {
            if !manager.poll_task_running(&denied_id).unwrap_or(false) {
                denied_settled = Some(manager.get_output(&denied_id, false, 0).expect("settled"));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let denied_settled = denied_settled.expect("denied job settled");
        assert_eq!(denied_settled.status, ShellStatus::Failed);
        assert!(
            denied_settled.sandbox_denied,
            "denial must be classified: {}",
            denied_settled.stderr
        );

        // Kill path: a long-running job dies through the task plane.
        let long = manager
            .execute_with_options_env(
                "sleep 30",
                None,
                60_000,
                true,
                None,
                false,
                None,
                HashMap::new(),
            )
            .expect("spawn");
        let long_id = long.task_id.expect("task id");
        let killed = manager.kill(&long_id).expect("kill");
        assert_eq!(killed.status, ShellStatus::Killed);
    }

    /// `Path::is_absolute` sanity for the mount-spelling assumption.
    #[test]
    fn cwd_mounts_use_absolute_paths() {
        let req = request(workspace_write(), "true", "/work/proj");
        let args = build_run_args(None, "i", "e", Some("t"), &req);
        for mount in mounts(&args) {
            let src = mount.split(':').next().expect("src");
            assert!(Path::new(src).is_absolute(), "mount src {src} not absolute");
        }
    }
}
