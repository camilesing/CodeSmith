//! Workspace trust + config-path resolution helpers.
//!
//! These functions resolve the CodeSmith config file (`~/.codesmith/config.toml`
//! or `$CODESMITH_CONFIG_PATH`) and determine whether a given workspace is
//! marked trusted in it. They were moved here from the TUI's `config.rs` so
//! that terminal-agnostic modules (`project_context`) can perform trust
//! checks without depending on the host. The CLI re-exports them at the
//! historical `crate::config::` paths for backwards compatibility.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::utils::effective_home_dir;

/// Canonicalize a path, falling back to a lexically absolutized copy on
/// failure (e.g. the path does not exist yet). Keeps comparisons stable
/// across symlink/realpath differences and — unlike a raw-path fallback —
/// produces the same key whether or not the target currently exists on disk.
pub fn canonicalize_or_keep(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| lexical_absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// `std::path::absolute` + lexical `.`/`..` normalization, so trust keys are
/// stable regardless of filesystem state.
fn lexical_absolute(path: &Path) -> std::io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let mut normalized = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Expand a leading `~` (tilde expansion via `shellexpand`) to an absolute
/// path. Only the tilde form is expanded — no env-var substitution happens.
pub fn expand_path(path: &str) -> PathBuf {
    // `~` is the home shorthand only when followed by a path separator (or
    // nothing). Backslash counts as a separator on Windows only: on Unix it
    // is a legal filename character, so `~\foo` must stay a relative path
    // instead of silently resolving to `$HOME/foo` and missing trust keys.
    #[cfg(windows)]
    let is_tilde_sep = |s: &str| s.is_empty() || s.starts_with('/') || s.starts_with('\\');
    #[cfg(not(windows))]
    let is_tilde_sep = |s: &str| s.is_empty() || s.starts_with('/');
    if let Some(stripped) = path.strip_prefix('~')
        && is_tilde_sep(stripped)
        && let Some(mut home) = effective_home_dir()
    {
        #[cfg(windows)]
        let suffix = stripped.trim_start_matches(['/', '\\']);
        #[cfg(not(windows))]
        let suffix = stripped.trim_start_matches('/');
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

/// Resolve the active config file path: `$CODESMITH_CONFIG_PATH` first, then
/// the home-directory default.
pub fn default_config_path() -> Option<PathBuf> {
    env_config_path().or_else(home_config_path)
}

/// Home-directory config path (`~/.codesmith/config.toml`), honoring the
/// documented `CODESMITH_HOME` base-directory override (CONFIGURATION.md:
/// "override the base data directory"). Delegating to
/// `codesmith_config::default_config_path` keeps the engine's config load,
/// the workspace-trust store (`is_workspace_trusted` /
/// `save_workspace_trust`), and the `codesmith config` facade resolving the
/// same file instead of splitting between `$CODESMITH_HOME` and `$HOME`.
pub fn home_config_path() -> Option<PathBuf> {
    codesmith_config::default_config_path().ok()
}

/// Config path overridden via the `CODESMITH_CONFIG_PATH` environment variable.
pub fn env_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CODESMITH_CONFIG_PATH") {
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
///
/// Fail-closed: an unreadable or unparseable config leaves the workspace
/// untrusted. Read failures other than "file missing" and parse failures
/// are logged (`tracing::warn!`) so a corrupted or permission-denied config
/// is distinguishable from a genuinely untrusted workspace instead of
/// silently re-prompting users who already ran the trust wizard.
pub fn is_workspace_trusted(workspace: &Path) -> bool {
    let Some(config_path) = default_config_path() else {
        return false;
    };
    let raw = match fs::read_to_string(&config_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
        Err(err) => {
            tracing::warn!(
                path = %config_path.display(),
                error = %err,
                "failed to read CodeSmith config; treating workspace as untrusted"
            );
            return false;
        }
    };
    let doc = match toml::from_str::<toml::Value>(&raw) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::warn!(
                path = %config_path.display(),
                error = %err,
                "failed to parse CodeSmith config; treating workspace as untrusted"
            );
            return false;
        }
    };
    workspace_trust_level_from_doc(&doc, workspace).is_some_and(is_trusted_level)
}

/// Write `trust_level = "trusted"` for `workspace` into a parsed config
/// document. The pure half of [`save_workspace_trust`], so callers already
/// editing the config file (the CLI first-run wizard) can batch the trust
/// write into their own single write.
pub fn apply_workspace_trust(doc: &mut toml::Value, workspace: &Path) -> Result<()> {
    let root = doc
        .as_table_mut()
        .context("Config root must be a TOML table.")?;
    let projects = root
        .entry("projects".to_string())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
        .context("`projects` must be a table.")?;
    let project = projects
        .entry(workspace_config_key(workspace))
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
        .context("Project entry must be a table.")?;
    project.insert(
        "trust_level".to_string(),
        toml::Value::String("trusted".to_string()),
    );
    Ok(())
}

/// Persist workspace trust into the CodeSmith config file via a raw TOML
/// merge that preserves every unrelated field. Moved from the TUI's
/// `config.rs` (same behavior byte-for-byte) so the CLI first-run wizard
/// performs the identical write; the CLI re-exports this path.
pub fn save_workspace_trust(workspace: &Path) -> Result<PathBuf> {
    let config_path = default_config_path()
        .context("Failed to resolve config path: home directory not found.")?;
    ensure_parent_dir_private(&config_path)?;

    // Serialize the read-modify-write against concurrent writers (the REPL
    // and the CLI first-run wizard both land here) with an advisory lock
    // next to the config file; without it, two processes each merge into
    // their own stale snapshot and one side's unrelated fields are lost.
    // The write itself goes through `write_config_file_private`, which is
    // a temp-file + rename, so concurrent readers never observe a torn
    // TOML file. Mirrors the evolution-log lock posture.
    let lock_path = config_path.with_extension("lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open config lock at {}", lock_path.display()))?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock
        .write()
        .map_err(|err| std::io::Error::other(format!("config lock failed: {err}")))?;

    let mut doc = if config_path.exists() {
        let raw = fs::read_to_string(&config_path)?;
        toml::from_str::<toml::Value>(&raw)
            .with_context(|| format!("Failed to parse config at {}", config_path.display()))?
    } else {
        toml::Value::Table(toml::value::Table::new())
    };
    apply_workspace_trust(&mut doc, workspace)?;

    let serialized = toml::to_string_pretty(&doc).context("failed to serialize updated config")?;
    codesmith_config::write_config_file_private(&config_path, &serialized)
        .with_context(|| format!("Failed to write config to {}", config_path.display()))?;
    Ok(config_path)
}

/// `create_dir_all` on the parent plus a best-effort Unix tightening of
/// group/other bits. `create_dir_all` can create several intermediate
/// components, so the tightening walks every component this call actually
/// created (pre-existing ancestors further up are out of scope to modify);
/// otherwise a custom `CODESMITH_CONFIG_PATH` like
/// `~/creds/nested/.codesmith/config.toml` would leave the fresh `~/creds`
/// and `~/creds/nested` at umask defaults. Mirrors the CLI's
/// `ensure_parent_dir`: filesystems that reject chmod (Docker bind-mounts
/// of NTFS, network shares — #897) only get a warning; the directory
/// already exists by then, so the write continues.
fn ensure_parent_dir_private(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        // Deepest ancestor that already exists before the create below.
        let mut deepest_existing = parent.to_path_buf();
        while !deepest_existing.exists() {
            match deepest_existing.parent() {
                Some(p) if !p.as_os_str().is_empty() => deepest_existing = p.to_path_buf(),
                _ => break,
            }
        }
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        #[cfg(unix)]
        {
            let mut dir = parent.to_path_buf();
            loop {
                tighten_dir_mode_best_effort(&dir);
                if dir == deepest_existing {
                    break;
                }
                match dir.parent() {
                    Some(p) if !p.as_os_str().is_empty() => dir = p.to_path_buf(),
                    _ => break,
                }
            }
        }
    }
    Ok(())
}

/// Best-effort `& !0o077` tightening of group/other bits with the
/// warn-and-continue semantics documented on [`ensure_parent_dir_private`].
#[cfg(unix)]
fn tighten_dir_mode_best_effort(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(dir) {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            let mut perms = meta.permissions();
            perms.set_mode(mode & !0o077);
            if let Err(err) = fs::set_permissions(dir, perms) {
                tracing::warn!(
                    path = %dir.display(),
                    error = %err,
                    "could not tighten parent dir permissions; \
                     filesystem may not support Unix chmod. Continuing."
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_path, home_config_path};

    #[cfg(not(windows))]
    #[test]
    fn backslash_after_tilde_is_not_a_home_separator_on_unix() {
        // Backslash is a legal filename character on Unix: `~\foo` is a
        // relative path, not `$HOME/foo`, and must survive expansion
        // untouched so trust-key comparisons don't silently miss.
        assert_eq!(expand_path(r"~\foo").to_str(), Some(r"~\foo"));
    }

    #[test]
    fn home_config_path_honors_codesmith_home_override() {
        let _env = crate::test_support::lock_test_env();
        let scratch = tempfile::tempdir().expect("scratch CODESMITH_HOME");
        let _guard =
            crate::test_support::EnvVarGuard::set("CODESMITH_HOME", scratch.path().as_os_str());

        assert_eq!(
            home_config_path(),
            Some(scratch.path().join("config.toml")),
            "CODESMITH_HOME is the documented base-data-directory override \
             (CONFIGURATION.md); the config file must resolve under it"
        );
    }
}
