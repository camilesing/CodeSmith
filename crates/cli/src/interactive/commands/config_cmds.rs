//! Configuration commands for the REPL — port of the non-view half of
//! `commands/config.rs`. The TUI's config editor view and the
//! `/statusline`/`/theme` pickers degrade to notes: the REPL status line
//! is fixed and theming is the terminal's own palette.

use colored::Colorize;

use super::ReplState;
use crate::app_state::ReasoningEffort;
use crate::core::ops::Op;
use crate::settings::Settings;
use codesmith_agent_runtime::mode::ApprovalMode;

/// `/config` — show the settings summary or the config file path.
pub fn show_config(state: &ReplState) {
    println!("{}", "configuration".bold());
    match crate::config::default_config_path() {
        Some(path) => println!("  config file:     {}", path.display()),
        None => println!("  config file:     (could not resolve home)"),
    }
    match Settings::load() {
        Ok(settings) => {
            println!("{}", settings.display(state.ui_locale));
        }
        Err(err) => println!("{} failed to load settings: {err}", "✗".red()),
    }
    println!(
        "{}",
        "usage: /config <key> <value> [--save] sets a value; /config <key> shows it".dimmed()
    );
}

/// `/config <key> [value] [--save|-s]`
pub fn config_command(state: &mut ReplState, arg: &str) {
    let raw = arg.trim();
    if raw.is_empty() {
        show_config(state);
        return;
    }
    let parts: Vec<&str> = raw.splitn(2, ' ').collect();
    if parts.len() == 1 {
        show_single_setting(state, parts[0]);
        return;
    }
    let raw_value = parts[1];
    let persist = raw_value.ends_with(" --save") || raw_value.ends_with(" -s");
    let value = if persist {
        raw_value
            .strip_suffix(" --save")
            .or_else(|| raw_value.strip_suffix(" -s"))
            .unwrap_or(raw_value)
            .trim()
    } else {
        raw_value.trim()
    };
    set_config_value(state, parts[0], value, persist);
}

fn show_single_setting(state: &ReplState, key: &str) {
    let key = key.to_ascii_lowercase();
    let value = match key.as_str() {
        "model" => Some(if state.auto_model {
            format!("auto (last: {})", state.model_display_label())
        } else {
            state.model.clone()
        }),
        "approval_mode" | "approval" => Some(state.approval_mode.label().to_string()),
        "mcp_config_path" | "mcp" => Some(state.mcp_config_path.display().to_string()),
        "reasoning_effort" | "effort" => Some(state.reasoning_effort_display_label()),
        "show_thinking" | "thinking" => Some(state.show_thinking.to_string()),
        "is_simple" | "simple" => Some(state.is_simple.to_string()),
        "editor_mode" | "composer_vim_mode" => Some(state.settings.composer_vim_mode.clone()),
        "preset" | "mode" => Some(
            state
                .active_preset
                .clone()
                .unwrap_or_else(|| "(none)".to_string()),
        ),
        _ => None,
    };
    match value {
        Some(value) => println!("{key} = {value}"),
        None => println!(
            "{} unknown key {key:?} — /settings lists everything; appearance keys are set via the config file",
            "✗".red()
        ),
    }
}

/// Set a config value. The REPL-meaningful keys apply live; appearance
/// keys persist (with --save) but have no live effect in line mode.
pub fn set_config_value(state: &mut ReplState, key: &str, value: &str, persist: bool) {
    let key = key.to_ascii_lowercase();
    match key.as_str() {
        "model" => {
            println!(
                "{} use /model {value} — the dedicated command handles auto-routing",
                "→".cyan()
            );
        }
        "approval_mode" | "approval" => match ApprovalMode::from_config_value(value) {
            Some(mode) => {
                state.approval_mode = mode;
                println!("approval_mode = {}", mode.label());
            }
            None => println!(
                "{} invalid approval_mode — use auto, suggest/on-request/untrusted, never/deny",
                "✗".red()
            ),
        },
        "show_thinking" | "thinking" => match parse_bool(value) {
            Some(next) => {
                state.show_thinking = next;
                if persist {
                    persist_setting_string("show_thinking", &next.to_string());
                }
                println!("show_thinking = {next}");
            }
            None => println!("{} expected true/false", "✗".red()),
        },
        "is_simple" | "simple" => match parse_bool(value) {
            Some(next) => {
                state.is_simple = next;
                if persist {
                    persist_setting_string("is_simple", &next.to_string());
                }
                println!("is_simple = {next}");
            }
            None => println!("{} expected true/false", "✗".red()),
        },
        "locale" | "language" => {
            let tag = value.trim();
            let resolved = crate::localization::resolve_locale(tag);
            if resolved.tag() != "en" || tag.eq_ignore_ascii_case("en") {
                if persist {
                    persist_setting_string("locale", tag);
                }
                println!("locale = {tag} (applies fully on restart)");
            } else {
                println!(
                    "{} unknown locale {tag:?} — try en, zh-Hans, zh-Hant, hi, es-419",
                    "✗".red()
                );
            }
        }
        "mcp_config_path" | "mcp" => {
            if value.trim().is_empty() {
                println!("{} mcp_config_path cannot be empty", "✗".red());
                return;
            }
            state.mcp_config_path = std::path::PathBuf::from(expand_tilde(value));
            if persist {
                match crate::interactive::commands::auto_route::persist_root_string_key(
                    state.config_path.as_deref(),
                    "mcp_config_path",
                    value,
                ) {
                    Ok(path) => println!(
                        "mcp_config_path = {} (saved to {}; restart required for the MCP tool pool)",
                        state.mcp_config_path.display(),
                        path.display()
                    ),
                    Err(err) => println!("{} failed to save: {err}", "✗".red()),
                }
            } else {
                println!(
                    "mcp_config_path = {} (session only; restart required for the MCP tool pool)",
                    state.mcp_config_path.display()
                );
            }
        }
        "base_url" => {
            if value.trim().is_empty() {
                println!("{} base_url cannot be empty", "✗".red());
                return;
            }
            match crate::interactive::commands::auto_route::persist_root_string_key(
                state.config_path.as_deref(),
                "base_url",
                value,
            ) {
                Ok(path) => println!("base_url = {value} (saved to {})", path.display()),
                Err(err) => println!("{} failed to save: {err}", "✗".red()),
            }
            println!(
                "{}",
                "restart and re-open your session for the new base URL".dimmed()
            );
        }
        "preset" | "mode" => {
            println!(
                "{} use /preset {value} — the dedicated command handles tiers",
                "→".cyan()
            );
        }
        "editor_mode" | "composer_vim_mode" => {
            let mode = match value.trim().to_ascii_lowercase().as_str() {
                "vim" | "vi" => "vim",
                "emacs" | "normal" | "default" => "normal",
                other => {
                    println!(
                        "{} unknown editor mode {other:?} — usage: /config editor_mode vim|normal",
                        "✗".red()
                    );
                    return;
                }
            };
            state.settings.composer_vim_mode = mode.to_string();
            if persist {
                persist_setting_string("composer_vim_mode", mode);
            }
            println!(
                "editor mode = {mode} (the prompt is rebuilt at the next line{}).",
                if persist {
                    ""
                } else {
                    "; add --save to keep it"
                }
            );
        }
        "reasoning_effort" | "effort" => {
            let Some(effort) = ReasoningEffort::from_setting(value) else {
                println!(
                    "{} unknown effort {value:?} — usage: /config reasoning_effort off|low|medium|high|auto|max",
                    "✗".red(),
                );
                return;
            };
            state.reasoning_effort = effort;
            state.update_model_compaction_budget();
            if persist {
                persist_setting_string("reasoning_effort", value);
            }
            println!("reasoning_effort = {}", effort.short_label());
        }
        _ => {
            // Appearance and unknown keys: persist verbatim when asked.
            if persist {
                match crate::interactive::commands::auto_route::persist_root_string_key(
                    state.config_path.as_deref(),
                    &key,
                    value,
                ) {
                    Ok(path) => println!(
                        "{key} = {value} (saved to {}; appearance keys have no live effect in the REPL)",
                        path.display()
                    ),
                    Err(err) => println!("{} failed to save: {err}", "✗".red()),
                }
            } else {
                println!(
                    "{} {key} is not live-settable in the REPL; append --save to persist it to config.toml",
                    "✗".red()
                );
            }
        }
    }
}

/// `/settings` — the full settings dump.
pub fn show_settings(state: &ReplState) {
    show_config(state);
}

/// `/verbose [on|off]` — thinking detail toggle. The REPL does not stream
/// thinking deltas; the flag still governs what the engine records.
pub fn verbose(state: &mut ReplState, arg: Option<&str>) {
    let next = match arg.map(str::trim).filter(|s| !s.is_empty()) {
        None => !state.show_thinking,
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "on" | "true" | "1" | "yes" => true,
            "off" | "false" | "0" | "no" => false,
            "toggle" => !state.show_thinking,
            _ => {
                println!("{} usage: /verbose [on|off]", "✗".red());
                return;
            }
        },
    };
    state.show_thinking = next;
    println!(
        "{}",
        format!("show_thinking = {next} (the REPL does not stream thinking deltas live)").dimmed()
    );
}

/// `/trust` — workspace trust state and per-path allowlist.
pub fn trust(state: &mut ReplState, arg: Option<&str>) {
    let raw = arg.map(str::trim).unwrap_or("");
    let mut parts = raw.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or_default().to_lowercase();
    let rest = parts.next().map(str::trim).unwrap_or("");
    let workspace = state.workspace.clone();

    match sub.as_str() {
        "" | "status" | "list" => trust_status(&workspace, state.trust_mode, sub == "list"),
        "on" | "enable" | "yes" | "y" => {
            state.trust_mode = true;
            println!(
                "{}",
                "workspace trust enabled — file tools can read/write any path. Use /trust off to revert; prefer /trust add <path> for a narrower opt-in".yellow()
            );
        }
        "off" | "disable" | "no" | "n" => {
            state.trust_mode = false;
            println!("workspace trust disabled.");
        }
        "add" => trust_add(&workspace, rest),
        "remove" | "rm" | "del" | "delete" => trust_remove(&workspace, rest),
        other => println!(
            "{} unknown /trust action `{other}` — use /trust, /trust on|off, /trust add <path>, /trust remove <path>",
            "✗".red()
        ),
    }
}

fn trust_status(workspace: &std::path::Path, trust_mode: bool, force_paths: bool) {
    let trust = crate::workspace_trust::WorkspaceTrust::load_for(workspace);
    println!(
        "workspace trust mode: {}",
        if trust_mode { "enabled" } else { "disabled" }
    );
    if trust.paths().is_empty() {
        if force_paths {
            println!("no external paths trusted from this workspace.");
        } else {
            println!("no external paths trusted yet — /trust add <path> to allow a directory.");
        }
    } else {
        println!("trusted external paths ({}):", trust.paths().len());
        for path in trust.paths() {
            println!("  • {}", path.display());
        }
    }
}

fn trust_add(workspace: &std::path::Path, raw: &str) {
    if raw.is_empty() {
        println!("{} usage: /trust add <path>", "✗".red());
        return;
    }
    let path = std::path::PathBuf::from(expand_tilde(raw));
    if !path.exists() {
        println!(
            "{} path not found: {} — supply an existing directory or file",
            "✗".red(),
            path.display()
        );
        return;
    }
    match crate::workspace_trust::add(workspace, &path) {
        Ok(stored) => println!(
            "added to trust list for this workspace: {}",
            stored.display()
        ),
        Err(err) => println!("{} failed to update trust list: {err}", "✗".red()),
    }
}

fn trust_remove(workspace: &std::path::Path, raw: &str) {
    if raw.is_empty() {
        println!("{} usage: /trust remove <path>", "✗".red());
        return;
    }
    let path = std::path::PathBuf::from(expand_tilde(raw));
    match crate::workspace_trust::remove(workspace, &path) {
        Ok(true) => println!("removed from trust list: {}", path.display()),
        Ok(false) => println!("not in trust list: {}", path.display()),
        Err(err) => println!("{} failed to update trust list: {err}", "✗".red()),
    }
}

/// `/slop` — the slop ledger (#2127).
pub fn slop(arg: Option<&str>) {
    let arg = arg.map(str::trim).unwrap_or("");
    let Ok(ledger) = crate::slop_ledger::SlopLedger::load() else {
        println!("{} failed to load slop ledger", "✗".red());
        return;
    };
    match arg {
        "" => println!("{}", ledger.summary()),
        "query" | "q" => {
            if ledger.is_empty() {
                println!("slop ledger is empty.");
                return;
            }
            for entry in ledger.query(&Default::default()) {
                println!(
                    "[{}] {} ({:?} | {:?}) — {}",
                    crate::slop_ledger::short_id(&entry.id),
                    entry.bucket.as_str(),
                    entry.severity,
                    entry.status,
                    entry.title
                );
            }
        }
        "export" | "e" => println!("{}", ledger.export_markdown(None, None)),
        other => println!(
            "{} unknown /slop action '{other}' — use /slop, /slop query, or /slop export",
            "✗".red()
        ),
    }
}

/// `/lsp` — LSP status (the TUI version also restarted diagnostics).
pub fn lsp(state: &ReplState) {
    println!(
        "lsp: {}",
        if state.lsp_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    match state.config.lsp.as_ref() {
        Some(config) => {
            println!(
                "  configured servers: {}",
                config
                    .servers
                    .as_ref()
                    .map_or(0, std::collections::HashMap::len)
            );
            println!(
                "{}",
                "restart the session for LSP configuration changes".dimmed()
            );
        }
        None => println!("  no [lsp] section in config.toml — using defaults"),
    }
}

/// `/keybindings` — the prompt-time key table. Read-only: the bindings are
/// compiled in (see `interactive::bindings` for what is and is not
/// rebindable).
pub fn keybindings() {
    println!("{}", "keyboard shortcuts:".bold());
    println!("{}", crate::interactive::bindings::help_panel().dimmed());
}

/// `/effort [low|medium|high|max|auto|off|status]` — the reasoning-effort
/// tier, which `/config reasoning_effort` also sets.
///
/// `max` is session-only, as in Claude Code: it is the most expensive stop
/// on the ring and every other stop persists to settings.toml.
pub fn effort(state: &mut ReplState, arg: Option<&str>) {
    let Some(value) = arg.map(str::trim).filter(|v| !v.is_empty()) else {
        println!("effort: {}", state.reasoning_effort_display_label());
        return;
    };
    let value = value.to_ascii_lowercase();
    if value == "status" {
        println!("effort: {}", state.reasoning_effort_display_label());
        return;
    }
    let accepted = ["off", "low", "medium", "high", "max", "auto"];
    if !accepted.contains(&value.as_str()) {
        println!(
            "{} unknown effort {value:?} — usage: /effort {}",
            "✗".red(),
            accepted.join("|")
        );
        return;
    }
    let effort =
        ReasoningEffort::from_setting(&value).expect("value validated against `accepted` above");
    state.reasoning_effort = effort;
    state.update_model_compaction_budget();
    if effort == ReasoningEffort::Max {
        println!(
            "effort = {} (this session only — /effort persists every other level)",
            effort.short_label()
        );
        return;
    }
    persist_setting_string("reasoning_effort", effort.as_setting());
    println!("effort = {}", effort.short_label());
}

/// `/autocompact [auto|<tokens>]` — the auto-compaction window.
///
/// `auto` returns the model-derived threshold; a size (`500k`, `250000`)
/// pins it for the session. Claude Code's 500k-style shorthand is accepted
/// so a value can be typed the way it is documented.
pub async fn autocompact(state: &mut ReplState, arg: Option<&str>) {
    let Some(value) = arg.map(str::trim).filter(|v| !v.is_empty()) else {
        println!(
            "autocompact: {} (threshold {}{})",
            if state.auto_compact { "on" } else { "off" },
            state.compact_threshold,
            if state.auto_compact {
                String::new()
            } else {
                " — compaction is disabled".to_string()
            }
        );
        return;
    };
    if value.eq_ignore_ascii_case("auto") {
        state.update_model_compaction_budget();
        state.auto_compact = true;
        apply_compaction(state).await;
        persist_setting_string("auto_compact", "true");
        println!(
            "autocompact: on, window back to the model default ({} tokens)",
            state.compact_threshold
        );
        return;
    }
    match parse_token_size(value) {
        Some(tokens) => {
            state.compact_threshold = tokens;
            state.auto_compact = true;
            apply_compaction(state).await;
            println!("autocompact: on, window pinned at {tokens} tokens (this session)");
        }
        None => println!(
            "{} unknown window {value:?} — usage: /autocompact auto|<tokens> (500k, 250000)",
            "✗".red()
        ),
    }
}

/// Push the current compaction settings into the running engine.
async fn apply_compaction(state: &ReplState) {
    let config = state.compaction_config();
    if let Ok(engine) = state.engine() {
        let _ = engine.send(Op::SetCompaction { config }).await;
    }
}

/// `500k` / `2m` / `250000` — Claude Code's size shorthand.
fn parse_token_size(value: &str) -> Option<usize> {
    let lowered = value.trim().to_ascii_lowercase();
    let (digits, scale) = match lowered.strip_suffix('k') {
        Some(head) => (head, 1_000usize),
        None => match lowered.strip_suffix('m') {
            Some(head) => (head, 1_000_000),
            None => (lowered.as_str(), 1),
        },
    };
    digits.trim().parse::<usize>().ok()?.checked_mul(scale)
}

/// `/sandbox [on|off|status|read-only|workspace-write|danger-full-access]`.
///
/// The sandbox policy is chosen when the engine is built, so a change here
/// is stored (and persisted with `--save`) and takes effect for the next
/// engine — a restart. `/status` always names the mode in force now.
pub fn sandbox(state: &mut ReplState, arg: Option<&str>, persist: bool) {
    let current = state
        .config
        .sandbox_mode
        .clone()
        .unwrap_or_else(|| "(default)".to_string());
    let Some(value) = arg.map(str::trim).filter(|v| !v.is_empty()) else {
        println!("sandbox: {current}");
        return;
    };
    let requested = value.to_ascii_lowercase();
    let mode = match requested.as_str() {
        "status" => {
            println!("sandbox: {current}");
            return;
        }
        "on" => "workspace-write",
        "off" => "danger-full-access",
        other => other,
    };
    if !matches!(
        mode,
        "read-only" | "workspace-write" | "danger-full-access" | "external-sandbox"
    ) {
        println!(
            "{} unknown sandbox mode {value:?} — on|off|read-only|workspace-write|danger-full-access",
            "✗".red()
        );
        return;
    }
    let changed = state.config.sandbox_mode.as_deref() != Some(mode);
    state.config.sandbox_mode = Some(mode.to_string());
    if persist {
        match super::auto_route::persist_root_string_key(
            state.config_path.as_deref(),
            "sandbox_mode",
            mode,
        ) {
            Ok(path) => println!("  saved to {}", path.display()),
            Err(err) => println!("{} could not save sandbox_mode: {err}", "✗".red()),
        }
    }
    if changed {
        println!(
            "sandbox = {mode} (applies from the next engine; add --save to keep it){}",
            if mode == "danger-full-access" {
                " — commands run unconfined"
            } else {
                ""
            }
        );
    } else {
        println!("sandbox = {mode} (unchanged)");
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "1" | "yes" => Some(true),
        "false" | "off" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Persist `[<section>] <key> = <bool>` in config.toml.
///
/// Same shape as [`super::auto_route::persist_root_string_key`] — parse,
/// set, serialize — which means the same trade-off: comments in the file are
/// not preserved by this class of edit. That is pre-existing behaviour for
/// config writes from the REPL, not a new loss introduced here.
pub(crate) fn persist_section_bool(
    config_path: Option<&std::path::Path>,
    section: &str,
    key: &str,
    value: bool,
) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context;

    let path = super::auto_route::config_toml_path(config_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut doc: toml::Value = if path.exists() {
        toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse config at {}", path.display()))?
    } else {
        toml::Value::Table(toml::value::Table::new())
    };
    let root = doc
        .as_table_mut()
        .context("config.toml root must be a table")?;
    let table = root
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut()
        .with_context(|| format!("[{section}] in config.toml must be a table"))?;
    table.insert(key.to_string(), toml::Value::Boolean(value));
    let body = toml::to_string_pretty(&doc).context("failed to serialize config.toml")?;
    codesmith_agent_runtime::utils::write_atomic(&path, body.as_bytes())
        .with_context(|| format!("failed to write config at {}", path.display()))?;
    Ok(path)
}

/// Persist an `Option<String>` settings key; `None` clears it.
///
/// A companion to [`persist_setting_string`] for the keys whose absent state
/// is meaningful (`prompt_color` follows the theme, `statusline_command` is
/// off), where writing an empty string would be a different value.
pub(crate) fn persist_setting(key: &str, value: Option<&str>) {
    match Settings::load() {
        Ok(mut settings) => {
            match key {
                "prompt_color" => settings.prompt_color = value.map(str::to_string),
                "statusline_command" => settings.statusline_command = value.map(str::to_string),
                _ => {}
            }
            if let Err(err) = settings.save() {
                println!("{} could not persist {key}: {err}", "✗".red());
            }
        }
        Err(err) => println!("{} could not load settings: {err}", "✗".red()),
    }
}

pub(crate) fn persist_setting_string(key: &str, value: &str) {
    match Settings::load() {
        Ok(mut settings) => {
            match key {
                "show_thinking" => settings.show_thinking = value == "true",
                "is_simple" => settings.is_simple = value == "true",
                "locale" => settings.locale = value.to_string(),
                "reasoning_effort" => settings.reasoning_effort = Some(value.to_string()),
                "composer_vim_mode" => settings.composer_vim_mode = value.to_string(),
                "auto_compact" => settings.auto_compact = value == "true",
                "theme" => settings.theme = value.to_string(),
                _ => {}
            }
            if let Err(err) = settings.save() {
                println!("{} could not persist {key}: {err}", "✗".red());
            }
        }
        Err(err) => println!(
            "{} could not load settings to persist {key}: {err}",
            "✗".red()
        ),
    }
}

fn expand_tilde(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    } else if raw == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home.to_string_lossy().into_owned();
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_token_size;

    #[test]
    fn token_sizes_accept_claude_codes_shorthand() {
        assert_eq!(parse_token_size("500k"), Some(500_000));
        assert_eq!(parse_token_size("2m"), Some(2_000_000));
        assert_eq!(parse_token_size("250000"), Some(250_000));
        assert_eq!(parse_token_size(" 12K "), Some(12_000));
        assert_eq!(parse_token_size(""), None);
        assert_eq!(parse_token_size("lots"), None);
        // Overflow is a refusal, not a silent wrap.
        assert_eq!(parse_token_size("99999999999999m"), None);
    }
}
