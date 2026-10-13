//! Debug/inspection commands for the REPL — port of the readout half of
//! `commands/debug.rs` against [`ReplState`]. Commands that opened TUI
//! views (`/context` inspector, `/cache inspect`) degrade to line output.

use colored::Colorize;

use super::ReplState;
use crate::core::ops::Op;
use crate::dependencies::{ExternalTool, Git};
use crate::models::SystemPrompt;

/// `/tokens` — last-turn + session token report.
pub fn tokens(state: &ReplState) {
    println!("{}", "token report".bold());
    println!(
        "  last turn in/out:   {}/{}",
        state
            .session
            .last_prompt_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into()),
        state
            .session
            .last_completion_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into()),
    );
    println!("  session total:      {}", state.session.total_tokens);
    println!(
        "  cache hit/miss:     {}/{}",
        state.session.total_cache_hit_tokens, state.session.total_cache_miss_tokens
    );
    println!(
        "  api messages:       {} (model {})",
        state.api_messages.len(),
        state.model_display_label()
    );
    if let Some((used, max, percent)) = super::super::status::context_usage(
        &state.api_messages,
        state.system_prompt.as_ref(),
        state.session.last_prompt_tokens,
        state.context_window(),
    ) {
        println!(
            "  context:            {used}/{max} ({percent:.0}%) — compact threshold {}",
            state.compact_threshold
        );
    }
}

/// `/usage` (aliases `/cost`, `/stats`) — what the session spent and
/// consumed. Claude Code's `/usage` is the cost view and `/tokens` is the
/// token view, so this prints both rather than inventing a third report.
pub fn usage(state: &ReplState) {
    cost(state);
    tokens(state);
}

/// `/cost` — session cost breakdown (USD + CNY, parent + sub-agents).
pub fn cost(state: &ReplState) {
    println!("{}", "cost report".bold());
    println!("  session (usd):      {:.4}", state.session.session_cost);
    println!(
        "  session (cny):      {:.4}",
        state.session.session_cost_cny
    );
    println!("  sub-agents (usd):   {:.4}", state.session.subagent_cost);
    println!(
        "  sub-agents (cny):   {:.4}",
        state.session.subagent_cost_cny
    );
    println!(
        "  total (usd):        {:.4}",
        state.session.session_cost + state.session.subagent_cost
    );
}

/// `/tools` — the current model-visible tool catalog baseline.
pub fn tools(state: &ReplState) {
    match state
        .tool_catalog
        .as_ref()
        .and_then(|catalog| catalog.current())
    {
        Some(snapshot) => println!("{}", snapshot.render()),
        None => println!(
            "{}",
            "no tool catalog yet — run a turn first (the baseline is captured at dispatch)"
                .dimmed()
        ),
    }
}

/// `/system` — current system prompt.
pub fn system_prompt(state: &ReplState) {
    let prompt_text = match &state.system_prompt {
        Some(SystemPrompt::Text(text)) => text.clone(),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
        None => {
            println!(
                "{}",
                "(no system prompt yet — appears after the first turn)".dimmed()
            );
            return;
        }
    };
    // Truncate very long prompts so the terminal history stays usable.
    const MAX_LINES: usize = 120;
    let lines: Vec<&str> = prompt_text.lines().collect();
    if lines.len() > MAX_LINES {
        println!("{}", lines[..MAX_LINES].join("\n"));
        println!(
            "{}",
            format!(
                "… ({} more lines; save via /export for the full prompt)",
                lines.len() - MAX_LINES
            )
            .dimmed()
        );
    } else {
        println!("{prompt_text}");
    }
}

/// `/context` / `/ctx` — line-mode context snapshot (the TUI opened an
/// inspector view; the data is the same).
pub fn context(state: &ReplState, arg: Option<&str>) {
    if arg.is_some_and(|value| value.eq_ignore_ascii_case("all")) {
        context_breakdown(state);
    }
    match super::super::status::context_usage(
        &state.api_messages,
        state.system_prompt.as_ref(),
        state.session.last_prompt_tokens,
        state.context_window(),
    ) {
        Some((used, max, percent)) => println!(
            "context: {used}/{max} tokens ({percent:.0}% of window) — {} messages, compaction threshold {}",
            state.api_messages.len(),
            state.compact_threshold
        ),
        None => println!(
            "{}",
            "context: no window known for the current model — set [providers.<id>] context_window, or run a turn first"
                .dimmed()
        ),
    }
}

/// `/context all` — the per-message breakdown behind the usage figure.
///
/// Token counts here are estimated from character counts (four characters
/// per token), not reported by the provider: the wire reports one prompt
/// total, so a per-message split can only ever be an estimate. That is why
/// the detail is behind `all` and why the numbers are labelled.
fn context_breakdown(state: &ReplState) {
    println!("{}", "context detail (estimated tokens)".bold());
    let system = state
        .system_prompt
        .as_ref()
        .map(prompt_text_len)
        .unwrap_or(0);
    println!("  system prompt:      ~{}", system.div_ceil(4));
    let mut rows: Vec<(usize, String)> = state
        .api_messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let chars: usize = message.content.iter().map(content_chars).sum();
            (
                chars.div_ceil(4),
                format!("#{} {}", index + 1, message.role),
            )
        })
        .collect();
    let total: usize = rows.iter().map(|(tokens, _)| *tokens).sum();
    rows.sort_by_key(|(tokens, _)| std::cmp::Reverse(*tokens));
    for (tokens, label) in rows.iter().take(10) {
        println!("  {label:<12} ~{tokens}");
    }
    if rows.len() > 10 {
        println!("  … {} more message(s)", rows.len() - 10);
    }
    println!(
        "  messages total:     ~{total} across {} message(s)",
        rows.len()
    );
}

fn prompt_text_len(prompt: &crate::models::SystemPrompt) -> usize {
    match prompt {
        crate::models::SystemPrompt::Text(text) => text.len(),
        crate::models::SystemPrompt::Blocks(blocks) => {
            blocks.iter().map(|block| block.text.len()).sum()
        }
    }
}

fn content_chars(block: &crate::models::ContentBlock) -> usize {
    match block {
        crate::models::ContentBlock::Text { text, .. } => text.len(),
        crate::models::ContentBlock::ToolUse { input, .. } => input.to_string().len(),
        crate::models::ContentBlock::ToolResult { content, .. } => content.to_string().len(),
        crate::models::ContentBlock::Image { .. } => 1024, // one image ≈ 1k tokens
        other => format!("{other:?}").len(),
    }
}

/// `/cache [n]` — per-turn prefix-cache telemetry table (#263).
pub fn cache(state: &ReplState, arg: Option<&str>) {
    let want = arg.and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
    let history = &state.session.turn_cache_history;
    if history.is_empty() {
        println!("{}", "no cache telemetry yet — run a turn first".dimmed());
        return;
    }
    let start = history.len().saturating_sub(want);
    println!(
        "{}",
        format!(
            "per-turn cache telemetry (last {} of {} turns)",
            history.len() - start,
            history.len()
        )
        .bold()
    );
    println!(
        "  {:>4}  {:>10}  {:>10}  {:>10}  {:>10}",
        "turn", "input", "output", "cache hit", "cache miss"
    );
    for (offset, record) in history.iter().skip(start).enumerate() {
        println!(
            "  {:>4}  {:>10}  {:>10}  {:>10}  {:>10}",
            start + offset + 1,
            record.input_tokens,
            record.output_tokens,
            record
                .cache_hit_tokens
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
            record
                .cache_miss_tokens
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
    }
}

/// `/diff` — git diff since session start.
pub fn diff(state: &ReplState) {
    let Some(mut name_only_cmd) = Git::command() else {
        println!("{} git not found on PATH", "✗".red());
        return;
    };
    let Some(mut stat_cmd) = Git::command() else {
        println!("{} git not found on PATH", "✗".red());
        return;
    };
    let name_only = name_only_cmd
        .args(["diff", "--name-only"])
        .current_dir(&state.workspace)
        .output();
    let stat = stat_cmd
        .args(["diff", "--stat"])
        .current_dir(&state.workspace)
        .output();
    match (name_only, stat) {
        (Ok(name), Ok(stat_output)) => {
            let name_stdout = String::from_utf8_lossy(&name.stdout);
            let stat_stdout = String::from_utf8_lossy(&stat_output.stdout);
            if name_stdout.trim().is_empty() {
                println!("no changes since session start");
                return;
            }
            let files: Vec<&str> = name_stdout.lines().filter(|l| !l.is_empty()).collect();
            println!("changed files ({}):", files.len());
            for file in &files {
                println!("  {file}");
            }
            let stat_str = stat_stdout.trim();
            if !stat_str.is_empty() {
                println!("\n── stat ──\n{stat_str}");
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            println!("git diff failed — is this a git repository?\n{e}");
        }
    }
}

/// `/edit` — print the last user message for re-editing. The REPL composer
/// is a single readline line, so the TUI's "load into composer" degrades
/// to printing the text (arrow-up recovers it from input history).
pub fn edit(state: &ReplState) {
    let last_user = state
        .api_messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    crate::models::ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        });
    match last_user {
        Some(content) if !content.trim().is_empty() => {
            println!("last message (copy, or ↑ in input history, then resubmit):");
            println!("{content}");
        }
        _ => println!("no previous message to edit"),
    }
}

/// Remove the last exchange (assistant tail + the triggering user message)
/// from the engine session and local state. Port of `undo_conversation`.
async fn truncate_to_before_last_user(state: &mut ReplState) -> usize {
    let mut removed = 0usize;
    while let Some(last) = state.api_messages.last() {
        let was_user = last.role == "user";
        state.api_messages.pop();
        removed += 1;
        if was_user {
            break;
        }
    }
    if removed > 0
        && let Ok(engine) = state.engine()
    {
        let _ = engine
            .send(Op::SyncSession {
                session_id: state.current_session_id.clone(),
                messages: state.api_messages.clone(),
                system_prompt: state.system_prompt.clone(),
                system_prompt_override: false,
                model: state.model.clone(),
                workspace: state.workspace.clone(),
            })
            .await;
    }
    removed
}

/// `/undo` — drop the last exchange.
pub async fn undo(state: &mut ReplState) {
    let removed = truncate_to_before_last_user(state).await;
    if removed > 0 {
        println!("removed {removed} message(s)");
    } else {
        println!("nothing to undo");
    }
}

/// `/retry` — drop the last exchange and re-send the same user message.
pub async fn retry(state: &mut ReplState) {
    let last_user = state
        .api_messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    crate::models::ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        });
    let Some(input) = last_user.filter(|text| !text.trim().is_empty()) else {
        println!("{} no previous request to retry", "✗".red());
        return;
    };
    truncate_to_before_last_user(state).await;
    println!("retrying: {}", truncate_for_display(&input, 50));
    let _ = super::super::submit(state, input).await;
}

fn truncate_for_display(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}
