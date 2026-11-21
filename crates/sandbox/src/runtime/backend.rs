//! Replaced-world sandbox backend seam.
//!
//! Boundary declaration — the two sandbox planes stay explicit. The local
//! OS plane (Seatbelt / Landlock / bwrap via `SandboxManager::prepare` →
//! `ExecEnv`) confines a process that still shares the host kernel and
//! filesystem. A [`SandboxBackend`] *replaces where the command runs*
//! (container, microVM, remote execution service); it is not a stronger
//! version of the local plane and never wraps a local spawn. Routing
//! through a configured backend is total: every `exec_shell` command goes
//! to it, foreground and background alike.
//!
//! Every backend declares what it truthfully enforces via
//! [`SandboxBackendCapabilities`] — the honesty table consumers gate on.
//! A backend must not claim a capability it does not enforce (see the
//! Windows stub's contract for the house rule).
//!
//! Backends come in two shapes:
//! - **one-shot** (e.g. hosted HTTP execution): only `exec` is implemented;
//!   the consumer runs a synchronous request/response per command.
//! - **task-plane** (e.g. the container family): additionally implements
//!   [`SandboxTaskPlane`], so commands join the caller's background-job
//!   machinery (task ids, streaming output, wait, kill) instead of a
//!   one-shot call.
//!
//! Known limitations of this seam: no stdin/TTY write path crosses it
//! (`capabilities.interactive` is the honest flag, false everywhere today);
//! denial classification (`classify_denial`) is best-effort per backend
//! dialect and may return `None` where a denial is indistinguishable from
//! a real error — the raw stderr always rides along regardless.

use std::io::Read;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::SandboxPolicy;

/// Request sent to a sandbox backend execution service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxExecRequest {
    pub cmd: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    pub policy: SandboxPolicy,
}

/// Output from a sandbox backend execution.
#[derive(Debug, Clone)]
pub struct SandboxOutput {
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error from the command.
    pub stderr: String,
    /// Exit code (0 for success).
    pub exit_code: i32,
}

/// The kind of external sandbox backend.
///
/// The closed id set of compiled-in backends. New variants land together
/// with their implementation and the factory arm that constructs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxKind {
    /// No external sandbox — execute commands locally.
    #[serde(rename = "none")]
    None,
    /// Alibaba OpenSandbox (or compatible) remote execution.
    #[serde(rename = "opensandbox")]
    OpenSandbox,
    /// OS containers via the docker CLI (`runc`).
    #[serde(rename = "docker")]
    Docker,
    /// gVisor userspace kernel via docker's `runsc` runtime.
    #[serde(rename = "gvisor")]
    Gvisor,
    /// Firecracker microVMs via Kata Containers' `kata-fc` runtime handler.
    #[serde(rename = "firecracker")]
    Firecracker,
    /// Gondolin microVMs (earendil-works/gondolin, QEMU-based) with
    /// host-side network egress policy.
    #[serde(rename = "gondolin")]
    Gondolin,
}

impl SandboxKind {
    /// Parse a sandbox backend name from config (case-insensitive).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "" => Some(Self::None),
            "opensandbox" | "open-sandbox" | "open_sandbox" => Some(Self::OpenSandbox),
            "docker" => Some(Self::Docker),
            "gvisor" | "runsc" => Some(Self::Gvisor),
            "firecracker" | "kata-fc" | "kata" => Some(Self::Firecracker),
            "gondolin" => Some(Self::Gondolin),
            _ => None,
        }
    }

    /// Human-readable label (matches the accepted config spelling).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OpenSandbox => "opensandbox",
            Self::Docker => "docker",
            Self::Gvisor => "gvisor",
            Self::Firecracker => "firecracker",
            Self::Gondolin => "gondolin",
        }
    }
}

/// What a backend truthfully enforces. The honesty table: consumers gate on
/// these declarations (e.g. refusing tty/stdin for a non-interactive
/// backend) instead of assuming every backend behaves alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxBackendCapabilities {
    /// Filesystem isolation mechanism the backend actually provides.
    pub fs_isolation: FsIsolation,
    /// Network control granularity.
    pub network: NetworkControl,
    /// Whether the backend requires a Linux host (KVM / runtime handlers
    /// not installable under e.g. Docker Desktop on macOS).
    pub requires_linux_host: bool,
    /// Whether interactive use (tty allocation, stdin writes) is supported.
    pub interactive: bool,
}

/// Filesystem isolation mechanism of a replaced-world backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsIsolation {
    /// OS container (namespaces/cgroups); host kernel is shared.
    Container,
    /// Userspace kernel intercepts syscalls (e.g. gVisor runsc).
    UserSpaceKernel,
    /// Hardware-virtualized microVM (e.g. Firecracker, Gondolin).
    MicroVm,
    /// Remote execution service; isolation is the service's contract.
    RemoteService,
}

/// Network control granularity a backend can enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkControl {
    /// No network restriction at all.
    None,
    /// All-or-nothing (e.g. `--network none`); no per-host policy.
    OnOff,
    /// Host-level allow/deny lists enforced by the backend.
    HostPolicy,
}

/// Abstract interface for a replaced-world sandbox backend.
///
/// Implementations route commands to a container runtime, microVM, or
/// remote execution service and return structured output. The trait is
/// `Send + Sync` so it can be stored in an `Arc` and shared across tasks.
#[async_trait]
pub trait SandboxBackend: Send + Sync + std::fmt::Debug {
    /// Stable backend id (e.g. `"opensandbox"`); stamps decision metadata.
    fn id(&self) -> &'static str;

    /// The honesty table for this backend. Must not over-claim.
    fn capabilities(&self) -> SandboxBackendCapabilities;

    /// One-time startup check (daemon reachability, runtime registration,
    /// orphan sweep) or VM boot. Called once at engine build; a loud
    /// failure here must abort engine startup — never degrade to running
    /// commands unconfined. Synchronous: implementations wrap short-lived
    /// control-plane subprocesses or local probes. Default: nothing to
    /// check.
    fn prepare(&self) -> Result<()> {
        Ok(())
    }

    /// Best-effort teardown at engine shutdown (kill residual task
    /// containers/VMs). Synchronous control-plane calls. Default: nothing
    /// to clean up.
    fn stop(&self) -> Result<()> {
        Ok(())
    }

    /// Translate a completed run's exit code / output into "the sandbox
    /// policy denied this" when it did. `None` (the default) means the
    /// backend does not classify denials; the raw output always reaches
    /// the model regardless, so a `None` here only forgoes the hint.
    fn classify_denial(&self, exit_code: i32, stderr: &str) -> Option<String> {
        let _ = (exit_code, stderr);
        None
    }

    /// The background-task plane, when this backend supports it. `None`
    /// (the default) keeps the one-shot `exec`-only contract: the consumer
    /// refuses background/interactive/TTY use honestly.
    fn task_plane(&self) -> Option<&dyn SandboxTaskPlane> {
        None
    }

    /// Execute a shell command to completion and return its output.
    async fn exec(&self, request: SandboxExecRequest) -> Result<SandboxOutput>;
}

/// A freshly spawned backend task: its handle plus readers that yield the
/// task's output streams and reach EOF when the task ends.
pub struct SandboxTaskSpawn {
    /// Backend-local task handle used by the plane's other methods.
    pub task: String,
    /// Task stdout stream; closes at task end.
    pub stdout: Box<dyn Read + Send>,
    /// Task stderr stream, when the backend can separate it (the docker CLI
    /// does through `docker logs`); `None` means stderr rides `stdout`.
    pub stderr: Option<Box<dyn Read + Send>>,
}

/// Terminal status of a backend task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxTaskStatus {
    /// Still executing.
    Running,
    /// Finished with this exit code.
    Exited(i32),
    /// The handle no longer resolves (already cleaned up / never existed).
    Gone,
}

/// Background-task plane of a [`SandboxBackend`].
///
/// Synchronous by design: the sole in-tree consumer (`ShellManager`'s
/// background-job poll loop, driven under its mutex) is synchronous, and
/// implementations wrap short-lived control-plane subprocesses
/// (`docker inspect` / `docker kill`). Long-running output never blocks a
/// plane method — it rides the [`SandboxTaskSpawn`] reader on the caller's
/// reader threads, mirroring the local piped-child architecture.
pub trait SandboxTaskPlane: Send + Sync + std::fmt::Debug {
    /// Start a task for `request` and return its handle plus the combined
    /// output stream. The task runs until it exits, is killed, or the
    /// backend is stopped; there is no lifetime timeout at the plane level
    /// (callers enforce their own deadlines via `kill`).
    fn spawn(&self, request: SandboxExecRequest) -> Result<SandboxTaskSpawn>;

    /// Current status of a task handle.
    fn status(&self, task: &str) -> Result<SandboxTaskStatus>;

    /// Kill a running task. Idempotent: killing an exited task succeeds.
    fn kill(&self, task: &str) -> Result<()>;

    /// Release a finished task's resources (e.g. `docker rm`). Called after
    /// the output stream has been drained; best-effort, errors are logged
    /// by the caller.
    fn cleanup(&self, task: &str) -> Result<()>;
}
