//! Teammate backend abstraction — pluggable execution targets for spawned
//! teammates.
//!
//! By default teammates run in-process (a supervised tokio task sharing the
//! leader's runtime). When the leader itself runs inside a terminal
//! multiplexer or a capable terminal (`tmux`, iTerm2), teammates can instead
//! be launched in a real pane/tab so the user can watch and interact with
//! each agent independently — mirroring Claude Code's pane-backed swarm.
//!
//! The trait below is implemented by the pane backends (`tmux`, `iterm`).
//! The in-process path stays a free function in `subagent` because it must
//! own the heavy `SubAgentRuntime` by value (the runtime contains
//! `mpsc::Sender`s which make it `!Sync`, so it cannot live behind a
//! `&self` trait object that yields a `Send` future).
//!
//! Detection is environment-driven and cached for the process lifetime —
//! see [`detect::detect_backend_kind`].
#![allow(dead_code)]

pub mod detect;
pub mod iterm;
pub mod tmux;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Permission mode for a spawned teammate. Modeled as an enum so invalid
/// values are unrepresentable at the spec boundary — the old free-form
/// `String` let typos or hostile values flow unchecked onto the pane
/// command line, silently degrading to "ask" far downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Auto,
    Ask,
}

impl PermissionMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
        }
    }

    /// Parse the CLI/env spelling (`--permission-mode`). Unknown values are
    /// `None` so the caller can warn instead of silently degrading.
    #[must_use]
    pub fn from_cli(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

/// Backend-agnostic description of a teammate to spawn.
///
/// Carries only portable fields — everything a pane backend needs to launch
/// `codesmith team-teammate` in a new pane, and everything the in-process
/// path needs to construct a `TeammateRuntime`. Leader-side team-context
/// registration (inserting the teammate into the in-memory `TeamContext`
/// and the on-disk team file) is performed by the caller before invoking
/// the backend, so backends can stay stateless.
#[derive(Debug, Clone)]
pub struct TeammateSpawnSpec {
    /// Stable teammate id (e.g. `team_agent_<8-hex>`).
    pub agent_id: String,
    /// Human-facing teammate name, unique within the team.
    pub agent_name: String,
    /// Team the teammate belongs to.
    pub team_name: String,
    /// Sub-agent type as a lowercase string (`SubAgentType::as_str`).
    pub agent_type: String,
    /// Initial prompt / objective handed to the teammate.
    pub prompt: String,
    /// Resolved model id, if any.
    pub model: Option<String>,
    /// Working directory for the teammate.
    pub cwd: PathBuf,
    /// Optional narrow tool allowlist.
    pub allowed_tools: Option<Vec<String>>,
    /// Permission mode for the teammate.
    pub permission_mode: PermissionMode,
    /// Extra environment variables to set in a spawned pane process.
    pub env: HashMap<String, String>,
    /// Binary to invoke for pane backends (defaults to `current_exe()`).
    pub codesmith_bin: PathBuf,
}

impl TeammateSpawnSpec {
    /// Build a spec with the given identity/portable fields and sensible
    /// defaults for the rest (`permission_mode = ask`, empty env,
    /// `codesmith_bin` from `current_exe()`).
    #[must_use]
    pub fn new(agent_id: String, agent_name: String, team_name: String) -> Self {
        Self {
            agent_id,
            agent_name,
            team_name,
            agent_type: "team".to_string(),
            prompt: String::new(),
            model: None,
            // Launch-directory default: `new()` has no workspace on hand;
            // callers with better context set `cwd` explicitly after
            // construction.
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            allowed_tools: None,
            permission_mode: PermissionMode::Ask,
            env: HashMap::new(),
            // An unresolvable exe leaves the path EMPTY on purpose: the old
            // fallback to a bare `codesmith` resolved the command word via
            // PATH, where a shadowed entry would run with the leader's
            // privileges. Pane backends fail loud on the empty path
            // (`validate`) instead of spawning an unchecked lookup.
            codesmith_bin: std::env::current_exe().unwrap_or_default(),
        }
    }

    /// Fail loud when the spec cannot be launched as a pane process.
    /// Called by the pane backends (tmux/iTerm); the in-process backend
    /// never execs a binary and skips this. Rejects:
    /// - an empty `codesmith_bin` (`current_exe()` failed; an unchecked
    ///   PATH lookup could run a shadowed binary with the leader's
    ///   privileges),
    /// - a relative `cwd`, which would resolve against the pane shell's
    ///   own working directory and silently defeat the explicit
    ///   `--cwd`/`-c` passing,
    /// - non-UTF-8 paths, which `to_string_lossy` would corrupt into
    ///   quoted-but-nonexistent paths failing at exec with a confusing
    ///   "no such file" error.
    pub(crate) fn validate(&self) -> Result<(), BackendError> {
        if self.codesmith_bin.as_os_str().is_empty() {
            return Err(BackendError::InvalidSpec(
                "codesmith binary could not be resolved (current_exe() failed and no explicit \
                 codesmith_bin was set); refusing an unchecked PATH lookup"
                    .to_string(),
            ));
        }
        if !self.cwd.is_absolute() {
            return Err(BackendError::InvalidSpec(format!(
                "teammate cwd must be absolute (got {:?}); a relative path resolves against \
                 the pane shell's working directory, not the leader's",
                self.cwd
            )));
        }
        for (what, path) in [
            ("codesmith binary", &self.codesmith_bin),
            ("cwd", &self.cwd),
        ] {
            if path.to_str().is_none() {
                return Err(BackendError::InvalidSpec(format!(
                    "teammate {what} path is not valid UTF-8: {path:?}"
                )));
            }
        }
        Ok(())
    }
}

/// A successfully spawned teammate.
#[derive(Debug, Clone)]
pub struct SpawnedTeammate {
    pub agent_id: String,
    pub agent_name: String,
    pub team_name: String,
    pub backend: BackendKind,
    /// Opaque, backend-specific handle for the live teammate.
    pub handle: TeammateHandle,
}

/// Backend-specific handle to a live teammate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeammateHandle {
    /// In-process supervised task. Lifecycle is owned by the leader's
    /// tokio runtime; cleanup runs in the task's drop/finally block.
    InProcess,
    /// `tmux` pane id (e.g. `%5`).
    TmuxPane(String),
    /// iTerm2 session id as returned by the AppleScript `id of session`.
    ITermSession(String),
}

/// Which backend a teammate was spawned through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    InProcess,
    Tmux,
    Iter,
}

impl BackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in_process",
            Self::Tmux => "tmux",
            Self::Iter => "iterm",
        }
    }
}

/// Errors produced by teammate backends.
#[derive(Debug, Error)]
pub enum BackendError {
    /// The leader has no active team runtime attached.
    #[error("team runtime not attached: {0}")]
    NoTeamRuntime(String),
    /// The spawn spec was missing or inconsistent.
    #[error("invalid spawn spec: {0}")]
    InvalidSpec(String),
    /// A teammate with this name already exists in the active team.
    #[error("teammate '{0}' already exists")]
    DuplicateTeammate(String),
    /// The active team does not match the requested team.
    #[error("active team is '{active}', not '{requested}'")]
    TeamMismatch { active: String, requested: String },
    /// Spawning the pane/process failed.
    #[error("backend `{backend}` failed to spawn teammate: {message}")]
    SpawnFailed {
        backend: &'static str,
        message: String,
    },
    /// The backend's runtime (tmux/iTerm) is not available in this context.
    #[error("backend `{backend}` not available: {message}")]
    Unavailable {
        backend: &'static str,
        message: String,
    },
}

impl BackendError {
    /// Convert into a [`crate::tools::spec::ToolError`], preserving the
    /// caller/user-vs-execution distinction: `InvalidSpec`,
    /// `DuplicateTeammate`, and `TeamMismatch` are caller errors (retry
    /// with a new name / the right team), not execution failures, so they
    /// classify as invalid input instead of being flattened into
    /// execution-failed.
    pub(crate) fn into_tool_error(self) -> crate::tools::spec::ToolError {
        match self {
            BackendError::InvalidSpec(_)
            | BackendError::DuplicateTeammate(_)
            | BackendError::TeamMismatch { .. } => {
                crate::tools::spec::ToolError::invalid_input(self.to_string())
            }
            _ => crate::tools::spec::ToolError::execution_failed(self.to_string()),
        }
    }
}

/// Pluggable teammate execution target.
///
/// Implemented by pane backends (`tmux`, `iTerm2`). The in-process path is
/// handled separately by `spawn_team_teammate` in `tools::subagent` because
/// it owns a `!Sync` `SubAgentRuntime` by value.
#[async_trait]
pub trait TeammateBackend: Send + Sync {
    /// Human-facing backend name (`"tmux"`, `"iterm"`).
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Which [`BackendKind`] this implementation represents.
    #[allow(dead_code)]
    fn kind(&self) -> BackendKind;

    /// Spawn a teammate pane/process for `spec`.
    ///
    /// The caller is responsible for leader-side team registration (team
    /// file + in-memory `TeamContext` entry) before invoking this; the
    /// backend only launches the external process.
    async fn spawn(&self, spec: &TeammateSpawnSpec) -> Result<SpawnedTeammate, BackendError>;
}

/// Resolve the [`BackendKind`] selected for this process and construct the
/// corresponding pane backend, if any. Returns `None` for the in-process
/// path (which has no stateless backend object).
///
/// Cached per-process via [`detect::detect_backend_kind`].
#[must_use]
#[allow(dead_code)]
pub fn selected_pane_backend() -> Option<Box<dyn TeammateBackend>> {
    match detect::detect_backend_kind() {
        BackendKind::InProcess => None,
        BackendKind::Tmux => Some(Box::new(tmux::TmuxBackend::new())),
        BackendKind::Iter => Some(Box::new(iterm::ITermBackend::new())),
    }
}

// ── shared command-construction helpers ──────────────────────────────────
// Pane backends both launch `codesmith team-teammate …` in a new pane/tab.
// The shell-quoted command string and the prompt-file path are identical
// across backends, so they live here and are reused by `tmux` / `iterm`.

/// Build the shell-quoted `codesmith team-teammate …` command string that a
/// pane backend writes into the new pane (tmux runs it via `sh -c`; iTerm2
/// types it into an interactive shell). The prompt is taken from
/// `prompt_file` rather than inlined, so arbitrarily long / hostile prompts
/// never touch the command line. Likewise, `env_file` (see
/// [`write_env_file`]) is sourced-and-unlinked by the wrapper prefix instead
/// of putting `KEY=VALUE` pairs on any argv.
#[must_use]
pub(crate) fn build_team_teammate_command(
    spec: &TeammateSpawnSpec,
    prompt_file: &Path,
    env_file: Option<&Path>,
) -> String {
    let bin = shell_quote(&spec.codesmith_bin.to_string_lossy());
    let mut parts = vec![
        bin,
        "team-teammate".to_string(),
        "--team".to_string(),
        shell_quote(&spec.team_name),
        "--name".to_string(),
        shell_quote(&spec.agent_name),
        "--prompt-file".to_string(),
        shell_quote(&prompt_file.to_string_lossy()),
        "--agent-type".to_string(),
        shell_quote(&spec.agent_type),
        "--permission-mode".to_string(),
        shell_quote(spec.permission_mode.as_str()),
    ];
    if let Some(model) = spec.model.as_ref() {
        parts.push("--model".to_string());
        parts.push(shell_quote(model));
    }
    // Pass cwd explicitly so the pane process agrees with the leader even if
    // the pane's shell rc overrides the multiplexer's `-c` / `cd`.
    parts.push("--cwd".to_string());
    parts.push(shell_quote(&spec.cwd.to_string_lossy()));
    if let Some(allowed) = spec.allowed_tools.as_ref()
        && !allowed.is_empty()
    {
        parts.push("--allowed-tools".to_string());
        parts.push(shell_quote(&allowed.join(",")));
    }
    let command = parts.join(" ");
    match env_file {
        // `set -a` exports every assignment sourced from the env file; the
        // `rm -f` right after keeps the 0600 file from outliving its use
        // even when the teammate binary fails to start.
        Some(path) => format!(
            "set -a; . {}; set +a; rm -f {}; {command}",
            shell_quote(&path.to_string_lossy()),
            shell_quote(&path.to_string_lossy())
        ),
        None => command,
    }
}

/// POSIX single-quote shell quoting: wrap in `'…'` and escape any embedded
/// single quote as `'\''`.
#[must_use]
pub(crate) fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Write the teammate's initial prompt to a temp file and return its path.
/// The pane process is expected to read (and unlink) it on startup.
///
/// The file is created with `O_EXCL` under a randomized name and 0600
/// permissions (via `tempfile`): the old predictable
/// `codesmith-team-prompt-<agent_id>.txt` path let any local user
/// pre-plant a symlink in the shared temp dir and gain an
/// arbitrary-file-overwrite primitive with the prompt as content, and the
/// 0644 default leaked the prompt (which carries repo context) to other
/// local users. Callers remove the file when the spawn fails; the pane
/// unlinks it on success.
pub(crate) fn write_prompt_file(
    backend: &'static str,
    agent_id: &str,
    prompt: &str,
) -> Result<PathBuf, BackendError> {
    let mut file = tempfile::Builder::new()
        .prefix(&format!("codesmith-team-prompt-{agent_id}-"))
        .suffix(".txt")
        .tempfile_in(std::env::temp_dir())
        .map_err(|e| BackendError::SpawnFailed {
            backend,
            message: format!("failed to create prompt file for {agent_id}: {e}"),
        })?;
    std::io::Write::write_all(&mut file, prompt.as_bytes()).map_err(|e| {
        BackendError::SpawnFailed {
            backend,
            message: format!("failed to write prompt file for {agent_id}: {e}"),
        }
    })?;
    // Detach the file from the guard without deleting it — the pane
    // process owns the unlink from here.
    let (_, path) = file.keep().map_err(|e| BackendError::SpawnFailed {
        backend,
        message: format!("failed to persist prompt file for {agent_id}: {e}"),
    })?;
    Ok(path)
}

/// Write the teammate's extra env vars to a 0600 temp file (one
/// `KEY='value'` shell assignment per line) and return its path, or `None`
/// when there is nothing to propagate. Sibling of [`write_prompt_file`]:
/// pane commands source-and-unlink the file on startup, so values (API keys,
/// proxy credentials) never ride an argv or a typed command line where
/// `ps`/shoulder-surfing can read them.
///
/// Keys must be plain shell identifiers and values must be newline-free —
/// anything else is skipped (a dropped var fails loudly at use time, a
/// malformed assignment line could inject shell code).
pub(crate) fn write_env_file(
    backend: &'static str,
    agent_id: &str,
    env: &std::collections::HashMap<String, String>,
) -> Result<Option<PathBuf>, BackendError> {
    if env.is_empty() {
        return Ok(None);
    }
    let mut body = String::new();
    for (key, value) in env {
        if !is_shell_identifier(key) || value.contains('\n') || value.contains('\0') {
            continue;
        }
        body.push_str(key);
        body.push('=');
        body.push_str(&shell_quote(value));
        body.push('\n');
    }
    if body.is_empty() {
        return Ok(None);
    }
    let mut file = tempfile::Builder::new()
        .prefix(&format!("codesmith-team-env-{agent_id}-"))
        .suffix(".env")
        .tempfile_in(std::env::temp_dir())
        .map_err(|e| BackendError::SpawnFailed {
            backend,
            message: format!("failed to create env file for {agent_id}: {e}"),
        })?;
    std::io::Write::write_all(&mut file, body.as_bytes()).map_err(|e| {
        BackendError::SpawnFailed {
            backend,
            message: format!("failed to write env file for {agent_id}: {e}"),
        }
    })?;
    let (_, path) = file.keep().map_err(|e| BackendError::SpawnFailed {
        backend,
        message: format!("failed to persist env file for {agent_id}: {e}"),
    })?;
    Ok(Some(path))
}

/// True when `ident` is usable as a shell variable name (letters, digits,
/// underscore, not starting with a digit).
#[must_use]
pub(crate) fn is_shell_identifier(ident: &str) -> bool {
    !ident.is_empty()
        && !ident.starts_with(|c: char| c.is_ascii_digit())
        && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_new_defaults() {
        let spec = TeammateSpawnSpec::new(
            "team_agent_abcd1234".to_string(),
            "worker-1".to_string(),
            "demo".to_string(),
        );
        assert_eq!(spec.agent_type, "team");
        assert_eq!(spec.permission_mode, PermissionMode::Ask);
        assert!(spec.model.is_none());
        assert!(spec.allowed_tools.is_none());
        assert!(spec.env.is_empty());
        let bin_name = spec
            .codesmith_bin
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        let expected = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
        assert_eq!(bin_name, expected);
    }

    #[test]
    fn shell_quote_plain() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_quote_escapes_single_quote() {
        // O'Brien -> 'O'\''Brien'
        assert_eq!(shell_quote("O'Brien"), "'O'\\''Brien'");
    }

    #[test]
    fn backend_kind_as_str_roundtrip() {
        assert_eq!(BackendKind::InProcess.as_str(), "in_process");
        assert_eq!(BackendKind::Tmux.as_str(), "tmux");
        assert_eq!(BackendKind::Iter.as_str(), "iterm");
    }

    #[test]
    fn backend_error_to_tool_error_preserves_message() {
        let err = BackendError::DuplicateTeammate("worker-1".to_string());
        let tool = err.into_tool_error();
        let msg = format!("{tool}");
        assert!(msg.contains("worker-1"), "msg = {msg}");
    }

    #[test]
    fn permission_mode_cli_roundtrip() {
        assert_eq!(PermissionMode::from_cli("auto"), Some(PermissionMode::Auto));
        assert_eq!(PermissionMode::from_cli(" ASK "), Some(PermissionMode::Ask));
        assert_eq!(PermissionMode::from_cli("yolo"), None);
        assert_eq!(PermissionMode::Auto.as_str(), "auto");
        assert_eq!(PermissionMode::Ask.as_str(), "ask");
    }

    #[test]
    fn validate_rejects_relative_cwd_and_non_utf8_paths() {
        let mut spec = TeammateSpawnSpec::new(
            "team_agent_abcd1234".to_string(),
            "worker-1".to_string(),
            "demo".to_string(),
        );
        spec.codesmith_bin = PathBuf::from("/usr/local/bin/codesmith");
        spec.cwd = PathBuf::from("relative/dir");
        let err = spec.validate().expect_err("relative cwd must be rejected");
        assert!(err.to_string().contains("absolute"), "got: {err}");

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            spec.cwd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff"));
            let err = spec.validate().expect_err("non-UTF-8 cwd must be rejected");
            assert!(err.to_string().contains("UTF-8"), "got: {err}");
        }

        spec.cwd = PathBuf::from("/repo");
        spec.validate().expect("absolute UTF-8 paths are valid");
    }

    #[test]
    fn spawned_teammate_handle_equality() {
        let a = TeammateHandle::TmuxPane("%5".to_string());
        let b = TeammateHandle::TmuxPane("%5".to_string());
        assert_eq!(a, b);
        assert_ne!(a, TeammateHandle::InProcess);
    }
}
