//! Runtime UI callbacks.
//!
//! The engine core invokes terminal-coupled UI side-effects (clipboard-image
//! path resolution) through this trait. The REPL binary provides the concrete
//! implementation; a headless host (app-server) can provide no-op stubs.

use std::path::{Path, PathBuf};

/// Terminal-coupled UI side-effects the engine needs during a turn.
///
/// Kept deliberately tiny: only the calls the engine makes directly to the
/// TUI layer (notifications + clipboard path) live here. Approval flow is
/// channel-based (`Event::ApprovalRequired`) and does not need a trait.
pub trait RuntimeUi: Send + Sync {
    /// Resolve the directory where clipboard images are stored, relative to
    /// the workspace. Used to mark it as a trusted external path for sandbox
    /// purposes.
    fn clipboard_images_dir(&self, workspace: &Path) -> PathBuf;
}
