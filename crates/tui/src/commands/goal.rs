//! /hunt command — declare a quarry with token budget and verdict tracking (#2092).

use std::io::Write;

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
            let prev = app.goal.verdict;
            app.goal.verdict = HuntVerdict::Hunted;
            let elapsed = app
                .goal
                .started_at
                .map(|t| crate::tui::notifications::humanize_duration(t.elapsed()))
                .unwrap_or_else(|| "unknown".to_string());
            if prev != HuntVerdict::Hunted {
                if let Err(e) = write_trophy_card(app) {
                    return CommandResult::error(format!(
                        "Hunt complete but trophy write failed: {e}"
                    ));
                }
            }
            CommandResult::message(format!("Hunt complete! Elapsed: {elapsed}"))
        }
        Some("wound") | Some("wounded") => {
            app.goal.verdict = HuntVerdict::Wounded;
            let _ = write_trophy_card(app);
            CommandResult::message("Hunt wounded — progress saved, can be resumed.")
        }
        Some("escape") | Some("escaped") => {
            app.goal.verdict = HuntVerdict::Escaped;
            let _ = write_trophy_card(app);
            CommandResult::message("Hunt escaped — quarry abandoned.")
        }
        Some(text) if !text.is_empty() => {
            let (objective, budget) = parse_hunt_budget(text);
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
                    "Hunt {verdict_label}: \"{obj}\" — elapsed: {elapsed}{budget_str}"
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
fn parse_hunt_budget(text: &str) -> (&str, Option<u32>) {
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

/// Write a trophy card to `~/.codewhale/trophies/<date>-<slug>.md` for the
/// current hunt verdict (#2092). Returns the path written on success.
fn write_trophy_card(app: &mut App) -> Result<std::path::PathBuf, std::io::Error> {
    let quarry = app.goal.quarry.as_deref().unwrap_or("untitled");
    let slug = quarry
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let slug = if slug.is_empty() { "untitled" } else { &slug };
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d");
    let dir = codewhale_config::resolve_state_dir("trophies")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::create_dir_all(&dir)?;
    let filename = format!("{date}-{slug}.md");
    let path = dir.join(&filename);

    let elapsed = app
        .goal
        .started_at
        .as_ref()
        .map(|t| crate::tui::notifications::humanize_duration(t.elapsed()))
        .unwrap_or_else(|| "unknown".to_string());
    let verdict_str = match app.goal.verdict {
        HuntVerdict::Hunting => "hunting",
        HuntVerdict::Hunted => "hunted",
        HuntVerdict::Wounded => "wounded",
        HuntVerdict::Escaped => "escaped",
    };
    let tokens = app.session.total_conversation_tokens;
    let budget_str = app
        .goal
        .token_budget
        .map(|b| format!("{b}"))
        .unwrap_or_else(|| "—".to_string());

    let mut f = std::fs::File::create(&path)?;
    writeln!(f, "# Trophy: {quarry}")?;
    writeln!(f)?;
    writeln!(f, "- **Verdict**: {verdict_str}")?;
    writeln!(f, "- **Date**: {date}")?;
    writeln!(f, "- **Elapsed**: {elapsed}")?;
    writeln!(f, "- **Tokens used**: {tokens}")?;
    writeln!(f, "- **Token budget**: {budget_str}")?;
    writeln!(f)?;
    writeln!(f, "_Generated by CodeWhale `/hunt` — {now}_")?;
    drop(f);

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_app() -> App {
        let options = crate::tui::app::TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: std::path::PathBuf::from("/tmp/test-workspace"),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: std::path::PathBuf::from("/tmp/test-skills"),
            memory_path: std::path::PathBuf::from("memory.md"),
            notes_path: std::path::PathBuf::from("notes.txt"),
            mcp_config_path: std::path::PathBuf::from("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            initial_input: None,
            resume_session_id: None,
            yolo: false,
        };
        let config = crate::config::Config::default();
        App::new(options, &config)
    }

    #[test]
    fn test_set_hunt() {
        let mut app = create_test_app();
        let result = hunt(&mut app, Some("Fix the login bug"));
        assert!(result.message.unwrap().contains("Hunt set"));
        assert_eq!(app.goal.quarry.as_deref(), Some("Fix the login bug"));
        assert!(matches!(
            result.action,
            Some(AppAction::SendMessage(msg)) if msg == "Fix the login bug"
        ));
    }

    #[test]
    fn test_hunt_without_argument_shows_state() {
        let mut app = create_test_app();
        let result = hunt(&mut app, None);
        assert!(result.action.is_none());
        assert!(result.message.as_deref().unwrap().contains("No hunt set"));
    }

    #[test]
    fn test_set_hunt_with_budget() {
        let mut app = create_test_app();
        let _ = hunt(&mut app, Some("Refactor auth | budget: 50000"));
        assert_eq!(app.goal.quarry.as_deref(), Some("Refactor auth"));
        assert_eq!(app.goal.token_budget, Some(50_000));
        assert!(app.goal.started_at.is_some());
    }

    #[test]
    fn test_set_hunt_rejects_budget_only_objective() {
        let mut app = create_test_app();
        app.goal.quarry = Some("existing objective".to_string());
        app.goal.token_budget = Some(10_000);

        let result = hunt(&mut app, Some("budget: 50000"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Usage: /hunt")
        );
        assert_eq!(app.goal.quarry.as_deref(), Some("existing objective"));
        assert_eq!(app.goal.token_budget, Some(10_000));
    }

    #[test]
    fn test_clear_hunt() {
        let mut app = create_test_app();
        app.goal.quarry = Some("test".to_string());
        let _ = hunt(&mut app, Some("clear"));
        assert!(app.goal.quarry.is_none());
        assert!(app.goal.token_budget.is_none());
    }

    #[test]
    fn test_show_hunt_when_none() {
        let mut app = create_test_app();
        let result = hunt(&mut app, None);
        assert!(result.message.unwrap().contains("No hunt set"));
    }

    #[test]
    fn test_parse_budget() {
        assert_eq!(
            parse_hunt_budget("Do a thing | budget: 50000"),
            ("Do a thing", Some(50_000))
        );
        assert_eq!(parse_hunt_budget("Simple goal"), ("Simple goal", None));
        assert_eq!(parse_hunt_budget("Goal budget:1000"), ("Goal", Some(1000)));
    }
}
