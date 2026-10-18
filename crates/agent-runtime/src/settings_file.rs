//! Settings-file location and a single-key locale writer.
//!
//! The full `Settings` struct stays in the CLI; only the path contract and a
//! key-preserving write live here so the CLI first-run wizard's language
//! question and the runtime cannot drift on where settings live. The CLI's
//! `Settings::path` delegates to [`settings_file_path`].

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Resolve the settings file path: a sibling `settings.toml` of the config
/// file when `CODESMITH_CONFIG_PATH` is set (the same knob tests use), else
/// the platform config dir.
pub fn settings_file_path() -> Result<PathBuf> {
    if let Some(config_path) = codesmith_config::codesmith_env("CONFIG_PATH") {
        let config_path = config_path.trim();
        if !config_path.is_empty() {
            let p = crate::workspace_trust::expand_path(config_path);
            let dir = match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                // A bare-filename CONFIG_PATH resolves against the process
                // launch directory (relative-argument semantics).
                _ => std::env::current_dir()
                    .context("Failed to resolve current directory for settings path")?,
            };
            return Ok(dir.join("settings.toml"));
        }
    }

    let config_dir = dirs::config_dir()
        .context("Failed to resolve config directory: not found.")?
        .join("deepseek");
    Ok(config_dir.join("settings.toml"))
}

/// How long [`write_settings_locale`] waits on a settings lock held by
/// another writer before giving up.
const SETTINGS_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
/// A settings lock older than this is assumed orphaned by a crashed
/// writer and broken.
const SETTINGS_LOCK_STALE_AFTER: Duration = Duration::from_secs(30);

/// Advisory sibling lock file serializing the read-merge-write in
/// [`write_settings_locale`] across processes: two concurrent writers (the
/// first-run wizard racing the runtime, or two CLI invocations) would
/// otherwise merge from the same baseline and the last rename would
/// silently drop the other's keys. Acquired with `create_new`
/// (`O_EXCL`); dropped (and removed) when the guard goes out of scope.
struct SettingsFileLock {
    path: PathBuf,
}

impl SettingsFileLock {
    fn acquire(settings: &Path) -> Result<Self> {
        let path = settings.with_extension("toml.lock");
        let deadline = Instant::now() + SETTINGS_LOCK_TIMEOUT;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        bail!("timed out waiting for settings lock at {}", path.display());
                    }
                    let stale = fs::metadata(&path)
                        .ok()
                        .and_then(|meta| meta.modified().ok())
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age >= SETTINGS_LOCK_STALE_AFTER);
                    if stale {
                        let _ = fs::remove_file(&path);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    return Err(anyhow::Error::new(err).context(format!(
                        "failed to create settings lock at {}",
                        path.display()
                    )));
                }
            }
        }
    }
}

impl Drop for SettingsFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Set `locale` in the settings file, creating it if needed. Uses
/// `toml_edit` so the edit is format-preserving: user comments, whitespace,
/// and key order all survive untouched (`toml::to_string_pretty` would drop
/// comments and re-sort keys). Writes atomically via a temp file + rename,
/// like the CLI's `Settings::save`; it only ever touches the one key a
/// first-run language choice contributes.
pub fn write_settings_locale(tag: &str) -> Result<PathBuf> {
    let path = settings_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory {}", parent.display()))?;
    }
    let _lock = SettingsFileLock::acquire(&path)?;

    let mut doc = if path.exists() {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read settings from {}", path.display()))?;
        raw.parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("Failed to parse settings from {}", path.display()))?
    } else {
        toml_edit::DocumentMut::new()
    };
    doc["locale"] = toml_edit::value(tag);
    let content = doc.to_string();
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, &content)
        .with_context(|| format!("Failed to write settings to {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("Failed to replace settings at {}", path.display()))?;
    Ok(path)
}
