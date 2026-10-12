//! Slash-command dispatcher for the REPL.
//!
//! The full TUI command surface is being ported here command-by-command
//! against [`ReplState`]; the retired `commands/` tree coupled every
//! handler to the ratatui `App`. Handlers print directly to stdout and
//! mutate state / send engine ops.

pub mod auto_route;
pub mod config_cmds;
pub mod debug_cmds;
pub mod extension_cmds;
pub mod misc_cmds;
pub mod mods_cmds;
pub mod network_store;
pub mod project_doc;
pub mod registry;
pub mod session_cmds;
pub mod share;
pub mod skills_cmds;
pub mod user_cmd_store;

use codesmith_config::presets::PresetCatalog;
use colored::Colorize;

use self::registry::Command;
use super::state::ReplState;
use crate::app_state::AppMode;
use crate::core::ops::Op;
use crate::presets;

/// Dispatch one `/command args…` line. Returns `true` when the REPL
/// should exit.
pub async fn dispatch(state: &mut ReplState, line: &str) -> bool {
    let mut parts = line.split_whitespace();
    let Some(raw) = parts.next() else {
        return false;
    };
    let command = raw.trim_start_matches('/');
    let arg_str = line[raw.len()..].trim().to_string();

    // User-defined commands override built-ins (mirrors the TUI's layer
    // order: user > extension > mods > static table).
    if let Some(handled) = skills_cmds::try_user_command(state, command, &arg_str).await
        && handled
    {
        return false;
    }

    // Built-ins resolve through the registry: every name `/help` prints and
    // Tab completes is dispatchable, and a registered name with no arm below
    // is a compile error because this match is exhaustive over `Command`.
    let Some(info) = Command::resolve(command) else {
        println!(
            "{} unknown command /{command} — {}",
            "✗".red(),
            "/help lists what exists".dimmed()
        );
        return false;
    };
    match info.command {
        Command::Help => {
            print_help(state);
            false
        }
        Command::Exit => true,
        Command::Clear => {
            print!("\x1b[2J\x1b[H");
            // The screen transcript is gone; so is its replay source.
            state.tool_log.clear();
            false
        }
        Command::Status => {
            print_status(state);
            false
        }
        Command::Expand => {
            cmd_expand(state, &arg_str);
            false
        }
        Command::Model => {
            cmd_model(state, &arg_str).await;
            false
        }
        Command::Mode => {
            cmd_mode(state, &arg_str);
            false
        }
        // Upstream #1247 kept /plan and /agent as compatibility shorthands
        // for /mode; the line-REPL port had dropped the first two.
        Command::Plan | Command::Agent => {
            cmd_mode(state, info.name);
            false
        }
        Command::Yolo => {
            state.set_mode(AppMode::Yolo);
            println!("{}", "YOLO mode: shell + trust + auto-approve ON".yellow());
            false
        }
        Command::Preset => {
            cmd_preset(state, &arg_str);
            false
        }
        Command::Provider => {
            cmd_provider(state, &arg_str);
            false
        }
        Command::Sessions => {
            session_cmds::sessions(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Resume => {
            session_cmds::resume(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Save => {
            session_cmds::save(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Fork => {
            session_cmds::fork(state).await;
            false
        }
        Command::Load => {
            session_cmds::load(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Compact => {
            session_cmds::compact(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Purge => {
            session_cmds::purge(state).await;
            false
        }
        Command::New => {
            session_cmds::new_session(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Export => {
            session_cmds::export(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Rename => {
            session_cmds::rename(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Agents => {
            cmd_agents(state);
            false
        }
        Command::Logout => {
            cmd_logout(state);
            false
        }
        Command::Tokens => {
            debug_cmds::tokens(state);
            false
        }
        Command::Cost => {
            debug_cmds::cost(state);
            false
        }
        Command::Tools => {
            debug_cmds::tools(state);
            false
        }
        Command::System => {
            debug_cmds::system_prompt(state);
            false
        }
        Command::Context => {
            debug_cmds::context(state);
            false
        }
        Command::Cache => {
            debug_cmds::cache(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Diff => {
            debug_cmds::diff(state);
            false
        }
        Command::Edit => {
            debug_cmds::edit(state);
            false
        }
        Command::Undo => {
            debug_cmds::undo(state).await;
            false
        }
        Command::Retry => {
            debug_cmds::retry(state).await;
            false
        }
        Command::Config => {
            config_cmds::config_command(state, &arg_str);
            false
        }
        Command::Settings => {
            config_cmds::show_settings(state);
            false
        }
        Command::Verbose => {
            config_cmds::verbose(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Trust => {
            config_cmds::trust(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Slop => {
            config_cmds::slop(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::Lsp => {
            config_cmds::lsp(state);
            false
        }
        Command::Theme => {
            config_cmds::theme();
            false
        }
        Command::Statusline => {
            config_cmds::status_line();
            false
        }
        Command::Init => {
            misc_cmds::init(state);
            false
        }
        Command::Hooks => {
            misc_cmds::hooks(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Mcp => {
            misc_cmds::mcp(state, &arg_str).await;
            false
        }
        Command::Network => {
            misc_cmds::network(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::Goal => {
            misc_cmds::goal(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Task => {
            misc_cmds::task(state, &arg_str).await;
            false
        }
        Command::Jobs => {
            misc_cmds::jobs(state);
            false
        }
        Command::Balance => {
            misc_cmds::balance(state);
            false
        }
        Command::Verify => {
            misc_cmds::verify(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::Links => {
            misc_cmds::links();
            false
        }
        Command::Home => {
            misc_cmds::home(state);
            false
        }
        Command::Profile => {
            misc_cmds::profile(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Workspace => {
            misc_cmds::workspace(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Translate => {
            misc_cmds::translate(state);
            false
        }
        Command::Anchor => {
            misc_cmds::anchor(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Note => {
            misc_cmds::note(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Attach => {
            misc_cmds::attach(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Feedback => {
            misc_cmds::feedback(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        Command::Share => {
            misc_cmds::share(state).await;
            false
        }
        Command::Skills => {
            skills_cmds::list_skills(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Skill => {
            skills_cmds::run_skill(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Review => {
            skills_cmds::review(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            )
            .await;
            false
        }
        Command::Queue => {
            misc_cmds::queue();
            false
        }
        Command::Stash => {
            misc_cmds::stash();
            false
        }
        Command::Cycles => {
            misc_cmds::cycles();
            false
        }
        Command::Relay => {
            misc_cmds::relay();
            false
        }
        Command::Restore => {
            misc_cmds::restore();
            false
        }
        Command::Change => {
            misc_cmds::change();
            false
        }
        Command::Rlm => {
            misc_cmds::rlm();
            false
        }
        Command::Extension => {
            extension_cmds::extension_command(state, &arg_str).await;
            false
        }
        Command::Mods => {
            mods_cmds::mods_command(state, &arg_str).await;
            false
        }
        Command::Models => {
            let json = arg_str.split_whitespace().any(|flag| flag == "--json");
            if let Err(err) =
                crate::main_root::run_models(&state.config, crate::main_root::ModelsArgs { json })
                    .await
            {
                println!("{} {err}", "✗".red());
            }
            false
        }
        Command::Memory => {
            misc_cmds::memory(
                state,
                if arg_str.is_empty() {
                    None
                } else {
                    Some(&arg_str)
                },
            );
            false
        }
        Command::Set => {
            println!(
                "{}",
                "the /set command was retired — use /config to edit settings and /settings to inspect values".dimmed()
            );
            false
        }
        Command::Deepseek => {
            println!(
                "{}",
                "the /deepseek command was renamed — use /links (aliases: /dashboard, /api)"
                    .dimmed()
            );
            false
        }
    }
}

/// `/help` — every registered command, straight from
/// [`registry::Command::ALL`].
///
/// Table-driven so it cannot fall behind the dispatcher (it was a
/// hardcoded 20-row subset of the 75 dispatchable names) and so it names
/// exactly what Tab completes.
fn print_help(state: &ReplState) {
    println!("{}", "commands (Tab completes these names):".bold());
    for info in registry::Command::ALL {
        let name = if info.usage.is_empty() {
            format!("/{}", info.name)
        } else {
            format!("/{} {}", info.name, info.usage)
        };
        if name.len() <= 32 {
            println!("  {name:<32} {}", info.help.dimmed());
        } else {
            println!("  {name}");
            println!("    {}", info.help.dimmed());
        }
    }
    let user = user_cmd_store::user_command_names(Some(&state.workspace));
    if !user.is_empty() {
        println!();
        println!("{}", "user commands (these shadow built-ins):".bold());
        for name in user {
            println!("  /{name}");
        }
    }
}

/// `/expand [n|all]` — reprint the detail hidden by collapsed tool-call
/// lines. Bare: every call from the most recent turn that logged one;
/// `n`: that turn's n-th call; `all`: the whole retained log.
fn cmd_expand(state: &ReplState, arg: &str) {
    let arg = arg.trim();
    if state.tool_log.is_empty() {
        println!("{}", "no tool calls recorded this session".dimmed());
        return;
    }
    let last_turn = state
        .tool_log
        .iter()
        .map(|record| record.turn)
        .max()
        .expect("tool_log is non-empty");
    let records: Vec<crate::tool_summary::ToolCallRecord> = if arg.eq_ignore_ascii_case("all") {
        state.tool_log.clone()
    } else if let Some(index) = arg.parse::<usize>().ok().filter(|n| *n > 0) {
        let turn_records: Vec<_> = state
            .tool_log
            .iter()
            .filter(|record| record.turn == last_turn)
            .collect();
        match turn_records.get(index - 1) {
            Some(record) => vec![(**record).clone()],
            None => {
                println!(
                    "{} no tool call #{index} in the last turn (it logged {})",
                    "✗".red(),
                    turn_records.len()
                );
                return;
            }
        }
    } else if arg.is_empty() {
        state
            .tool_log
            .iter()
            .filter(|record| record.turn == last_turn)
            .cloned()
            .collect()
    } else {
        println!("{}", "usage: /expand [n|all]".dimmed());
        return;
    };
    if records.is_empty() {
        println!("{}", "no tool calls recorded this session".dimmed());
        return;
    }
    println!("{}", "tool-call detail:".bold());
    for record in &records {
        println!(
            "{}",
            crate::tool_summary::tool_detail_block(record, crate::tool_summary::DETAIL_MAX_LINES)
                .dimmed()
        );
    }
}

fn print_status(state: &ReplState) {
    println!("{}", "session status".bold());
    println!("  model:            {}", state.model_display_label());
    println!("  provider:         {}", state.api_provider.as_str());
    println!("  mode:             {}", state.mode.label());
    println!(
        "  preset:           {}",
        state.active_preset.as_deref().unwrap_or("(none)")
    );
    println!(
        "  reasoning:        {}",
        state.reasoning_effort_display_label()
    );
    println!("  running agents:   {}", state.running_agent_count());
    match state.current_session_id.as_deref() {
        Some(id) => println!(
            "  session:          {}",
            crate::session_manager::truncate_id(id)
        ),
        None => println!("  session:          (unsaved)"),
    }
    println!(
        "  tokens in/out:    {}/{}",
        state.session.total_input_tokens, state.session.total_output_tokens
    );
    println!("  cost (usd):       {:.4}", state.session.session_cost);
    println!("  worked:           {:?}", state.cumulative_turn_duration);
    println!(
        "  memory/kod/lsp:   {}/{}/{}",
        state.use_memory, state.kod_enabled, state.lsp_enabled
    );
    if state.use_memory {
        println!("  memory file:      {}", state.memory_path.display());
    }
    if state.kod_enabled {
        println!("  memory dir:       {}", state.memory_dir.display());
    }
    println!("  mcp servers:      {}", state.mcp_configured_count);
    if state.mcp_configured_count > 0 {
        println!("  mcp config:       {}", state.mcp_config_path.display());
    }
    if let Some(path) = state.config_path.as_ref() {
        println!(
            "  config file:      {} ({})",
            path.display(),
            state.config_profile.as_deref().unwrap_or("default profile")
        );
    }
    if state.yolo {
        println!("  yolo:             {}", "ON".yellow());
    }
}

async fn cmd_model(state: &mut ReplState, arg: &str) {
    if arg.trim().is_empty() {
        println!("model: {}", state.model_display_label());
        if state.auto_model {
            println!(
                "{}",
                "auto routing is ON; the effective model is picked per request".dimmed()
            );
        }
        return;
    }
    let selection = arg.trim().to_string();
    if !state.accepts_custom_model_ids() && selection != "auto" {
        println!(
            "{}",
            format!(
                "note: provider {} may reject unknown model ids",
                state.api_provider.as_str()
            )
            .dimmed()
        );
    }
    state.set_model_selection(selection.clone());
    if let Ok(engine) = state.engine() {
        let _ = engine.send(Op::SetModel { model: selection }).await;
    }
    // Persist the selection (mirrors the retired /model command): pinned
    // per provider in settings.toml so restarts keep it.
    match crate::settings::Settings::load() {
        Ok(mut settings) => {
            let provider = state.api_provider.as_str().to_string();
            settings
                .provider_models
                .get_or_insert_with(Default::default)
                .insert(provider, state.model_selection_for_persistence());
            if let Err(err) = settings.save() {
                println!(
                    "{}",
                    format!("warning: could not persist model selection: {err}").yellow()
                );
            }
        }
        Err(err) => println!(
            "{}",
            format!("warning: could not load settings to persist model: {err}").yellow()
        ),
    }
    println!("model set to {}", state.model_display_label());
}

fn cmd_mode(state: &mut ReplState, arg: &str) {
    let Some(name) = arg.split_whitespace().next() else {
        println!("mode: {}", state.mode.label());
        return;
    };
    let mode = match name.to_ascii_lowercase().as_str() {
        "agent" => AppMode::Agent,
        "yolo" => AppMode::Yolo,
        "plan" => AppMode::Plan,
        "coordinator" => AppMode::Coordinator,
        other => {
            println!(
                "{} unknown mode {other:?} (agent | yolo | plan | coordinator)",
                "✗".red()
            );
            return;
        }
    };
    state.set_mode(mode);
    println!("switched to {} mode", state.mode.label());
}

fn cmd_preset(state: &mut ReplState, arg: &str) {
    let arg = arg.trim();
    if let Some(name) = arg.strip_prefix("export").map(str::trim) {
        export_preset(state, name);
        return;
    }
    if arg == "off" {
        preset_off(state);
        return;
    }
    if arg.is_empty() || arg == "list" {
        let catalog = PresetCatalog::load(Some(&state.workspace));
        print!(
            "{}",
            presets::describe(&catalog, state.active_preset.as_deref())
        );
        return;
    }
    let name = arg.to_string();
    state.apply_preset_at_startup(&name);
    match state.active_preset.as_deref() {
        Some(applied) if applied == name => {
            println!("preset: {applied} (live dials applied)");
            // Persist the selection so the next start picks it up (mirrors
            // the retired /preset command, which wrote
            // `settings.active_preset`).
            match crate::settings::Settings::load() {
                Ok(mut settings) => {
                    settings.active_preset = Some(name);
                    if let Err(err) = settings.save() {
                        println!(
                            "{}",
                            format!("warning: could not persist preset selection: {err}").yellow()
                        );
                    }
                }
                Err(err) => println!(
                    "{}",
                    format!("warning: could not load settings to persist preset: {err}").yellow()
                ),
            }
        }
        Some(applied) => {
            println!(
                "{}",
                format!("preset {name:?} not found; active preset is {applied:?} (diy deviations report as diy)").yellow()
            );
        }
        None => println!(
            "{} preset {name:?} not found; no preset is active",
            "✗".red()
        ),
    }
}

/// `/preset export <name>` — snapshot the current dials as a user-layer
/// preset file (`~/.codesmith/presets/<name>.toml`): the active definition
/// as the base, overlaid with the live dials the REPL tracks (mode + tool
/// lists).
fn export_preset(state: &mut ReplState, name: &str) {
    if name.is_empty() {
        println!(
            "{}",
            "usage: /preset export <name> — snapshots the current dials as a shareable preset"
                .dimmed()
        );
        return;
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        println!("{} invalid preset name {name:?}", "✗".red());
        return;
    }
    let catalog = PresetCatalog::load(Some(&state.workspace));
    let base = state
        .active_preset
        .as_deref()
        .and_then(|active| catalog.get_ci(active))
        .map(|loaded| loaded.definition.clone());
    let definition = presets::export_definition(
        base.as_ref(),
        state.mode.as_setting(),
        state.active_allowed_tools.as_deref(),
        state.active_blocked_tools.as_deref(),
        name,
    );
    let Ok(home) = codesmith_config::codesmith_home() else {
        println!("{} cannot resolve the CodeSmith home directory", "✗".red());
        return;
    };
    let dir = home.join("presets");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        println!("{} create {}: {err}", "✗".red(), dir.display());
        return;
    }
    let body = match toml::to_string_pretty(&definition) {
        Ok(body) => body,
        Err(err) => {
            println!("{} serialize preset: {err}", "✗".red());
            return;
        }
    };
    let path = dir.join(format!("{name}.toml"));
    if let Err(err) = std::fs::write(&path, body) {
        println!("{} write {}: {err}", "✗".red(), path.display());
        return;
    }
    println!(
        "exported preset {name} → {} (user layer; /preset {name} applies it)",
        path.display()
    );
}

/// `/preset off` — drop the preset layer, keep the current dials: clears
/// the session's preset state and the persisted selection. The next start
/// uses the default tier (`middle`) unless config.toml / `--preset` picks
/// one.
fn preset_off(state: &mut ReplState) {
    state.active_preset = None;
    state.active_allowed_tools = None;
    state.active_blocked_tools = None;
    match crate::settings::Settings::load() {
        Ok(mut settings) => {
            settings.active_preset = None;
            if let Err(err) = settings.save() {
                println!(
                    "{}",
                    format!("warning: could not persist preset selection: {err}").yellow()
                );
            }
        }
        Err(err) => println!(
            "{}",
            format!("warning: could not load settings to clear preset: {err}").yellow()
        ),
    }
    println!(
        "{}",
        "preset layer off — current dials kept; next start uses the default tier (middle) unless config.toml picks one".dimmed()
    );
}

fn cmd_provider(state: &mut ReplState, arg: &str) {
    if arg.trim().is_empty() {
        println!("provider: {}", state.api_provider.as_str());
        println!(
            "{}",
            "switching providers takes effect for new sessions; restart after /provider <name> for a full re-config".dimmed()
        );
        return;
    }
    match crate::config::ApiProvider::parse(arg.trim()) {
        Some(provider) => {
            state.api_provider = provider;
            println!("provider set to {}", state.api_provider.as_str());
            println!(
                "{}",
                "restart codesmith (or run the onboarding again) to rebuild the client for the new provider".dimmed()
            );
        }
        None => println!("{} unknown provider {arg:?}", "✗".red()),
    }
}

fn cmd_agents(state: &ReplState) {
    if state.agent_progress.is_empty() {
        println!("no sub-agents running");
        return;
    }
    println!("{}", "running sub-agents:".bold());
    for (id, status) in &state.agent_progress {
        println!("  {id}: {status}");
    }
}

fn cmd_logout(_state: &mut ReplState) {
    // Wipe the stored key and the onboarding marker so the next start is a
    // clean first run: the next bare `codesmith` re-asks the setup
    // questions. Signing back in is explicit — `codesmith auth set` or
    // `codesmith login` skips the flow.
    match crate::config::clear_api_key() {
        Ok(()) => {
            if let Some(marker) = codesmith_agent_runtime::onboarding::default_marker_path()
                && let Err(err) = std::fs::remove_file(&marker)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                println!(
                    "{}",
                    format!("warning: could not remove onboarding marker: {err}").yellow()
                );
            }
            println!(
                "{}",
                "logged out: API key removed; sign back in with `codesmith auth set` or `codesmith login`".green()
            );
        }
        Err(err) => println!("{} logout failed: {err}", "✗".red()),
    }
}
