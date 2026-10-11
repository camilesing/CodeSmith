//! First-run onboarding marker and workspace-trust gate inputs.
//!
//! Shared by the CLI dispatcher's question/answer onboarding and the TUI's
//! boot-time gates so the two binaries agree on what "already onboarded"
//! means. Moved from `crates/tui/src/tui/onboarding/mod.rs`; the TUI
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
    let path = default_marker_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Home directory not found"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, "")?;
    Ok(path)
}

/// Whether the workspace still needs an explicit trust decision, including
/// the legacy in-repo markers the config-file check does not know about.
pub fn needs_trust(workspace: &Path) -> bool {
    if crate::workspace_trust::is_workspace_trusted(workspace) {
        return false;
    }

    let markers = [
        workspace.join(".codesmith").join("trusted"),
        workspace.join(".codesmith").join("trust.json"),
    ];
    !markers.iter().any(|path| path.exists())
}
