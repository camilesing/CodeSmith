//! Configuration commands for the REPL — port of the non-view half of
//! `commands/config.rs`. The TUI's config editor view and the
//! `/statusline`/`/theme` pickers degrade to notes: the REPL status line
//! is fixed and theming is the terminal's own palette.

use colored::Colorize;

use super::ReplState;
use crate::settings::Settings;
use crate::tui::app::ReasoningEffort;
use crate::tui::approval::ApprovalMode;

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
        "reasoning_effort" | "effort" => {
            let effort = ReasoningEffort::from_setting(value);
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

/// `/theme` — theming is a TUI concept; the REPL inherits terminal colors.
pub fn theme() {
    println!(
        "{}",
        "themes were a TUI feature — the REPL uses your terminal's own colors. Set theme = \"…\" in ~/.codesmith/settings.toml if you return to a fullscreen frontend.".dimmed()
    );
}

/// `/statusline` — the REPL status line is a fixed six-item layout.
pub fn status_line() {
    println!(
        "{}",
        "the REPL status line is fixed: state · preset · ctx% · mode · agents · model · version"
            .dimmed()
    );
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "1" | "yes" => Some(true),
        "false" | "off" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn persist_setting_string(key: &str, value: &str) {
    match Settings::load() {
        Ok(mut settings) => {
            match key {
                "show_thinking" => settings.show_thinking = value == "true",
                "is_simple" => settings.is_simple = value == "true",
                "locale" => settings.locale = value.to_string(),
                "reasoning_effort" => settings.reasoning_effort = Some(value.to_string()),
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
