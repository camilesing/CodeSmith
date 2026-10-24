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

/// How long [`with_settings_lock`] waits on a settings lock held by
/// another writer before giving up.
const SETTINGS_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `f` as a read-modify-write serialized against every other writer of
/// the same file across processes. Named for its first user (settings.toml)
/// but generic over the path: the CLI's `/network` store, workspace trust,
/// and the grants store route through it with their own file, and every
/// writer of a given file must use the same lock or the unlocked writer's
/// last rename silently drops the locked writer's merge. Backed by an OS
/// advisory lock (`fd-lock`): the kernel releases it when the holder dies,
/// so there is no stale-lock heuristic whose break could race a fresh
/// holder (the old `create_new` + mtime-stale-break scheme could delete a
/// freshly acquired lock between the mtime read and the removal).
///
/// Lock-file naming appends `.lock` after the full extension
/// (`settings.toml` → `settings.toml.lock`, `config.json` →
/// `config.json.lock`) so sibling files with different extensions never
/// share a lock. Known limitation: the wait polls `try_write` every 50 ms
/// on the calling thread for up to [`SETTINGS_LOCK_TIMEOUT`] — callers on
/// an async executor block for that long under contention (the lock body
/// is expected to be short file IO).
pub fn with_settings_lock<T>(settings: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_ext = match settings.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => format!("{ext}.lock"),
        None => "lock".to_string(),
    };
    let path = settings.with_extension(lock_ext);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open settings lock at {}", path.display()))?;
    let mut rw = fd_lock::RwLock::new(file);
    let deadline = Instant::now() + SETTINGS_LOCK_TIMEOUT;
    loop {
        match rw.try_write() {
            Ok(guard) => {
                // Hold the guard across `f` (a `_`-binding would drop it
                // immediately).
                let _held = guard;
                return f();
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for settings lock at {}", path.display());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!(
                    "failed to acquire settings lock at {}",
                    path.display()
                )));
            }
        }
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
    with_settings_lock(&path, || {
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
        // fsync the temp before the rename so a crash cannot publish an empty
        // or torn settings file, and the parent dir after so the rename itself
        // is durable.
        if let Ok(file) = fs::File::open(&tmp) {
            let _ = file.sync_all();
        }
        fs::rename(&tmp, &path)
            .with_context(|| format!("Failed to replace settings at {}", path.display()))?;
        if let Some(parent) = path.parent()
            && let Ok(dir) = fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }
        // The `&path` lock argument still borrows `path`, so hand back a
        // clone instead of moving it out of the closure.
        Ok(path.clone())
    })
}
