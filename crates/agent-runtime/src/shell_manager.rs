//! Concrete shell-process manager (pty / process / sandbox plumbing).
//!
//! This module hosts the terminal-agnostic `ShellManager` — background
//! process management, sandboxed sync/interactive execution, output
//! truncation — plus its supporting types (`BackgroundShell`, decision
//! helpers, and the `ShellManagerHost` bridge onto `ShellManagerApi`).
//!
//! ## Terminal raw-mode trait-erasure
//!
//! `ShellManager`'s sandboxed sync/interactive exec paths must briefly
//! release the host terminal from ratatui raw mode so the child can own the
//! tty (issue #1690). `crossterm` is a terminal-coupled dependency that this
//! runtime crate deliberately does NOT pull in. To keep `ShellManager`
//! portable while preserving that save/restore, the raw-mode toggle is
//! abstracted behind the [`ShellTerminalControl`] trait: the TUI supplies a
//! crossterm-backed implementation; non-TUI hosts (tests, app-server) use
//! [`NoopTerminalControl`] (raw mode is never enabled there, so the toggle
//! is a no-op).

use std::sync::Arc;

/// Terminal raw-mode control for shell execution.
///
/// Implemented by the TUI (crossterm-backed) and by
/// [`NoopTerminalControl`] for non-terminal hosts. See the module docs for
/// the rationale.
pub trait ShellTerminalControl: Send + Sync {
    /// Whether the host currently has terminal raw mode enabled.
    fn raw_mode_enabled(&self) -> bool;
    /// Disable terminal raw mode (best-effort; errors are swallowed by the
    /// caller).
    fn disable_raw_mode(&self);
    /// Re-enable terminal raw mode (best-effort).
    fn enable_raw_mode(&self);
}

/// No-op [`ShellTerminalControl`] for non-terminal hosts.
///
/// Raw mode is never enabled outside the TUI, so every method is a no-op and
/// [`raw_mode_enabled`](ShellTerminalControl::raw_mode_enabled) returns
/// `false`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTerminalControl;

impl ShellTerminalControl for NoopTerminalControl {
    fn raw_mode_enabled(&self) -> bool {
        false
    }
    fn disable_raw_mode(&self) {}
    fn enable_raw_mode(&self) {}
}

/// Convenience: the default terminal control (no-op) used when a host does
/// not inject a crossterm-backed implementation.
pub fn default_terminal_control() -> Arc<dyn ShellTerminalControl> {
    Arc::new(NoopTerminalControl)
}
