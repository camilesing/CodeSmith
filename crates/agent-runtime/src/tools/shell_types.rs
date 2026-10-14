//! Portable shell-execution data types.
//!
//! These are the pure-data result/view types produced by shell execution:
//! [`ShellStatus`], [`ShellResult`], [`ShellJobSnapshot`], [`ShellJobDetail`],
//! and [`ShellDeltaResult`]. They carry no terminal- or platform-coupled
//! state (no `portable_pty`, `libc`, or process handles), so they can live in
//! the runtime crate and cross the `Arc<dyn HostServices>` boundary once the
//! `ShellManager` (pty / process management) is trait-erased behind a
//! `ShellManagerHost`.
//!
//! The concrete `ShellManager` stays in the host (TUI) and returns these
//! portable types from its methods.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Status of a shell process
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShellStatus {
    Running,
    Completed,
    Failed,
    Killed,
    TimedOut,
}

/// Result from a shell command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellResult {
    pub task_id: Option<String>,
    pub status: ShellStatus,
    pub exit_code: Option<i32>,
    /// Captured stdout. The complete original stream when
    /// `stdout_truncated` is `false`; a head+tail excerpt with injected
    /// annotation lines when truncated — see the invariants on
    /// [`Self::stdout_len`].
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    /// Byte length of the ORIGINAL stdout stream, before any truncation.
    ///
    /// Truncation invariants (the stderr mirror fields behave identically):
    /// - All bookkeeping counters describe the ORIGINAL stream; `stdout` is
    ///   the (possibly truncated) view of it.
    /// - When `stdout_truncated` is `false`, `stdout` is the complete
    ///   original stream: `stdout.len() == stdout_len` and
    ///   `stdout_omitted == 0`.
    /// - When `stdout_truncated` is `true`, truncation keeps the first
    ///   ~22 KB (head) and the last ~8 KB (tail) of the original, splits
    ///   only at UTF-8 char boundaries (no codepoint is ever split), and
    ///   joins the two excerpts with injected annotation lines
    ///   (`[Output truncated: ...]`, optional preserved summary lines from
    ///   the elided middle, `[Output tail]`). `stdout_omitted` counts only
    ///   the bytes of the elided middle.
    /// - The annotations are injected bytes that were never part of the
    ///   original stream, so `stdout.len() + stdout_omitted == stdout_len`
    ///   does NOT hold for truncated results — it overshoots by exactly
    ///   the injected annotation bytes. The identity holds only
    ///   untruncated.
    #[serde(default)]
    pub stdout_len: usize,
    /// Original stderr length in bytes (pre-truncation; same semantics as
    /// [`Self::stdout_len`]).
    #[serde(default)]
    pub stderr_len: usize,
    /// Bytes omitted from the original stdout by truncation — the elided
    /// middle only, not the difference between `stdout_len` and
    /// `stdout.len()` (see [`Self::stdout_len`] for why those diverge).
    #[serde(default)]
    pub stdout_omitted: usize,
    /// Bytes omitted from the original stderr due to truncation (see
    /// [`Self::stdout_omitted`]).
    #[serde(default)]
    pub stderr_omitted: usize,
    /// Whether stdout was truncated (head+tail+annotations excerpt; see
    /// [`Self::stdout_len`]).
    #[serde(default)]
    pub stdout_truncated: bool,
    /// Whether stderr was truncated (see [`Self::stdout_truncated`]).
    #[serde(default)]
    pub stderr_truncated: bool,
    /// Whether the command was executed in a sandbox. Legacy mirror of the
    /// authoritative `sandbox_effective` (both are set from the same
    /// decision field); kept for serialized-output compatibility.
    #[serde(default)]
    pub sandboxed: bool,
    /// Type of sandbox used (if any). Legacy mirror of the authoritative
    /// `sandbox_backend` (both are set from the same decision field); kept
    /// for serialized-output compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_type: Option<String>,
    /// Whether the command was blocked by sandbox restrictions.
    #[serde(default)]
    pub sandbox_denied: bool,
    #[serde(default)]
    pub sandbox_requested: bool,
    #[serde(default)]
    pub sandbox_effective: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_unavailable_reason: Option<String>,
    #[serde(default)]
    pub sandbox_fallback_allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_excluded_command: Option<String>,
    #[serde(default)]
    pub sandbox_fail_closed: bool,
}

/// Compact, UI-oriented view of a tracked background shell job.
///
/// In-process UI view moved across the host boundary; never persisted or
/// deserialized across versions, so per-field serde defaults are
/// deliberately omitted (revisit if these ever hit disk).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellJobSnapshot {
    pub id: String,
    pub job_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub status: ShellStatus,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_len: usize,
    pub stderr_len: usize,
    pub stdin_available: bool,
    pub stale: bool,
    pub linked_task_id: Option<String>,
}

/// Full output view used by `/jobs show <id>`.
///
/// In-process UI view moved across the host boundary; never persisted or
/// deserialized across versions, so per-field serde defaults are
/// deliberately omitted (revisit if these ever hit disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellJobDetail {
    pub snapshot: ShellJobSnapshot,
    pub stdout: String,
    pub stderr: String,
}

/// Incremental output delta from an interactive shell read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellDeltaResult {
    pub command: String,
    pub result: ShellResult,
    pub stdout_total_len: usize,
    pub stderr_total_len: usize,
}
