//! TUI settings-file location and a single-key locale writer.
//!
//! The full `Settings` struct stays in the TUI; only the path contract and a
//! key-preserving write live here so the CLI first-run wizard's language
//! question and the TUI cannot drift on where settings live. The TUI's
//! `Settings::path` delegates to [`settings_file_path`].

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Resolve the settings file path: a sibling `settings.toml` of the config
/// file when `CODESMITH_CONFIG_PATH` is set (the same knob tests use), else
/// the platform config dir.
pub fn settings_file_path() -> Result<PathBuf> {
    if let Some(config_path) = codesmith_config::codesmith_env("CONFIG_PATH") {
        let config_path = config_path.trim();
        if !config_path.is_empty() {
            let p = crate::workspace_trust::expand_path(config_path);
            if let Some(parent) = p.parent() {
                return Ok(parent.join("settings.toml"));
            }
        }
    }

    let config_dir = dirs::config_dir()
        .context("Failed to resolve config directory: not found.")?
        .join("deepseek");
    Ok(config_dir.join("settings.toml"))
}

/// Set `locale` in the settings file, creating it if needed and preserving
/// every other key (raw TOML merge). Known limitation: unlike the TUI's
/// `Settings::save` this is not an atomic write; it only ever touches the
/// one key a first-run language choice contributes.
pub fn write_settings_locale(tag: &str) -> Result<PathBuf> {
    let path = settings_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory {}", parent.display()))?;
    }

    let mut doc = if path.exists() {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read settings from {}", path.display()))?;
        toml::from_str::<toml::Value>(&raw)
            .with_context(|| format!("Failed to parse settings from {}", path.display()))?
    } else {
        toml::Value::Table(toml::value::Table::new())
    };
    let root = doc
        .as_table_mut()
        .context("Settings root must be a TOML table.")?;
    root.insert("locale".to_string(), toml::Value::String(tag.to_string()));
    let content = toml::to_string_pretty(&doc).context("Failed to serialize settings")?;
    fs::write(&path, content)
        .with_context(|| format!("Failed to write settings to {}", path.display()))?;
    Ok(path)
}
