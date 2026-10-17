//! Stable storage location for pasted/attached images.
//!
//! Extracted from the retired ratatui clipboard module; the engine's
//! RuntimeUi seam resolves this path when image paste is materialized.

use std::path::{Path, PathBuf};

/// Resolve the directory pasted images should land in. Prefers
/// `~/.codesmith/clipboard-images/` so the path is stable across workspaces
/// and matches the location described in user-facing docs; falls back to
/// `<workspace>/clipboard-images/` if the home dir is unavailable.
pub fn clipboard_images_dir(workspace: &Path) -> PathBuf {
    clipboard_images_dir_for_home(
        workspace,
        codesmith_config::codesmith_home().ok().as_deref(),
    )
}

fn clipboard_images_dir_for_home(workspace: &Path, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        return home.join(".codesmith").join("clipboard-images");
    }
    workspace.join("clipboard-images")
}
