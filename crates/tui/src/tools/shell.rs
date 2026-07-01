//! crossterm-backed terminal controller + shared-shell-manager constructor.
//!
//! The concrete shell tool implementations (`ExecShellTool`, `ShellWaitTool`,
//! `ShellInteractTool`, `ShellCancelTool`, `NoteTool`) and their helpers live
//! in [`codesmith_tool_impls::tools::shell`]. This module retains only the
//! crossterm-specific bits that must live in the terminal-coupled binary:
//! - [`CrosstermTerminalControl`] — a [`ShellTerminalControl`] impl that
//!   saves/restores terminal raw mode around sandboxed child spawn.
//! - [`new_shared_shell_manager`] — constructs a [`SharedShellManager`]
//!   wired with the crossterm controller.
//!
//! Everything else is re-exported from the tool-impls crate below.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// `ShellTerminalControl` is not re-exported by the tool-impls crate (only the
// TUI's crossterm impl needs it), so import it directly. `ShellManager` and
// `SharedShellManager` come in via the glob re-export below.
use codesmith_agent_runtime::shell_manager::ShellTerminalControl;

// Re-export all tool implementations, helpers, and agent-runtime types so
// that historical `crate::tools::shell::X` paths keep resolving.
pub use codesmith_tool_impls::tools::shell::*;

/// crossterm-backed [`ShellTerminalControl`] for the TUI.
///
/// Used by [`new_shared_shell_manager`] so `ShellManager`'s sandboxed
/// sync/interactive exec paths can save/restore terminal raw mode around
/// child spawn (#1690) without the runtime crate depending on crossterm.
struct CrosstermTerminalControl;

impl ShellTerminalControl for CrosstermTerminalControl {
    fn raw_mode_enabled(&self) -> bool {
        crossterm::terminal::is_raw_mode_enabled().unwrap_or(false)
    }
    fn disable_raw_mode(&self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    fn enable_raw_mode(&self) {
        let _ = crossterm::terminal::enable_raw_mode();
    }
}

/// Construct the TUI's crossterm-backed terminal controller for injection
/// into [`ShellManager::with_terminal_control`].
fn crossterm_terminal_control() -> Arc<dyn ShellTerminalControl> {
    Arc::new(CrosstermTerminalControl)
}

/// Create a new shared shell manager with default sandbox policy and the
/// TUI's crossterm-backed terminal raw-mode controller (so sandboxed
/// sync/interactive exec saves/restores raw mode around child spawn).
pub fn new_shared_shell_manager(workspace: PathBuf) -> SharedShellManager {
    Arc::new(Mutex::new(ShellManager::with_terminal_control(
        workspace,
        crossterm_terminal_control(),
    )))
}
