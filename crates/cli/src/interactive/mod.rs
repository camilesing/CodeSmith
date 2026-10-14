//! Line-based interactive REPL frontend — the successor to the ratatui
//! TUI. A Claude Code / Codex style loop: the user types a line, the
//! engine runs a turn, output streams to the terminal, and a status line
//! above the prompt shows the session state (preset tier, context-window
//! occupancy, mode, sub-agent count, model, version).
//!
//! Construction mirrors the retired `tui::run_tui` minus most
//! terminal-mode concerns (no alt screen, no mouse; during a turn stdin
//! goes non-canonical only for the key tap — see `keys.rs`). See `state.rs`
//! for the known-limitations note.
//!
//! Prompt-time keys live in [`bindings`]. A key handler cannot print or
//! reach `ReplState`, so it records what to do in a shared cell and the loop
//! applies it when `readline` returns; the drafted line comes back as the
//! next prompt's initial text, so no action key costs the user their
//! typing. Known limitations of that arrangement, recorded beside the
//! behaviour it owns: each action key re-renders the prompt (the transcript
//! cannot be rewritten), and the restored cursor sits at the end of the
//! draft rather than where it was. Terminals that encode `Shift+Tab` as
//! `CSI 1;2Z` (the kitty keyboard protocol and friends) are not decoded by
//! rustyline 14, so that binding never fires there — Terminal.app, iTerm2,
//! and tmux defaults send `ESC [ Z`.

pub mod bindings;
pub mod commands;
pub mod completion;
pub mod keys;
pub mod path_index;
pub mod render;
pub mod shell_mode;
pub mod state;
pub mod status;
pub mod statusline;
pub mod theme;

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use colored::Colorize;

use crate::app_state::{InitialInput, ReasoningEffort, TuiOptions};
use crate::config::Config;
use crate::core::engine::{EngineHost, spawn_engine};
use crate::core::ops::Op;
use crate::hooks::{HookEvent, MessageSubmitOutcome};
use bindings::{Action, Bind, Bindings, KeyHandler, Shared as SharedBindings};
use completion::ReplHelper;
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
    let automation_scheduler = crate::automation_manager::spawn_scheduler(
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
                "mods discovered but not activated: {} — review with /mods list or the manage_mods tool",
                ids.join(", ")
            )
            .yellow()
        );
    }

    // Session resume.
    if let Some(resume_id) = options.resume_session_id.as_deref() {
        state::resume_session(&mut state, resume_id).await?;
    }

    // `[sandbox] auto_allow` is the user's standing decision: merge it into
    // the grant set the approval path reads (in memory, so removing a rule
    // leaves no stale grant behind).
    commands::permission_cmds::apply_config_rules(&mut state);

    // Prompt-time keys record what they want here; the loop applies it once
    // `readline` returns (see `bindings` for why the handler cannot act
    // directly).
    let bindings = Bindings::shared();
    let (mut editor, mut history_path) = build_editor(&state, bindings.clone())?;

    // session_start hooks run before the first prompt (awaited — a hook
    // that loads state or sets up logging has finished by the time the
    // user types).
    if state.hooks.has_hooks_for_event(HookEvent::SessionStart) {
        let executor = state.hooks.clone();
        let context = state.hook_context();
        // Surface the worker's failure (a panicking hook executor is
        // otherwise invisible), unlike the message_submit path which
        // prints both JoinErrors and per-hook failures.
        if let Err(join) =
            tokio::task::spawn_blocking(move || executor.execute(HookEvent::SessionStart, &context))
                .await
        {
            println!("{} session_start hook worker failed: {join}", "✗".red());
        }
    }

    print_banner(&state);

    // Top-level prompt mode (`codesmith -p …` / `codesmith pr <N>`):
    // submit the initial text once, then keep the session open (#2370).
    let mut pending_submit: Option<String> = match options.initial_input.as_ref() {
        Some(InitialInput::Submit(text)) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    };

    let mut consecutive_interrupts = 0u32;
    // The prompt text a key press or the external editor handed back.
    let mut prefill: Option<String> = None;
    // The edit mode the prompt was built with, so a `/config editor_mode`
    // change can be applied by rebuilding it.
    let mut editor_mode = state.settings.composer_vim_mode.clone();
    loop {
        if let Some(text) = pending_submit.take() {
            if let Err(err) = submit(&mut state, text).await {
                println!("{} {err}", "✗".red());
                break;
            }
            match render::pump_turn(&mut state).await {
                Ok(render::PumpExit::UserQuit | render::PumpExit::EngineGone) => break,
                Ok(render::PumpExit::TurnDone) => continue,
                Err(err) => {
                    println!("{} {err}", "✗".red());
                    break;
                }
            }
        }

        // A message queued mid-turn that the turn never picked up goes out
        // now, in the order it was typed (the pump steers what it can into
        // the running turn; these are the leftovers).
        if !state.queued_messages.is_empty() {
            let text = state.queued_messages.remove(0);
            println!("{}", "▶ sending the queued message".dimmed());
            if let Err(err) = submit(&mut state, text).await {
                println!("{} {err}", "✗".red());
                break;
            }
            match render::pump_turn(&mut state).await {
                Ok(render::PumpExit::UserQuit | render::PumpExit::EngineGone) => break,
                Ok(render::PumpExit::TurnDone) => continue,
                Err(err) => {
                    println!("{} {err}", "✗".red());
                    break;
                }
            }
        }

        if state.show_tasks {
            print_task_list(&state).await;
        }
        refresh_statusline(&mut state).await;
        // `/config editor_mode` changes the keymap rustyline was built with
        // and the editor has no runtime setter, so the prompt is rebuilt
        // here. History is re-read from its file, which every submitted line
        // has already updated.
        if state.settings.composer_vim_mode != editor_mode {
            let _ = save_history(&mut editor, history_path.as_deref());
            editor_mode = state.settings.composer_vim_mode.clone();
            match build_editor(&state, bindings.clone()) {
                Ok((rebuilt, rebuilt_history)) => {
                    editor = rebuilt;
                    history_path = rebuilt_history;
                    println!("{}", format!("— editor mode: {editor_mode}").dimmed());
                }
                Err(err) => println!(
                    "{}",
                    format!("warning: could not rebuild the prompt: {err}").yellow()
                ),
            }
        }
        println!("{}", status::status_line(&state));
        let _ = std::io::stdout().flush();
        // The hint reports these without holding `ReplState`.
        Bindings::lock(&bindings).set_display(
            state.tool_detail,
            state.show_tasks,
            state.queued_messages.len(),
        );
        // Whatever was typed mid-turn comes back as this prompt's text.
        if let Some(draft) = state.turn_end_draft.take() {
            prefill = Some(draft);
        }
        // Text a command prepared for the prompt (`/pr` fetches into a draft).
        // It wins over the mid-turn draft: the command ran later.
        if let Some(prepared) = state.next_input.take() {
            prefill = Some(prepared);
        }
        let prompt = prompt_str(&state);
        let read = match prefill.take() {
            Some(text) => editor.readline_with_initial(&prompt, ("", text.as_str())),
            None => editor.readline(&prompt),
        };
        // Drained before `read` is matched, so a Ctrl+C or Ctrl+D cannot
        // strand a key the user already pressed.
        let actions = Bindings::lock(&bindings).drain();
        let had_actions = !actions.is_empty();
        let mut next_prompt: Option<String> = None;
        for action in actions {
            if action == Action::TakeBackQueue {
                let queued: Vec<String> = state.queued_messages.drain(..).collect();
                if !queued.is_empty() {
                    next_prompt = Some(queued.join("\n"));
                    println!(
                        "{}",
                        format!("↑ took back {} queued message(s)", queued.len()).dimmed()
                    );
                }
                continue;
            }
            if action == Action::EditInEditor {
                let draft = Bindings::lock(&bindings).snapshot().to_string();
                // A missing editor binary is a printed notice, not a lost
                // draft — put the original text back (the function's own
                // doc promise, previously defeated by `?` aborting the
                // whole session).
                next_prompt = Some(match open_in_editor(&draft).await {
                    Ok(text) => text,
                    Err(err) => {
                        println!("{} editor failed, draft restored: {err}", "✗".red());
                        draft
                    }
                });
                continue;
            }
            if let Err(err) = apply_action(&mut state, action).await {
                println!("{} {err}", "✗".red());
                break;
            }
        }
        if let Some(text) = next_prompt {
            prefill = (!text.is_empty()).then_some(text);
            continue;
        }
        let line = match read {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) if had_actions => {
                // An action key ended the prompt on purpose: the draft it
                // interrupted comes back untouched.
                let draft = Bindings::lock(&bindings).snapshot().to_string();
                prefill = (!draft.is_empty()).then_some(draft);
                continue;
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                // Ctrl+C clears the prompt; the draft is kept so `Up` can
                // recall it (Claude Code's behaviour) and a second press
                // ends the session.
                let draft = Bindings::lock(&bindings).snapshot().to_string();
                Bindings::lock(&bindings).keep_draft(draft);
                consecutive_interrupts += 1;
                if consecutive_interrupts >= 2 {
                    break;
                }
                println!(
                    "{}",
                    "^C (press Ctrl+C again to quit · Up recalls the draft)".dimmed()
                );
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(err) => {
                // A broken stdin ends the loop like Eof — the shutdown tail
                // still runs instead of propagating out of run_repl.
                println!("{} input error: {err}", "✗".red());
                break;
            }
        };
        consecutive_interrupts = 0;
        // A `\` at the end of a line continued the prompt; it is a marker,
        // not content.
        let line = completion::strip_line_continuations(&line);
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

        // `!command` runs in the user's shell and joins the conversation
        // (Claude Code's shell mode). Checked before the command dispatch so
        // a shell line never reaches the slash-command table.
        if let Some(command) = shell_mode::command_from_line(trimmed) {
            if let Err(err) = run_shell_mode(&mut state, command).await {
                println!("{} {err}", "✗".red());
                break;
            }
            match render::pump_turn(&mut state).await {
                Ok(render::PumpExit::UserQuit | render::PumpExit::EngineGone) => break,
                Ok(render::PumpExit::TurnDone) => {}
                Err(err) => {
                    println!("{} {err}", "✗".red());
                    break;
                }
            }
            continue;
        }

        // `# text` quick-appends to memory — no turn fires. `##` and `#!`
        // fall through to normal submission (Markdown headings, shebangs).
        if let Some(entry) = commands::misc_cmds::quick_memory_entry(trimmed) {
            append_memory_entry(&state, entry).await;
            continue;
        }

        let (content, mut image_paths) = extract_attachments(trimmed, &state.workspace);
        image_paths.extend(
            state
                .pending_attachments
                .drain(..)
                .map(|path| path.display().to_string()),
        );
        if let Err(err) = submit_with_images(&mut state, content, image_paths).await {
            println!("{} {err}", "✗".red());
            break;
        }
        match render::pump_turn(&mut state).await {
            Ok(render::PumpExit::UserQuit | render::PumpExit::EngineGone) => break,
            Ok(render::PumpExit::TurnDone) => {}
            Err(err) => {
                println!("{} {err}", "✗".red());
                break;
            }
        }
    }

    // Graceful shutdown: tell the engine to stop before dropping the
    // handle.
    if let Some(engine) = state.engine.as_ref() {
        let _ = engine.send(Op::Shutdown).await;
    }
    // The automation scheduler only exits on its token — cancel it (and
    // give it a moment to leave) so it cannot keep waking every 15s and
    // ticking automations against managers that are shutting down. Harmless
    // in the CLI today only because the process exits right after; any
    // embedding or test that keeps the runtime alive would leak it.
    automation_cancel.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), automation_scheduler).await;
    let _ = save_history(&mut editor, history_path.as_deref());

    // session_end hooks run before the process exits (awaited so a hook
    // that flushes state finishes first). In-loop errors are converted to
    // a printed notice + `break`, so every exit path reaches this tail.
    if state.hooks.has_hooks_for_event(HookEvent::SessionEnd) {
        let executor = state.hooks.clone();
        let context = state.hook_context();
        // Same observability as SessionStart: a hook that flushes state on
        // the way out must not fail silently.
        if let Err(join) =
            tokio::task::spawn_blocking(move || executor.execute(HookEvent::SessionEnd, &context))
                .await
        {
            println!("{} session_end hook worker failed: {join}", "✗".red());
        }
    }

    println!("{}", "bye.".dimmed());
    Ok(())
}

/// The prompt marker. The accent comes from the theme (and `/color`), so the
/// prompt is the one place a line REPL can show a theme at all.
/// `/doctor [json]` — the same setup checks the CLI runs, inside the REPL.
///
/// Reuses `main_root::run_doctor` rather than reimplementing the checks: the
/// doctor's whole value is that there is one of it.
pub(crate) async fn doctor_command(state: &ReplState, arg: &str) {
    if arg.trim().eq_ignore_ascii_case("json") {
        if let Err(err) = crate::main_root::run_doctor_json(
            &state.config,
            &state.workspace,
            state.config_path.as_deref(),
        ) {
            println!("{} doctor --json failed: {err}", "✗".red());
        }
        return;
    }
    crate::main_root::run_doctor(
        &state.config,
        &state.workspace,
        state.config_path.as_deref(),
    )
    .await;
}

/// `/debug [on|off|status]` — verbose diagnostics for this session.
///
/// The flag is the one `logging::set_verbose` already exposed (the retired
/// TUI's verbose toggle); the session transcript is the log a `/debug`
/// investigation reads, so its path is printed with the status.
pub(crate) fn debug_command(state: &ReplState, arg: &str) {
    match arg.trim().to_ascii_lowercase().as_str() {
        "on" => {
            crate::logging::set_verbose(true);
            println!("{}", "verbose diagnostics on (this session)".green());
        }
        "off" => {
            crate::logging::set_verbose(false);
            println!("{}", "verbose diagnostics off".dimmed());
        }
        "" | "status" => {}
        other => {
            println!(
                "{} unknown argument {other:?} — usage: /debug [on|off|status]",
                "✗".red()
            );
            return;
        }
    }
    println!(
        "verbose: {}",
        if crate::logging::is_verbose() {
            "on"
        } else {
            "off"
        }
    );
    match state.current_session_id.as_deref() {
        Some(id) => println!("session log: {id} — /export writes the transcript to a file"),
        None => println!("session log: not started yet"),
    }
    println!(
        "{}",
        "level logging comes from --debug / CODESMITH_LOG, which are startup flags; /doctor checks the setup this session is running with"
            .dimmed()
    );
}

/// `/copy [N]` — put the Nth-from-last assistant reply on the clipboard.
///
/// Claude Code shows a block picker when the reply has code blocks; the
/// clipboard here takes the whole reply (the picker needs a fullscreen
/// renderer). `w`-to-file, one of its options, has no equivalent — `/export`
/// writes the transcript instead.
pub(crate) fn copy_command(state: &ReplState, arg: &str) {
    let index: usize = match arg.trim() {
        "" => 1,
        other => match other.parse() {
            Ok(n) if n > 0 => n,
            _ => {
                println!("{} usage: /copy [N]", "✗".red());
                return;
            }
        },
    };
    let replies: Vec<String> = state
        .api_messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| {
            let text: String = message
                .content
                .iter()
                .filter_map(|block| match block {
                    crate::models::ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        })
        .collect();
    let Some(text) = replies.iter().rev().nth(index - 1) else {
        println!(
            "{} no reply #{index} in this session ({} assistant repl(ies) so far)",
            "✗".red(),
            replies.len()
        );
        return;
    };
    match crate::clipboard::copy_text(text) {
        Ok(()) => println!(
            "{}",
            format!("copied answer #{index} ({} chars)", text.chars().count()).dimmed()
        ),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

/// `/theme [name|list]` — the accent the prompt and status dot use.
pub(crate) fn theme_command(state: &mut ReplState, arg: &str, persist: bool) {
    let value = arg.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("list") {
        println!("theme: {}", state.settings.theme);
        println!("  {}", theme::THEME_NAMES.join(" · "));
        println!(
            "{}",
            "a theme picks the prompt accent; status segments keep their semantic colors".dimmed()
        );
        return;
    }
    if !theme::THEME_NAMES.contains(&value.to_ascii_lowercase().as_str()) {
        println!(
            "{} unknown theme {value:?} — one of: {}",
            "✗".red(),
            theme::THEME_NAMES.join("|")
        );
        return;
    }
    state.settings.theme = value.to_ascii_lowercase();
    if persist {
        commands::config_cmds::persist_setting_string("theme", value);
    }
    println!(
        "theme = {value}{}",
        if persist { "" } else { " (this session)" }
    );
}

/// `/color [name|default]` — the prompt bar's color alone.
pub(crate) fn color_command(state: &mut ReplState, arg: &str, persist: bool) {
    let value = arg.trim();
    if value.is_empty() {
        println!(
            "prompt color: {}",
            state.settings.prompt_color.as_deref().unwrap_or("(theme)")
        );
        println!("  {}", theme::COLOR_NAMES.join(" · "));
        return;
    }
    if value.eq_ignore_ascii_case("default") {
        state.settings.prompt_color = None;
        if persist {
            commands::config_cmds::persist_setting("prompt_color", None);
        }
        println!("prompt color reset to the theme's accent");
        return;
    }
    if theme::parse_color(value).is_none() {
        println!(
            "{} unknown color {value:?} — one of: {}",
            "✗".red(),
            theme::COLOR_NAMES.join("|")
        );
        return;
    }
    state.settings.prompt_color = Some(value.to_ascii_lowercase());
    if persist {
        commands::config_cmds::persist_setting("prompt_color", Some(value));
    }
    println!(
        "prompt color = {value}{}",
        if persist { "" } else { " (this session)" }
    );
}

/// `/statusline [command|off]` — a shell command whose output joins the
/// status line.
pub(crate) fn statusline_command(state: &mut ReplState, arg: &str) {
    let value = arg.trim();
    if value.is_empty() {
        match state.settings.statusline_command.as_deref() {
            Some(command) => println!("statusline command: {command}"),
            None => println!(
                "{}",
                "no statusline command — /statusline <command> appends its stdout to the status line"
                    .dimmed()
            ),
        }
        return;
    }
    if value.eq_ignore_ascii_case("off") {
        state.settings.statusline_command = None;
        state.statusline_text = None;
        commands::config_cmds::persist_setting("statusline_command", None);
        println!("statusline command cleared");
        return;
    }
    state.settings.statusline_command = Some(value.to_string());
    commands::config_cmds::persist_setting("statusline_command", Some(value));
    println!("statusline command set — its stdout appears on the next status line");
}

/// Refresh the statusline segment when its command is due.
///
/// Runs under `spawn_blocking` with a hard timeout: a status command that
/// hangs must not hang the prompt, and the cached text is what the renderer
/// prints in the meantime.
async fn refresh_statusline(state: &mut ReplState) {
    let Some(command) = state.settings.statusline_command.clone() else {
        state.statusline_text = None;
        return;
    };
    if !crate::interactive::statusline::is_due() {
        return;
    }
    let output = tokio::time::timeout(
        STATUSLINE_TIMEOUT,
        tokio::task::spawn_blocking(move || crate::interactive::statusline::run(&command)),
    )
    .await;
    match output {
        Ok(Ok(Ok(text))) => {
            state.statusline_text = Some(text);
        }
        Ok(Ok(Err(err))) => {
            state.statusline_text = Some(format!("✗ {err}"));
        }
        Ok(Err(join)) => {
            state.statusline_text = Some(format!("✗ statusline task failed: {join}"));
        }
        Err(_) => {
            state.statusline_text = Some("✗ statusline command timed out".to_string());
        }
    }
}

/// How long a statusline command may run before its output is abandoned.
const STATUSLINE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// The message list `/btw` sends: the session's conversation, then the
/// question as the last user message.
///
/// `api_messages` is the REPL's copy of what the session has exchanged, so
/// the side question sees exactly what the model saw — and because the
/// result is never appended back, the next real turn is unchanged.
pub(crate) fn btw_request_messages(
    state: &ReplState,
    question: &str,
) -> Vec<crate::models::Message> {
    let mut messages = state.api_messages.clone();
    messages.push(crate::models::Message {
        role: "user".to_string(),
        content: vec![crate::models::ContentBlock::Text {
            text: format!(
                "Side question (answer from this conversation alone; do not use or request tools; \
                 this exchange will not be part of the conversation): {question}"
            ),
            cache_control: None,
        }],
    });
    messages
}

/// Run one `!command` line: print it, hand it to the model as a user
/// message, and let the turn answer.
///
/// The command runs unsandboxed because the user typed it (the approval
/// surface gates what the model runs); `respond_to_shell_commands = false`
/// makes the run report itself without starting a turn.
async fn run_shell_mode(state: &mut ReplState, command: &str) -> Result<()> {
    println!("{}", format!("$ {command}").dimmed());
    let (status, output) =
        match super::interactive::shell_mode::run(command, &state.workspace).await {
            Ok(result) => result,
            Err(err) => {
                println!("{} {err}", "✗".red());
                return Ok(());
            }
        };
    if !output.is_empty() {
        print!("{output}");
        if !output.ends_with('\n') {
            println!();
        }
    }
    println!("{}", format!("— {status}").dimmed());
    if !state.settings.respond_to_shell_commands {
        println!(
            "{}",
            "respond_to_shell_commands = false: the output is in the transcript but no turn was started"
                .dimmed()
        );
        return Ok(());
    }
    submit(
        state,
        super::interactive::shell_mode::transcript_message(command, &status, &output),
    )
    .await
}

fn prompt_str(state: &ReplState) -> String {
    let accent = theme::accent(
        &state.settings.theme,
        state.settings.prompt_color.as_deref(),
    );
    "❯ ".color(accent.prompt).to_string()
}

/// Carry out one key press. Everything a handler could not do itself lands
/// here: printing, engine traffic, and the settings a switch persists.
async fn apply_action(state: &mut ReplState, action: Action) -> Result<()> {
    match action {
        Action::CycleMode => {
            state.cycle_mode();
            println!(
                "{}",
                format!("switched to {} mode (Shift+Tab cycles)", state.mode.label()).dimmed()
            );
        }
        Action::ToggleDetail => {
            state.tool_detail = !state.tool_detail;
            if state.tool_detail {
                println!("{}", "— tool detail expanded (Ctrl+O to collapse)".dimmed());
                render::replay_turn_tool_detail(state);
            } else {
                println!("{}", "— tool detail collapsed (Ctrl+O to expand)".dimmed());
            }
        }
        Action::ToggleTaskList => {
            state.show_tasks = !state.show_tasks;
            if state.show_tasks {
                print_task_list(state).await;
            } else {
                println!("{}", "— task list hidden (Ctrl+T)".dimmed());
            }
        }
        Action::Stashed => println!(
            "{}",
            "— draft stashed (Ctrl+S on an empty prompt brings it back)".dimmed()
        ),
        Action::StashHint => println!("{}", "— nothing stashed yet".dimmed()),
        Action::DraftCleared => println!("{}", "— draft cleared (Up recalls it)".dimmed()),
        Action::ExitHint => println!(
            "{}",
            "— press Ctrl+D again within a second to quit".dimmed()
        ),
        Action::StopAllHint => println!(
            "{}",
            "— press Ctrl+X Ctrl+K again within 3 seconds to stop all background work".dimmed()
        ),
        Action::StopBackground => match state.engine.as_ref() {
            Some(engine) => {
                let _ = engine.send(Op::CancelAllBackgroundTasks).await;
            }
            None => println!("{}", "— no engine attached".dimmed()),
        },
        Action::CycleModel => cycle_model(state).await,
        Action::ToggleThinking => toggle_thinking(state),
        Action::Help => {
            println!("{}", "keyboard shortcuts:".bold());
            println!("{}", bindings::help_panel().dimmed());
        }
        Action::TakeBackQueue => println!("{}", "— nothing queued".dimmed()),
        // Handled by the caller, which has the prompt to restore.
        Action::EditInEditor => {}
    }
    Ok(())
}

/// `Alt+P`: switch between the session's model and the configured default.
///
/// Claude Code opens a model picker here. The line REPL has no picker, and
/// the provider's model list is only knowable by asking the endpoint (which
/// `/models` does) — so this toggles between the two ids the session already
/// knows and points at `/model` when there is nothing to toggle to.
async fn cycle_model(state: &mut ReplState) {
    let current = state.model_selection_for_persistence();
    let configured = state
        .config
        .default_text_model
        .clone()
        .filter(|name| !name.trim().is_empty() && *name != current);
    match configured {
        Some(next) => {
            state.set_model_selection(next.clone());
            if let Ok(engine) = state.engine() {
                let _ = engine
                    .send(Op::SetModel {
                        model: next.clone(),
                    })
                    .await;
            }
            println!(
                "{}",
                format!("model → {next} (Alt+P toggles · /model picks any id)").dimmed()
            );
        }
        None => println!(
            "{}",
            format!(
                "model {} — only one id configured; /model <id> switches",
                state.model_display_label()
            )
            .dimmed()
        ),
    }
}

/// `Alt+T`: extended thinking on or off. `Off` and `High` are the two
/// behaviourally distinct stops the effort ring is built around; the finer
/// tiers stay on `/effort` and `/config reasoning_effort`.
fn toggle_thinking(state: &mut ReplState) {
    let next = if state.reasoning_effort == ReasoningEffort::Off {
        ReasoningEffort::High
    } else {
        ReasoningEffort::Off
    };
    state.reasoning_effort = next;
    state.update_model_compaction_budget();
    println!(
        "{}",
        format!("extended thinking {} (Alt+T)", next.short_label()).dimmed()
    );
}

/// The `Ctrl+T` panel: the model's own checklist, which otherwise only
/// surfaces as collapsed tool lines.
async fn print_task_list(state: &ReplState) {
    let snapshot = state.todos.lock().await.snapshot();
    if snapshot.items.is_empty() {
        println!(
            "{}",
            "— no tasks yet; the model fills this with its checklist tools".dimmed()
        );
        return;
    }
    println!(
        "{}",
        format!(
            "tasks · {} item(s) · {}% done — Ctrl+T hides",
            snapshot.items.len(),
            snapshot.completion_pct
        )
        .bold()
    );
    for item in &snapshot.items {
        let marker = match item.status {
            codesmith_agent_runtime::tool_state::todo::TodoStatus::Completed => "✓".green(),
            codesmith_agent_runtime::tool_state::todo::TodoStatus::InProgress => "▸".cyan(),
            codesmith_agent_runtime::tool_state::todo::TodoStatus::Pending => "·".dimmed(),
        };
        println!("  {marker} {}", item.content);
    }
}

/// `Ctrl+G` / `Ctrl+X Ctrl+E`: hand the draft to `$VISUAL`/`$EDITOR`.
///
/// The editor runs with the terminal restored: rustyline is between prompts
/// here, so nothing is holding raw mode. A missing editor binary is a
/// printed notice, not a lost draft — the caller puts the original text
/// back.
async fn open_in_editor(draft: &str) -> Result<String> {
    // The editor round-trip is minutes of blocking time (plus fs IO
    // around it) — it must not sit on the async runtime worker.
    let draft = draft.to_string();
    tokio::task::spawn_blocking(move || open_in_editor_blocking(&draft))
        .await
        .context("editor task failed")?
}

fn open_in_editor_blocking(draft: &str) -> Result<String> {
    use std::io::Write as _;

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "vi".to_string()
            }
        });
    let mut file = tempfile::Builder::new()
        .prefix("codesmith-prompt-")
        .suffix(".md")
        .tempfile()
        .context("creating the editor buffer")?;
    file.write_all(draft.as_bytes())
        .context("writing the editor buffer")?;
    file.flush().context("flushing the editor buffer")?;
    let path = file.path().to_path_buf();

    // $VISUAL/$EDITOR commonly carries arguments ("code --wait", "vim -f");
    // `Command::new` with the whole string tries to exec a binary
    // literally named "code --wait" and fails to spawn.
    let mut editor_words = editor.split_whitespace();
    let program = editor_words.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(editor_words)
        .arg(&path)
        .status()
        .with_context(|| format!("running {editor}"))
        .map_err(|err| anyhow::anyhow!("{err} (set $VISUAL or $EDITOR)"))?;
    if !status.success() {
        println!(
            "{}",
            format!("— {editor} exited with {status}; draft unchanged").yellow()
        );
        return Ok(draft.to_string());
    }
    let edited = std::fs::read_to_string(&path).context("reading the editor buffer back")?;
    println!("{}", format!("— returned from {editor}").dimmed());
    Ok(edited)
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
            "workspace {} · /help lists commands (Tab completes, Shift+Tab cycles the mode) · ? shows the keyboard shortcuts · Ctrl+C cancels a turn",
            state.workspace.display()
        )
        .dimmed()
    );
}

/// The REPL's editor: rustyline with the slash-command helper attached.
/// rustyline runs completion only when a helper is present, which is why
/// typing `/` and pressing Tab used to do nothing at all.
type ReplEditor = rustyline::Editor<ReplHelper, rustyline::history::DefaultHistory>;

/// Every prompt-time binding, as `(key sequence, action)`. The decision
/// table itself lives in [`bindings::decide`]; this is only the key map.
///
/// Registering `Shift+Tab` also takes BackTab away from rustyline's default
/// `Cmd::CompleteBackward`: custom bindings are looked up before the default
/// keymap, so completion loses only its reverse candidate walk (`Tab` still
/// completes forward and lists the candidates).
fn key_bindings() -> Vec<(rustyline::Event, Bind)> {
    use rustyline::{Event, KeyCode, KeyEvent, Modifiers};

    fn one(code: KeyCode, modifiers: Modifiers) -> Event {
        Event::KeySeq(vec![KeyEvent(code, modifiers)])
    }
    fn ctrl(c: char) -> Event {
        one(KeyCode::Char(c), Modifiers::CTRL)
    }
    fn alt(c: char) -> Event {
        one(KeyCode::Char(c), Modifiers::ALT)
    }
    fn ctrl_pair(first: char, second: char) -> Event {
        Event::KeySeq(vec![
            KeyEvent(KeyCode::Char(first), Modifiers::CTRL),
            KeyEvent(KeyCode::Char(second), Modifiers::CTRL),
        ])
    }

    vec![
        (one(KeyCode::BackTab, Modifiers::NONE), Bind::ShiftTab),
        (one(KeyCode::Esc, Modifiers::NONE), Bind::Esc),
        (ctrl('d'), Bind::CtrlD),
        (ctrl('s'), Bind::CtrlS),
        (ctrl('o'), Bind::CtrlO),
        (ctrl('t'), Bind::CtrlT),
        (ctrl('g'), Bind::CtrlG),
        (ctrl_pair('x', 'k'), Bind::StopAll),
        (alt('p'), Bind::AltP),
        (alt('t'), Bind::AltT),
        // Newline: Ctrl+J reaches every terminal; the modifier forms only
        // fire where the terminal reports the modifier (see bindings.rs).
        (ctrl('j'), Bind::Newline),
        (one(KeyCode::Enter, Modifiers::ALT), Bind::Newline),
        (one(KeyCode::Enter, Modifiers::SHIFT), Bind::Newline),
        (one(KeyCode::Up, Modifiers::NONE), Bind::RecallDraft),
        (one(KeyCode::Char('?'), Modifiers::NONE), Bind::Question),
    ]
}

fn build_editor(
    state: &ReplState,
    bindings: SharedBindings,
) -> Result<(ReplEditor, Option<std::path::PathBuf>)> {
    // List mode, not in-place cycling: one Tab completes to the longest
    // common prefix, a second Tab prints every match in columns. The prompt
    // limit is raised past the registry size (77 names today) so a growing
    // command set never turns Tab into a "display all N?" question.
    let mut builder = rustyline::Config::builder()
        .completion_type(rustyline::CompletionType::List)
        .completion_prompt_limit(200)
        // `editor_mode = "vim"` in settings.toml (or `/config editor_mode`)
        // selects rustyline's vi keymap — motions, operators, text objects.
        .edit_mode(
            if state.settings.composer_vim_mode.eq_ignore_ascii_case("vim") {
                rustyline::EditMode::Vi
            } else {
                rustyline::EditMode::Emacs
            },
        )
        // The setting existed (documented as the input-history cap) but was
        // never read: rustyline's default of 100 silently won. Zero would
        // disable history outright, so the floor is one entry.
        .bracketed_paste(state.settings.bracketed_paste);
    if let Ok(with_history) = builder
        .clone()
        .max_history_size(state.settings.max_input_history.max(1))
    {
        builder = with_history;
    }
    let config = builder.build();
    let mut editor = ReplEditor::with_config(config)?;
    for (key, bind) in key_bindings() {
        let _ = editor.bind_sequence(
            key,
            rustyline::EventHandler::Conditional(Box::new(KeyHandler::new(bind, bindings.clone()))),
        );
    }
    // `@path` completion walks the workspace at most once per
    // `path_index::TTL`, bounded by the two mention settings (which were
    // written but never read until this wiring).
    let paths = path_index::PathIndex::new(
        state.workspace.clone(),
        state.settings.mention_walk_depth,
        state.settings.mention_menu_limit,
    );
    editor.set_helper(Some(ReplHelper::new(
        commands::user_cmd_store::user_command_names(Some(&state.workspace)),
        paths,
        bindings,
        Some(state.todos.clone()),
    )));
    let history = history_path(&state.workspace);
    if let Some(history) = history.as_ref() {
        if let Some(parent) = history.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if editor.load_history(history).is_err()
            && let Some(legacy) = legacy_history_path()
        {
            // Pre-upgrade history lived in one file for every workspace;
            // read it once so the first project after the upgrade keeps its
            // recall. The old file is left alone.
            let _ = editor.load_history(&legacy);
        }
    }
    Ok((editor, history))
}

/// Per-workspace history file. Claude Code keeps input history per working
/// directory; a single shared file mixed unrelated projects' prompts into
/// every session's Up-arrow recall.
fn history_path(workspace: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = crate::config::effective_home_dir()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&workspace.to_string_lossy().as_ref(), &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    Some(home.join("history").join(format!("{key:016x}")))
}

/// The pre-per-project history file, read once when a workspace has none.
fn legacy_history_path() -> Option<std::path::PathBuf> {
    crate::config::effective_home_dir().map(|home| home.join("history"))
}

fn save_history(editor: &mut ReplEditor, history: Option<&std::path::Path>) -> Result<()> {
    if let Some(history) = history {
        editor
            .save_history(history)
            .context("failed to save REPL history")?;
    }
    Ok(())
}

/// Split `@path` image attachments out of the input line. Non-image
/// tokens and non-existent paths stay in the message text. Relative paths
/// resolve against the session workspace (the REPL can be started with a
/// workspace other than the process CWD); the resolved path is what gets
/// attached.
fn extract_attachments(input: &str, workspace: &std::path::Path) -> (String, Vec<String>) {
    let mut image_paths = Vec::new();
    let mut words = Vec::new();
    for token in input.split_whitespace() {
        if let Some(path) = token.strip_prefix('@') {
            let resolved = if std::path::Path::new(path).is_absolute() {
                std::path::PathBuf::from(path)
            } else {
                workspace.join(path)
            };
            let matches_image = path
                .rsplit('.')
                .next()
                .map(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            if matches_image && resolved.is_file() {
                image_paths.push(resolved.display().to_string());
                continue;
            }
        }
        words.push(token);
    }
    (words.join(" "), image_paths)
}

/// Append one `#`-prefixed line to the session's memory file (KoD
/// entrypoint or flat memory file) and confirm — the input is consumed,
/// no turn starts.
async fn append_memory_entry(state: &ReplState, entry: &str) {
    let path = commands::misc_cmds::memory_file(state);
    let target = path.clone();
    let entry = entry.to_string();
    let result = tokio::task::spawn_blocking(move || {
        codesmith_agent_runtime::memory::append_entry(&target, &entry)
    })
    .await;
    match result {
        Ok(Ok(())) => println!("{}", format!("✎ remembered → {}", path.display()).dimmed()),
        Ok(Err(err)) => println!("{} {err}", "✗".red()),
        Err(err) => println!("{} memory worker failed: {err}", "✗".red()),
    }
}

/// Submit a plain user message (auto-model routing resolved per request).
pub(crate) async fn submit(state: &mut ReplState, content: String) -> Result<()> {
    submit_with_images(state, content, Vec::new()).await
}

async fn submit_with_images(
    state: &mut ReplState,
    mut content: String,
    image_paths: Vec<String>,
) -> Result<()> {
    // message_submit hooks transform or block the submitted text before it
    // reaches routing, session history, or the model — the transformed text
    // is what the model sees. Runs on `spawn_blocking`: the executor spawns
    // hook processes and waits on them (up to their timeout).
    if state.hooks.has_hooks_for_event(HookEvent::MessageSubmit) {
        let executor = state.hooks.clone();
        let context = state.hook_context();
        let original = content.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            executor.execute_message_submit_transform(&context, &original)
        })
        .await;
        match outcome {
            Ok(MessageSubmitOutcome::Blocked { reason }) => {
                println!("{} submission blocked: {reason}", "✗".red());
                return Ok(());
            }
            Ok(outcome) => {
                if let MessageSubmitOutcome::Replaced { text, .. } = &outcome {
                    content = text.clone();
                }
                if let Some(warning) = outcome.warning() {
                    println!("{}", warning.dimmed());
                }
            }
            Err(join_err) => {
                println!(
                    "{} message_submit hook worker failed ({join_err}); submitting original text",
                    "✗".red()
                );
            }
        }
    }

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
