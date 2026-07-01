//! Workspace trust + config-path resolution helpers.
//!
//! These functions resolve the CodeSmith config file (`~/.codesmith/config.toml`
//! or `$CODESMITH_CONFIG_PATH`) and determine whether a given workspace is
//! marked trusted in it. They were moved here from the TUI's `config.rs` so
//! that terminal-agnostic modules (`project_context`) can perform trust
//! checks without depending on the TUI. The TUI re-exports them at the
//! historical `crate::config::` paths for backwards compatibility.

use std::fs;
use std::path::{Path, PathBuf};

use crate::utils::effective_home_dir;

/// Canonicalize a path, falling back to the original on failure (e.g. the
/// path does not exist yet). Keeps comparisons stable across symlink/realpath
/// differences.
pub fn canonicalize_or_keep(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Expand a leading `~` (and env vars via `shellexpand`) to an absolute path.
pub fn expand_path(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix('~')
        && (stripped.is_empty() || stripped.starts_with('/') || stripped.starts_with('\\'))
        && let Some(mut home) = effective_home_dir()
    {
        let suffix = stripped.trim_start_matches(['/', '\\']);
        if !suffix.is_empty() {
            home.push(suffix);
        }
        return home;
    }

    let expanded = shellexpand::tilde(path);
    PathBuf::from(expanded.as_ref())
}

/// `expand_path` for an owned `PathBuf`.
pub fn expand_pathbuf(path: PathBuf) -> PathBuf {
    if let Some(raw) = path.to_str() {
        return expand_path(raw);
    }
    path
}

/// Resolve the active config file path: `$CODESMITH_CONFIG_PATH` /
/// `$DEEPSEEK_CONFIG_PATH` first, then the home-directory default.
pub fn default_config_path() -> Option<PathBuf> {
    env_config_path().or_else(home_config_path)
}

/// Home-directory config path (`~/.codesmith/config.toml`, falling back to the
/// legacy `~/.deepseek/config.toml`).
pub fn home_config_path() -> Option<PathBuf> {
    effective_home_dir().map(|home| {
        let primary = home.join(".codesmith").join("config.toml");
        if primary.exists() {
            return primary;
        }
        let legacy = home.join(".deepseek").join("config.toml");
        if legacy.exists() {
            return legacy;
        }
        primary
    })
}

/// Config path overridden via the `CODESMITH_CONFIG_PATH` / `DEEPSEEK_CONFIG_PATH`
/// environment variables.
pub fn env_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CODESMITH_CONFIG_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(expand_path(trimmed));
        }
    }
    if let Ok(path) = std::env::var("DEEPSEEK_CONFIG_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(expand_path(trimmed));
        }
    }
    None
}

/// Canonicalized string key under which a workspace is filed in the config.
pub fn workspace_config_key(workspace: &Path) -> String {
    canonicalize_or_keep(workspace)
        .to_string_lossy()
        .into_owned()
}

/// Whether a trust-level string equals "trusted" (case-insensitive).
pub fn is_trusted_level(level: &str) -> bool {
    level.trim().eq_ignore_ascii_case("trusted")
}

/// Look up the `trust_level` for `workspace` in a parsed config document.
pub fn workspace_trust_level_from_doc<'a>(
    doc: &'a toml::Value,
    workspace: &Path,
) -> Option<&'a str> {
    let workspace = canonicalize_or_keep(workspace);
    let projects = doc.get("projects")?.as_table()?;
    for (raw_path, project) in projects {
        let project_path = canonicalize_or_keep(&expand_path(raw_path));
        if project_path == workspace {
            return project.get("trust_level").and_then(toml::Value::as_str);
        }
    }
    None
}

/// Whether `workspace` is marked trusted in the CodeSmith config file.
pub fn is_workspace_trusted(workspace: &Path) -> bool {
    let Some(config_path) = default_config_path() else {
        return false;
    };
    let Ok(raw) = fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return false;
    };
    workspace_trust_level_from_doc(&doc, workspace).is_some_and(is_trusted_level)
}
