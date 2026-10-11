//! Slash-command dispatcher for the REPL.
//!
//! The full TUI command surface is being ported here command-by-command
//! against [`ReplState`]; the retired `commands/` tree coupled every
//! handler to the ratatui `App`. Handlers print directly to stdout and
//! mutate state / send engine ops.

use codesmith_config::presets::PresetCatalog;
use colored::Colorize;

use super::state::ReplState;
use crate::core::ops::Op;
use crate::presets;
use crate::session_manager::SessionManager;
use crate::tui::app::AppMode;

/// Dispatch one `/command args…` line. Returns `true` when the REPL
/// should exit.
pub async fn dispatch(state: &mut ReplState, line: &str) -> bool {
    let mut parts = line.split_whitespace();
    let Some(raw) = parts.next() else {
        return false;
    };
    let command = raw.trim_start_matches('/');
    let arg_str = line[raw.len()..].trim().to_string();

    match command {
        "help" | "?" => {
            print_help();
            false
        }
        "exit" | "quit" | "q" => true,
        "clear" => {
            print!("\x1b[2J\x1b[H");
            false
        }
        "status" => {
            print_status(state);
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
            cmd_sessions(&arg_str);
            false
        }
        "agents" => {
            cmd_agents(state);
            false
        }
        "compact" => {
            if let Ok(engine) = state.engine() {
                let _ = engine.send(Op::CompactContext).await;
                println!("{}", "compacting context…".magenta());
            }
            false
        }
        "new" => {
            cmd_new(state).await;
            false
        }
        "logout" => {
            cmd_logout(state);
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
    let rows: [(&str, &str); 13] = [
        ("/help", "this list"),
        (
            "/status",
            "session status: tokens, cost, model, mode, preset, agents",
        ),
        (
            "/model [name]",
            "show or switch the model (`auto` re-enables routing)",
        ),
        ("/mode <name>", "agent | yolo | plan | coordinator"),
        ("/yolo", "shortcut for /mode yolo"),
        (
            "/preset [name|list]",
            "show or switch the runtime profile tier",
        ),
        ("/provider [name]", "show or switch the API provider"),
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

fn cmd_sessions(arg: &str) {
    let limit = arg.trim().parse::<usize>().unwrap_or(20).clamp(1, 100);
    let Ok(manager) = SessionManager::default_location() else {
        println!("{} could not open the session store", "✗".red());
        return;
    };
    let Ok(mut sessions) = manager.list_sessions() else {
        println!("{} could not list sessions", "✗".red());
        return;
    };
    sessions.truncate(limit);
    if sessions.is_empty() {
        println!("no saved sessions");
        return;
    }
    println!("{}", "saved sessions (newest first):".bold());
    for meta in sessions {
        println!(
            "  {}  {}  {}",
            crate::session_manager::truncate_id(&meta.id),
            meta.updated_at.format("%Y-%m-%d %H:%M"),
            if meta.title.is_empty() {
                "(untitled)"
            } else {
                meta.title.as_str()
            }
        );
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

async fn cmd_new(state: &mut ReplState) {
    state.api_messages.clear();
    state.system_prompt = None;
    state.current_session_id = None;
    state.session = Default::default();
    state.agent_progress.clear();
    state.subagent_cache.clear();
    if let Ok(engine) = state.engine() {
        let _ = engine
            .send(Op::SyncSession {
                session_id: None,
                messages: Vec::new(),
                system_prompt: None,
                system_prompt_override: false,
                model: state.model.clone(),
                workspace: state.workspace.clone(),
            })
            .await;
    }
    println!("{}", "started a fresh session".green());
}

fn cmd_logout(_state: &mut ReplState) {
    // Port of the retired /logout (commands/config.rs): wipe the stored
    // key. Re-entry happens on the next start — the first-run CLI wizard
    // re-runs once the onboarding marker is also removed.
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
                "logged out: API key removed; the first-run wizard will run on next start".green()
            );
        }
        Err(err) => println!("{} logout failed: {err}", "✗".red()),
    }
}
