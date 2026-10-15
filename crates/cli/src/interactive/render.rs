//! Turn event pump for the REPL: drains `EngineHandle::rx_event` for one
//! turn and renders it as plain terminal lines (port of the exec pump in
//! `main::run_exec_agent` plus the accounting half of the retired TUI
//! event loop).
//!
//! Rendering protocol: permanent output (assistant text, tool lines) goes
//! to stdout as whole lines. Tool calls print one collapsed line on
//! completion by default (Ctrl+O in-turn, `/expand` after the fact — the
//! terminal is append-only, so expansion is a replay, never a rewrite).
//! While the turn runs, a transient region of up to two rows is refreshed
//! in place via `ESC[2K CR`: the live status line, and — once the user types
//! — the composer the typed line lands in. The region is never interleaved
//! with permanent text: every permanent write clears it first, and the
//! blocking prompts (approval / user input / elevation) clear it too before
//! they take stdin. The status-line ticker additionally yields while a
//! permanent row is open, so streamed text is never force-closed (broken
//! mid-word) just to refresh a frame the next delta would erase anyway.
//!
//! Known limitations (recorded beside the behaviour it owns): assistant text
//! is printed verbatim as the model streams it — there is no markdown
//! rendering and no width-aware reflow, so `**`/backticks and any line breaks
//! the model wrote reach the terminal as-is, and the terminal owns wrapping
//! (and therefore where a CJK paragraph breaks). Tool lines are single lines
//! assembled from column-capped parts (`tool_summary`), but their total width
//! is not clamped to the terminal, so a narrow terminal still wraps them.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use colored::Colorize;
use unicode_width::UnicodeWidthStr;

use super::keys::TurnKey;
use super::state::ReplState;
use crate::core::events::{Event, TurnOutcomeStatus};
use crate::core::ops::Op;
use crate::hooks::HookEvent;
use codesmith_agent_runtime::error_taxonomy::ErrorEnvelope;
use codesmith_agent_runtime::mode::ApprovalMode;

/// Interval between live-line refreshes (spinner/elapsed/ctx).
const LIVE_REFRESH: Duration = Duration::from_millis(250);

/// Prompt for the mid-turn composer row.
const COMPOSER_PROMPT: &str = "❯ ";

/// How long the second Ctrl+X Ctrl+K has to arrive to stop everything.
const STOP_ALL_WINDOW: Duration = Duration::from_secs(3);

/// Why the pump returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpExit {
    /// The engine emitted `TurnComplete`.
    TurnDone,
    /// The event channel closed — the engine task is gone.
    EngineGone,
    /// The user pressed Ctrl+C twice.
    UserQuit,
}

/// The mid-turn composer: what the user types while a turn streams.
///
/// Pure buffer editing so the editing rules are unit-tested; drawing the row
/// and parking the cursor is [`TurnView`]'s job. Insert/delete are
/// char-aware, so multi-byte input (and paste) never splits a character.
#[derive(Default)]
struct Composer {
    buffer: String,
    /// Cursor position as a byte offset into `buffer`, always on a char
    /// boundary.
    cursor: usize,
}

impl Composer {
    fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn insert(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some(prev) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
            self.buffer.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if let Some(prev) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.buffer[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    fn kill_to_end(&mut self) -> String {
        self.buffer.split_off(self.cursor)
    }

    fn kill_to_start(&mut self) -> String {
        let head: String = self.buffer.drain(..self.cursor).collect();
        self.cursor = 0;
        head
    }

    /// Ctrl+W: back to the previous whitespace, matching readline and
    /// Claude Code (one press removes a whole path).
    fn kill_word(&mut self) -> String {
        let head = &self.buffer[..self.cursor];
        let cut = head
            .trim_end()
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + 1);
        let removed = self.buffer[cut..self.cursor].to_string();
        self.buffer.replace_range(cut..self.cursor, "");
        self.cursor = cut;
        removed
    }

    /// Empty the composer and hand the text over (Enter, or send-now).
    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.buffer)
    }
}

/// One-turn rendering state.
struct TurnView {
    live_active: bool,
    current_tool: Option<String>,
    ends_with_newline: bool,
    /// Started-but-not-completed tool calls, keyed by tool-call id — the
    /// collapsed form prints only on completion. Keyed, not a single slot:
    /// one message can carry several calls of the same tool (two `agent_open`
    /// calls, one per repository), and a by-name slot pairs a completion with
    /// whichever call was announced last, mislabeling both lines.
    pending_tools: HashMap<String, (String, Option<String>)>,
    /// The last permanent line was a tool/agent line; the next assistant
    /// message gets a blank separator so the answer stands out.
    after_tool_line: bool,
    /// The mid-turn composer; empty means its row is not drawn.
    composer: Composer,
    /// Rows the live region occupies right now: 0, 1 (status), or 2 (status
    /// + composer).
    live_rows: usize,
    /// When Ctrl+X Ctrl+K was last pressed, for its confirm window.
    stop_all_at: Option<Instant>,
}

impl TurnView {
    fn new() -> Self {
        Self {
            live_active: false,
            current_tool: None,
            ends_with_newline: true,
            pending_tools: HashMap::new(),
            after_tool_line: false,
            composer: Composer::default(),
            live_rows: 0,
            stop_all_at: None,
        }
    }

    /// Clear the whole live region (status row and composer row).
    fn clear_live(&mut self) {
        match self.live_rows {
            // Clear the composer row first, step up, clear the status row.
            2 => eprint!("\x1b[2K\r\x1b[1A\x1b[2K\r"),
            1 => eprint!("\x1b[2K\r"),
            _ => {}
        }
        self.live_rows = 0;
        self.live_active = false;
    }

    /// Claim the cursor's row(s) for one live frame.
    ///
    /// A live frame is a whole-row write, so it closes an open permanent row
    /// first (a prompt whose answer arrived without an echoed newline) and
    /// then erases whatever the row held before redrawing from column 0.
    /// Without both halves the frame shares a row with permanent text: it
    /// was appended to the open row, and the next frame's `ESC[2K` erased
    /// that row's real content with it.
    ///
    /// The composer row is drawn from the same call, so a keystroke only has
    /// to update the buffer and redraw. The caller owns the row-closing
    /// policy: the ticker goes through [`Self::tick_live`], which never
    /// closes a row, while composer redraws close one deliberately — user
    /// input must be visible immediately.
    fn draw_live(&mut self, frame: &str) {
        self.clear_live();
        if !self.ends_with_newline {
            println!();
            self.ends_with_newline = true;
        }
        eprint!("\r\x1b[K{frame}");
        self.live_rows = 1;
        if !self.composer.is_empty() {
            eprint!("\n\r\x1b[K{COMPOSER_PROMPT}{}", self.composer.buffer);
            // Park the cursor at the caret. Column math is display width,
            // and assumes the row fits the terminal (see the module note in
            // keys.rs about what the composer does not do).
            let column = UnicodeWidthStr::width(COMPOSER_PROMPT)
                + UnicodeWidthStr::width(&self.composer.buffer[..self.composer.cursor]);
            if column > 0 {
                eprint!("\r\x1b[{column}C");
            }
            self.live_rows = 2;
        }
        let _ = std::io::stderr().flush();
        self.live_active = true;
    }

    /// One status-line refresh from the `LIVE_REFRESH` ticker.
    ///
    /// The ticker never closes an open permanent row: doing so breaks a
    /// streamed line mid-word wherever the stream happened to pause (the
    /// break is permanent — the terminal is append-only), while the frame
    /// itself is erased by the next delta within milliseconds. So an open
    /// row simply skips the frame; it resumes once the row closes.
    fn tick_live(&mut self, frame: &str) {
        if !self.ends_with_newline {
            return;
        }
        self.draw_live(frame);
    }
}

/// Read one line from stdin without holding the async worker hostage.
///
/// The prompt is written without a trailing newline so the answer echoes on the
/// prompt's own row. A terminal echoes the Enter that ends the answer, which
/// terminates that row; a pipe, or Ctrl+D, does not — and an open row is what
/// let a later live frame glue itself to the prompt text.
async fn read_stdin_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let echo_terminates_row = std::io::stdin().is_terminal();
    let prompt_owned = prompt.to_string();
    let (line, hit_eof) = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        let read = std::io::stdin().read_line(&mut line);
        let _ = prompt_owned;
        (line.trim().to_string(), matches!(read, Ok(0) | Err(_)))
    })
    .await
    .unwrap_or_default();
    if hit_eof || !echo_terminates_row {
        println!();
    }
    line
}

/// One-line action label for an approval prompt: the call's argument summary
/// when it has one, else the first sentence of the tool's static description.
///
/// The description is the wrong label for the question — a paragraph for
/// shell-like tools, it buried the actual call under prose and read as if the
/// whole paragraph were the thing being approved.
fn approval_action(description: &str, args_summary: Option<&str>) -> String {
    if let Some(args) = args_summary.filter(|args| !args.trim().is_empty()) {
        return args.to_string();
    }
    let first_line = description.lines().next().unwrap_or(description);
    let first_sentence = first_line
        .split_once(". ")
        .map_or(first_line, |(head, _)| head);
    crate::localization::truncate_to_width(first_sentence, APPROVAL_LABEL_MAX)
}

/// Cap on the fallback action label, in terminal columns (CJK counts as two),
/// so the header stays one row on a terminal wider than the cap.
const APPROVAL_LABEL_MAX: usize = 100;

/// What `[a]` actually covers for this tool, in the user's words.
///
/// The scope is decided engine-side by `build_approval_grouping_key`; its
/// `grouping_key_kind` classifier is the shared source of truth for which shape
/// a tool produces, so the prompt can say what "always" will cover instead of
/// implying a per-tool grant. For most tools the grouping key is the *input
/// hash* — "always" then covers this exact call, no more than `[e]` denies.
fn approval_always_scope(tool_name: &str) -> &'static str {
    use codesmith_agent_runtime::tools::approval_cache::{GroupingKeyKind, grouping_key_kind};
    match grouping_key_kind(tool_name) {
        GroupingKeyKind::ShellPrefix => "this command family",
        GroupingKeyKind::PatchPaths => "patches to the same paths",
        GroupingKeyKind::NetHost => "requests to the same host",
        GroupingKeyKind::InputHash => "this exact call",
    }
}

/// The approval question, whole: what is being asked, why, and what each answer
/// scopes to. Ends at the unterminated prompt — the answer echoes on that row,
/// and [`read_stdin_line`] decides whether the row still needs terminating.
fn approval_frame(
    tool_name: &str,
    description: &str,
    args_summary: Option<&str>,
    intent_summary: Option<&str>,
) -> String {
    let mut frame = format!(
        "{} approval needed: {} — {}\n",
        "⚠".yellow(),
        tool_name.bold(),
        approval_action(description, args_summary)
    );
    if let Some(intent) = intent_summary.map(str::trim).filter(|i| !i.is_empty()) {
        frame.push_str(&format!("{}\n", format!("  intent: {intent}").dimmed()));
    }
    frame.push_str(&format!(
        "allow? [y] run it once · [n] skip · [a] always allow {scope} for this session \
         · [e] never this exact call: ",
        scope = approval_always_scope(tool_name)
    ));
    frame
}

/// Pump engine events until the current turn finishes.
pub async fn pump_turn(state: &mut ReplState) -> anyhow::Result<PumpExit> {
    let mut view = TurnView::new();
    let mut cancelled_once = false;
    let mut live_ticker = tokio::time::interval(LIVE_REFRESH);
    live_ticker.tick().await; // first tick fires immediately; discard
    let mut keys = super::keys::KeyTap::start();

    let engine = match state.engine.as_ref() {
        Some(engine) => engine.clone(),
        None => return Ok(PumpExit::EngineGone),
    };

    loop {
        let event = tokio::select! {
            event = async {
                let mut rx = engine.rx_event.write().await;
                rx.recv().await
            } => event,
            key = keys.recv() => {
                // None: the tap is gone (non-TTY stdin) — the composer and
                // its keys are unavailable; the turn runs on.
                if let Some(key) = key {
                    handle_turn_key(state, &mut view, key, &engine).await;
                }
                continue;
            }
            _ = live_ticker.tick() => {
                let frame = super::status::live_line(state, view.current_tool.as_deref());
                view.tick_live(&frame);
                continue;
            }
            _ = tokio::signal::ctrl_c() => {
                if cancelled_once {
                    println!();
                    return Ok(PumpExit::UserQuit);
                }
                cancelled_once = true;
                engine.cancel();
                view.clear_live();
                println!("{}", "^C — cancelling (Ctrl+C again to quit)".yellow());
                continue;
            }
        };

        let Some(event) = event else {
            view.clear_live();
            return Ok(PumpExit::EngineGone);
        };

        match event {
            Event::TurnStarted { .. } => {
                state.is_loading = true;
                state.turn_started_at = Some(Instant::now());
                state.tool_log_turn += 1;
                save_crash_recovery_checkpoint(state);
            }
            Event::MessageStarted { .. } => {
                view.clear_live();
                if view.after_tool_line {
                    println!();
                    view.after_tool_line = false;
                }
            }
            Event::MessageDelta { content, .. } => {
                view.clear_live();
                // Bold so the answer stands out between the styled tool
                // lines around it.
                print!("{}", content.bold());
                let _ = std::io::stdout().flush();
                view.ends_with_newline = content.ends_with('\n');
            }
            Event::MessageComplete { .. } => {
                view.clear_live();
                if !view.ends_with_newline {
                    println!();
                    view.ends_with_newline = true;
                }
            }
            Event::ThinkingDelta { .. }
            | Event::ThinkingStarted { .. }
            | Event::ThinkingComplete { .. } => {
                // Known limitation: reasoning deltas are not streamed in
                // the REPL (see module notes in state.rs).
            }
            Event::LlmRequest { .. } => {}
            Event::ToolCallStarted { id, name, input } => {
                view.clear_live();
                let args_summary = crate::tool_summary::summarize_tool_args(&input);
                if state.tool_detail {
                    // Expanded form keeps the immediate ⏺ line; the
                    // collapsed form stays silent until completion (the
                    // live line shows the running tool meanwhile).
                    let args = args_summary
                        .as_ref()
                        .map(|summary| format!("({summary})"))
                        .unwrap_or_default();
                    println!("{} {}{}", "⏺".cyan(), name.cyan(), args.dimmed());
                }
                view.current_tool = Some(name.clone());
                view.pending_tools.insert(id, (name, args_summary));
            }
            Event::ToolCallProgress { .. } => {}
            Event::ToolCallComplete { id, name, result } => {
                view.clear_live();
                if view.current_tool.as_deref() == Some(name.as_str()) {
                    view.current_tool = None;
                }
                let args_summary = view.pending_tools.remove(&id).and_then(|(_, args)| args);
                let args = args_summary
                    .as_ref()
                    .map(|summary| format!("({summary})"))
                    .unwrap_or_default();
                match result {
                    Ok(output) => {
                        let summary = crate::tool_summary::summarize_tool_output(&output.content);
                        if state.tool_detail {
                            if summary.is_empty() {
                                println!("  {} ok", "⎿".dimmed());
                            } else {
                                println!("  {} {}", "⎿".dimmed(), summary.dimmed());
                            }
                        } else {
                            let tail = if summary.is_empty() {
                                "ok".to_string()
                            } else {
                                summary
                            };
                            println!(
                                "{} {}{} {} {}",
                                "⏺".cyan(),
                                name.cyan(),
                                args.dimmed(),
                                "⎿".dimmed(),
                                tail.dimmed()
                            );
                        }
                        push_tool_record(state, &name, args_summary, Some(output.content), None);
                    }
                    Err(err) => {
                        let message = err.to_string();
                        if state.tool_detail {
                            println!("  {} {}", "⎿".red(), format!("failed: {message}").red());
                        } else {
                            println!(
                                "{} {}{} {} {}",
                                "⏺".cyan(),
                                name.cyan(),
                                args.dimmed(),
                                "⎿".red(),
                                format!("failed: {message}").red()
                            );
                        }
                        push_tool_record(state, &name, args_summary, None, Some(message));
                    }
                }
                view.after_tool_line = true;
                // A queued message goes to the model as soon as the tool
                // calls it waited on finish — Claude Code's rule — rather
                // than waiting for the turn to end.
                flush_queue_into_turn(state, &mut view, &engine).await;
                // Keep the sub-agent cache fresh after agent-lifecycle
                // tools (mirrors the retired loop's ListSubAgents nudge).
                if matches!(
                    name.as_str(),
                    "agent_open"
                        | "agent_eval"
                        | "agent_close"
                        | "agent_cancel"
                        | "agent_wait"
                        | "agent_result"
                        | "agent_status"
                        | "agent_spawn"
                ) {
                    let _ = engine.send(Op::ListSubAgents).await;
                }
            }
            Event::AgentSpawned { id, prompt } => {
                state
                    .agent_progress
                    .insert(id.clone(), "running".to_string());
                view.clear_live();
                println!(
                    "{} spawned {id}: {}",
                    "◇".cyan(),
                    crate::tool_summary::summarize_tool_output(&prompt).dimmed()
                );
                view.after_tool_line = true;
            }
            Event::AgentProgress { id, status } => {
                state.agent_progress.insert(id, status);
            }
            Event::AgentComplete { id, result } => {
                state.agent_progress.remove(&id);
                view.clear_live();
                println!(
                    "{} agent {id} done: {}",
                    "◇".dimmed(),
                    crate::tool_summary::summarize_tool_output(&result).dimmed()
                );
                view.after_tool_line = true;
            }
            Event::AgentList { agents } => {
                state.subagent_cache = agents;
            }
            Event::SubAgentMailbox { .. } => {}
            Event::ApprovalRequired {
                id,
                tool_name,
                description,
                input,
                approval_key,
                approval_grouping_key,
                intent_summary,
            } => {
                handle_approval(
                    state,
                    &engine,
                    &mut view,
                    &mut keys,
                    id,
                    tool_name,
                    description,
                    input,
                    approval_key,
                    approval_grouping_key,
                    intent_summary,
                )
                .await;
            }
            Event::UserInputRequired { id, request } => {
                handle_user_input(state, &engine, &mut view, &mut keys, id, request).await;
            }
            Event::PathAccessRequired {
                id,
                tool_name,
                path,
            } => {
                handle_path_access(state, &engine, &mut view, &mut keys, id, tool_name, path).await;
            }
            Event::ElevationRequired {
                tool_id,
                tool_name,
                command,
                denial_reason,
                ..
            } => {
                handle_elevation(
                    state,
                    &engine,
                    &mut view,
                    &mut keys,
                    tool_id,
                    tool_name,
                    command,
                    denial_reason,
                )
                .await;
            }
            Event::CompactionStarted { message, .. } => {
                state.is_compacting = true;
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::CompactionCompleted { message, .. } => {
                state.is_compacting = false;
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::CompactionFailed { message, .. } => {
                state.is_compacting = false;
                view.clear_live();
                println!("{} compaction failed: {message}", "≡".red());
            }
            Event::PurgeStarted { message } => {
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::PurgeCompleted { message, .. } => {
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::PurgeFailed { message } => {
                view.clear_live();
                println!("{} purge failed: {message}", "≡".red());
            }
            Event::SessionUpdated {
                session_id,
                messages,
                system_prompt,
                model,
                ..
            } => {
                state.api_messages = messages;
                state.system_prompt = system_prompt;
                state.current_session_id = Some(session_id);
                if state.auto_model && model != "auto" {
                    state.last_effective_model = Some(model.clone());
                }
            }
            Event::Error { envelope, .. } => {
                view.clear_live();
                println!("{} {}", "✗".red(), error_envelope_message(&envelope).red());
            }
            Event::Status { message } => {
                view.clear_live();
                println!("{}", message.dimmed());
            }
            Event::TurnComplete {
                usage,
                status,
                error,
                tool_catalog,
                base_url,
                reported_model,
            } => {
                view.clear_live();
                // Snapshot the per-turn figures before the accounting pass
                // consumes them — the turn_end hook payload reports them
                // after state, usage, and cost have settled (contract).
                let (turn_input_tokens, turn_output_tokens) =
                    (usage.input_tokens, usage.output_tokens);
                let turn_duration_secs = state
                    .turn_started_at
                    .map(|started| started.elapsed().as_secs_f64())
                    .unwrap_or_default();
                let turn_error = error.clone();
                apply_turn_accounting(
                    state,
                    usage,
                    status,
                    error,
                    tool_catalog,
                    base_url,
                    reported_model,
                );
                if status != TurnOutcomeStatus::Completed {
                    let _ = engine.send(Op::ListSubAgents).await;
                }
                state.is_loading = false;
                state.turn_started_at = None;
                persist_session_turn_boundary(state);
                emit_turn_end_hooks(
                    state,
                    status,
                    turn_error.as_deref(),
                    turn_input_tokens,
                    turn_output_tokens,
                    turn_duration_secs,
                );
                carry_composer_out(state, &mut view);
                return Ok(PumpExit::TurnDone);
            }
            Event::PauseEvents { .. }
            | Event::ResumeEvents
            | Event::CycleAdvanced { .. }
            | Event::CoherenceState { .. }
            | Event::CapacityDecision { .. }
            | Event::CapacityIntervention { .. }
            | Event::CapacityMemoryPersistFailed { .. }
            | Event::PrefixCacheChange { .. }
            | Event::ResultVerification { .. }
            | Event::TranscriptRebuilt { .. }
            | Event::BackgroundTaskStarted { .. }
            | Event::BackgroundTaskProgress { .. }
            | Event::BackgroundTaskComplete { .. }
            | Event::BackgroundTaskFailed { .. }
            | Event::BackgroundTaskNotification { .. }
            | Event::BackgroundTaskList { .. } => {}
        }
    }
}

fn error_envelope_message(envelope: &ErrorEnvelope) -> String {
    // `ErrorEnvelope` renders through its Display in the engine; the REPL
    // only needs the human-readable line.
    envelope.to_string()
}

/// One mid-turn key press.
///
/// Typing and editing touch only the composer; the keys that act (Enter,
/// send-now, Escape, Ctrl+B, Ctrl+X Ctrl+K, Ctrl+O, Ctrl+T) print and talk
/// to the engine, so they clear the live region first — permanent output
/// must never share a row with it.
async fn handle_turn_key(
    state: &mut ReplState,
    view: &mut TurnView,
    key: TurnKey,
    engine: &crate::core::engine::EngineHandle,
) {
    match key {
        TurnKey::Char('\0') => {} // consumed sequence with no composer meaning
        TurnKey::Char(c) => {
            view.composer.insert(c);
            redraw_region(state, view);
        }
        TurnKey::Newline => {
            view.composer.insert('\n');
            redraw_region(state, view);
        }
        TurnKey::Backspace => {
            view.composer.backspace();
            redraw_region(state, view);
        }
        TurnKey::Delete => {
            view.composer.delete();
            redraw_region(state, view);
        }
        TurnKey::Left => {
            view.composer.move_left();
            redraw_region(state, view);
        }
        TurnKey::Right => {
            view.composer.move_right();
            redraw_region(state, view);
        }
        TurnKey::Home => {
            view.composer.move_home();
            redraw_region(state, view);
        }
        TurnKey::End => {
            view.composer.move_end();
            redraw_region(state, view);
        }
        TurnKey::Up => {
            // Claude Code's "take back what you queued": with an empty
            // composer, Up pulls the newest queued message back for editing.
            if view.composer.is_empty()
                && let Some(last) = state.queued_messages.pop()
            {
                view.composer.buffer = last;
                view.composer.cursor = view.composer.buffer.len();
                view.clear_live();
                println!("{}", "↑ took back the newest queued message".dimmed());
                return;
            }
            redraw_region(state, view);
        }
        TurnKey::Down => {
            // No composer history mid-turn; the key is consumed so it cannot
            // leak into the buffer.
        }
        TurnKey::KillToEnd | TurnKey::KillToStart | TurnKey::KillWord => {
            // The killed text goes nowhere: there is no kill ring mid-turn,
            // and a composer is short-lived. Claude Code's Ctrl+Y ring is a
            // prompt-time affordance here.
            match key {
                TurnKey::KillToEnd => drop(view.composer.kill_to_end()),
                TurnKey::KillToStart => drop(view.composer.kill_to_start()),
                _ => drop(view.composer.kill_word()),
            }
            redraw_region(state, view);
        }
        TurnKey::Queue => {
            let text = view.composer.take();
            if text.trim().is_empty() {
                redraw_region(state, view);
                return;
            }
            state.queued_messages.push(text.clone());
            view.clear_live();
            println!(
                "{}",
                queued_line(&text, state.queued_messages.len()).dimmed()
            );
        }
        TurnKey::SendNow => {
            let text = view.composer.take();
            if !text.trim().is_empty() {
                state.queued_messages.push(text);
            }
            flush_queue_into_turn(state, view, engine).await;
        }
        TurnKey::ToggleDetail => {
            state.tool_detail = !state.tool_detail;
            view.clear_live();
            if state.tool_detail {
                println!("{}", "— tool detail expanded (Ctrl+O to collapse)".dimmed());
                replay_turn_tool_detail(state);
            } else {
                println!("{}", "— tool detail collapsed (Ctrl+O to expand)".dimmed());
            }
        }
        TurnKey::TaskList => {
            state.show_tasks = !state.show_tasks;
            view.clear_live();
            if state.show_tasks {
                super::print_task_list(state).await;
            } else {
                println!("{}", "— task list hidden (Ctrl+T)".dimmed());
            }
        }
        TurnKey::CycleMode => {
            state.cycle_mode();
            view.clear_live();
            println!(
                "{}",
                format!(
                    "switched to {} mode — applies from the next turn",
                    state.mode.label()
                )
                .dimmed()
            );
        }
        TurnKey::Background => {
            view.clear_live();
            let _ = engine.send(Op::BackgroundCurrentShell).await;
            println!("{}", "— backgrounding the running shell (Ctrl+B)".dimmed());
        }
        TurnKey::StopAll => {
            let now = Instant::now();
            let confirmed = view
                .stop_all_at
                .is_some_and(|at| now.duration_since(at) <= STOP_ALL_WINDOW);
            view.stop_all_at = Some(now);
            view.clear_live();
            if confirmed {
                let _ = engine.send(Op::CancelAllBackgroundTasks).await;
                println!("{}", "— stopping all background work".yellow());
            } else {
                println!(
                    "{}",
                    "— press Ctrl+X Ctrl+K again within 3 seconds to stop all background work"
                        .dimmed()
                );
            }
        }
        TurnKey::Esc => {
            view.clear_live();
            // Claude Code's rule: work that can move to the background does,
            // and the model keeps reading; only otherwise does Escape
            // interrupt the turn. Anything already queued goes out either
            // way (a steer when the turn lives on, the next turn when it
            // does not).
            let backgroundable = view
                .current_tool
                .as_deref()
                .is_some_and(is_backgroundable_tool);
            if backgroundable {
                let _ = engine.send(Op::BackgroundCurrentShell).await;
                println!("{}", "⎋ — sent the running tool to the background".dimmed());
                flush_queue_into_turn(state, view, engine).await;
            } else {
                engine.cancel();
                let queued = state.queued_messages.len();
                if queued > 0 {
                    println!(
                        "{}",
                        format!("⎋ interrupted — {queued} queued message(s) go out next").yellow()
                    );
                } else {
                    println!("{}", "⎋ interrupted".yellow());
                }
            }
        }
    }
}

/// Tools whose work can be detached from the turn: a shell command keeps
/// running in the background, and a sub-agent has its own task.
fn is_backgroundable_tool(name: &str) -> bool {
    name.starts_with("exec_shell") || name.starts_with("agent_")
}

/// Redraw the live region after a composer edit, using the status line the
/// ticker would draw (so the composer row keeps its place without waiting
/// for the next tick).
fn redraw_region(state: &ReplState, view: &mut TurnView) {
    let frame = super::status::live_line(state, view.current_tool.as_deref());
    view.draw_live(&frame);
}

/// The dim line a queued message prints as.
fn queued_line(text: &str, pending: usize) -> String {
    let first = text.lines().next().unwrap_or(text);
    let more = text.lines().count().saturating_sub(1);
    let ellipsis = if more > 0 {
        format!(" (+{more} more line(s))")
    } else {
        String::new()
    };
    format!("⏸ queued ({pending} pending): {first}{ellipsis}")
}

/// Hand every queued message to the running turn, in the order typed.
///
/// `EngineHandle::steer` is the engine's mid-turn input path: the executor
/// drains it at the next step boundary and records it as a user message, so
/// the session log keeps agreeing with what the model saw.
async fn flush_queue_into_turn(
    state: &mut ReplState,
    view: &mut TurnView,
    engine: &crate::core::engine::EngineHandle,
) {
    if state.queued_messages.is_empty() {
        return;
    }
    let sent: Vec<String> = state.queued_messages.drain(..).collect();
    view.clear_live();
    for text in &sent {
        if let Err(err) = engine.steer(text.clone()).await {
            println!(
                "{}",
                format!("✗ could not deliver a queued message: {err}").red()
            );
            // Nothing is lost: the message goes back on the queue and the
            // REPL sends it as the next turn.
            state.queued_messages.push(text.clone());
        }
    }
    if sent.len() > state.queued_messages.len() {
        println!(
            "{}",
            format!(
                "→ sent {} queued message(s) to the running turn",
                sent.len()
            )
            .dimmed()
        );
    }
}

/// Carry whatever the user was typing mid-turn out to the prompt.
///
/// The composer dies with the turn, and losing a half-typed message to a turn
/// that happened to end first would feel like the input vanished; the REPL
/// loop picks this up as the next prompt's text.
fn carry_composer_out(state: &mut ReplState, view: &mut TurnView) {
    if view.composer.is_empty() {
        return;
    }
    let text = view.composer.take();
    view.clear_live();
    state.turn_end_draft = Some(text);
}

/// Record a completed call in the session tool log (`/expand` replay),
/// dropping the oldest entries past the cap.
fn push_tool_record(
    state: &mut ReplState,
    name: &str,
    args_summary: Option<String>,
    output: Option<String>,
    error: Option<String>,
) {
    state.tool_log.push(crate::tool_summary::ToolCallRecord {
        seq: state.tool_log.len() + 1,
        turn: state.tool_log_turn,
        name: name.to_string(),
        args_summary,
        output,
        error,
        replayed: false,
    });
    let overflow = state
        .tool_log
        .len()
        .saturating_sub(crate::tool_summary::TOOL_LOG_CAP);
    if overflow > 0 {
        state.tool_log.drain(..overflow);
    }
}

/// Reprint detail for the current turn's collapsed tool calls (the Ctrl+O
/// expand path). Already-replayed calls are skipped so toggling more than
/// once never duplicates output.
pub(super) fn replay_turn_tool_detail(state: &mut ReplState) {
    let turn = state.tool_log_turn;
    let mut blocks = Vec::new();
    for record in state.tool_log.iter_mut() {
        if record.turn == turn && !record.replayed {
            blocks.push(crate::tool_summary::tool_detail_block(
                record,
                crate::tool_summary::DETAIL_MAX_LINES,
            ));
            record.replayed = true;
        }
    }
    for block in blocks {
        println!("{}", block.dimmed());
    }
}

/// Auto-save the session at each turn boundary (the retired TUI's
/// persistence-actor contract, reduced to the turn boundary). Failures are
/// silent-by-print: a save miss never interrupts the conversation.
fn persist_session_turn_boundary(state: &mut ReplState) {
    let Some(session) = saved_session_from_state(state) else {
        return;
    };
    let Ok(manager) = crate::session_manager::SessionManager::default_location() else {
        return;
    };
    if let Err(err) = manager.save_session(&session) {
        println!(
            "{}",
            format!("warning: could not auto-save session: {err}").yellow()
        );
        return;
    }
    // Adopt the id so subsequent turns update the same session file
    // instead of minting a new one per turn.
    state.current_session_id = Some(session.metadata.id.clone());
    // The turn is over and the regular session file is now the durable
    // record, so the in-flight crash-recovery checkpoint is redundant.
    let _ = manager.clear_checkpoint();
}

/// Snapshot the session as it stands at turn start, so a crash mid-turn is
/// recoverable on the next launch (`load_checkpoint` path). Best-effort and
/// silent: a checkpoint miss must never interrupt a running turn.
fn save_crash_recovery_checkpoint(state: &ReplState) {
    let Some(session) = saved_session_from_state(state) else {
        return;
    };
    let Ok(manager) = crate::session_manager::SessionManager::default_location() else {
        return;
    };
    let _ = manager.save_checkpoint(&session);
}

/// Build the persistable session record from REPL state, or `None` when
/// there is nothing worth persisting yet.
fn saved_session_from_state(state: &ReplState) -> Option<crate::session_manager::SavedSession> {
    use crate::session_manager::create_saved_session_with_id_and_mode;

    if state.api_messages.is_empty() {
        return None;
    }
    let session_id = state
        .current_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut session = create_saved_session_with_id_and_mode(
        session_id,
        &state.api_messages,
        &state.model,
        &state.workspace,
        u64::from(state.session.total_tokens),
        state.system_prompt.as_ref(),
        Some(state.mode.label()),
    );
    session.metadata.cost.session_cost_usd = state.session.session_cost;
    session.metadata.cost.session_cost_cny = state.session.session_cost_cny;
    session.metadata.cost.subagent_cost_usd = state.session.subagent_cost;
    session.metadata.cost.subagent_cost_cny = state.session.subagent_cost_cny;
    session.metadata.cumulative_turn_secs = state.cumulative_turn_duration.as_secs();
    // Keep the persisted record in agreement with state: the `MessageStart`
    // model name alongside the requested id.
    session.metadata.reported_model = state.reported_model.clone();
    Some(session)
}

/// Fire the observer `turn_end` hook (every outcome) and `on_error` (failed
/// turns) after turn accounting and persistence have settled. Detached —
/// the REPL does not wait on hook processes between turns.
fn emit_turn_end_hooks(
    state: &ReplState,
    status: TurnOutcomeStatus,
    error: Option<&str>,
    input_tokens: u32,
    output_tokens: u32,
    duration_secs: f64,
) {
    if state.hooks.has_hooks_for_event(HookEvent::TurnEnd) {
        let context = state
            .hook_context()
            .with_turn_status(turn_status_str(status))
            .with_input_tokens(input_tokens)
            .with_output_tokens(output_tokens)
            .with_duration_secs(duration_secs);
        crate::hooks::emit_detached(
            std::sync::Arc::new(state.hooks.clone()),
            HookEvent::TurnEnd,
            context,
        );
    }
    if let Some(error) = error
        && status != TurnOutcomeStatus::Completed
        && state.hooks.has_hooks_for_event(HookEvent::OnError)
    {
        let context = state.hook_context().with_error(error);
        crate::hooks::emit_detached(
            std::sync::Arc::new(state.hooks.clone()),
            HookEvent::OnError,
            context,
        );
    }
}

fn turn_status_str(status: TurnOutcomeStatus) -> &'static str {
    match status {
        TurnOutcomeStatus::Completed => "completed",
        TurnOutcomeStatus::Interrupted => "interrupted",
        TurnOutcomeStatus::Failed => "failed",
    }
}

/// Mirror of the retired TurnComplete accounting (ui.rs): token totals,
/// cache telemetry, cost accrual.
fn apply_turn_accounting(
    state: &mut ReplState,
    usage: crate::models::Usage,
    status: TurnOutcomeStatus,
    error: Option<String>,
    tool_catalog: Option<Vec<crate::models::Tool>>,
    base_url: Option<String>,
    reported_model: Option<String>,
) {
    state.session.last_tool_catalog = tool_catalog;
    state.session.last_base_url = base_url;
    // The `MessageStart` model name feeds the status-bar label (and the saved
    // session metadata below); a configured `display_model` overrides it.
    if let Some(reported) = reported_model.as_deref()
        && !reported.is_empty()
    {
        state.reported_model = Some(reported.to_string());
    }

    let turn_elapsed = state
        .turn_started_at
        .map(|started| started.elapsed())
        .unwrap_or_default();
    state.cumulative_turn_duration = state.cumulative_turn_duration.saturating_add(turn_elapsed);

    let turn_tokens = usage.input_tokens + usage.output_tokens;
    state.session.total_tokens = state.session.total_tokens.saturating_add(turn_tokens);
    state.session.total_conversation_tokens = state
        .session
        .total_conversation_tokens
        .saturating_add(turn_tokens);
    state.session.total_input_tokens = state
        .session
        .total_input_tokens
        .saturating_add(usage.input_tokens);
    state.session.total_output_tokens = state
        .session
        .total_output_tokens
        .saturating_add(usage.output_tokens);
    if let Some(hit_tokens) = usage.prompt_cache_hit_tokens {
        state.session.total_cache_hit_tokens = state
            .session
            .total_cache_hit_tokens
            .saturating_add(hit_tokens);
        let cache_miss = usage
            .prompt_cache_miss_tokens
            .unwrap_or_else(|| usage.input_tokens.saturating_sub(hit_tokens));
        state.session.total_cache_miss_tokens = state
            .session
            .total_cache_miss_tokens
            .saturating_add(cache_miss);
    }
    state.session.last_prompt_tokens = Some(usage.input_tokens);
    state.session.last_completion_tokens = Some(usage.output_tokens);
    state.session.last_prompt_cache_hit_tokens = usage.prompt_cache_hit_tokens;
    state.session.last_prompt_cache_miss_tokens = usage.prompt_cache_miss_tokens;
    state.session.last_reasoning_replay_tokens = usage.reasoning_replay_tokens;
    state
        .session
        .turn_cache_history
        .push_back(crate::app_state::TurnCacheRecord {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_hit_tokens: usage.prompt_cache_hit_tokens,
            cache_miss_tokens: usage.prompt_cache_miss_tokens,
        });

    let pricing_model = if state.auto_model {
        state
            .last_effective_model
            .clone()
            .unwrap_or_else(|| state.model.clone())
    } else {
        state.model.clone()
    };
    if let Some(cost) =
        crate::pricing::calculate_turn_cost_estimate_from_usage(&pricing_model, &usage)
    {
        state.session.session_cost += cost.usd;
        state.session.session_cost_cny += cost.cny;
    }

    if let Some(error) = error
        && status != TurnOutcomeStatus::Completed
    {
        println!("{} turn failed: {error}", "✗".red());
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_approval(
    state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    id: String,
    tool_name: String,
    description: String,
    input: serde_json::Value,
    approval_key: String,
    approval_grouping_key: String,
    intent_summary: Option<String>,
) {
    view.clear_live();
    let workspace_key = state.workspace.to_string_lossy().into_owned();

    if state.approval_mode == ApprovalMode::Auto {
        let _ = engine.approve_tool_call(id).await;
        return;
    }
    if state.approval_session_denied.contains(&approval_key) {
        let _ = engine.deny_tool_call(id).await;
        return;
    }
    if state
        .approval_session_approved
        .contains(&approval_grouping_key)
        || state
            .approval_grants
            .is_granted(&workspace_key, &approval_grouping_key)
    {
        let _ = engine.approve_tool_call(id).await;
        return;
    }

    let args_summary = crate::tool_summary::summarize_tool_args(&input);
    state.awaiting_approval = true;
    // The prompt reads stdin line-by-line; the key tap must let go first.
    keys.suspend();
    print!(
        "{}",
        approval_frame(
            &tool_name,
            &description,
            args_summary.as_deref(),
            intent_summary.as_deref(),
        )
    );
    let _ = std::io::stdout().flush();
    let answer = read_stdin_line("").await;
    keys.resume();
    state.awaiting_approval = false;
    let answer = answer.to_ascii_lowercase();
    match answer.as_str() {
        "y" | "yes" => {
            let _ = engine.approve_tool_call(id).await;
        }
        "a" | "always" => {
            state
                .approval_session_approved
                .insert(approval_grouping_key);
            let _ = engine.approve_tool_call(id).await;
        }
        "e" | "never" => {
            state.approval_session_denied.insert(approval_key);
            let _ = engine.deny_tool_call(id).await;
        }
        _ => {
            let _ = engine.deny_tool_call(id).await;
        }
    }
}

/// What a path-access answer does. Split from the prompt because mapping a
/// typed line to (allow, persist) is the security-relevant half, and this way
/// it is testable without stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathAccessAnswer {
    /// `[y]` — allow it for this turn; the engine grants the path in-process.
    AllowNow,
    /// `[a]` — allow it and remember it for this workspace (the host writes
    /// `~/.codesmith/workspace-trust.json`, the store `/trust add` owns).
    AllowAndRemember,
    /// Everything else, including `[n]` and an empty line — the call stays
    /// refused.
    Decline,
}

fn parse_path_access_answer(raw: &str) -> PathAccessAnswer {
    match raw.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => PathAccessAnswer::AllowNow,
        "a" | "always" => PathAccessAnswer::AllowAndRemember,
        _ => PathAccessAnswer::Decline,
    }
}

/// The path-access question: a call names a directory outside the workspace.
/// Ends at the unterminated prompt, like [`approval_frame`].
fn path_access_frame(tool_name: &str, path: &std::path::Path) -> String {
    format!(
        "{} path outside the workspace: {} ({})\nallow? [y] allow it now · [a] always allow it \
         in this workspace (saved) · [n] skip: ",
        "⚠".yellow(),
        path.display().to_string().bold(),
        tool_name.bold()
    )
}

/// `Event::PathAccessRequired` — the user decides whether a call may touch a
/// path outside the workspace, instead of the engine refusing it outright.
/// Allowing either way makes the engine grant the path for this turn, so the
/// call that hit the boundary proceeds; `[a]` additionally persists it
/// (`/trust add`'s store), which later turns and sub-agents pick up. Declining
/// keeps the plain refusal.
async fn handle_path_access(
    state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    id: String,
    tool_name: String,
    path: std::path::PathBuf,
) {
    view.clear_live();
    if state.approval_mode == ApprovalMode::Auto {
        // Same posture as `handle_approval`: an auto session does not stall on
        // prompts. (YOLO never gets here — `trust_mode` short-circuits
        // `resolve_path` before any question exists.)
        let _ = engine.approve_tool_call(id).await;
        return;
    }

    state.awaiting_approval = true;
    // The prompt reads stdin line-by-line; the key tap must let go first.
    keys.suspend();
    print!("{}", path_access_frame(&tool_name, &path));
    let _ = std::io::stdout().flush();
    let answer = read_stdin_line("").await;
    keys.resume();
    state.awaiting_approval = false;

    match parse_path_access_answer(&answer) {
        PathAccessAnswer::AllowNow => {
            let _ = engine.approve_tool_call(id).await;
        }
        PathAccessAnswer::AllowAndRemember => {
            // Persist first: the engine's in-process grant covers this turn,
            // and a failure to write must not silently look like "remembered".
            match crate::workspace_trust::add(&state.workspace, &path) {
                Ok(stored) => println!(
                    "{}",
                    format!(
                        "trusted for this workspace: {} — `/trust remove {}` to undo",
                        stored.display(),
                        stored.display()
                    )
                    .dimmed()
                ),
                Err(err) => println!(
                    "{} could not save the grant ({err}) — allowing it for this run only",
                    "✗".red()
                ),
            }
            let _ = engine.approve_tool_call(id).await;
        }
        PathAccessAnswer::Decline => {
            let _ = engine.deny_tool_call(id).await;
        }
    }
}

async fn handle_user_input(
    _state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    id: String,
    request: codesmith_agent_runtime::user_input::UserInputRequest,
) {
    use codesmith_agent_runtime::user_input::{UserInputAnswer, UserInputResponse};

    view.clear_live();
    let mut answers = Vec::new();
    for question in &request.questions {
        println!("{} {}", "?".cyan(), question.question);
        for (idx, option) in question.options.iter().enumerate() {
            println!(
                "  {}) {} {}",
                idx + 1,
                option.label,
                format!("— {}", option.description).dimmed()
            );
        }
        keys.suspend();
        let answer = read_stdin_line("choice: ").await;
        keys.resume();
        let picked = answer
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=question.options.len()).contains(n))
            .and_then(|n| question.options.get(n - 1));
        match picked {
            Some(option) => answers.push(UserInputAnswer {
                id: question.id.clone(),
                label: option.label.clone(),
                // `UserInputOption` carries no value field; the label is the
                // value the model round-trips.
                value: option.label.clone(),
            }),
            None => {
                let _ = engine.cancel_user_input(id).await;
                return;
            }
        }
    }
    let _ = engine
        .submit_user_input(id, UserInputResponse { answers })
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_elevation(
    state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    tool_id: String,
    tool_name: String,
    command: Option<String>,
    denial_reason: String,
) {
    use crate::core::engine::tool_setup::sandbox_policy_for_mode;

    view.clear_live();
    if state.approval_mode == ApprovalMode::Auto {
        // Mirror the retired auto path: strip catastrophic commands;
        // elevate the rest only with the trust/YOLO opt-in.
        let elevation_target = sandbox_policy_for_mode(state.mode, &state.workspace);
        let allow_elevation = state.trust_mode || state.mode == crate::app_state::AppMode::Yolo;
        match crate::auto_mode::decide_auto_elevation(
            command.as_deref(),
            allow_elevation,
            elevation_target,
        ) {
            crate::auto_mode::AutoElevationDecision::Deny { reason } => {
                println!("{} sandbox denied {tool_name}: {reason}", "⚠".yellow());
                let _ = engine.deny_tool_call(tool_id).await;
            }
            crate::auto_mode::AutoElevationDecision::ElevateTo(policy) => {
                println!(
                    "{}",
                    format!("sandbox denied {tool_name}: {denial_reason} — auto-elevating to mode sandbox")
                        .dimmed()
                );
                let _ = engine.retry_tool_with_policy(tool_id, policy).await;
            }
        }
        return;
    }

    println!(
        "{} sandbox denied {} ({denial_reason})",
        "⚠".yellow(),
        command.as_deref().unwrap_or(&tool_name)
    );
    keys.suspend();
    let answer = read_stdin_line("retry with elevated sandbox (mode policy)? [y/N]: ").await;
    keys.resume();
    if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        let policy = sandbox_policy_for_mode(state.mode, &state.workspace);
        let _ = engine.retry_tool_with_policy(tool_id, policy).await;
    } else {
        let _ = engine.deny_tool_call(tool_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_edits_on_char_boundaries() {
        let mut composer = Composer::default();
        for c in "héllo".chars() {
            composer.insert(c);
        }
        assert_eq!(composer.buffer, "héllo");
        assert_eq!(composer.cursor, "héllo".len());
        composer.backspace();
        assert_eq!(composer.buffer, "héll");
        composer.move_home();
        assert_eq!(composer.cursor, 0);
        composer.move_right(); // past 'h'
        assert_eq!(composer.cursor, 1);
        // The 'é' is two bytes; deleting it must remove the whole character
        // and leave the rest of the buffer intact.
        composer.delete();
        assert_eq!(composer.buffer, "hll");
        composer.move_end();
        composer.insert('!');
        assert_eq!(composer.buffer, "hll!");
    }

    #[test]
    fn composer_kills_like_readline() {
        let mut composer = Composer {
            buffer: "src/utils/foo.ts and more".to_string(),
            cursor: "src/utils/foo.ts".len(),
        };
        // Ctrl+W ignores punctuation: one press removes the whole path.
        assert_eq!(composer.kill_word(), "src/utils/foo.ts");
        assert_eq!(composer.buffer, " and more");
        assert_eq!(composer.cursor, 0);

        composer.cursor = composer.buffer.len();
        assert_eq!(composer.kill_to_start(), " and more");
        assert!(composer.buffer.is_empty());

        composer.buffer = "keep this".to_string();
        composer.cursor = "keep".len();
        assert_eq!(composer.kill_to_end(), " this");
        assert_eq!(composer.buffer, "keep");
    }

    #[test]
    fn composer_take_empties_it() {
        let mut composer = Composer::default();
        composer.insert('a');
        composer.insert('b');
        assert_eq!(composer.take(), "ab");
        assert!(composer.is_empty());
        assert_eq!(composer.cursor, 0);
        assert!(composer.take().is_empty());
    }

    #[test]
    fn queued_line_names_the_first_line_and_the_rest() {
        assert_eq!(queued_line("just one", 1), "⏸ queued (1 pending): just one");
        assert_eq!(
            queued_line("first\nsecond\nthird", 2),
            "⏸ queued (2 pending): first (+2 more line(s))"
        );
    }

    /// A stand-in for the paragraph-length static descriptions that tools like
    /// `exec_shell` carry (the real one is ~1 KB).
    const LONG_DESCRIPTION: &str = "Write content to a UTF-8 file in the workspace. Use this \
         instead of heredocs (`cat <<EOF > file`) or `echo > file` in `exec_shell` — diffs \
         render inline and approval is handled cleanly. Creates or overwrites; parent \
         directories are auto-created.";

    #[test]
    fn approval_frame_header_names_the_tool_and_the_action() {
        let frame = approval_frame(
            "write_file",
            LONG_DESCRIPTION,
            Some("path: notes.txt"),
            Some("recording the state"),
        );
        let header = frame.lines().next().expect("header line");
        assert!(header.contains("approval needed"), "{header}");
        assert!(header.contains("write_file"), "{header}");
        assert!(header.contains("path: notes.txt"), "{header}");
        assert!(
            !header.contains("heredocs"),
            "the static description must not stand in for the action: {header}"
        );
        assert!(frame.contains("intent: recording the state"), "{frame}");
    }

    #[test]
    fn approval_frame_falls_back_to_one_sentence_of_the_description() {
        let frame = approval_frame("diagnostics", LONG_DESCRIPTION, None, None);
        let header = frame.lines().next().expect("header line");
        assert!(
            header.contains("Write content to a UTF-8 file in the workspace"),
            "{header}"
        );
        assert!(
            !header.contains("heredocs"),
            "only the first sentence may reach the header: {header}"
        );
        assert!(header.width() <= 140, "header must stay one row: {header}");
    }

    #[test]
    fn approval_label_caps_cjk_by_columns() {
        // A wide-character description must not spend two columns per
        // character against the one-row cap.
        let label = approval_action(&"输".repeat(120), None);
        assert!(
            label.width() <= APPROVAL_LABEL_MAX,
            "action label ran to {} columns",
            label.width()
        );
    }

    #[test]
    fn approval_frame_prompt_is_unterminated_and_scopes_every_answer() {
        let frame = approval_frame("exec_shell", "Run a shell command.", None, None);
        assert!(
            !frame.ends_with('\n'),
            "the answer echoes on the prompt row, so the frame must not end it"
        );
        assert!(frame.ends_with(": "), "{frame}");
        for scoped in [
            "[y] run it once",
            "[n] skip",
            "[a] always allow this command family for this session",
            "[e] never this exact call",
        ] {
            assert!(frame.contains(scoped), "missing {scoped:?} in {frame:?}");
        }
    }

    #[test]
    fn approval_frame_always_scope_follows_the_grouping_key() {
        // Most tools group by input hash: "always" covers this exact call and
        // nothing else, and the legend must not promise more than that.
        let frame = approval_frame("write_file", "Write a file.", Some("path: a.txt"), None);
        assert!(
            frame.contains("[a] always allow this exact call for this session"),
            "{frame}"
        );
        let fetch = approval_frame("fetch_url", "Fetch a URL.", Some("url: https://x/y"), None);
        assert!(
            fetch.contains("[a] always allow requests to the same host for this session"),
            "{fetch}"
        );
    }

    #[test]
    fn live_frame_closes_an_open_permanent_row_first() {
        let mut view = TurnView::new();
        view.ends_with_newline = false;
        view.draw_live("◉ working");
        assert!(
            view.ends_with_newline,
            "an open permanent row (unterminated prompt) must be closed \
             before the frame takes the row"
        );
        assert!(
            view.live_active,
            "the frame owns the row until it is cleared"
        );
    }

    #[test]
    fn ticker_frame_yields_to_an_open_permanent_row() {
        // A streamed delta mid-paragraph leaves the row open; a tick that
        // drew over it would close the row mid-word — the 250 ms line breaks
        // this repository shipped before the guard.
        let mut view = TurnView::new();
        view.ends_with_newline = false;
        view.tick_live("◉ working");
        assert!(
            !view.live_active,
            "a tick must not draw a frame over an open text row"
        );
        assert!(
            !view.ends_with_newline,
            "a tick must not close an open text row mid-word"
        );
        view.ends_with_newline = true;
        view.tick_live("◉ working");
        assert!(
            view.live_active,
            "once the row is closed the frame draws as usual"
        );
    }

    #[test]
    fn path_access_frame_names_the_path_and_every_answer() {
        let frame = path_access_frame("list_dir", std::path::Path::new("/Users/dev/other-repo"));
        assert!(frame.contains("/Users/dev/other-repo"), "{frame}");
        assert!(frame.contains("list_dir"), "{frame}");
        assert!(frame.contains("[y] allow it now"), "{frame}");
        assert!(
            frame.contains("[a] always allow it in this workspace"),
            "{frame}"
        );
        assert!(frame.contains("[n] skip"), "{frame}");
    }

    /// Only an explicit "yes" allows, and only an explicit "always" persists:
    /// a typo, an empty line, or EOF must leave the path untrusted.
    #[test]
    fn path_access_answers_fail_closed() {
        assert_eq!(parse_path_access_answer("y"), PathAccessAnswer::AllowNow);
        assert_eq!(
            parse_path_access_answer(" YES "),
            PathAccessAnswer::AllowNow
        );
        assert_eq!(
            parse_path_access_answer("a"),
            PathAccessAnswer::AllowAndRemember
        );
        assert_eq!(
            parse_path_access_answer("Always"),
            PathAccessAnswer::AllowAndRemember
        );
        for refuse in ["n", "no", "", "  ", "nope", "1", "yolo"] {
            assert_eq!(
                parse_path_access_answer(refuse),
                PathAccessAnswer::Decline,
                "{refuse:?} must not allow"
            );
        }
    }
}
