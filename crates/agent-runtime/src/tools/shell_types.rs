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
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    /// Original stdout length in bytes.
    #[serde(default)]
    pub stdout_len: usize,
    /// Original stderr length in bytes.
    #[serde(default)]
    pub stderr_len: usize,
    /// Bytes omitted from stdout due to truncation.
    #[serde(default)]
    pub stdout_omitted: usize,
    /// Bytes omitted from stderr due to truncation.
    #[serde(default)]
    pub stderr_omitted: usize,
    /// Whether stdout was truncated.
    #[serde(default)]
    pub stdout_truncated: bool,
    /// Whether stderr was truncated.
    #[serde(default)]
    pub stderr_truncated: bool,
    /// Whether the command was executed in a sandbox.
    #[serde(default)]
    pub sandboxed: bool,
    /// Type of sandbox used (if any).
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellJobDetail {
    pub snapshot: ShellJobSnapshot,
    pub stdout: String,
    pub stderr: String,
}

pub struct ShellDeltaResult {
    pub command: String,
    pub result: ShellResult,
    pub stdout_total_len: usize,
    pub stderr_total_len: usize,
}
