//! Settings system - Persistent user preferences
//!
//! Settings are stored at ~/.config/deepseek/settings.toml

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

/// Validate/normalize a `#rrggbb` color (moved from the retired palette,
/// which round-tripped through RGB and reformatted lowercased).
pub(crate) fn normalize_hex_rgb_color(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{}", hex.to_ascii_lowercase()))
    } else {
        None
    }
}

/// User settings with defaults
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Auto-compact conversations when they approach the model limit.
    pub auto_compact: bool,
    /// Reduce status noise and collapse details more aggressively
    pub calm_mode: bool,
    /// Streaming pacing mode. `true` pins the chunker to one-character-per-
    /// commit-tick (typewriter); `false` drains the upstream cadence (each
    /// commit flushes everything queued, which matches V4-pro's burst pattern
    /// when the prefix cache is warm). Has no effect on the footer water-spout
    /// animation — that is gated independently by [`Self::fancy_animations`].
    pub low_motion: bool,
    /// Enable the footer water-spout animation strip during live turns. The
    /// strip's wave cadence is synchronized with the character-commit rate, so
    /// the visual flow matches whatever streaming pacing [`Self::low_motion`]
    /// selects: typewriter mode drips, upstream mode surges, tool calls /
    /// planning pauses freeze the surface. Set `false` to keep the gap as
    /// plain whitespace.
    pub fancy_animations: bool,
    /// Enable terminal bracketed-paste mode. Default true. Disable if your
    /// terminal mishandles the `\e[?2004h` escape (rare; some legacy
    /// terminals over SSH+screen multiplex without the cap).
    pub bracketed_paste: bool,
    /// Enable rapid-key paste-burst detection for terminals that do not emit
    /// bracketed-paste events. Independent from `bracketed_paste`.
    pub paste_burst_detection: bool,
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
    /// Show detailed tool output
    pub show_tool_details: bool,
    /// UI locale: auto, en, zh-Hans, zh-Hant, hi, es-419
    pub locale: String,
    /// Named UI theme. Defaults to `"terminal"`, which inherits the host
    /// terminal's colors (transparent surfaces, ANSI accents) the same way
    /// CLI output does. Also accepts `"system"` (COLORFGBG / macOS
    /// appearance detection), `"dark"`, `"light"`, `"grayscale"`, or one of
    /// the community presets: `"catppuccin-mocha"`, `"tokyo-night"`,
    /// `"dracula"`, `"gruvbox-dark"`. The `background_color` setting still
    /// overrides the surface color on top of the resolved theme.
    pub theme: String,
    /// Optional main terminal background color as a 6-digit hex RGB value.
    pub background_color: Option<String>,
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
    /// Composer layout density: compact, comfortable, spacious
    pub composer_density: String,
    /// Show a border around the composer input area
    pub composer_border: bool,
    /// Composer editing mode: "normal" (default) or "vim" for modal editing.
    /// When set to "vim" the composer starts in Normal mode; press i/a/o to
    /// enter Insert mode and Esc to return to Normal.
    pub composer_vim_mode: String,
    /// Transcript spacing rhythm: compact, comfortable, spacious
    pub transcript_spacing: String,
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
    /// Sidebar width as percentage of terminal width
    pub sidebar_width_percent: u16,
    /// Sidebar focus mode: auto, work, tasks, agents, context, hidden
    pub sidebar_focus: String,
    /// Enable the session-context panel (#504). Shows working set, tokens,
    /// cost, MCP/LSP status, cycle count, and memory info.
    pub context_panel: bool,
    /// Cost display currency: usd or cny.
    pub cost_currency: String,
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
    /// Header status indicator next to the effort chip. Cycles through a
    /// per-turn animation keyed off `App::turn_started_at`:
    /// - `"dots"` (default): the 6-frame geometric sequence (`◍ ◉ ◌ ◌ ◉ ◍`).
    ///   Idle frame is a steady `◍`.
    /// - `"off"`: hide the indicator entirely.
    pub status_indicator: String,
    /// Whether to wrap each draw in DEC mode 2026 synchronized output
    /// (`\x1b[?2026h` … `\x1b[?2026l`). Synchronized output asks the
    /// terminal to defer rendering until the whole frame is staged so
    /// GPU-accelerated terminals (Ghostty, VS Code, Kitty, WezTerm)
    /// don't flash a blank intermediate frame.
    ///
    /// - `"auto"` (default): emit DEC 2026 unless an environment signal
    ///   says the active terminal mishandles it (currently Ptyxis 50.x
    ///   on VTE 0.84.x — see [`Settings::apply_env_overrides`]).
    /// - `"on"`: always emit DEC 2026 (override the auto opt-out).
    /// - `"off"`: never emit DEC 2026. Use this if your terminal flashes
    ///   the whole screen on every redraw — most often Ptyxis on
    ///   Ubuntu 26.04 today; historically also some legacy ssh+screen
    ///   stacks. The cost of `off` is brief tearing on terminals that
    ///   *do* support DEC 2026; it is purely a rendering-quality knob,
    ///   not a correctness one.
    pub synchronized_output: String,
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
            calm_mode: false,
            low_motion: false,
            fancy_animations: true,
            bracketed_paste: true,
            paste_burst_detection: true,
            mention_menu_limit: 128,
            mention_walk_depth: 6,
            show_thinking: true,
            is_simple: false,
            show_tool_details: true,
            locale: "auto".to_string(),
            theme: "terminal".to_string(),
            background_color: None,
            prompt_color: None,
            statusline_command: None,
            respond_to_shell_commands: true,
            composer_density: "comfortable".to_string(),
            composer_border: true,
            composer_vim_mode: "normal".to_string(),
            transcript_spacing: "comfortable".to_string(),
            default_mode: "agent".to_string(),
            active_preset: None,
            sidebar_width_percent: 28,
            sidebar_focus: "auto".to_string(),
            context_panel: false,
            cost_currency: "usd".to_string(),
            max_input_history: 500,
            default_provider: None,
            default_model: None,
            reasoning_effort: None,
            provider_models: None,
            status_indicator: "dots".to_string(),
            synchronized_output: "auto".to_string(),
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
        let mut settings = if !path.exists() {
            Self::default()
        } else {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read settings from {}", path.display()))?;
            let mut s: Settings = toml::from_str(&content)
                .with_context(|| format!("Failed to parse settings from {}", path.display()))?;
            s.default_mode = normalize_mode(&s.default_mode).to_string();
            s.composer_density = normalize_composer_density(&s.composer_density).to_string();
            s.transcript_spacing = normalize_transcript_spacing(&s.transcript_spacing).to_string();
            s.sidebar_focus = normalize_sidebar_focus(&s.sidebar_focus).to_string();
            s.status_indicator = normalize_status_indicator(&s.status_indicator).to_string();
            s.synchronized_output =
                normalize_synchronized_output(&s.synchronized_output).to_string();
            s.locale = normalize_configured_locale(&s.locale)
                .unwrap_or("en")
                .to_string();
            s.background_color = normalize_optional_background_color(s.background_color.as_deref());
            s.theme = normalize_settings_theme(&s.theme).to_string();
            s.default_model = s.default_model.as_deref().and_then(normalize_default_model);
            s.reasoning_effort = s
                .reasoning_effort
                .as_deref()
                .and_then(|value| normalize_reasoning_effort_setting(value).ok().flatten());
            s
        };
        settings.apply_env_overrides();
        Ok(settings)
    }

    /// Apply environment-driven overlays after disk load. Used for
    /// platform a11y signals that should ignore the user's saved
    /// preference (#450). The env values are consulted at startup;
    /// changing them mid-session has no effect because settings are
    /// only re-read on `Settings::load()`.
    pub fn apply_env_overrides(&mut self) {
        if env_truthy("NO_ANIMATIONS") {
            self.low_motion = true;
            self.fancy_animations = false;
        }
        // VS Code (TERM_PROGRAM=vscode, #1356), Ghostty (TERM_PROGRAM=ghostty,
        // #1445), and a few VTE terminals (#1470) produce visible flicker at
        // 120 FPS. Drop to the 30 FPS low-motion cap for them automatically.
        // Like NO_ANIMATIONS above, this unconditionally overrides any
        // disk-loaded value — consistent precedence: env signals always win.
        let vte_env_forces_low_motion = std::env::var_os("TILIX_ID").is_some_and(|v| !v.is_empty())
            || std::env::var_os("TERMINATOR_UUID").is_some_and(|v| !v.is_empty());
        if matches!(
            std::env::var("TERM_PROGRAM").as_deref(),
            Ok("vscode") | Ok("ghostty")
        ) || vte_env_forces_low_motion
        {
            self.low_motion = true;
            self.fancy_animations = false;
        }

        // Termius (TERM_PROGRAM=Termius) and SSH sessions exhibit the
        // same 120-FPS flicker class as VS Code — the SSH round-trip
        // races ahead of what the remote renderer can flush, so rapid
        // cursor-positioning sequences cycle through input boxes.
        // Drop both to the 30 FPS low-motion cap. Harvested from
        // PR #1479 by @CrepuscularIRIS / autoghclaw (closes #1433).
        //
        // SSH_CLIENT is exported by sshd for every TCP SSH session;
        // SSH_TTY is exported only for interactive PTY logins, so we
        // check both so non-PTY-allocating tools (rsync wrappers, etc.)
        // still pick this up if they end up running the REPL.
        let term_is_termius = std::env::var("TERM_PROGRAM").as_deref() == Ok("Termius");
        let in_ssh_session = std::env::var_os("SSH_CLIENT").is_some_and(|v| !v.is_empty())
            || std::env::var_os("SSH_TTY").is_some_and(|v| !v.is_empty());
        if term_is_termius || in_ssh_session {
            self.low_motion = true;
            self.fancy_animations = false;
        }

        // tmux/screen activity monitors treat purely animated redraws as
        // activity. Keep multiplexer sessions calm by pinning animations.
        let in_terminal_multiplexer = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
            || std::env::var_os("STY").is_some_and(|v| !v.is_empty());
        if in_terminal_multiplexer {
            self.low_motion = true;
            self.fancy_animations = false;
        }

        // Plain Windows PowerShell / cmd.exe under legacy ConHost exposes none
        // of the modern terminal markers below. Keep rendering calmer there:
        // lower the motion rate, disable animated chrome, and avoid DEC 2026
        // synchronized-output wrapping unless the user explicitly forced it on.
        if detected_legacy_windows_console_host() {
            self.low_motion = true;
            self.fancy_animations = false;
            if self.synchronized_output.eq_ignore_ascii_case("auto") {
                self.synchronized_output = "off".to_string();
            }
        }

        // Ptyxis 50.x (the new default terminal on Ubuntu 26.04) ships with
        // VTE 0.84.x which mishandles DEC mode 2026 synchronized output: the
        // begin/end pair is parsed but each wrapped frame still triggers a
        // full-viewport flash on the GPU compositor side, so any TUI that
        // uses DEC 2026 to avoid tearing instead gets visible flicker on
        // every redraw. gnome-terminal 3.58 on the same VTE renders cleanly,
        // so we can't broaden the opt-out to all VTE-based terminals —
        // only the Ptyxis-specific signals trigger it. Confirmed
        // user-visible regression starting with Ubuntu 26.04's default
        // terminal swap; cargo-installed binaries are not exempt because
        // the bug is in the terminal, not the binary.
        //
        // Only flip `auto` to `off`; respect an explicit `"on"` so users
        // who upgrade Ptyxis or want to confirm the fix landed upstream
        // can override the heuristic from `~/.config/deepseek/settings.toml`
        // or `/set synchronized_output on`.
        if self.synchronized_output.eq_ignore_ascii_case("auto") && detected_ptyxis_terminal() {
            self.synchronized_output = "off".to_string();
        }
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

        let content = toml::to_string_pretty(self).context("Failed to serialize settings")?;
        crate::utils::write_atomic(&path, content.as_bytes())
            .with_context(|| format!("Failed to write settings to {}", path.display()))?;
        Ok(())
    }

    /// Get all settings as a displayable string
    pub fn display(&self, locale: crate::localization::Locale) -> String {
        use crate::localization::{MessageId, tr};
        let mut lines = Vec::new();
        lines.push(tr(locale, MessageId::SettingsTitle).to_string());
        lines.push("─────────────────────────────".to_string());
        lines.push(format!("  auto_compact:       {}", self.auto_compact));
        lines.push(format!("  calm_mode:          {}", self.calm_mode));
        lines.push(format!("  low_motion:         {}", self.low_motion));
        lines.push(format!("  fancy_animations:   {}", self.fancy_animations));
        lines.push(format!("  bracketed_paste:    {}", self.bracketed_paste));
        lines.push(format!(
            "  paste_burst_detect: {}",
            self.paste_burst_detection
        ));
        lines.push(format!("  mention_menu_limit: {}", self.mention_menu_limit));
        lines.push(format!("  mention_walk_depth: {}", self.mention_walk_depth));
        lines.push(format!("  show_thinking:      {}", self.show_thinking));
        lines.push(format!("  show_tool_details:  {}", self.show_tool_details));
        lines.push(format!("  locale:            {}", self.locale));
        lines.push(format!("  theme:              {}", self.theme));
        lines.push(format!(
            "  background_color:   {}",
            self.background_color.as_deref().unwrap_or("(default)")
        ));
        lines.push(format!("  composer_density:   {}", self.composer_density));
        lines.push(format!("  composer_border:    {}", self.composer_border));
        lines.push(format!("  composer_vim_mode:  {}", self.composer_vim_mode));
        lines.push(format!("  transcript_spacing: {}", self.transcript_spacing));
        lines.push(format!("  status_indicator:   {}", self.status_indicator));
        lines.push(format!(
            "  synchronized_output: {}",
            self.synchronized_output
        ));
        lines.push(format!(
            "  prefer_external_pdftotext: {}",
            self.prefer_external_pdftotext
        ));
        lines.push(format!("  default_mode:       {}", self.default_mode));
        lines.push(format!(
            "  sidebar_width:      {}%",
            self.sidebar_width_percent
        ));
        lines.push(format!("  sidebar_focus:      {}", self.sidebar_focus));
        lines.push(format!("  context_panel:      {}", self.context_panel));
        lines.push(format!("  cost_currency:      {}", self.cost_currency));
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

    /// Get available setting keys and their descriptions
    #[allow(dead_code)]
    pub fn available_settings() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "auto_compact",
                "Auto-compact near the hard context limit: on/off (default off)",
            ),
            ("calm_mode", "Calmer UI defaults: on/off"),
            (
                "low_motion",
                "Streaming pacing: on = typewriter (one char/tick), off = upstream cadence",
            ),
            (
                "fancy_animations",
                "Footer water-spout strip (wave synced to typing speed): on/off",
            ),
            (
                "bracketed_paste",
                "Terminal bracketed-paste mode: on/off (rare to disable)",
            ),
            (
                "paste_burst_detection",
                "Fallback rapid-key paste detection: on/off",
            ),
            (
                "mention_menu_limit",
                "Maximum @-mention popup candidates retained before rendering (default 128)",
            ),
            (
                "mention_walk_depth",
                "Maximum @-mention workspace walk depth; 0 means unlimited (default 6)",
            ),
            ("show_thinking", "Show model thinking: on/off"),
            ("show_tool_details", "Show detailed tool output: on/off"),
            (
                "base_url",
                "HTTP base URL for DeepSeek-compatible endpoints.",
            ),
            (
                "locale",
                "UI locale and default model language: auto, en, zh-Hans, zh-Hant, hi, es-419",
            ),
            (
                "theme",
                "UI theme: terminal, system, dark, light, grayscale, catppuccin-mocha, tokyo-night, dracula, gruvbox-dark, solarized-light",
            ),
            (
                "background_color",
                "Main terminal background color: #RRGGBB or default",
            ),
            (
                "composer_density",
                "Composer density: compact, comfortable, spacious",
            ),
            (
                "composer_border",
                "Show a border around the composer input area: on/off",
            ),
            ("composer_vim_mode", "Composer editing mode: normal, vim"),
            (
                "transcript_spacing",
                "Transcript spacing: compact, comfortable, spacious",
            ),
            (
                "status_indicator",
                "Header status indicator next to effort chip: dots, off",
            ),
            (
                "synchronized_output",
                "DEC 2026 synchronized output: auto, on, off (set off if your terminal flickers)",
            ),
            (
                "prefer_external_pdftotext",
                "Route PDF reads through Poppler's pdftotext instead of the bundled pure-Rust extractor: on/off (default off)",
            ),
            ("default_mode", "Default mode: agent, plan, yolo"),
            ("sidebar_width", "Sidebar width percentage: 10-50"),
            (
                "sidebar_focus",
                "Sidebar focus: auto, work, tasks, agents, context, hidden",
            ),
            (
                "context_panel",
                "Show the session context sidebar panel: on/off",
            ),
            ("cost_currency", "Cost display currency: usd, cny"),
            ("max_history", "Max input history entries"),
            (
                "default_model",
                "Default model: auto or any DeepSeek model ID (e.g. deepseek-v4-pro)",
            ),
            (
                "reasoning_effort",
                "Default thinking effort: auto, off, low, medium, high, max, or default",
            ),
        ]
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

fn normalize_composer_density(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "compact" | "tight" => "compact",
        "comfortable" | "default" | "normal" => "comfortable",
        "spacious" | "loose" => "spacious",
        _ => value,
    }
}

fn normalize_transcript_spacing(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "compact" | "tight" => "compact",
        "comfortable" | "default" | "normal" => "comfortable",
        "spacious" | "loose" => "spacious",
        _ => value,
    }
}

/// Normalize the `status_indicator` header chip setting. Accepts the
/// canonical names plus common aliases ("none"/"hidden" → "off",
/// "dot" → "dots"). Unknown values fall through unchanged so the parser
/// in `update_setting` can surface a clear error.
fn normalize_status_indicator(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "dots" | "dot" => "dots",
        "off" | "none" | "hidden" | "false" => "off",
        _ => value,
    }
}

/// Normalize the `synchronized_output` setting. Accepts the canonical
/// `"auto"` / `"on"` / `"off"` plus the usual truthy/falsey spellings.
/// Unknown values fall through unchanged so the parser in `set` can
/// surface a clear error.
fn normalize_synchronized_output(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" | "default" => "auto",
        "on" | "true" | "yes" | "1" | "enabled" => "on",
        "off" | "false" | "no" | "0" | "disabled" => "off",
        _ => value,
    }
}

fn normalize_settings_theme(value: &str) -> &'static str {
    normalize_theme_name(value).unwrap_or("system")
}

/// Returns `true` when the active terminal is Ptyxis (the new default
/// terminal on Ubuntu 26.04). Used by [`Settings::apply_env_overrides`]
/// to flip `synchronized_output` from `auto` to `off` so DEC mode 2026
/// flicker on Ptyxis 50.x + VTE 0.84.x stops at the source.
///
/// We deliberately keep this narrow:
///
/// - `TERM_PROGRAM` matches `ptyxis` case-insensitively (the value
///   Ptyxis sets when it forwards a process-launch context).
/// - `PTYXIS_VERSION` is set to any non-empty value (the binary's
///   own version probe, present whether or not `TERM_PROGRAM` made it
///   into the child environment).
///
/// Either signal is sufficient. We do *not* trigger on `VTE_VERSION`
/// alone because gnome-terminal 3.58 ships with the same VTE 0.84.x
/// and renders cleanly — broadening the heuristic would regress every
/// gnome-terminal user.
pub fn detected_ptyxis_terminal() -> bool {
    if let Ok(program) = std::env::var("TERM_PROGRAM")
        && program.trim().to_ascii_lowercase().contains("ptyxis")
    {
        return true;
    }
    matches!(std::env::var("PTYXIS_VERSION"), Ok(v) if !v.trim().is_empty())
}

/// Returns `true` for the unmarked Windows console-host path used by plain
/// PowerShell / cmd.exe. Modern Windows terminals set at least one marker that
/// lets us keep the richer rendering path.
pub fn detected_legacy_windows_console_host() -> bool {
    cfg!(windows)
        && legacy_windows_console_host_env([
            std::env::var_os("WT_SESSION").as_deref(),
            std::env::var_os("ConEmuPID").as_deref(),
            std::env::var_os("TERM_PROGRAM").as_deref(),
            std::env::var_os("WEZTERM_EXECUTABLE").as_deref(),
            std::env::var_os("WEZTERM_PANE").as_deref(),
            std::env::var_os("ALACRITTY_WINDOW_ID").as_deref(),
            std::env::var_os("ANSICON").as_deref(),
            std::env::var_os("TERM").as_deref(),
        ])
}

fn legacy_windows_console_host_env(markers: [Option<&std::ffi::OsStr>; 8]) -> bool {
    fn has_value(value: Option<&std::ffi::OsStr>) -> bool {
        value.is_some_and(|v| !v.is_empty())
    }

    markers.into_iter().all(|value| !has_value(value))
}

fn normalize_optional_background_color(value: Option<&str>) -> Option<String> {
    value.and_then(|raw| normalize_background_color_setting(raw).ok().flatten())
}

fn normalize_background_color_setting(value: &str) -> Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "default" | "none" | "reset" | "off"
        )
    {
        return Ok(None);
    }

    normalize_hex_rgb_color(trimmed).map(Some).ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to update setting: invalid background_color '{value}'. Expected #RRGGBB, RRGGBB, or default."
        )
    })
}

fn normalize_sidebar_focus(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "work" | "plan" | "todos" => "work",
        "tasks" => "tasks",
        "agents" | "subagents" | "sub-agents" => "agents",
        "context" | "session" => "context",
        "hidden" | "hide" | "closed" | "off" | "none" => "hidden",
        _ => "auto",
    }
}

/// Resolve an environment variable as a boolean. Recognises the
/// common truthy spellings (`1`, `true`, `yes`, `on`) case-
/// insensitively. Used by [`Settings::apply_env_overrides`] for
/// platform a11y signals like `NO_ANIMATIONS`.
fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
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
    fn default_settings_show_footer_water_strip() {
        let settings = Settings::default();
        assert!(settings.fancy_animations);
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

    /// Tests that mutate process-global `NO_ANIMATIONS` serialise
    /// through this guard so the cargo parallel runner doesn't
    /// observe interleaved overrides. Uses the process-wide test env
    /// lock so this serializes with the TERM_PROGRAM tests too —
    /// otherwise a `NO_ANIMATIONS=1` leak from this test family can
    /// flip a concurrent `TERM_PROGRAM=iTerm` test's `low_motion`
    /// assertion through the shared `apply_env_overrides` path.
    fn no_animations_test_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::test_support::lock_test_env()
    }

    #[test]
    fn no_animations_env_forces_low_motion_on() {
        let _g = no_animations_test_guard();
        // SAFETY: tests in this group serialise through the guard.
        unsafe {
            std::env::set_var("NO_ANIMATIONS", "1");
        }
        let mut settings = Settings::default();
        assert!(!settings.low_motion, "default is animated");
        assert!(settings.fancy_animations, "default shows the water strip");
        settings.apply_env_overrides();
        assert!(settings.low_motion, "NO_ANIMATIONS=1 forces low_motion");
        assert!(
            !settings.fancy_animations,
            "NO_ANIMATIONS=1 keeps fancy off"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("NO_ANIMATIONS");
        }
    }

    #[test]
    fn no_animations_env_overrides_user_opt_in() {
        let _g = no_animations_test_guard();
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("NO_ANIMATIONS", "true");
        }
        // User had explicitly opted into fancy animations on disk.
        let mut settings = Settings {
            fancy_animations: true,
            ..Settings::default()
        };
        settings.apply_env_overrides();
        assert!(
            !settings.fancy_animations,
            "platform NO_ANIMATIONS overrides user-opt-in fancy_animations"
        );
        assert!(settings.low_motion);
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("NO_ANIMATIONS");
        }
    }

    #[test]
    fn no_animations_env_recognises_truthy_spellings_only() {
        let _g = no_animations_test_guard();
        let prev_wt_session = std::env::var_os("WT_SESSION");
        let prev_tmux = std::env::var_os("TMUX");
        let prev_sty = std::env::var_os("STY");
        let prev_term_program = std::env::var_os("TERM_PROGRAM");
        let prev_ssh_client = std::env::var_os("SSH_CLIENT");
        let prev_ssh_tty = std::env::var_os("SSH_TTY");
        let prev_tilix_id = std::env::var_os("TILIX_ID");
        let prev_terminator_uuid = std::env::var_os("TERMINATOR_UUID");

        // The test is about NO_ANIMATIONS only. On Windows CI, an unmarked
        // console host now independently enables low_motion, so mark the host
        // as non-legacy while checking falsy spellings.
        // Clear multiplexer markers for the same reason: they also force
        // low_motion independently of NO_ANIMATIONS.
        // Clear TERM_PROGRAM, SSH, and other terminal-specific variables as they
        // also force low_motion independently of NO_ANIMATIONS.
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::remove_var("TMUX");
            std::env::remove_var("STY");
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("SSH_CLIENT");
            std::env::remove_var("SSH_TTY");
            std::env::remove_var("TILIX_ID");
            std::env::remove_var("TERMINATOR_UUID");
        }
        #[cfg(windows)]
        unsafe {
            std::env::set_var("WT_SESSION", "test");
        }
        for truthy in ["1", "true", "True", "YES", "on"] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::set_var("NO_ANIMATIONS", truthy);
            }
            let mut s = Settings::default();
            s.apply_env_overrides();
            assert!(s.low_motion, "{truthy:?} should be truthy");
        }
        for falsy in ["0", "false", "no", "off", ""] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::set_var("NO_ANIMATIONS", falsy);
            }
            let mut s = Settings::default();
            s.apply_env_overrides();
            assert!(!s.low_motion, "{falsy:?} should be falsy");
        }
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("NO_ANIMATIONS");
            match prev_wt_session {
                Some(v) => std::env::set_var("WT_SESSION", v),
                None => std::env::remove_var("WT_SESSION"),
            }
            match prev_tmux {
                Some(v) => std::env::set_var("TMUX", v),
                None => std::env::remove_var("TMUX"),
            }
            match prev_sty {
                Some(v) => std::env::set_var("STY", v),
                None => std::env::remove_var("STY"),
            }
            match prev_term_program {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_ssh_client {
                Some(v) => std::env::set_var("SSH_CLIENT", v),
                None => std::env::remove_var("SSH_CLIENT"),
            }
            match prev_ssh_tty {
                Some(v) => std::env::set_var("SSH_TTY", v),
                None => std::env::remove_var("SSH_TTY"),
            }
            match prev_tilix_id {
                Some(v) => std::env::set_var("TILIX_ID", v),
                None => std::env::remove_var("TILIX_ID"),
            }
            match prev_terminator_uuid {
                Some(v) => std::env::set_var("TERMINATOR_UUID", v),
                None => std::env::remove_var("TERMINATOR_UUID"),
            }
        }
    }

    /// Serialise tests that mutate `TERM_PROGRAM` through this guard.
    /// Uses the process-wide test env lock so this serializes not just
    /// with itself but with every other env-mutating test in the suite
    /// — otherwise a concurrent test that calls `Settings::default()`
    /// can read whatever value our two `set_var`s have raced into the
    /// env at that instant.
    fn term_program_test_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::test_support::lock_test_env()
    }

    #[test]
    fn vscode_term_program_forces_low_motion_on() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "vscode");
        }
        let mut settings = Settings::default();
        assert!(!settings.low_motion, "default is animated");
        settings.apply_env_overrides();
        assert!(
            settings.low_motion,
            "TERM_PROGRAM=vscode must enable low_motion to prevent flickering (#1356)"
        );
        assert!(
            !settings.fancy_animations,
            "TERM_PROGRAM=vscode must disable fancy_animations"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn ghostty_term_program_forces_low_motion_on() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "ghostty");
        }
        let mut settings = Settings::default();
        assert!(!settings.low_motion, "default is animated");
        settings.apply_env_overrides();
        assert!(
            settings.low_motion,
            "TERM_PROGRAM=ghostty must enable low_motion to prevent flickering (#1445)"
        );
        assert!(
            !settings.fancy_animations,
            "TERM_PROGRAM=ghostty must disable fancy_animations"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn non_vscode_term_program_does_not_force_low_motion() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        let prev_ssh_client = std::env::var_os("SSH_CLIENT");
        let prev_ssh_tty = std::env::var_os("SSH_TTY");
        let prev_tilix_id = std::env::var_os("TILIX_ID");
        let prev_terminator_uuid = std::env::var_os("TERMINATOR_UUID");
        let prev_tmux = std::env::var_os("TMUX");
        let prev_sty = std::env::var_os("STY");
        // SAFETY: serialised by the guard. Clear SSH_* so a real
        // SSH session running the test suite doesn't make this
        // assertion trivially fail — the SSH path is exercised
        // separately by `ssh_session_forces_low_motion_on`.
        unsafe {
            std::env::remove_var("SSH_CLIENT");
            std::env::remove_var("SSH_TTY");
            std::env::remove_var("TILIX_ID");
            std::env::remove_var("TERMINATOR_UUID");
            std::env::remove_var("TMUX");
            std::env::remove_var("STY");
        }
        for program in ["iTerm.app", "Apple_Terminal", "WezTerm", "xterm-256color"] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::set_var("TERM_PROGRAM", program);
            }
            let mut s = Settings::default();
            s.apply_env_overrides();
            assert!(
                !s.low_motion,
                "TERM_PROGRAM={program:?} should not force low_motion"
            );
        }
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            if let Some(v) = prev_ssh_client {
                std::env::set_var("SSH_CLIENT", v);
            }
            if let Some(v) = prev_ssh_tty {
                std::env::set_var("SSH_TTY", v);
            }
            if let Some(v) = prev_tilix_id {
                std::env::set_var("TILIX_ID", v);
            }
            if let Some(v) = prev_terminator_uuid {
                std::env::set_var("TERMINATOR_UUID", v);
            }
            if let Some(v) = prev_tmux {
                std::env::set_var("TMUX", v);
            }
            if let Some(v) = prev_sty {
                std::env::set_var("STY", v);
            }
        }
    }

    #[test]
    fn tilix_and_terminator_env_force_low_motion_on() {
        let _g = term_program_test_guard();
        let prev_term_program = std::env::var_os("TERM_PROGRAM");
        let prev_tilix_id = std::env::var_os("TILIX_ID");
        let prev_terminator_uuid = std::env::var_os("TERMINATOR_UUID");

        for (var, val) in [
            ("TILIX_ID", "d5b5b5d6-tilix-session"),
            ("TERMINATOR_UUID", "urn:uuid:terminator-session"),
        ] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::remove_var("TERM_PROGRAM");
                std::env::remove_var("TILIX_ID");
                std::env::remove_var("TERMINATOR_UUID");
                std::env::set_var(var, val);
            }
            let mut settings = Settings::default();
            assert!(!settings.low_motion, "default is animated");
            settings.apply_env_overrides();
            assert!(
                settings.low_motion,
                "{var} must enable low_motion to prevent VTE flicker (#1470)"
            );
            assert!(
                !settings.fancy_animations,
                "{var} must disable fancy_animations"
            );
        }

        // SAFETY: cleanup under the guard.
        unsafe {
            match prev_term_program {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_tilix_id {
                Some(v) => std::env::set_var("TILIX_ID", v),
                None => std::env::remove_var("TILIX_ID"),
            }
            match prev_terminator_uuid {
                Some(v) => std::env::set_var("TERMINATOR_UUID", v),
                None => std::env::remove_var("TERMINATOR_UUID"),
            }
        }
    }

    #[test]
    fn termius_term_program_forces_low_motion_on() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "Termius");
        }
        let mut settings = Settings::default();
        assert!(!settings.low_motion, "default is animated");
        settings.apply_env_overrides();
        assert!(
            settings.low_motion,
            "TERM_PROGRAM=Termius must enable low_motion to prevent flickering (#1433)"
        );
        assert!(
            !settings.fancy_animations,
            "TERM_PROGRAM=Termius must disable fancy_animations"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn legacy_windows_console_host_detects_unmarked_shell() {
        assert!(legacy_windows_console_host_env([
            None, None, None, None, None, None, None, None
        ]));
    }

    #[test]
    fn legacy_windows_console_host_excludes_modern_terminal_markers() {
        use std::ffi::OsStr;

        let marker = Some(OsStr::new("1"));
        assert!(!legacy_windows_console_host_env([
            marker, None, None, None, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, marker, None, None, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, marker, None, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, marker, None, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, marker, None, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, None, marker, None, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, None, None, marker, None
        ]));
        assert!(!legacy_windows_console_host_env([
            None, None, None, None, None, None, None, marker
        ]));
    }

    #[cfg(windows)]
    #[test]
    fn unmarked_windows_console_forces_calm_rendering() {
        let _g = term_program_test_guard();
        let vars = [
            "WT_SESSION",
            "ConEmuPID",
            "TERM_PROGRAM",
            "WEZTERM_EXECUTABLE",
            "WEZTERM_PANE",
            "ALACRITTY_WINDOW_ID",
            "ANSICON",
            "TERM",
            "SSH_CLIENT",
            "SSH_TTY",
            "NO_ANIMATIONS",
            "PTYXIS_VERSION",
        ];
        let prev: Vec<_> = vars
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();

        // SAFETY: serialised by the guard.
        unsafe {
            for name in vars {
                std::env::remove_var(name);
            }
        }

        let mut settings = Settings::default();
        assert!(!settings.low_motion, "default is animated");
        assert!(settings.fancy_animations, "default shows the water strip");
        assert_eq!(settings.synchronized_output, "auto");
        settings.apply_env_overrides();
        assert!(settings.low_motion);
        assert!(!settings.fancy_animations);
        assert_eq!(settings.synchronized_output, "off");

        // SAFETY: cleanup under the guard.
        unsafe {
            for (name, value) in prev {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn ssh_session_forces_low_motion_on() {
        let _g = term_program_test_guard();
        let prev_client = std::env::var_os("SSH_CLIENT");
        let prev_tty = std::env::var_os("SSH_TTY");
        let prev_term_program = std::env::var_os("TERM_PROGRAM");
        for (var, val) in [
            ("SSH_CLIENT", "192.168.1.100 50000 22"),
            ("SSH_TTY", "/dev/pts/0"),
        ] {
            // SAFETY: serialised by the guard.
            unsafe {
                std::env::remove_var("SSH_CLIENT");
                std::env::remove_var("SSH_TTY");
                // Clear TERM_PROGRAM so the test isolates the SSH signal
                // — otherwise a leaked `TERM_PROGRAM=vscode` from a
                // concurrent test would already have forced low_motion
                // and the SSH-only assertion below would be a tautology.
                std::env::remove_var("TERM_PROGRAM");
                std::env::set_var(var, val);
            }
            let mut s = Settings::default();
            s.apply_env_overrides();
            assert!(
                s.low_motion,
                "{var}={val:?} must enable low_motion to prevent flickering in SSH sessions (#1433)"
            );
            assert!(
                !s.fancy_animations,
                "{var}={val:?} must disable fancy_animations in SSH sessions (#1433)"
            );
        }
        // SAFETY: cleanup under the guard.
        unsafe {
            std::env::remove_var("SSH_CLIENT");
            std::env::remove_var("SSH_TTY");
            if let Some(v) = prev_client {
                std::env::set_var("SSH_CLIENT", v);
            }
            if let Some(v) = prev_tty {
                std::env::set_var("SSH_TTY", v);
            }
            match prev_term_program {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn terminal_multiplexer_env_forces_low_motion_on() {
        let _g = term_program_test_guard();
        let vars = [
            "TMUX",
            "STY",
            "TERM_PROGRAM",
            "SSH_CLIENT",
            "SSH_TTY",
            "TILIX_ID",
            "TERMINATOR_UUID",
            "NO_ANIMATIONS",
        ];
        let prev: Vec<_> = vars
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();

        for (var, val) in [
            ("TMUX", "/tmp/tmux-501/default,1234,0"),
            ("STY", "1234.pts-0.host"),
        ] {
            // SAFETY: serialised by the guard.
            unsafe {
                for name in vars {
                    std::env::remove_var(name);
                }
                std::env::set_var(var, val);
            }
            let mut settings = Settings::default();
            assert!(!settings.low_motion, "default is animated");
            assert!(settings.fancy_animations, "default shows the water strip");
            settings.apply_env_overrides();
            assert!(
                settings.low_motion,
                "{var}={val:?} must enable low_motion under terminal multiplexers (#1925)"
            );
            assert!(
                !settings.fancy_animations,
                "{var}={val:?} must disable fancy_animations under terminal multiplexers (#1925)"
            );
        }

        // SAFETY: cleanup under the guard.
        unsafe {
            for (name, value) in prev {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────────
    // synchronized_output / Ptyxis flicker detection
    // ────────────────────────────────────────────────────────────────────────

    #[test]
    fn ptyxis_version_env_alone_flips_synchronized_output_off() {
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        let prev_ptyxis = std::env::var_os("PTYXIS_VERSION");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::set_var("PTYXIS_VERSION", "50.1");
        }
        let mut s = Settings::default();
        s.apply_env_overrides();
        assert_eq!(
            s.synchronized_output, "off",
            "PTYXIS_VERSION alone is sufficient — Ptyxis sets this even when TERM_PROGRAM isn't propagated"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_ptyxis {
                Some(v) => std::env::set_var("PTYXIS_VERSION", v),
                None => std::env::remove_var("PTYXIS_VERSION"),
            }
        }
    }

    #[test]
    fn ptyxis_does_not_override_user_explicit_on() {
        // Users who set `synchronized_output = "on"` (e.g. to confirm a
        // Ptyxis upgrade fixed it) must keep DEC 2026 even on Ptyxis.
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "ptyxis");
        }
        let mut s = Settings {
            synchronized_output: "on".to_string(),
            ..Settings::default()
        };
        s.apply_env_overrides();
        assert_eq!(
            s.synchronized_output, "on",
            "explicit user override must beat the Ptyxis env heuristic"
        );
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    #[test]
    fn ptyxis_does_not_override_user_explicit_off() {
        // A user with `synchronized_output = "off"` on a non-Ptyxis
        // terminal stays off after env detection (no-op flip).
        let _g = term_program_test_guard();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: serialised by the guard.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "xterm-256color");
        }
        let mut s = Settings {
            synchronized_output: "off".to_string(),
            ..Settings::default()
        };
        s.apply_env_overrides();
        assert_eq!(s.synchronized_output, "off");
        // SAFETY: cleanup under the guard.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────────
}
