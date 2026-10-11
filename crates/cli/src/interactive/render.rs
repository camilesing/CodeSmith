//! Turn event pump for the REPL: drains `EngineHandle::rx_event` for one
//! turn and renders it as plain terminal lines (port of the exec pump in
//! `main::run_exec_agent` plus the accounting half of the retired TUI
//! event loop).
//!
//! Rendering protocol: permanent output (assistant text, tool lines) goes
//! to stdout as whole lines. Tool calls print one collapsed line on
//! completion by default (Ctrl+O in-turn, `/expand` after the fact — the
//! terminal is append-only, so expansion is a replay, never a rewrite).
//! While the turn runs, a single transient "live" line is refreshed in
//! place via `ESC[2K CR` — never interleaved with permanent text (it is
//! cleared before any permanent write).

use std::io::Write;
use std::time::{Duration, Instant};

use colored::Colorize;

use super::state::ReplState;
use crate::core::events::{Event, TurnOutcomeStatus};
use crate::core::ops::Op;
use codesmith_agent_runtime::error_taxonomy::ErrorEnvelope;
use codesmith_agent_runtime::mode::ApprovalMode;

/// Interval between live-line refreshes (spinner/elapsed/ctx).
const LIVE_REFRESH: Duration = Duration::from_millis(250);

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

/// One-turn rendering state.
struct TurnView {
    live_active: bool,
    current_tool: Option<String>,
    ends_with_newline: bool,
    /// Started-but-not-completed tool call (name, arg summary) — the
    /// collapsed form prints only on completion; the live line carries
    /// the "running" feedback meanwhile.
    pending_tool: Option<(String, Option<String>)>,
    /// The last permanent line was a tool/agent line; the next assistant
    /// message gets a blank separator so the answer stands out.
    after_tool_line: bool,
}

impl TurnView {
    fn new() -> Self {
        Self {
            live_active: false,
            current_tool: None,
            ends_with_newline: true,
            pending_tool: None,
            after_tool_line: false,
        }
    }

    fn clear_live(&mut self) {
        if self.live_active {
            eprint!("\x1b[2K\r");
            self.live_active = false;
        }
    }
}

/// Read one line from stdin without holding the async worker hostage.
async fn read_stdin_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let prompt_owned = prompt.to_string();
    tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = prompt_owned;
        line.trim().to_string()
    })
    .await
    .unwrap_or_default()
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
                match key {
                    Some(super::keys::TurnKey::ToggleDetail) => {
                        state.tool_detail = !state.tool_detail;
                        view.clear_live();
                        if state.tool_detail {
                            println!(
                                "{}",
                                "— tool detail expanded (Ctrl+O to collapse)".dimmed()
                            );
                            replay_turn_tool_detail(state);
                        } else {
                            println!(
                                "{}",
                                "— tool detail collapsed (Ctrl+O to expand)".dimmed()
                            );
                        }
                    }
                    // None: the tap is gone (non-TTY stdin) — Ctrl+O
                    // unavailable; `/expand` still works.
                    None => {}
                }
                continue;
            }
            _ = live_ticker.tick() => {
                view.clear_live();
                eprint!("{}", super::status::live_line(state, view.current_tool.as_deref()));
                let _ = std::io::stderr().flush();
                view.live_active = true;
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
            Event::ToolCallStarted { name, input, .. } => {
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
                view.pending_tool = Some((name, args_summary));
            }
            Event::ToolCallProgress { .. } => {}
            Event::ToolCallComplete { name, result, .. } => {
                view.clear_live();
                if view.current_tool.as_deref() == Some(name.as_str()) {
                    view.current_tool = None;
                }
                let args_summary = match view.pending_tool.take() {
                    Some((pending, args)) if pending == name => args,
                    _ => None,
                };
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
            } => {
                view.clear_live();
                apply_turn_accounting(state, usage, status, error, tool_catalog, base_url);
                if status != TurnOutcomeStatus::Completed {
                    let _ = engine.send(Op::ListSubAgents).await;
                }
                state.is_loading = false;
                state.turn_started_at = None;
                persist_session_turn_boundary(state);
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
fn replay_turn_tool_detail(state: &mut ReplState) {
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
    use crate::session_manager::create_saved_session_with_id_and_mode;

    if state.api_messages.is_empty() {
        return;
    }
    let Ok(manager) = crate::session_manager::SessionManager::default_location() else {
        return;
    };
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
) {
    state.session.last_tool_catalog = tool_catalog;
    state.session.last_base_url = base_url;

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

    println!(
        "{} approval required: {} — {}",
        "⚠".yellow(),
        tool_name.bold(),
        description
    );
    if let Some(intent) = intent_summary.as_deref()
        && !intent.trim().is_empty()
    {
        println!("{}", format!("  intent: {intent}").dimmed());
    }
    if let Some(args) = crate::tool_summary::summarize_tool_args(&input) {
        println!("{}", format!("  args: {args}").dimmed());
    }
    state.awaiting_approval = true;
    // The prompt reads stdin line-by-line; the key tap must let go first.
    keys.suspend();
    let answer =
        read_stdin_line("allow? [y]es / [n]o / [a]lways this session / n[e]ver this session: ")
            .await;
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
