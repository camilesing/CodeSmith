//! Shell abstraction shim.
//!
//! The portable shell-detection and command-building core (`ShellKind`,
//! `ShellDispatcher`, `global_dispatcher`, …) now lives in
//! `codesmith-agent-runtime`'s `shell_dispatcher` module. This file re-exports
//! it and keeps the terminal-coupled `run_foreground` (crossterm raw-mode
//! save/restore), which cannot live in the terminal-agnostic runtime crate.

pub use codesmith_agent_runtime::shell_dispatcher::{ShellDispatcher, global_dispatcher};

use std::path::Path;

/// Execute a foreground command with raw-mode save/restore.
///
/// A scope guard ensures raw mode is restored even if the command fails
/// to spawn or returns early (review feedback, issue #1690).
///
/// # Known limitations (deliberate)
///
/// - **Capture-only, not tty-attached.** Despite the name, the child runs
///   with a null stdin and piped stdout/stderr (`Command::output`), so an
///   interactive child (sudo/ssh prompt, editor, pager) sees EOF and its
///   output is not shown live. Commands must be non-interactive; the sole
///   caller (`eval.rs::exec_shell`) consumes the captured output.
/// - **Unbounded wait.** There is no timeout or kill path: a hung child
///   (or a grandchild holding the pipes open) blocks the caller while raw
///   mode stays disabled. Callers needing a bound must run this under
///   their own deadline.
pub fn run_foreground(
    dispatcher: &ShellDispatcher,
    shell_command: &str,
    cwd: &Path,
) -> Result<String, anyhow::Error> {
    use anyhow::Context;

    // Log the execution (same format as `ShellDispatcher::log_exec`).
    ShellDispatcher::log_exec(shell_command);

    // Disable raw mode; guard restores it only if it was already enabled.
    // A failed query is reported rather than silently read as "off" (which
    // would skip both the disable and the restore while raw mode is on);
    // a failed disable is propagated — running the child with the tty
    // still raw defeats the whole purpose.
    let raw_mode_was_enabled = crossterm::terminal::is_raw_mode_enabled().unwrap_or_else(|err| {
        eprintln!("warning: failed to query terminal raw-mode state: {err}");
        false
    });
    if raw_mode_was_enabled {
        crossterm::terminal::disable_raw_mode()
            .context("failed to disable raw mode before running foreground command")?;
    }
    struct FgRawModeGuard {
        restore: bool,
    }
    impl Drop for FgRawModeGuard {
        fn drop(&mut self) {
            if self.restore
                && let Err(err) = crossterm::terminal::enable_raw_mode()
            {
                // Drop cannot propagate, but the user must learn the
                // terminal was left in cooked mode (issue #1690's guard
                // exists precisely to prevent that state).
                eprintln!("warning: failed to restore terminal raw mode: {err}");
            }
        }
    }
    let _guard = FgRawModeGuard {
        restore: raw_mode_was_enabled,
    };

    let mut cmd = dispatcher.build_command(shell_command);
    cmd.current_dir(cwd);

    let output = cmd
        .output()
        .with_context(|| format!("failed to execute shell command: {shell_command}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "shell command failed (status={}): {}",
            output.status,
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(stdout)
}
