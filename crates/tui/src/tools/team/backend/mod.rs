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

pub mod detect;
pub mod iterm;
pub mod tmux;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    /// Permission mode: `"auto"` or `"ask"`.
    pub permission_mode: String,
    /// Extra environment variables to set in a spawned pane process.
    pub env: HashMap<String, String>,
    /// Binary to invoke for pane backends (defaults to `current_exe()`).
    pub codesmith_bin: PathBuf,
}

impl TeammateSpawnSpec {
    /// Build a spec with the given identity/portable fields and sensible
    /// defaults for the rest (`permission_mode = "ask"`, empty env,
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
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            allowed_tools: None,
            permission_mode: "ask".to_string(),
            env: HashMap::new(),
            codesmith_bin: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codesmith")),
        }
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
    SpawnFailed { backend: &'static str, message: String },
    /// The backend's runtime (tmux/iTerm) is not available in this context.
    #[error("backend `{backend}` not available: {message}")]
    Unavailable { backend: &'static str, message: String },
}

impl BackendError {
    /// Convert into a [`crate::tools::spec::ToolError`] with an
    /// execution-failed classification, matching the existing spawn path's
    /// error posture.
    pub(crate) fn into_tool_error(self) -> crate::tools::spec::ToolError {
        crate::tools::spec::ToolError::execution_failed(self.to_string())
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
    fn name(&self) -> &'static str;

    /// Which [`BackendKind`] this implementation represents.
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
/// never touch the command line.
#[must_use]
pub(crate) fn build_team_teammate_command(spec: &TeammateSpawnSpec, prompt_file: &Path) -> String {
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
        shell_quote(&spec.permission_mode),
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
    parts.join(" ")
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

/// Write the teammate's initial prompt to a stable temp file and return its
/// path. The pane process is expected to read (and unlink) it on startup.
pub(crate) fn write_prompt_file(
    backend: &'static str,
    agent_id: &str,
    prompt: &str,
) -> Result<PathBuf, BackendError> {
    let mut path = std::env::temp_dir();
    path.push(format!("codesmith-team-prompt-{agent_id}.txt"));
    std::fs::write(&path, prompt).map_err(|e| BackendError::SpawnFailed {
        backend,
        message: format!("failed to write prompt file {}: {e}", path.display()),
    })?;
    Ok(path)
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
        assert_eq!(spec.permission_mode, "ask");
        assert!(spec.model.is_none());
        assert!(spec.allowed_tools.is_none());
        assert!(spec.env.is_empty());
        let bin_name = spec.codesmith_bin.file_name().map(|s| s.to_string_lossy().into_owned());
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
    fn spawned_teammate_handle_equality() {
        let a = TeammateHandle::TmuxPane("%5".to_string());
        let b = TeammateHandle::TmuxPane("%5".to_string());
        assert_eq!(a, b);
        assert_ne!(a, TeammateHandle::InProcess);
    }
}
