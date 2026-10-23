//! Debug/inspection commands for the REPL — port of the readout half of
//! `commands/debug.rs` against [`ReplState`]. Commands that opened TUI
//! views (`/context` inspector, `/cache inspect`) degrade to line output.

use colored::Colorize;
use std::sync::Arc;

use super::ReplState;
use crate::models::SystemPrompt;
use codesmith_agent_runtime::dependencies::{ExternalTool, Git};
use codesmith_agent_runtime::ops::Op;

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
    // Character counts (see `content_chars`): byte lengths inflate CJK.
    match prompt {
        crate::models::SystemPrompt::Text(text) => text.chars().count(),
        crate::models::SystemPrompt::Blocks(blocks) => {
            blocks.iter().map(|block| block.text.chars().count()).sum()
        }
    }
}

fn content_chars(block: &crate::models::ContentBlock) -> usize {
    // Character counts, not byte lengths: the caller's estimate assumes
    // four characters per token, and byte lengths inflate CJK content
    // ~3x.
    match block {
        crate::models::ContentBlock::Text { text, .. } => text.chars().count(),
        crate::models::ContentBlock::ToolUse { input, .. } => input.to_string().chars().count(),
        crate::models::ContentBlock::ToolResult { content, .. } => {
            content.to_string().chars().count()
        }
        crate::models::ContentBlock::Image { .. } => 4000, // ~1k tokens at 4 chars/token
        other => format!("{other:?}").chars().count(),
    }
}

/// `/cache [n]` — per-turn prefix-cache telemetry table (#263).
pub fn cache(state: &ReplState, arg: Option<&str>) {
    let want = match arg {
        Some(s) => match s.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                println!(
                    "{} '/cache {s}' is not a number — showing last 10 turns",
                    "⚠".yellow()
                );
                10
            }
        },
        None => 10,
    };
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

/// `/diff` — working-tree changes via `git diff` (worktree vs index; no
/// session-start baseline is captured).
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
            // `.output()` only errors on spawn failure — a non-git
            // directory (or any git error) exits non-zero with empty
            // stdout, which the old "no changes" message misreported.
            if !name.status.success() || !stat_output.status.success() {
                let stderr = String::from_utf8_lossy(&name.stderr);
                println!(
                    "git diff failed (exit {:?}) — is this a git repository?\n{}",
                    name.status.code(),
                    stderr.trim()
                );
                return;
            }
            let name_stdout = String::from_utf8_lossy(&name.stdout);
            let stat_stdout = String::from_utf8_lossy(&stat_output.stdout);
            if name_stdout.trim().is_empty() {
                println!("no changes in the working tree");
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
/// from the engine session and local state, returning the drained messages
/// for a caller that may need to restore them (`/retry`). Port of
/// `undo_conversation`.
///
/// `None` means the operation failed and was rolled back (local history
/// restored, engine re-synced) — the caller must not report success. A
/// missing engine is not a failure: an engine-less REPL has no session to
/// diverge from, so the local truncation stands with a warning.
async fn truncate_to_before_last_user(
    state: &mut ReplState,
) -> Option<Vec<crate::models::Message>> {
    // Locate the last user message first: a history with no user turn at
    // all (resumed/imported/seeded sessions) must not be drained wholesale
    // and reported as a successful undo.
    let Some(last_user_idx) = state.api_messages.iter().rposition(|m| m.role == "user") else {
        return Some(Vec::new());
    };
    let mut owned = (*state.api_messages).clone();
    let removed = owned.split_off(last_user_idx);
    state.api_messages = Arc::new(owned);
    if !removed.is_empty() {
        match state.engine() {
            Ok(engine) => {
                if let Err(err) = engine
                    .send(Op::SyncSession {
                        session_id: state.current_session_id.clone(),
                        messages: (*state.api_messages).clone(),
                        system_prompt: state.system_prompt.clone(),
                        system_prompt_override: super::system_prompt_override_active(state),
                        model: state.model.clone(),
                        workspace: state.workspace.clone(),
                    })
                    .await
                {
                    // A failed sync would leave the engine serving the old
                    // (longer) history while local state is short — restore
                    // both rather than half-commit the truncation.
                    restore_exchange(state, removed).await;
                    println!(
                        "{} could not sync the engine — nothing was removed: {err}",
                        "✗".red()
                    );
                    return None;
                }
            }
            Err(err) => println!(
                "{} local history truncated, but engine is unavailable: {err}",
                "✗".red()
            ),
        }
    }
    Some(removed)
}

/// Put a drained tail back into local history and re-sync the engine with
/// it, so a failed follow-up (`/retry`'s resubmission) costs nothing.
async fn restore_exchange(state: &mut ReplState, tail: Vec<crate::models::Message>) {
    let mut merged = (*state.api_messages).clone();
    merged.extend(tail);
    state.api_messages = Arc::new(merged);
    if let Ok(engine) = state.engine() {
        let _ = engine
            .send(Op::SyncSession {
                session_id: state.current_session_id.clone(),
                messages: (*state.api_messages).clone(),
                system_prompt: state.system_prompt.clone(),
                system_prompt_override: super::system_prompt_override_active(state),
                model: state.model.clone(),
                workspace: state.workspace.clone(),
            })
            .await;
    }
}

/// `/undo` — drop the last exchange.
pub async fn undo(state: &mut ReplState) {
    match truncate_to_before_last_user(state).await {
        Some(removed) if !removed.is_empty() => {
            println!("removed {} message(s)", removed.len());
        }
        Some(_) => println!("nothing to undo"),
        None => {}
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
    // Retry reconstructs the turn from Text blocks only — images and
    // tool-result blocks in the original user message are dropped, so say
    // so instead of silently re-sending a materially different request.
    if let Some(msg) = state.api_messages.iter().rev().find(|m| m.role == "user") {
        let dropped = msg
            .content
            .iter()
            .filter(|b| !matches!(b, crate::models::ContentBlock::Text { .. }))
            .count();
        if dropped > 0 {
            println!(
                "{} retry resends text only — {dropped} image/tool-result block(s) dropped",
                "⚠".yellow()
            );
        }
    }
    let Some(removed) = truncate_to_before_last_user(state).await else {
        // The truncation itself failed and was rolled back.
        return;
    };
    if removed.is_empty() {
        println!("{} no previous request to retry", "✗".red());
        return;
    }
    println!("retrying: {}", truncate_for_display(&input, 50));
    // The previous exchange is drained — a failed resubmission restores it
    // (locally and in the engine) instead of losing it permanently.
    if let Err(err) = super::super::submit(state, input).await {
        println!("{} retry failed: {err}", "✗".red());
        restore_exchange(state, removed).await;
    }
}

fn truncate_for_display(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}
