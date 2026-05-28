//! /hunt command — declare a quarry with token budget and verdict tracking (#2092).

use crate::tui::app::{App, AppAction, HuntVerdict};

use super::CommandResult;

/// Declare, show, or close a hunt
pub fn hunt(app: &mut App, arg: Option<&str>) -> CommandResult {
    match arg {
        Some("clear") | Some("reset") => {
            app.goal.quarry = None;
            app.goal.token_budget = None;
            app.goal.started_at = None;
            app.goal.verdict = HuntVerdict::default();
            CommandResult::message("Hunt cleared.")
        }
        Some("done") | Some("complete") | Some("hunted") => {
            app.goal.verdict = HuntVerdict::Hunted;
            let elapsed = app
                .goal
                .started_at
                .map(|t| crate::tui::notifications::humanize_duration(t.elapsed()))
                .unwrap_or_else(|| "unknown".to_string());
            CommandResult::message(format!("Hunt complete! Elapsed: {elapsed}"))
        }
        Some("wound") | Some("wounded") => {
            app.goal.verdict = HuntVerdict::Wounded;
            CommandResult::message("Hunt wounded — progress saved, can be resumed.")
        }
        Some("escape") | Some("escaped") => {
            app.goal.verdict = HuntVerdict::Escaped;
            CommandResult::message("Hunt escaped — quarry abandoned.")
        }
        Some(text) if !text.is_empty() => {
            let (objective, budget) = parse_goal_budget(text);
            let objective = objective.trim().to_string();
            if objective.is_empty() || objective.chars().all(|c| c == '|') {
                return CommandResult::error("Usage: /hunt <quarry> [budget: N]");
            }
            app.goal.quarry = Some(objective.clone());
            app.goal.token_budget = budget;
            app.goal.started_at = Some(std::time::Instant::now());
            app.goal.verdict = HuntVerdict::Hunting;
            let budget_str = budget
                .map(|b| format!(" (budget: {b} tokens)"))
                .unwrap_or_default();
            CommandResult::with_message_and_action(
                format!("Hunt set: \"{objective}\"{budget_str} — tracking progress."),
                AppAction::SendMessage(objective),
            )
        }
        _ => {
            if let Some(ref obj) = app.goal.quarry {
                let elapsed = app
                    .goal
                    .started_at
                    .map(|t| crate::tui::notifications::humanize_duration(t.elapsed()))
                    .unwrap_or_else(|| "unknown".to_string());
                let budget_str = app
                    .goal
                    .token_budget
                    .map(|b| {
                        let used = app.session.total_conversation_tokens;
                        let pct = if b > 0 {
                            (used as f64 / b as f64 * 100.0).min(100.0)
                        } else {
                            0.0
                        };
                        format!(" | tokens: {used}/{b} ({pct:.0}%)")
                    })
                    .unwrap_or_default();
                let verdict_label = match app.goal.verdict {
                    HuntVerdict::Hunting => "[HUNTING]",
                    HuntVerdict::Hunted => "[HUNTED]",
                    HuntVerdict::Wounded => "[WOUNDED]",
                    HuntVerdict::Escaped => "[ESCAPED]",
                };
                CommandResult::message(format!(
                    "Hunt{verdict_label}: \"{obj}\" — elapsed: {elapsed}{budget_str}"
                ))
            } else {
                CommandResult::message(
                    "No hunt set. Use /hunt <quarry> [budget: N] to declare one.\n\
                     /hunt hunted — mark complete\n\
                     /hunt wounded — mark interrupted (resumable)\n\
                     /hunt escaped — mark abandoned\n\
                     /hunt clear — remove the current hunt.",
                )
            }
        }
    }
}

/// Parse text like "Implement login | budget: 50000" into (objective, budget).
fn parse_goal_budget(text: &str) -> (&str, Option<u32>) {
    if let Some(pipe_pos) = text.find('|') {
        let (objective, rest) = text.split_at(pipe_pos);
        let budget = rest[1..]
            .split_whitespace()
            .filter_map(|part| {
                if part.eq_ignore_ascii_case("budget:") {
                    None
                } else {
                    part.parse::<u32>().ok()
                }
            })
            .next();
        (objective, budget)
    } else {
        (text, None)
    }
}
