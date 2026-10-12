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
pub mod session_cmds;
pub mod share;
pub mod skills_cmds;
pub mod user_cmd_store;

use codesmith_config::presets::PresetCatalog;
use colored::Colorize;

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

    match command {
        "help" | "?" => {
            print_help();
            false
        }
        "exit" | "quit" | "q" => true,
        "clear" => {
            print!("\x1b[2J\x1b[H");
            // The screen transcript is gone; so is its replay source.
            state.tool_log.clear();
            false
        }
        "status" => {
            print_status(state);
            false
        }
        "expand" => {
            cmd_expand(state, &arg_str);
            false
        }
        "model" => {
            cmd_model(state, &arg_str).await;
            false
        }
        "mode" => {
            cmd_mode(state, &arg_str);
            false
        }
        "yolo" => {
            state.set_mode(AppMode::Yolo);
            println!("{}", "YOLO mode: shell + trust + auto-approve ON".yellow());
            false
        }
        "preset" => {
            cmd_preset(state, &arg_str);
            false
        }
        "provider" => {
            cmd_provider(state, &arg_str);
            false
        }
        "sessions" => {
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
        "resume" => {
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
        "save" => {
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
        "fork" | "branch" => {
            session_cmds::fork(state).await;
            false
        }
        "load" => {
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
        "compact" | "yasuo" => {
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
        "purge" | "qingchu" => {
            session_cmds::purge(state).await;
            false
        }
        "new" => {
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
        "export" | "daochu" => {
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
        "rename" | "gaiming" => {
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
        "agents" | "subagents" => {
            cmd_agents(state);
            false
        }
        "logout" => {
            cmd_logout(state);
            false
        }
        "tokens" => {
            debug_cmds::tokens(state);
            false
        }
        "cost" => {
            debug_cmds::cost(state);
            false
        }
        "tools" => {
            debug_cmds::tools(state);
            false
        }
        "system" | "xitong" => {
            debug_cmds::system_prompt(state);
            false
        }
        "context" | "ctx" => {
            debug_cmds::context(state);
            false
        }
        "cache" => {
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
        "diff" => {
            debug_cmds::diff(state);
            false
        }
        "edit" => {
            debug_cmds::edit(state);
            false
        }
        "undo" => {
            debug_cmds::undo(state).await;
            false
        }
        "retry" | "chongshi" => {
            debug_cmds::retry(state).await;
            false
        }
        "config" => {
            config_cmds::config_command(state, &arg_str);
            false
        }
        "settings" => {
            config_cmds::show_settings(state);
            false
        }
        "verbose" => {
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
        "trust" | "xinren" => {
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
        "slop" | "canzha" => {
            config_cmds::slop(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        "lsp" => {
            config_cmds::lsp(state);
            false
        }
        "theme" => {
            config_cmds::theme();
            false
        }
        "statusline" => {
            config_cmds::status_line();
            false
        }
        "init" => {
            misc_cmds::init(state);
            false
        }
        "hooks" | "hook" | "gouzi" => {
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
        "mcp" => {
            misc_cmds::mcp(state, &arg_str).await;
            false
        }
        "network" => {
            misc_cmds::network(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        "goal" | "hunt" | "mubiao" => {
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
        "task" | "tasks" => {
            misc_cmds::task(state, &arg_str).await;
            false
        }
        "jobs" | "job" | "zuoye" => {
            misc_cmds::jobs(state);
            false
        }
        "balance" => {
            misc_cmds::balance(state);
            false
        }
        "verify" => {
            misc_cmds::verify(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        "links" | "dashboard" | "api" | "lianjie" => {
            misc_cmds::links();
            false
        }
        "home" | "stats" | "overview" | "zhuye" | "shouye" => {
            misc_cmds::home(state);
            false
        }
        "profile" | "dangan" => {
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
        "workspace" | "cwd" => {
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
        "translate" | "translation" | "transale" => {
            misc_cmds::translate(state);
            false
        }
        "anchor" | "maodian" => {
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
        "note" => {
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
        "attach" | "image" | "media" | "fujian" => {
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
        "feedback" => {
            misc_cmds::feedback(if arg_str.is_empty() {
                None
            } else {
                Some(&arg_str)
            });
            false
        }
        "share" => {
            misc_cmds::share(state).await;
            false
        }
        "skills" | "jinengliebiao" => {
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
        "skill" | "jineng" => {
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
        "review" | "shencha" => {
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
        "queue" | "queued" => {
            misc_cmds::queue();
            false
        }
        "stash" | "park" => {
            misc_cmds::stash();
            false
        }
        "cycles" | "zhouqi" | "cycle" | "recall" => {
            misc_cmds::cycles();
            false
        }
        "relay" | "batonpass" => {
            misc_cmds::relay();
            false
        }
        "restore" => {
            misc_cmds::restore();
            false
        }
        "change" => {
            misc_cmds::change();
            false
        }
        "rlm" | "recursive" | "digui" => {
            misc_cmds::rlm();
            false
        }
        "extension" | "ext" => {
            extension_cmds::extension_command(state, &arg_str).await;
            false
        }
        "mods" => {
            mods_cmds::mods_command(state, &arg_str).await;
            false
        }
        "models" => {
            let json = arg_str.split_whitespace().any(|flag| flag == "--json");
            if let Err(err) =
                crate::main_root::run_models(&state.config, crate::main_root::ModelsArgs { json })
                    .await
            {
                println!("{} {err}", "✗".red());
            }
            false
        }
        "memory" => {
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
        "set" => {
            println!(
                "{}",
                "the /set command was retired — use /config to edit settings and /settings to inspect values".dimmed()
            );
            false
        }
        "deepseek" => {
            println!(
                "{}",
                "the /deepseek command was renamed — use /links (aliases: /dashboard, /api)"
                    .dimmed()
            );
            false
        }
        other => {
            println!(
                "{} unknown command /{other} — {}",
                "✗".red(),
                "/help lists what exists".dimmed()
            );
            false
        }
    }
}

fn print_help() {
    println!("{}", "commands:".bold());
    let rows: [(&str, &str); 20] = [
        ("/help", "this list"),
        (
            "/status",
            "session status: tokens, cost, model, mode, preset, agents",
        ),
        (
            "/expand [n|all]",
            "reprint hidden tool-call detail (Ctrl+O toggles mid-turn)",
        ),
        (
            "/model [name]",
            "show or switch the model (`auto` re-enables routing)",
        ),
        (
            "/models [--json]",
            "list live models from the active endpoint",
        ),
        ("/mode <name>", "agent | yolo | plan | coordinator"),
        ("/yolo", "shortcut for /mode yolo"),
        (
            "/preset [name|list|export|off]",
            "show, switch, export, or drop the runtime profile tier",
        ),
        ("/provider [name]", "show or switch the API provider"),
        (
            "/skills [prefix|--remote|sync]",
            "list local skills, browse the registry, refresh the cache",
        ),
        (
            "/skill <name>|install|update|uninstall|trust",
            "activate a skill or manage installed ones",
        ),
        (
            "/memory [show|path|clear|edit]",
            "inspect the memory file (`# text` quick-appends)",
        ),
        (
            "/extension <sub>",
            "list|info|enable|disable|status|reload|install|uninstall",
        ),
        (
            "/mods <sub>",
            "list|status|info|activate|enable|disable|remove|reload",
        ),
        ("/sessions [n]", "list saved sessions"),
        ("/agents", "list running sub-agents"),
        ("/compact", "compact the context now"),
        ("/new", "start a fresh session"),
        ("/logout", "remove the saved API key"),
        ("/exit", "quit (also Ctrl+D; Ctrl+C twice mid-turn)"),
    ];
    for (name, help) in rows {
        println!("  {:<20} {}", name, help.dimmed());
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
    // clean first run. Signing back in is explicit — there is no startup
    // key prompt; use `codesmith auth set` or `codesmith login`.
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
