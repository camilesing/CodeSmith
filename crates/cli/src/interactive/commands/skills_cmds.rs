//! Skills commands for the REPL — port of `commands/skills.rs`'s
//! activation and listing paths. install/update/uninstall/trust defer to
//! the file layout under `~/.codesmith/skills` for now.

use colored::Colorize;

use super::ReplState;
use crate::skills::{Skill, SkillRegistry};

fn discover(state: &ReplState) -> SkillRegistry {
    crate::skills::discover_for_workspace_and_dir(&state.workspace, &state.skills_dir)
}

/// `/skills [prefix]`
pub fn list_skills(state: &ReplState, arg: Option<&str>) {
    let mut prefix: Option<String> = None;
    if let Some(arg) = arg.map(str::trim) {
        if arg.is_empty() || arg == "--remote" || arg == "sync" {
            println!(
                "{}",
                "remote skill registries and sync are not wired in the REPL yet — manage skills under the skills dir".dimmed()
            );
            return;
        }
        if arg.starts_with('-') || arg.split_whitespace().count() > 1 {
            println!("{} usage: /skills [<name-prefix>]", "✗".red());
            return;
        }
        prefix = Some(arg.to_ascii_lowercase());
    }
    let registry = discover(state);
    let visible: Vec<&Skill> = registry
        .list()
        .iter()
        .filter(|skill| skill.user_invocable)
        .filter(|skill| {
            prefix
                .as_deref()
                .is_none_or(|p| skill.name.to_ascii_lowercase().starts_with(p))
        })
        .collect();
    if visible.is_empty() {
        println!(
            "no skills found{} — add {}/<name>/SKILL.md",
            prefix
                .as_deref()
                .map(|p| format!(" matching `{p}`"))
                .unwrap_or_default(),
            state.skills_dir.display()
        );
        return;
    }
    println!("available skills ({}):", visible.len());
    for skill in visible {
        println!("  /{} — {}", skill.name, skill.description);
    }
    println!("{}", "run one with /skill <name> [args]".dimmed());
}

/// `/skill <name> [args]`
pub async fn run_skill(state: &mut ReplState, name: Option<&str>) {
    let Some(raw) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        println!(
            "{} usage: /skill <name> — manages activation; install/update/uninstall are file operations under {}",
            "✗".red(),
            state.skills_dir.display()
        );
        return;
    };
    let registry = discover(state);
    let target = if raw == "new" { "skill-creator" } else { raw };
    let invocation = resolve_invocation(&registry, target);
    let Some((skill, args)) = invocation else {
        println!(
            "{} skill '{target}' not found — /skills lists what exists",
            "✗".red()
        );
        return;
    };
    if !skill.user_invocable {
        println!(
            "{} skill '{}' is not user-invocable (loaded internally when relevant)",
            "✗".red(),
            skill.name
        );
        return;
    }
    let body = crate::skills::preprocess_skill_body_for_activation(skill);
    let body = expand_skill_variables(skill, &body, args);
    let instruction = format!(
        "You are now using a skill. Follow these instructions:\n\n# Skill: {}\n\n{}\n\n---\n\nNow respond to the user's request following the above skill instructions.",
        skill.name, body
    );
    println!("{}", format!("activating skill {}…", skill.name).dimmed());
    let _ = super::super::submit(state, instruction).await;
}

/// `/review <target>` — activate the `review` skill (port of
/// commands/review.rs).
pub async fn review(state: &mut ReplState, args: Option<&str>) {
    let Some(target) = args.map(str::trim).filter(|s| !s.is_empty()) else {
        println!("{} usage: /review <target>", "✗".red());
        return;
    };
    let registry = discover(state);
    let Some(skill) = registry.get("review") else {
        println!(
            "{} review skill not found — create {}/review/SKILL.md",
            "✗".red(),
            state.skills_dir.display()
        );
        return;
    };
    let body = crate::skills::preprocess_skill_body_for_activation(skill);
    let instruction = format!(
        "You are now using a skill. Follow these instructions:\n\n# Skill: {}\n\n{}\n\n---\n\nNow respond to the user's request following the above skill instructions.",
        skill.name, body
    );
    let _ = super::super::submit(state, format!("{instruction}\n\nReview target: {target}")).await;
}

fn resolve_invocation<'a>(
    registry: &'a SkillRegistry,
    raw: &'a str,
) -> Option<(&'a Skill, &'a str)> {
    let trimmed = raw.trim();
    let mut matches: Vec<(&Skill, &str)> = registry
        .list()
        .iter()
        .filter_map(|skill| {
            if !skill.user_invocable {
                return None;
            }
            if trimmed == skill.name {
                return Some((skill, ""));
            }
            trimmed
                .strip_prefix(&skill.name)
                .filter(|rest| rest.starts_with(char::is_whitespace))
                .map(|rest| (skill, rest.trim()))
        })
        .collect();
    matches.sort_by(|(a, _), (b, _)| b.name.len().cmp(&a.name.len()));
    matches.into_iter().next()
}

/// `$ARGUMENTS` / `$1..` substitution in skill bodies (port of
/// commands/skills.rs::expand_skill_variables, minus TUI-only variables).
fn expand_skill_variables(skill: &Skill, body: &str, args: &str) -> String {
    let mut expanded = body.replace("$ARGUMENTS", args);
    for (index, arg) in args.split_whitespace().enumerate() {
        expanded = expanded.replace(&format!("${}", index + 1), arg);
    }
    expanded.replace("$SKILL_NAME", &skill.name)
}

/// User-defined commands (`~/.codesmith/commands/*.md`) — the highest
/// precedence command layer before the static table (port of
/// commands/user_commands.rs). Returns `Some(true)` when handled.
pub async fn try_user_command(state: &mut ReplState, command: &str, args: &str) -> Option<bool> {
    let parts: Vec<&str> = command.trim().splitn(2, ' ').collect();
    let name = parts[0].trim_start_matches('/').to_lowercase();
    let user_commands = super::user_cmd_store::load_user_commands(Some(&state.workspace));
    for (user_name, content) in &user_commands {
        if user_name != &name {
            continue;
        }
        let (metadata, body) = super::user_cmd_store::parse_frontmatter(content);
        // Reset per-command goal state, mirroring the TUI behavior.
        state.hunt.quarry = None;
        state.hunt.started_at = None;
        state.hunt.verdict = crate::app_state::HuntVerdict::Hunting;
        state.hunt.token_budget = None;
        state.active_allowed_tools = None;
        for (key, value) in &metadata {
            match key.as_str() {
                "description" => {
                    state.hunt.quarry = Some(value.clone());
                    state.hunt.started_at = Some(std::time::Instant::now());
                }
                "allowed-tools" => {
                    state.active_allowed_tools =
                        Some(super::user_cmd_store::parse_allowed_tools(value));
                }
                _ => {}
            }
        }
        let message = super::user_cmd_store::apply_template(body, args);
        let _ = super::super::submit(state, message).await;
        return Some(true);
    }
    None
}
