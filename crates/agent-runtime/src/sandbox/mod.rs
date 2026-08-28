//! Runtime sandbox configuration and portable policy/backend types.
//!
//! The runtime-config types (`SandboxRuntimeConfig`, `SandboxBackendKind`, …)
//! were extracted from `crates/tui/src/sandbox/runtime.rs`. The portable
//! policy/backend data types (`SandboxPolicy`, `WritableRoot`,
//! `SandboxExecRequest`, `SandboxOutput`, `SandboxKind`, `SandboxBackend`
//! trait) were extracted from `crates/tui/src/sandbox/{policy,backend}.rs` —
//! they carry no platform-coupled state, so they can live here and cross the
//! `Arc<dyn HostServices>` boundary. The command-spec and sandbox-decision
//! types (`CommandSpec`, `ExecEnv`, `SandboxType`, `SandboxDecision`) were
//! extracted from `crates/tui/src/sandbox/{mod,runtime}.rs`; they carry no
//! platform-coupled state and are referenced by the runtime's shell
//! dispatcher. `SandboxManager`, `get_platform_sandbox`,
//! `is_sandbox_available`, and the platform executors (seatbelt / landlock /
//! seccomp / bwrap / windows / process_hardening) were extracted from
//! `crates/tui/src/sandbox/mod.rs` and the per-platform executor files; they
//! drive OS-level sandboxing via `libc` syscalls and are gated with
//! file-local `#![allow(unsafe_code)]` (matching `child_env`).

#![allow(dead_code)]

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::mode::AppMode;

#[cfg(target_os = "macos")]
pub mod seatbelt;

#[cfg(target_os = "linux")]
pub mod landlock;

#[cfg(target_os = "linux")]
pub mod seccomp;

#[cfg(target_os = "linux")]
pub mod bwrap;

#[cfg(target_os = "windows")]
pub mod windows;

pub mod process_hardening;

/// Runtime sandbox configuration after merging legacy top-level keys, the
/// `[sandbox]` table, environment overrides, and per-mode policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRuntimeConfig {
    pub enabled: bool,
    pub fail_if_unavailable: bool,
    pub enabled_platforms: Vec<String>,
    pub excluded_commands: Vec<String>,
    pub auto_allow_bash_if_sandboxed: bool,
    pub prefer_bwrap: bool,
    pub backend: SandboxBackendKind,
    pub filesystem: SandboxFilesystemConfig,
    pub network: SandboxNetworkConfig,
}

impl Default for SandboxRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fail_if_unavailable: false,
            enabled_platforms: Vec::new(),
            excluded_commands: Vec::new(),
            auto_allow_bash_if_sandboxed: false,
            prefer_bwrap: false,
            backend: SandboxBackendKind::Local,
            filesystem: SandboxFilesystemConfig::default(),
            network: SandboxNetworkConfig::default(),
        }
    }
}

impl SandboxRuntimeConfig {
    #[must_use]
    pub fn platform_enabled(&self) -> bool {
        if self.enabled_platforms.is_empty() {
            return true;
        }
        let current = current_platform();
        self.enabled_platforms
            .iter()
            .any(|platform| platform.trim().eq_ignore_ascii_case(current))
    }

    #[must_use]
    pub fn command_is_excluded(&self, program: &str, command_line: &str) -> bool {
        self.excluded_commands.iter().any(|entry| {
            let trimmed = entry.trim();
            !trimmed.is_empty()
                && (trimmed.eq_ignore_ascii_case(program)
                    || command_line.trim_start().starts_with(trimmed))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SandboxFilesystemConfig {
    pub mode: Option<String>,
    pub writable_roots: Vec<PathBuf>,
    pub allow_read: Vec<PathBuf>,
    pub deny_read: Vec<PathBuf>,
    pub allow_write: Vec<PathBuf>,
    pub deny_write: Vec<PathBuf>,
    pub exclude_tmpdir: Option<bool>,
    pub exclude_slash_tmp: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SandboxNetworkConfig {
    pub enabled: Option<bool>,
    pub allow_managed_domains_only: bool,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackendKind {
    #[default]
    Local,
    OpenSandbox,
}

impl SandboxBackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::OpenSandbox => "opensandbox",
        }
    }
}

#[must_use]
pub fn current_platform() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "unknown"
    }
}

#[must_use]
pub fn managed_domains() -> BTreeSet<String> {
    [
        "api.deepseek.com",
        "chat.deepseek.com",
        "deepseek.com",
        "api.anthropic.com",
        "api.openai.com",
        "openrouter.ai",
        "api.tavily.com",
        "api.bochaai.com",
        "metaso.cn",
        "www.googleapis.com",
        "github.com",
        "raw.githubusercontent.com",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[must_use]
pub fn is_managed_domain(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    managed_domains()
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

// ---------------------------------------------------------------------------
// Portable sandbox policy types (extracted from tui sandbox/policy.rs)
// ---------------------------------------------------------------------------

/// Determines execution restrictions for shell commands.
///
/// The sandbox policy controls filesystem access, network access, and other
/// system resources for executed commands. Choose the most restrictive policy
/// that still allows your command to function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SandboxPolicy {
    /// No restrictions whatsoever. Use with extreme caution.
    #[serde(rename = "danger-full-access")]
    DangerFullAccess,
    /// Read-only access to the entire filesystem.
    #[serde(rename = "read-only")]
    ReadOnly,
    /// Indicates the process is already running in an external sandbox.
    #[serde(rename = "external-sandbox")]
    ExternalSandbox {
        /// Whether network access is allowed in the external sandbox.
        #[serde(default)]
        network_access: bool,
    },
    /// Read-only filesystem access plus write access to specified directories.
    #[serde(rename = "workspace-write")]
    WorkspaceWrite {
        /// Additional directories where writes are allowed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        writable_roots: Vec<PathBuf>,
        /// Whether outbound network connections are permitted.
        #[serde(default)]
        network_access: bool,
        /// Exclude TMPDIR from writable paths.
        #[serde(default)]
        exclude_tmpdir: bool,
        /// Exclude /tmp from writable paths.
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

impl Default for SandboxPolicy {
    /// Returns the default policy: workspace-write with no extra roots and no network.
    fn default() -> Self {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: false,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        }
    }
}

/// Pick the sandbox policy that gates shell commands for a given UI mode.
///
/// - **Plan** (#1077): `ReadOnly` — no writes, no network. The previous
///   `WorkspaceWrite` policy let `python -c "open('f','w').write('x')"` mutate
///   files inside the workspace because it whitelisted the workspace as
///   writable. Plan mode is investigation only; if the user wants to change
///   files they should switch to Agent.
/// - **Coordinator**: `ReadOnly` — same rationale as Plan; the coordinator
///   cannot directly execute or write, so no sandbox writes needed.
/// - **Agent**: `WorkspaceWrite` with workspace as writable root and network
///   on. Approval flow gates risky individual commands; the sandbox handles
///   the rest. Network is allowed because cargo / npm / curl-style commands
///   are normal during agent work and DNS-deny breaks them silently.
/// - **YOLO**: `DangerFullAccess` — explicit no-guardrails contract.
pub fn sandbox_policy_for_mode(mode: AppMode, workspace: &Path) -> SandboxPolicy {
    match mode {
        AppMode::Plan | AppMode::Coordinator => SandboxPolicy::ReadOnly,
        AppMode::Agent => SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![workspace.to_path_buf()],
            network_access: true,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        },
        AppMode::Yolo => SandboxPolicy::DangerFullAccess,
    }
}

impl SandboxPolicy {
    /// Stable policy name for metadata and backend requests.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            SandboxPolicy::DangerFullAccess => "danger-full-access",
            SandboxPolicy::ReadOnly => "read-only",
            SandboxPolicy::ExternalSandbox { .. } => "external-sandbox",
            SandboxPolicy::WorkspaceWrite { .. } => "workspace-write",
        }
    }

    /// Create a workspace-write policy with network access enabled.
    pub fn workspace_with_network() -> Self {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        }
    }

    /// Create a workspace-write policy with additional writable directories.
    pub fn workspace_with_roots(roots: Vec<PathBuf>, network: bool) -> Self {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: roots,
            network_access: network,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        }
    }

    /// Returns true if the policy allows reading any file on the filesystem.
    pub fn has_full_disk_read_access() -> bool {
        // All current policies allow full disk read access
        true
    }

    /// Returns true if the policy allows writing to any file on the filesystem.
    pub fn has_full_disk_write_access(&self) -> bool {
        matches!(
            self,
            SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. }
        )
    }

    /// Returns true if the policy allows outbound network connections.
    pub fn has_network_access(&self) -> bool {
        match self {
            SandboxPolicy::DangerFullAccess => true,
            SandboxPolicy::ReadOnly => false,
            SandboxPolicy::ExternalSandbox { network_access }
            | SandboxPolicy::WorkspaceWrite { network_access, .. } => *network_access,
        }
    }

    /// Returns true if the sandbox should be applied (not bypassed).
    pub fn should_sandbox(&self) -> bool {
        !matches!(
            self,
            SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. }
        )
    }

    /// Get the list of writable roots for this policy.
    ///
    /// This includes:
    /// - The current working directory
    /// - Any explicitly specified `writable_roots`
    /// - /tmp (unless excluded)
    /// - TMPDIR (unless excluded)
    ///
    /// For policies with full write access, returns an empty vec since
    /// there's no need to enumerate specific paths.
    pub fn get_writable_roots(&self, cwd: &Path) -> Vec<WritableRoot> {
        match self {
            // Full write access or read-only - no enumeration needed
            SandboxPolicy::DangerFullAccess
            | SandboxPolicy::ExternalSandbox { .. }
            | SandboxPolicy::ReadOnly => vec![],

            // Workspace write - enumerate all writable paths
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                exclude_tmpdir,
                exclude_slash_tmp,
                ..
            } => {
                let mut roots: Vec<PathBuf> = writable_roots.clone();

                // Add the current working directory
                if let Ok(canonical_cwd) = cwd.canonicalize() {
                    roots.push(canonical_cwd);
                } else {
                    roots.push(cwd.to_path_buf());
                }

                // Add /tmp unless excluded
                if !exclude_slash_tmp && let Ok(tmp) = Path::new("/tmp").canonicalize() {
                    roots.push(tmp);
                }

                // Add TMPDIR unless excluded
                if !exclude_tmpdir
                    && let Ok(tmpdir) = std::env::var("TMPDIR")
                    && let Ok(canonical) = Path::new(&tmpdir).canonicalize()
                {
                    roots.push(canonical);
                }

                // Convert to WritableRoot with read-only subpaths
                roots
                    .into_iter()
                    .map(|root| {
                        let read_only_subpaths = protected_control_plane_subpaths(&root);

                        WritableRoot {
                            root,
                            read_only_subpaths,
                        }
                    })
                    .collect()
            }
        }
    }
}

fn protected_control_plane_subpaths(root: &Path) -> Vec<PathBuf> {
    const PROTECTED_DIRS: &[&str] = &[".codesmith", ".claude", ".opencode", ".cursor", "skills"];
    const PROTECTED_NESTED_DIRS: &[&[&str]] = &[&[".agents", "skills"]];
    const PROTECTED_FILES: &[&[&str]] = &[
        &[".codesmith", "config.toml"],
        &[".deepseek", "config.toml"],
        &[".deepseek", "mcp.json"],
        &[".codesmith", "mcp.json"],
        &["CLAUDE.md"],
        &["AGENTS.md"],
        &[".cursorrules"],
    ];

    let mut protected = Vec::new();
    for name in PROTECTED_DIRS {
        let path = root.join(name);
        if path.exists() {
            protected.push(path);
        }
    }
    for parts in PROTECTED_NESTED_DIRS {
        let path = parts
            .iter()
            .fold(root.to_path_buf(), |acc, part| acc.join(part));
        if path.exists() {
            protected.push(path);
        }
    }
    for parts in PROTECTED_FILES {
        let path = parts
            .iter()
            .fold(root.to_path_buf(), |acc, part| acc.join(part));
        if path.exists() {
            protected.push(path);
        }
    }
    protected
}

/// A directory tree where writes are allowed, with optional read-only subpaths.
///
/// This allows fine-grained control like "allow writes to /project but not /project/.deepseek".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritableRoot {
    /// The root directory where writes are allowed.
    pub root: PathBuf,
    /// Subdirectories within root that should remain read-only.
    pub read_only_subpaths: Vec<PathBuf>,
}

impl WritableRoot {
    /// Create a new writable root with no read-only exceptions.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            read_only_subpaths: vec![],
        }
    }

    /// Create a writable root with specific read-only subpaths.
    pub fn with_exceptions(root: PathBuf, read_only: Vec<PathBuf>) -> Self {
        Self {
            root,
            read_only_subpaths: read_only,
        }
    }

    /// Check if a path is writable under this root.
    ///
    /// Returns true if the path is under the root and not under any read-only subpath.
    pub fn is_path_writable(&self, path: &Path) -> bool {
        // Must be under the root
        if !path.starts_with(&self.root) {
            return false;
        }

        // Must not be under any read-only subpath
        for subpath in &self.read_only_subpaths {
            if path.starts_with(subpath) {
                return false;
            }
        }

        true
    }
}

// ---------------------------------------------------------------------------
// Portable sandbox backend types (extracted from tui sandbox/backend.rs)
// ---------------------------------------------------------------------------

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxKind {
    /// No external sandbox — execute commands locally.
    None,
    /// Alibaba OpenSandbox remote execution.
    OpenSandbox,
}

impl SandboxKind {
    /// Parse a sandbox backend name from config (case-insensitive).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "" => Some(Self::None),
            "opensandbox" | "open-sandbox" | "open_sandbox" => Some(Self::OpenSandbox),
            _ => None,
        }
    }

    /// Human-readable label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OpenSandbox => "opensandbox",
        }
    }
}

/// Abstract interface for an external sandbox backend.
///
/// Implementations send commands to a remote execution environment and return
/// structured output. The trait is `Send + Sync` so it can be stored in an
/// `Arc` and shared across async tasks.
#[async_trait]
pub trait SandboxBackend: Send + Sync + std::fmt::Debug {
    /// Execute a shell command and return its output.
    async fn exec(&self, request: SandboxExecRequest) -> Result<SandboxOutput>;
}

// ---------------------------------------------------------------------------
// Command spec, execution env, and sandbox decision
// (extracted from tui sandbox/{mod,runtime}.rs)
// ---------------------------------------------------------------------------

/// Specification for a command to be executed, potentially within a sandbox.
///
/// This struct captures all the information needed to execute a command:
/// the program and arguments, working directory, environment variables,
/// timeout, and sandbox policy.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// The program to execute (e.g., "sh", "python", "cargo").
    pub program: String,

    /// Arguments to pass to the program.
    pub args: Vec<String>,

    /// Working directory for the command.
    pub cwd: PathBuf,

    /// Additional environment variables to set.
    pub env: HashMap<String, String>,

    /// Maximum execution time before the command is killed.
    pub timeout: Duration,

    /// Sandbox policy controlling resource access.
    pub sandbox_policy: SandboxPolicy,

    /// Optional justification for why this command needs to run.
    /// Used for logging and audit purposes.
    pub justification: Option<String>,
}

impl CommandSpec {
    /// Create a `CommandSpec` for running a shell command via the platform shell.
    pub fn shell(command: &str, cwd: PathBuf, timeout: Duration) -> Self {
        let dispatcher = crate::shell_dispatcher::global_dispatcher();

        #[cfg(windows)]
        let (program, args) = {
            // Force UTF-8 output. cmd.exe uses chcp; PowerShell sets the
            // console output encoding directly. See issue #982.
            let kind = dispatcher.kind();
            let cmd = if matches!(
                kind,
                crate::shell_dispatcher::ShellKind::Pwsh
                    | crate::shell_dispatcher::ShellKind::WindowsPowerShell
            ) {
                format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {command}")
            } else if matches!(kind, crate::shell_dispatcher::ShellKind::Cmd) {
                format!("chcp 65001 >NUL & {command}")
            } else {
                command.to_string()
            };
            dispatcher.build_command_parts(&cmd)
        };
        #[cfg(not(windows))]
        let (program, args) = dispatcher.build_command_parts(command);

        Self {
            program,
            args,
            cwd,
            env: HashMap::new(),
            timeout,
            sandbox_policy: SandboxPolicy::default(),
            justification: None,
        }
    }

    /// Create a `CommandSpec` for running a program directly.
    pub fn program(program: &str, args: Vec<String>, cwd: PathBuf, timeout: Duration) -> Self {
        Self {
            program: program.to_string(),
            args,
            cwd,
            env: HashMap::new(),
            timeout,
            sandbox_policy: SandboxPolicy::default(),
            justification: None,
        }
    }

    /// Set the sandbox policy for this command.
    pub fn with_policy(mut self, policy: SandboxPolicy) -> Self {
        self.sandbox_policy = policy;
        self
    }

    /// Add environment variables for this command.
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Add a single environment variable.
    pub fn with_env_var(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    /// Set a justification for this command (for logging/audit).
    pub fn with_justification(mut self, justification: &str) -> Self {
        self.justification = Some(justification.to_string());
        self
    }

    /// Get the original command as a single string (for display).
    pub fn display_command(&self) -> String {
        if self.args.len() == 2
            && self.args[0] == "-c"
            && matches!(
                self.program.as_str(),
                "sh" | "bash" | "/bin/sh" | "/bin/bash" | "/usr/bin/sh" | "/usr/bin/bash"
            )
        {
            // For shell commands, show the actual command
            self.args[1].clone()
        } else if self.args.len() == 2
            && self.args[0] == "-c"
            && !self.program.eq_ignore_ascii_case("cmd")
            && !self.program.eq_ignore_ascii_case("pwsh")
            && !self.program.eq_ignore_ascii_case("pwsh.exe")
            && !self.program.eq_ignore_ascii_case("powershell")
            && !self.program.eq_ignore_ascii_case("powershell.exe")
        {
            self.args[1].clone()
        } else if self.program.eq_ignore_ascii_case("cmd")
            && self.args.len() == 2
            && self.args[0].eq_ignore_ascii_case("/C")
        {
            // Strip the `chcp 65001 >NUL & ` prefix we add on Windows for
            // UTF-8 output (issue #982).
            let raw = &self.args[1];
            raw.strip_prefix("chcp 65001 >NUL & ")
                .unwrap_or(raw)
                .to_string()
        } else if {
            let program = self.program.to_ascii_lowercase();
            program == "pwsh"
                || program == "pwsh.exe"
                || program == "powershell"
                || program == "powershell.exe"
        } && self.args.len() >= 3
            && self.args[0].eq_ignore_ascii_case("-NoProfile")
            && self.args[1].eq_ignore_ascii_case("-Command")
        {
            // Strip the PowerShell encoding prefix.
            let raw = &self.args[2];
            raw.strip_prefix("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; ")
                .unwrap_or(raw)
                .to_string()
        } else {
            // For other commands, join program and args
            let mut parts = vec![self.program.clone()];
            parts.extend(self.args.clone());
            parts.join(" ")
        }
    }
}

/// The type of sandbox being used for execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxType {
    /// No sandboxing - command runs with full permissions.
    #[default]
    None,

    /// macOS Seatbelt (sandbox-exec) sandboxing.
    #[cfg(target_os = "macos")]
    MacosSeatbelt,

    /// Linux Landlock sandboxing (kernel 5.13+).
    #[cfg(target_os = "linux")]
    LinuxLandlock,

    /// Linux Bubblewrap mount namespace sandboxing.
    #[cfg(target_os = "linux")]
    LinuxBwrap,

    /// Windows process-containment helper.
    ///
    /// Not advertised until a helper enforces Job Object cleanup. This does
    /// not imply filesystem, network, registry, or AppContainer isolation.
    #[cfg(target_os = "windows")]
    Windows,
}

impl std::fmt::Display for SandboxType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxType::None => write!(f, "none"),
            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => write!(f, "macos-seatbelt"),
            #[cfg(target_os = "linux")]
            SandboxType::LinuxLandlock => write!(f, "linux-landlock"),
            #[cfg(target_os = "linux")]
            SandboxType::LinuxBwrap => write!(f, "linux-bwrap"),
            #[cfg(target_os = "windows")]
            SandboxType::Windows => write!(f, "windows-sandbox"),
        }
    }
}

/// The execution environment after sandbox transformation.
///
/// This contains the actual command to run (which may include sandbox wrapper
/// commands) and all necessary environment configuration.
#[derive(Debug)]
pub struct ExecEnv {
    /// The full command to execute (may include sandbox wrapper).
    pub command: Vec<String>,

    /// Working directory for execution.
    pub cwd: PathBuf,

    /// Environment variables to set.
    pub env: HashMap<String, String>,

    /// Timeout for the command.
    pub timeout: Duration,

    /// The type of sandbox being used.
    pub sandbox_type: SandboxType,

    /// The original policy (for reference).
    pub policy: SandboxPolicy,
}

impl ExecEnv {
    /// Get the program to execute (first element of command).
    pub fn program(&self) -> &str {
        self.command
            .first()
            .map_or("sh", std::string::String::as_str)
    }

    /// Get the arguments (all elements after the first).
    pub fn args(&self) -> &[String] {
        if self.command.len() > 1 {
            &self.command[1..]
        } else {
            &[]
        }
    }

    /// Check if this execution is sandboxed.
    pub fn is_sandboxed(&self) -> bool {
        !matches!(self.sandbox_type, SandboxType::None)
    }
}

/// The outcome of a sandbox decision for a command.
///
/// Captures whether sandboxing was requested/effective, the backend selected,
/// and whether execution may proceed (fail-closed vs. fallback).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDecision {
    pub sandbox_requested: bool,
    pub sandbox_effective: bool,
    pub sandbox_policy: String,
    pub sandbox_backend: Option<String>,
    pub sandbox_unavailable_reason: Option<String>,
    pub sandbox_fallback_allowed: bool,
    pub sandbox_excluded_command: Option<String>,
    pub sandbox_fail_closed: bool,
}

impl SandboxDecision {
    #[must_use]
    pub fn unsandboxed(policy: &SandboxPolicy) -> Self {
        Self {
            sandbox_requested: false,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: false,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn enforcing(policy: &SandboxPolicy, backend: SandboxType) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: true,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: Some(backend.to_string()),
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: false,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn unavailable(
        policy: &SandboxPolicy,
        reason: impl Into<String>,
        fail_closed: bool,
    ) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: Some(reason.into()),
            sandbox_fallback_allowed: !fail_closed,
            sandbox_excluded_command: None,
            sandbox_fail_closed: fail_closed,
        }
    }

    #[must_use]
    pub fn disabled(policy: &SandboxPolicy, reason: impl Into<String>) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: Some(reason.into()),
            sandbox_fallback_allowed: true,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn excluded(policy: &SandboxPolicy, command: impl Into<String>) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: true,
            sandbox_excluded_command: Some(command.into()),
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn allows_execution(&self) -> bool {
        self.sandbox_effective || !self.sandbox_fail_closed
    }
}

/// Detect what sandbox technology is available on the current platform.
pub fn get_platform_sandbox() -> Option<SandboxType> {
    #[cfg(target_os = "macos")]
    {
        if seatbelt::is_available() {
            return Some(SandboxType::MacosSeatbelt);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if landlock::is_available() {
            return Some(SandboxType::LinuxLandlock);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if windows::is_available() {
            return Some(SandboxType::Windows);
        }
    }

    None
}

/// Check if sandboxing is available on this platform.
pub fn is_sandbox_available() -> bool {
    get_platform_sandbox().is_some()
}

/// Manager for sandbox operations.
///
/// The `SandboxManager` is responsible for:
/// - Detecting available sandbox technologies
/// - Transforming `CommandSpecs` into sandboxed `ExecEnvs`
/// - Detecting sandbox denials from command output
#[derive(Debug, Default)]
pub struct SandboxManager {
    /// Cached sandbox availability check.
    sandbox_available: Option<bool>,

    /// Force a specific sandbox type (for testing).
    #[allow(dead_code)]
    forced_sandbox: Option<SandboxType>,

    /// When true and bwrap is available on Linux, route commands through
    /// bubblewrap instead of Landlock alone (#2184).
    prefer_bwrap: bool,
}

impl SandboxManager {
    /// Create a new `SandboxManager`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new `SandboxManager` with bwrap preference (#2184).
    ///
    /// When `prefer_bwrap` is true and `/usr/bin/bwrap` is present on Linux,
    /// exec_shell commands will be routed through bubblewrap.
    pub fn with_bwrap_preference(prefer_bwrap: bool) -> Self {
        Self {
            prefer_bwrap,
            ..Self::default()
        }
    }

    /// Set the bwrap preference (#2184).
    pub fn set_prefer_bwrap(&mut self, prefer: bool) {
        self.prefer_bwrap = prefer;
    }

    /// Check if sandboxing is available.
    pub fn is_available(&mut self) -> bool {
        if let Some(available) = self.sandbox_available {
            return available;
        }

        let available = is_sandbox_available();
        self.sandbox_available = Some(available);
        available
    }

    /// Select the appropriate sandbox type for the given policy.
    pub fn select_sandbox(&self, policy: &SandboxPolicy) -> SandboxType {
        // If the policy doesn't want sandboxing, return None
        if !policy.should_sandbox() {
            return SandboxType::None;
        }

        // Check for forced sandbox (testing)
        if let Some(forced) = self.forced_sandbox {
            return forced;
        }

        // Use platform default
        #[cfg(target_os = "linux")]
        {
            if self.prefer_bwrap && bwrap::is_available() {
                return SandboxType::LinuxBwrap;
            }
        }
        get_platform_sandbox().unwrap_or(SandboxType::None)
    }

    /// Transform a `CommandSpec` into a sandboxed `ExecEnv`.
    ///
    /// This is the main entry point for sandboxing. It takes a command
    /// specification and returns the actual command to run, which may
    /// include sandbox wrapper commands.
    pub fn prepare(&self, spec: &CommandSpec) -> ExecEnv {
        let sandbox_type = self.select_sandbox(&spec.sandbox_policy);

        match sandbox_type {
            SandboxType::None => Self::prepare_unsandboxed(spec),

            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => Self::prepare_seatbelt(spec),

            #[cfg(target_os = "linux")]
            SandboxType::LinuxLandlock => self.prepare_landlock(spec),

            #[cfg(target_os = "linux")]
            SandboxType::LinuxBwrap => Self::prepare_bwrap(spec),

            #[cfg(target_os = "windows")]
            SandboxType::Windows => Self::prepare_windows(spec),
        }
    }

    pub fn prepare_unsandboxed_for_fallback(spec: &CommandSpec) -> ExecEnv {
        Self::prepare_unsandboxed(spec)
    }

    /// Prepare an unsandboxed execution environment.
    fn prepare_unsandboxed(spec: &CommandSpec) -> ExecEnv {
        let mut command = vec![spec.program.clone()];
        command.extend(spec.args.clone());

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            timeout: spec.timeout,
            sandbox_type: SandboxType::None,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Prepare a Seatbelt-sandboxed execution environment (macOS).
    #[cfg(target_os = "macos")]
    fn prepare_seatbelt(spec: &CommandSpec) -> ExecEnv {
        // Build the original command
        let mut original_command = vec![spec.program.clone()];
        original_command.extend(spec.args.clone());

        // Generate sandbox-exec arguments
        let seatbelt_args =
            seatbelt::create_seatbelt_args(original_command, &spec.sandbox_policy, &spec.cwd);

        // Prepend sandbox-exec to the command
        let mut command = vec![seatbelt::SANDBOX_EXEC_PATH.to_string()];
        command.extend(seatbelt_args);

        // Add sandbox indicator to environment
        let mut env = spec.env.clone();
        env.insert("DEEPSEEK_SANDBOX".to_string(), "seatbelt".to_string());

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env,
            timeout: spec.timeout,
            sandbox_type: SandboxType::MacosSeatbelt,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Prepare a bwrap-sandboxed execution environment (Linux).
    #[cfg(target_os = "linux")]
    fn prepare_bwrap(spec: &CommandSpec) -> ExecEnv {
        let command =
            bwrap::build_bwrap_command(&spec.cwd, &spec.program, &spec.args, &spec.sandbox_policy);

        let mut env = spec.env.clone();
        env.insert("DEEPSEEK_SANDBOX".to_string(), "bwrap".to_string());

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env,
            timeout: spec.timeout,
            sandbox_type: SandboxType::LinuxBwrap,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Prepare a Landlock-sandboxed execution environment (Linux).
    ///
    /// Landlock is currently only advertised when the platform probe succeeds;
    /// this path keeps metadata truthful and does not claim bwrap enforcement.
    #[cfg(target_os = "linux")]
    fn prepare_landlock(&self, spec: &CommandSpec) -> ExecEnv {
        let _ = self;
        // Full Landlock enforcement requires applying rules inside the child
        // process before exec. Until that helper path is wired, report this as
        // a marker only when selected by tests/platform detection and let the
        // higher-level decision layer fail closed when strict enforcement is
        // required.
        let mut command = vec![spec.program.clone()];
        command.extend(spec.args.clone());

        let mut env = spec.env.clone();
        env.insert("DEEPSEEK_SANDBOX".to_string(), "landlock".to_string());

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env,
            timeout: spec.timeout,
            sandbox_type: SandboxType::LinuxLandlock,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Prepare a Windows helper execution environment.
    ///
    /// Windows support is currently not advertised by `get_platform_sandbox`.
    /// This branch only exists for forced tests and future helper wiring.
    /// The first supported helper contract is process-tree containment only;
    /// it must not be presented as filesystem or network isolation.
    #[cfg(target_os = "windows")]
    fn prepare_windows(spec: &CommandSpec) -> ExecEnv {
        let mut command = vec![spec.program.clone()];
        command.extend(spec.args.clone());

        let mut env = spec.env.clone();
        let kind = windows::select_best_kind(&spec.sandbox_policy, &spec.cwd);
        env.insert("DEEPSEEK_SANDBOX".to_string(), format!("windows:{kind}"));
        if !spec.sandbox_policy.has_network_access() {
            env.insert(
                "DEEPSEEK_SANDBOX_BLOCK_NETWORK".to_string(),
                "1".to_string(),
            );
        }

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env,
            timeout: spec.timeout,
            sandbox_type: SandboxType::Windows,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Check if a command failure was due to sandbox denial.
    ///
    /// This helps distinguish between legitimate command failures and
    /// sandbox-blocked operations.
    pub fn was_denied(sandbox_type: SandboxType, exit_code: i32, stderr: &str) -> bool {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = (exit_code, stderr);

        match sandbox_type {
            SandboxType::None => false,

            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => seatbelt::detect_denial(exit_code, stderr),

            #[cfg(target_os = "linux")]
            SandboxType::LinuxLandlock | SandboxType::LinuxBwrap => {
                landlock::detect_denial(exit_code, stderr)
            }

            #[cfg(target_os = "windows")]
            SandboxType::Windows => windows::detect_denial(exit_code, stderr),
        }
    }

    /// Get a human-readable description of why a command was blocked.
    pub fn denial_message(sandbox_type: SandboxType, stderr: &str) -> String {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = stderr;

        match sandbox_type {
            SandboxType::None => "Command failed (no sandbox)".to_string(),

            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => {
                if stderr.contains("file-write") {
                    "Sandbox blocked write access. The command tried to write to a protected location.".to_string()
                } else if stderr.contains("network") {
                    "Sandbox blocked network access. Enable network_access in sandbox policy if needed.".to_string()
                } else {
                    format!(
                        "Sandbox blocked operation: {}",
                        stderr.lines().next().unwrap_or("unknown")
                    )
                }
            }

            #[cfg(target_os = "linux")]
            SandboxType::LinuxLandlock | SandboxType::LinuxBwrap => {
                // Seccomp patterns checked first because they are more specific (#2182).
                if stderr.contains("Bad system call")
                    || stderr.contains("bad system call")
                    || stderr.contains("SIGSYS")
                    || stderr.contains("seccomp")
                {
                    "Seccomp blocked a disallowed system call (e.g., ptrace, mount, kexec)."
                        .to_string()
                } else if stderr.contains("Permission denied") {
                    "Landlock blocked access. The command tried to access a restricted path."
                        .to_string()
                } else {
                    format!(
                        "Landlock blocked operation: {}",
                        stderr.lines().next().unwrap_or("unknown")
                    )
                }
            }

            #[cfg(target_os = "windows")]
            SandboxType::Windows => {
                if stderr.contains("Access is denied") {
                    "Windows sandbox blocked access. The command lacked required privileges."
                        .to_string()
                } else if stderr.contains("network") {
                    "Windows sandbox blocked network access. Enable network_access in policy if needed."
                        .to_string()
                } else {
                    format!(
                        "Windows sandbox blocked operation: {}",
                        stderr.lines().next().unwrap_or("unknown")
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn sandbox_policy_for_mode_plan_is_readonly() {
        let policy = sandbox_policy_for_mode(AppMode::Plan, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::ReadOnly));
    }

    #[test]
    fn sandbox_policy_for_mode_coordinator_is_readonly() {
        let policy = sandbox_policy_for_mode(AppMode::Coordinator, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::ReadOnly));
    }

    #[test]
    fn sandbox_policy_for_mode_agent_is_workspace_write() {
        let policy = sandbox_policy_for_mode(AppMode::Agent, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::WorkspaceWrite { .. }));
        if let SandboxPolicy::WorkspaceWrite {
            writable_roots,
            network_access,
            ..
        } = policy
        {
            assert_eq!(writable_roots, vec![PathBuf::from("/repo")]);
            assert!(network_access);
        }
    }

    #[test]
    fn sandbox_policy_for_mode_yolo_is_danger_full_access() {
        let policy = sandbox_policy_for_mode(AppMode::Yolo, Path::new("/repo"));
        assert!(matches!(policy, SandboxPolicy::DangerFullAccess));
    }

    #[test]
    fn test_command_spec_shell() {
        let spec = CommandSpec::shell("echo hello", PathBuf::from("/tmp"), Duration::from_secs(30));

        // Program and args depend on the detected shell.
        assert!(!spec.program.is_empty(), "program must not be empty");
        assert!(!spec.args.is_empty(), "args must not be empty");
        assert_eq!(spec.display_command(), "echo hello");
    }

    #[test]
    fn test_command_spec_shell_custom_posix_path_display() {
        let spec = CommandSpec {
            program: "/bin/zsh".to_string(),
            args: vec!["-c".to_string(), "echo hello".to_string()],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            sandbox_policy: SandboxPolicy::default(),
            justification: None,
        };

        assert_eq!(spec.display_command(), "echo hello");
    }

    #[test]
    fn test_command_spec_shell_quoted_arg_not_split() {
        // Regression for #1691: a `-m` message containing spaces must remain a
        // single, unsplit argv entry. The shell command string is passed
        // verbatim as ONE argument (`sh -c <cmd>` / `cmd /C <payload>`); we
        // must never tokenize it ourselves into `feat:` / `complete` /
        // `sub-pages"`.
        let cmd = r#"git commit -m "feat: complete sub-pages""#;
        let spec = CommandSpec::shell(cmd, PathBuf::from("/tmp"), Duration::from_secs(30));

        let dispatcher = crate::shell_dispatcher::global_dispatcher();
        assert_eq!(spec.program, dispatcher.kind().binary());
        if dispatcher.kind().is_powershell() {
            assert_eq!(
                spec.args,
                vec![
                    dispatcher.kind().command_flag().to_string(),
                    "-Command".to_string(),
                    format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {cmd}")
                ]
            );
        } else {
            let expected = if matches!(dispatcher.kind(), crate::shell_dispatcher::ShellKind::Cmd) {
                vec!["/C".to_string(), format!("chcp 65001 >NUL & {cmd}")]
            } else {
                vec![
                    dispatcher.kind().command_flag().to_string(),
                    cmd.to_string(),
                ]
            };
            assert_eq!(spec.args, expected);
            // The quoted message is intact in a single argv slot — shell `-c`
            // performs POSIX tokenization, yielding the correct argv:
            // ["git","commit","-m","feat: complete sub-pages"].
            assert_eq!(spec.args.len(), 2);
            assert!(spec.args[1].contains(r#""feat: complete sub-pages""#));
        }
        assert_eq!(spec.display_command(), cmd);
    }

    #[test]
    fn test_command_spec_program() {
        let spec = CommandSpec::program(
            "cargo",
            vec!["build".to_string(), "--release".to_string()],
            PathBuf::from("/project"),
            Duration::from_secs(300),
        );

        assert_eq!(spec.program, "cargo");
        assert_eq!(spec.display_command(), "cargo build --release");
    }

    #[test]
    fn test_command_spec_builder() {
        let spec = CommandSpec::shell("test", PathBuf::from("."), Duration::from_secs(10))
            .with_policy(SandboxPolicy::ReadOnly)
            .with_env_var("FOO", "bar")
            .with_justification("Testing");

        assert!(matches!(spec.sandbox_policy, SandboxPolicy::ReadOnly));
        assert_eq!(spec.env.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(spec.justification, Some("Testing".to_string()));
    }

    #[test]
    fn test_sandbox_manager_new() {
        let manager = SandboxManager::new();
        assert!(manager.sandbox_available.is_none());
    }

    #[test]
    fn test_sandbox_manager_select_sandbox() {
        let manager = SandboxManager::new();

        // DangerFullAccess should never sandbox
        let no_sandbox = manager.select_sandbox(&SandboxPolicy::DangerFullAccess);
        assert_eq!(no_sandbox, SandboxType::None);

        // ExternalSandbox should never sandbox
        let external = manager.select_sandbox(&SandboxPolicy::ExternalSandbox {
            network_access: true,
        });
        assert_eq!(external, SandboxType::None);
    }

    #[test]
    fn test_prepare_unsandboxed() {
        let manager = SandboxManager::new();
        let spec = CommandSpec::shell("echo test", PathBuf::from("/tmp"), Duration::from_secs(30))
            .with_policy(SandboxPolicy::DangerFullAccess);

        let env = manager.prepare(&spec);
        let dispatcher = crate::shell_dispatcher::global_dispatcher();

        assert_eq!(env.sandbox_type, SandboxType::None);
        if dispatcher.kind().is_powershell() {
            assert_eq!(
                env.command,
                vec![
                    dispatcher.kind().binary().to_string(),
                    dispatcher.kind().command_flag().to_string(),
                    "-Command".to_string(),
                    "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; echo test"
                        .to_string(),
                ]
            );
        } else if matches!(dispatcher.kind(), crate::shell_dispatcher::ShellKind::Cmd) {
            assert_eq!(
                env.command,
                vec![
                    dispatcher.kind().binary().to_string(),
                    "/C".to_string(),
                    "chcp 65001 >NUL & echo test".to_string(),
                ]
            );
        } else {
            assert_eq!(
                env.command,
                vec![
                    dispatcher.kind().binary().to_string(),
                    dispatcher.kind().command_flag().to_string(),
                    "echo test".to_string(),
                ]
            );
        }
        assert!(!env.is_sandboxed());
    }

    #[test]
    fn test_exec_env_helpers() {
        let env = ExecEnv {
            command: vec![
                "sandbox-exec".to_string(),
                "-p".to_string(),
                "policy".to_string(),
                "--".to_string(),
                "echo".to_string(),
                "hello".to_string(),
            ],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            sandbox_type: SandboxType::None,
            policy: SandboxPolicy::default(),
        };

        assert_eq!(env.program(), "sandbox-exec");
        assert_eq!(env.args().len(), 5);
    }

    #[test]
    fn test_sandbox_type_display() {
        assert_eq!(format!("{}", SandboxType::None), "none");

        #[cfg(target_os = "macos")]
        assert_eq!(format!("{}", SandboxType::MacosSeatbelt), "macos-seatbelt");
    }

    // ── Parity tests (#2187) ──────────────────────────────────────────────

    #[test]
    fn test_parity_platform_sandbox_detection() {
        let sandbox_type = get_platform_sandbox();
        let available = is_sandbox_available();
        if available {
            assert!(sandbox_type.is_some());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parity_macos_seatbelt_available() {
        let st = get_platform_sandbox();
        assert!(matches!(st, Some(SandboxType::MacosSeatbelt)));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_parity_linux_sandbox_available_reports_available_backend() {
        let st = get_platform_sandbox();
        assert!(matches!(st, Some(SandboxType::LinuxLandlock)) || st.is_none());
    }

    #[test]
    fn test_parity_denial_zero_exit_never_denied() {
        assert!(!SandboxManager::was_denied(
            SandboxType::None,
            0,
            "anything"
        ));
        #[cfg(target_os = "macos")]
        assert!(!SandboxManager::was_denied(
            SandboxType::MacosSeatbelt,
            0,
            ""
        ));
        #[cfg(target_os = "linux")]
        assert!(!SandboxManager::was_denied(
            SandboxType::LinuxLandlock,
            0,
            ""
        ));
        #[cfg(target_os = "windows")]
        assert!(!SandboxManager::was_denied(SandboxType::Windows, 0, ""));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_parity_seccomp_sigsys_detected() {
        assert!(SandboxManager::was_denied(
            SandboxType::LinuxLandlock,
            31,
            ""
        ));
        assert!(SandboxManager::was_denied(
            SandboxType::LinuxLandlock,
            1,
            "Bad system call"
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parity_seatbelt_file_write_detected() {
        // Seatbelt patterns use "Sandbox: <cmd> denied <operation>" format.
        assert!(SandboxManager::was_denied(
            SandboxType::MacosSeatbelt,
            1,
            "Sandbox: ls denied file-write*"
        ));
        assert!(SandboxManager::was_denied(
            SandboxType::MacosSeatbelt,
            1,
            "Operation not permitted"
        ));
    }

    #[test]
    fn test_parity_manager_default_no_bwrap() {
        let manager = SandboxManager::default();
        let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
            .with_policy(SandboxPolicy::default());
        let env = manager.prepare(&spec);
        #[cfg(target_os = "linux")]
        {
            let marker = env.env.get("DEEPSEEK_SANDBOX");
            assert!(marker.is_none_or(|v| v != "bwrap"));
        }
        let _ = env;
    }

    #[test]
    fn test_parity_manager_with_bwrap() {
        let manager = SandboxManager::with_bwrap_preference(true);
        let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
            .with_policy(SandboxPolicy::default());
        let env = manager.prepare(&spec);
        #[cfg(target_os = "linux")]
        {
            if crate::sandbox::bwrap::is_available() {
                let marker = env.env.get("DEEPSEEK_SANDBOX");
                assert_eq!(marker.map(String::as_str), Some("bwrap"));
            }
        }
        let _ = env;
    }

    #[test]
    fn test_parity_exec_env_for_all_policies() {
        let manager = SandboxManager::new();
        let policies = [
            SandboxPolicy::DangerFullAccess,
            SandboxPolicy::ReadOnly,
            SandboxPolicy::workspace_with_network(),
            SandboxPolicy::default(),
        ];
        for policy in &policies {
            let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
                .with_policy(policy.clone());
            let env = manager.prepare(&spec);
            assert_eq!(env.policy, *policy);
        }
    }
}
