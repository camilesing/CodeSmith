//! Shell abstraction shim.
//!
//! The portable shell-detection and command-building core (`ShellKind`,
//! `ShellDispatcher`, `global_dispatcher`, …) now lives in
//! `codesmith-agent-runtime`'s `shell_dispatcher` module. This file re-exports
//! it and keeps the terminal-coupled `run_foreground` (crossterm raw-mode
//! save/restore), which cannot live in the terminal-agnostic runtime crate.

pub use codesmith_agent_runtime::shell_dispatcher::*;

use std::path::Path;

/// Execute a foreground command with raw-mode save/restore.
///
/// A scope guard ensures raw mode is restored even if the command fails
/// to spawn or returns early (review feedback, issue #1690).
pub fn run_foreground(
    dispatcher: &ShellDispatcher,
    shell_command: &str,
    cwd: &Path,
) -> Result<String, anyhow::Error> {
    use anyhow::Context;

    // Log the execution (same format as `ShellDispatcher::log_exec`).
    ShellDispatcher::log_exec(shell_command);

    // Disable raw mode; guard restores it only if it was already enabled.
    let raw_mode_was_enabled = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    if raw_mode_was_enabled {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    struct FgRawModeGuard {
        restore: bool,
    }
    impl Drop for FgRawModeGuard {
        fn drop(&mut self) {
            if self.restore {
                let _ = crossterm::terminal::enable_raw_mode();
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
