//! First-run onboarding marker and workspace-trust gate inputs.
//!
//! Shared by the CLI dispatcher's question/answer onboarding and the
//! boot-time gates so both entry points agree on what "already onboarded"
//! means. Moved from `crates/tui/src/tui/onboarding/mod.rs`; the CLI
//! re-exports these at the historical paths.
//!
//! Known limitation: this module only answers "has onboarding happened";
//! the credential and trust gates themselves stay with each binary.

use std::io;
use std::path::{Path, PathBuf};

/// Marker file recording that first-run onboarding completed.
///
/// Deliberately `home_dir()/.codesmith/...` rather than the
/// `CODESMITH_HOME`-aware config home: this is the exact path the TUI gate
/// has always read, and the two binaries must agree on it bit-for-bit.
pub fn default_marker_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codesmith").join(".onboarded"))
}

pub fn is_onboarded() -> bool {
    default_marker_path().is_some_and(|path| path.exists())
}

pub fn mark_onboarded() -> io::Result<PathBuf> {
    let path = default_marker_path().ok_or_else(|| {
        io::Error::other("home directory not found; cannot resolve onboarding marker path")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("failed to create {}: {e}", parent.display()),
            )
        })?;
    }
    std::fs::write(&path, "").map_err(|e| {
        io::Error::new(e.kind(), format!("failed to write {}: {e}", path.display()))
    })?;
    Ok(path)
}

/// Whether the workspace still needs an explicit trust decision.
///
/// Trust comes only from the user's own config file: marker files inside
/// the workspace never grant trust, because a cloned repository can ship
/// them and self-grant trust.
pub fn needs_trust(workspace: &Path) -> bool {
    !crate::workspace_trust::is_workspace_trusted(workspace)
}
