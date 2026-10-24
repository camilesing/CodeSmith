//! Settings system - Persistent user preferences
//!
//! Settings are stored at the path
//! [`codesmith_agent_runtime::settings_file::settings_file_path`] resolves.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::normalize_model_name;
use crate::localization::normalize_configured_locale;
/// Normalize a theme name to its canonical form (moved from the retired
/// ratatui palette; settings validation is the surviving consumer).
pub(crate) fn normalize_theme_name(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" | "system" | "default" => Some("system"),
        "terminal" | "term" | "transparent" | "follow-terminal" | "inherit" => Some("terminal"),
        "dark" => Some("dark"),
        "light" => Some("light"),
        "grayscale" | "greyscale" | "gray" | "grey" | "mono" | "monochrome" | "black-white"
        | "black_and_white" | "blackwhite" | "bw" | "b&w" => Some("grayscale"),
        "catppuccin-mocha" | "catppuccin" | "mocha" => Some("catppuccin-mocha"),
        "tokyo-night" | "tokyonight" | "tokyo" => Some("tokyo-night"),
        "dracula" => Some("dracula"),
        "gruvbox-dark" | "gruvbox" => Some("gruvbox-dark"),
        "claude" => Some("claude"),
        "matrix" | "hacker" => Some("matrix"),
        "solarized-light" | "solarized" => Some("solarized-light"),
        _ => None,
    }
}

/// User settings with defaults
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Auto-compact conversations when they approach the model limit.
    pub auto_compact: bool,
    /// Enable terminal bracketed-paste mode. Default true. Disable if your
    /// terminal mishandles the `\e[?2004h` escape (rare; some legacy
    /// terminals over SSH+screen multiplex without the cap).
    pub bracketed_paste: bool,
    /// Maximum number of file-mention popup candidates retained before the
    /// composer renders its visible window. The widget paginates by terminal
    /// height, so this is a data-side cap rather than a visible-row budget.
    pub mention_menu_limit: usize,
    /// Maximum workspace depth for `@`-mention completion walks. `0` means
    /// unlimited depth; use with care in very large repositories.
    pub mention_walk_depth: usize,
    /// Show thinking blocks from the model
    pub show_thinking: bool,
    /// Answer in maximum-compression "simple" (caveman) conversation style:
    /// short sentences, no filler, technical content byte-exact. Toggle via
    /// `/config is_simple on|off`; applies from the next turn.
    pub is_simple: bool,
    /// UI locale: auto, en, zh-Hans, zh-Hant, hi, es-419
    pub locale: String,
    /// Named UI theme. Defaults to `"terminal"`, which inherits the host
    /// terminal's colors (transparent surfaces, ANSI accents) the same way
    /// CLI output does. Also accepts `"system"` (COLORFGBG / macOS
    /// appearance detection), `"dark"`, `"light"`, `"grayscale"`, or one of
    /// the community presets: `"catppuccin-mocha"`, `"tokyo-night"`,
    /// `"dracula"`, `"gruvbox-dark"`.
    pub theme: String,
    /// Prompt-bar color (`/color`), one of the eight names Claude Code
    /// accepts. `None` follows the theme's accent.
    pub prompt_color: Option<String>,
    /// A shell command whose stdout is appended to the status line
    /// (`/statusline`), refreshed at most every [`STATUSLINE_TTL`] seconds.
    pub statusline_command: Option<String>,
    /// Whether a `!command` line starts a turn that answers from its output.
    /// `false` records the run without asking the model about it — Claude
    /// Code's `respondToBashCommands`.
    pub respond_to_shell_commands: bool,
    /// Composer editing mode: "normal" (default) or "vim" for modal editing.
    /// When set to "vim" the composer starts in Normal mode; press i/a/o to
    /// enter Insert mode and Esc to return to Normal.
    pub composer_vim_mode: String,
    /// Default mode: "agent", "plan", "yolo"
    pub default_mode: String,
    /// Last preset selected via `/preset <name>` or `--preset` (e.g.
    /// "simple", "all", or a user/project preset). Restored on the next
    /// launch unless `--preset` or `preset = "..."` in config.toml
    /// overrides it. `None` means no persisted choice. Free-form:
    /// validated against the preset catalog at use time, not at save
    /// time. Older settings files used the `active_mode` key, still
    /// accepted as an alias.
    #[serde(default, alias = "active_mode")]
    pub active_preset: Option<String>,
    /// Maximum number of input history entries to save
    pub max_input_history: usize,
    /// Default provider override (e.g. "deepseek", "openai").
    pub default_provider: Option<String>,
    /// Default model to use
    pub default_model: Option<String>,
    /// Default reasoning effort selected in the REPL.
    /// `None` falls back to `config.toml` and then the runtime default.
    pub reasoning_effort: Option<String>,
    /// Per-provider model overrides. Key is provider name (e.g. "openai"),
    /// value is the model id. Takes precedence over `default_model`.
    pub provider_models: Option<std::collections::HashMap<String, String>>,
    /// Prefer the external `pdftotext` binary (Poppler) over the bundled
    /// pure-Rust `pdf-extract` extractor for PDF reads in `read_file`.
    /// Pure-Rust extraction is the v0.8.32 default because it removes the
    /// install-poppler-first hurdle most users hit, but `pdftotext -layout`
    /// still wins for column-heavy or complex-table PDFs (academic papers
    /// laid out in two columns, financial filings, etc.). Set to `true` to
    /// route every PDF read through `pdftotext` instead — when the binary
    /// is missing in that mode the tool returns the structured
    /// `binary_unavailable` response with an install hint, matching the
    /// pre-v0.8.32 behavior.
    pub prefer_external_pdftotext: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Auto-compaction is provider-neutral by default. It prevents
            // 128K/200K providers from reaching hard context errors while
            // explicit `auto_compact = off` remains available for cache-first
            // workflows.
            auto_compact: true,
            bracketed_paste: true,
            mention_menu_limit: 128,
            mention_walk_depth: 6,
            show_thinking: true,
            is_simple: false,
            locale: "auto".to_string(),
            theme: "terminal".to_string(),
            prompt_color: None,
            statusline_command: None,
            respond_to_shell_commands: true,
            composer_vim_mode: "normal".to_string(),
            default_mode: "agent".to_string(),
            active_preset: None,
            max_input_history: 500,
            default_provider: None,
            default_model: None,
            reasoning_effort: None,
            provider_models: None,
            prefer_external_pdftotext: false,
        }
    }
}

impl Settings {
    /// Get the settings file path
    pub fn path() -> Result<PathBuf> {
        // Shared with the CLI first-run wizard (language question) so the
        // two binaries cannot drift on where settings live.
        codesmith_agent_runtime::settings_file::settings_file_path()
    }

    /// Load settings from disk, or return defaults if not found
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read settings from {}", path.display()))?;
        let mut s: Settings = toml::from_str(&content)
            .with_context(|| format!("Failed to parse settings from {}", path.display()))?;
        s.default_mode = normalize_mode(&s.default_mode).to_string();
        // Load stays lenient, but an unrecognized persisted value falls back
        // to the SAME default a fresh install gets (not a second, diverging
        // fallback), and says so — otherwise a typo silently changes
        // behavior with nothing to discover why.
        s.locale = match normalize_configured_locale(&s.locale) {
            Some(locale) => locale.to_string(),
            None => {
                tracing::warn!(
                    "settings: unrecognized locale '{}', using default 'auto'",
                    s.locale
                );
                "auto".to_string()
            }
        };
        s.theme = match normalize_theme_name(&s.theme) {
            Some(theme) => theme.to_string(),
            None => {
                tracing::warn!(
                    "settings: unrecognized theme '{}', using default 'terminal'",
                    s.theme
                );
                "terminal".to_string()
            }
        };
        s.default_model = s.default_model.as_deref().and_then(normalize_default_model);
        s.reasoning_effort = match s.reasoning_effort.as_deref() {
            Some(value) => match normalize_reasoning_effort_setting(value) {
                Ok(normalized) => normalized,
                Err(err) => {
                    tracing::warn!("settings: ignoring persisted reasoning_effort: {err}");
                    None
                }
            },
            None => None,
        };
        Ok(s)
    }

    /// Save settings to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;

        // Create config directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }

        // Same advisory lock `write_settings_locale` takes: an unlocked
        // full-file save racing the runtime's key-preserving merge would
        // have both write from the same baseline and the last rename would
        // silently drop the other's keys.
        let path_for_lock = path.clone();
        codesmith_agent_runtime::settings_file::with_settings_lock(&path_for_lock, move || {
            let content = toml::to_string_pretty(self).context("Failed to serialize settings")?;
            codesmith_agent_runtime::utils::write_atomic(&path, content.as_bytes())
                .with_context(|| format!("Failed to write settings to {}", path.display()))
        })
    }

    /// Get all settings as a displayable string
    pub fn display(&self, locale: crate::localization::Locale) -> String {
        use crate::localization::{MessageId, tr};
        let mut lines = Vec::new();
        lines.push(tr(locale, MessageId::SettingsTitle).to_string());
        lines.push("─────────────────────────────".to_string());
        lines.push(format!("  auto_compact:       {}", self.auto_compact));
        lines.push(format!("  bracketed_paste:    {}", self.bracketed_paste));
        lines.push(format!("  mention_menu_limit: {}", self.mention_menu_limit));
        lines.push(format!("  mention_walk_depth: {}", self.mention_walk_depth));
        lines.push(format!("  show_thinking:      {}", self.show_thinking));
        lines.push(format!("  locale:            {}", self.locale));
        lines.push(format!("  theme:              {}", self.theme));
        lines.push(format!("  composer_vim_mode:  {}", self.composer_vim_mode));
        lines.push(format!(
            "  prefer_external_pdftotext: {}",
            self.prefer_external_pdftotext
        ));
        lines.push(format!("  default_mode:       {}", self.default_mode));
        lines.push(format!("  max_history:        {}", self.max_input_history));
        lines.push(format!(
            "  default_model:      {}",
            self.default_model.as_deref().unwrap_or("(default)")
        ));
        lines.push(format!(
            "  reasoning_effort:   {}",
            self.reasoning_effort
                .as_deref()
                .unwrap_or("(config/default)")
        ));
        lines.push(String::new());
        lines.push(format!(
            "{} {}",
            tr(locale, MessageId::SettingsConfigFile),
            Self::path().map_or_else(|_| "(unknown)".to_string(), |p| p.display().to_string())
        ));
        lines.join("\n")
    }
}

fn normalize_default_model(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("auto") {
        Some("auto".to_string())
    } else {
        normalize_model_name(trimmed)
    }
}

fn normalize_reasoning_effort_setting(value: &str) -> Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "default" | "(default)" | "config" | "configured" | "unset"
        )
    {
        return Ok(None);
    }

    let normalized = match trimmed.to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" => "off",
        "low" | "minimal" => "low",
        "medium" | "mid" => "medium",
        "high" => "high",
        "auto" | "automatic" => "auto",
        "max" | "maximum" | "xhigh" => "max",
        _ => {
            anyhow::bail!(
                "Failed to update setting: invalid reasoning_effort '{value}'. Expected: auto, off, low, medium, high, max, or default."
            );
        }
    };
    Ok(Some(normalized.to_string()))
}

fn normalize_mode(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "edit" => "agent",
        "normal" => "agent",
        "agent" => "agent",
        "plan" => "plan",
        "yolo" => "yolo",
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_enable_provider_neutral_auto_compact() {
        let settings = Settings::default();
        assert!(settings.auto_compact);
    }

    #[test]
    fn display_localizes_header_and_config_file_label() {
        let settings = Settings::default();
        let en = settings.display(crate::localization::Locale::En);
        assert!(en.contains("Settings:"), "english header missing:\n{en}");
        assert!(
            en.contains("Config file:"),
            "english config label missing:\n{en}"
        );

        let zh = settings.display(crate::localization::Locale::ZhHans);
        assert!(zh.contains("设置"), "chinese header missing:\n{zh}");
        assert!(
            zh.contains("配置文件"),
            "chinese config label missing:\n{zh}"
        );
    }
}
