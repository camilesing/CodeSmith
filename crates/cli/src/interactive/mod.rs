//! Line-based interactive REPL frontend — the successor to the ratatui
//! TUI. A Claude Code / Codex style loop: the user types a line, the
//! engine runs a turn, output streams to the terminal, and a status line
//! above the prompt shows the session state (preset tier, context-window
//! occupancy, mode, sub-agent count, model, version).
//!
//! Construction mirrors the retired `tui::run_tui` minus every
//! terminal-mode concern (no raw mode, no alt screen, no mouse). See
//! `state.rs` for the known-limitations note.

pub mod commands;
pub mod render;
pub mod state;
pub mod status;

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use colored::Colorize;

use crate::app_state::{InitialInput, TuiOptions};
use crate::config::Config;
use crate::core::engine::{EngineHost, spawn_engine};
use crate::core::ops::Op;
use state::ReplState;

/// Image extensions accepted for `@path` attachments in the composer.
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

pub async fn run_repl(
    config: &Config,
    options: TuiOptions,
    telemetry_sink: codesmith_agent_runtime::telemetry::TelemetrySink,
) -> Result<()> {
    let mut state = ReplState::new(options.clone(), config);
    state.telemetry_sink = Some(telemetry_sink);

    // Named preset layer (mirrors the retired run_tui startup dial).
    state.apply_preset_at_startup(config.preset.as_deref().unwrap_or("middle"));

    // Durable task manager + automation scheduler (same wiring run_tui
    // performed; nothing here is UI-specific).
    let task_manager = crate::task_manager::TaskManager::start(
        crate::task_manager::TaskManagerConfig::from_runtime(
            config,
            state.workspace.clone(),
            Some(state.model.clone()),
            Some(state.max_subagents.clamp(1, 4)),
        ),
        config.clone(),
    )
    .await?;
    let automations = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::automation_manager::AutomationManager::default_location()?,
    ));
    let automation_cancel = tokio_util::sync::CancellationToken::new();
    let _automation_scheduler = crate::automation_manager::spawn_scheduler(
        automations.clone(),
        task_manager.clone(),
        automation_cancel.clone(),
        crate::automation_manager::AutomationSchedulerConfig::default(),
    );

    // Runtime services: shell, tasks, automations, hooks, notifier, index.
    if state.index_service.is_none() {
        state.index_service = crate::index::build_index_service(&state.workspace, config);
    }
    state.runtime_services = crate::tools::spec::RuntimeToolServices {
        shell_manager: Some(crate::tools::shell::wrap_shell_manager(
            state.shell_manager.clone(),
        )),
        task_manager: Some(crate::task_manager::wrap_task_manager(task_manager.clone())),
        automations: Some(crate::automation_manager::wrap_automation_manager(
            automations,
        )),
        task_data_dir: Some(task_manager.data_dir()),
        active_task_id: None,
        active_thread_id: None,
        hook_executor: Some(std::sync::Arc::new(state.hooks.clone())),
        handle_store: state.runtime_services.handle_store.clone(),
        rlm_sessions: state.runtime_services.rlm_sessions.clone(),
        task_v2_manager: None,
        task_mailbox: None,
        team_context: None,
        permission_request_registry: None,
        background_task_registry: state.runtime_services.background_task_registry.clone(),
        team_sender: Some(crate::tools::team::team_lead_name().to_string()),
        notifier: Some(crate::notifications::wrap_notifier()),
        index_service: state.index_service.clone(),
    };

    let engine_config = state::build_engine_config(&state, config)?;
    let engine_handle = spawn_engine(
        engine_config,
        config,
        EngineHost {
            runtime_services: state.runtime_services.clone(),
            hooks: Some(state.hooks.clone()),
            shell_manager: Some(state.shell_manager.clone()),
            ..Default::default()
        },
    );
    state.extension_runner = engine_handle.extension_runner.clone();
    state.tool_catalog = Some(engine_handle.tool_catalog.clone());
    state.fact_ledger = Some(engine_handle.fact_ledger.clone());
    state.recent_read_files = Some(engine_handle.recent_read_files.clone());
    state.engine = Some(engine_handle);
    state.task_manager = Some(task_manager.clone());

    // Passive first-run notice for discovered-but-unactivated mods (the
    // consent gate), mirroring the retired startup path.
    if let Some(engine) = state.engine.as_ref()
        && !engine.mods_pending.is_empty()
    {
        let ids: Vec<String> = engine
            .mods_pending
            .iter()
            .map(|pending| pending.name.clone())
            .collect();
        println!(
            "{}",
            format!(
                "mods discovered but not activated: {} — /mods list to review",
                ids.join(", ")
            )
            .yellow()
        );
    }

    // Session resume.
    if let Some(resume_id) = options.resume_session_id.as_deref() {
        state::resume_session(&mut state, resume_id).await?;
    }

    let (mut editor, history_path) = build_editor()?;

    print_banner(&state);

    // Top-level prompt mode (`codesmith -p …` / `codesmith pr <N>`):
    // submit the initial text once, then keep the session open (#2370).
    let mut pending_submit: Option<String> = match options.initial_input.as_ref() {
        Some(InitialInput::Submit(text)) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    };

    let mut consecutive_interrupts = 0u32;
    loop {
        if let Some(text) = pending_submit.take() {
            submit(&mut state, text).await?;
            match render::pump_turn(&mut state).await? {
                render::PumpExit::UserQuit | render::PumpExit::EngineGone => break,
                render::PumpExit::TurnDone => continue,
            }
        }

        println!("{}", status::status_line(&state));
        let _ = std::io::stdout().flush();
        let prompt = prompt_str();
        let line = match editor.readline(&prompt) {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                consecutive_interrupts += 1;
                if consecutive_interrupts >= 2 {
                    break;
                }
                println!("{}", "^C (press Ctrl+C again to quit)".dimmed());
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(err) => return Err(anyhow::anyhow!("input error: {err}")),
        };
        consecutive_interrupts = 0;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(trimmed);
        let _ = save_history(&mut editor, history_path.as_deref());

        if trimmed.starts_with('/') {
            if commands::dispatch(&mut state, trimmed).await {
                break;
            }
            continue;
        }

        let (content, mut image_paths) = extract_attachments(trimmed);
        image_paths.extend(
            state
                .pending_attachments
                .drain(..)
                .map(|path| path.display().to_string()),
        );
        submit_with_images(&mut state, content, image_paths).await?;
        match render::pump_turn(&mut state).await? {
            render::PumpExit::UserQuit | render::PumpExit::EngineGone => break,
            render::PumpExit::TurnDone => {}
        }
    }

    // Graceful shutdown: tell the engine to stop before dropping the
    // handle.
    if let Some(engine) = state.engine.as_ref() {
        let _ = engine.send(Op::Shutdown).await;
    }
    let _ = save_history(&mut editor, history_path.as_deref());
    println!("{}", "bye.".dimmed());
    Ok(())
}

fn prompt_str() -> String {
    "❯ ".cyan().to_string()
}

fn print_banner(state: &ReplState) {
    println!(
        "{} {} — {} · {} mode · preset {}",
        "CodeSmith".bold(),
        env!("CODESMITH_BUILD_VERSION").dimmed(),
        state.model_display_label(),
        state.mode.label(),
        state.active_preset.as_deref().unwrap_or("(none)"),
    );
    println!(
        "{}",
        format!(
            "workspace {} · /help for commands · Ctrl+C cancels a turn",
            state.workspace.display()
        )
        .dimmed()
    );
}

fn build_editor() -> Result<(rustyline::DefaultEditor, Option<std::path::PathBuf>)> {
    let mut editor = rustyline::DefaultEditor::new()?;
    let history = history_path();
    if let Some(history) = history.as_ref() {
        if let Some(parent) = history.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if editor.load_history(history).is_err() {
            // First run — no history yet.
        }
    }
    Ok((editor, history))
}

fn history_path() -> Option<std::path::PathBuf> {
    crate::config::effective_home_dir().map(|home| home.join("history"))
}

fn save_history(
    editor: &mut rustyline::DefaultEditor,
    history: Option<&std::path::Path>,
) -> Result<()> {
    if let Some(history) = history {
        editor
            .save_history(history)
            .context("failed to save REPL history")?;
    }
    Ok(())
}

/// Split `@path` image attachments out of the input line. Non-image
/// tokens and non-existent paths stay in the message text.
fn extract_attachments(input: &str) -> (String, Vec<String>) {
    let mut image_paths = Vec::new();
    let mut words = Vec::new();
    for token in input.split_whitespace() {
        if let Some(path) = token.strip_prefix('@')
            && let Some(extension) = path.rsplit('.').next().map(str::to_ascii_lowercase)
            && IMAGE_EXTENSIONS.contains(&extension.as_str())
            && std::path::Path::new(path).is_file()
        {
            image_paths.push(path.to_string());
        } else {
            words.push(token);
        }
    }
    (words.join(" "), image_paths)
}

/// Submit a plain user message (auto-model routing resolved per request).
pub(crate) async fn submit(state: &mut ReplState, content: String) -> Result<()> {
    submit_with_images(state, content, Vec::new()).await
}

async fn submit_with_images(
    state: &mut ReplState,
    content: String,
    image_paths: Vec<String>,
) -> Result<()> {
    let engine = state.engine()?.clone();

    // Auto-model routing per request (mirror of the retired dispatch path
    // / `resolve_cli_auto_route` in exec).
    let (effective_model, effective_reasoning_effort) = if state.auto_model {
        let selection = crate::interactive::commands::auto_route::resolve_auto_route(
            &state.config,
            &content,
            "",
            "auto",
            "auto",
        )
        .await;
        state.last_effective_model = Some(selection.model.clone());
        if let Some(effort) = selection.reasoning_effort {
            state.last_effective_reasoning_effort = Some(effort);
        }
        (
            selection.model,
            selection
                .reasoning_effort
                .map(|effort| effort.as_setting().to_string()),
        )
    } else {
        (
            state.model.clone(),
            Some(state.reasoning_effort.as_setting().to_string()),
        )
    };

    engine
        .send(Op::SendMessage {
            content,
            image_paths: image_paths.into_iter().map(PathBuf::from).collect(),
            mode: state.mode,
            model: effective_model,
            goal_objective: state.hunt.quarry.clone(),
            allowed_tools: state.active_allowed_tools.clone(),
            blocked_tools: state.active_blocked_tools.clone().unwrap_or_default(),
            reasoning_effort: effective_reasoning_effort,
            reasoning_effort_auto: state.auto_model,
            auto_model: state.auto_model,
            allow_shell: state.allow_shell,
            trust_mode: state.trust_mode,
            auto_approve: state.approval_mode == codesmith_agent_runtime::mode::ApprovalMode::Auto,
            translation_enabled: state.translation_enabled,
            show_thinking: state.show_thinking,
            is_simple: state.is_simple,
            approval_mode: state.approval_mode,
        })
        .await
        .context("failed to send message to engine")
}
