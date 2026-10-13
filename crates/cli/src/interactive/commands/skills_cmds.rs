//! Skills commands for the REPL — port of `commands/skills.rs`'s
//! activation and listing paths, plus the community-skill surface
//! (`/skills --remote`, `/skills sync`, `/skill install|update|uninstall|trust`)
//! driving the runtime install library.

use colored::Colorize;

use super::ReplState;
use crate::network_policy::{DecisionToml, NetworkPolicy};
use crate::skills::install::{
    self, InstallOutcome, RegistryFetchResult, SkillSyncOutcome, SyncResult, UpdateResult,
};
use crate::skills::{Skill, SkillRegistry};

/// Resolved `[skills]` knobs (defaults when the table is absent).
fn skills_config(config: &crate::config::Config) -> crate::config::SkillsConfig {
    config.skills.clone().unwrap_or_default()
}

/// The network policy for user-initiated skill operations: the user typed
/// the command, so a `prompt` default counts as consent — but an explicit
/// deny still blocks (deny wins).
fn user_initiated_policy(config: &crate::config::Config) -> NetworkPolicy {
    let mut policy = config.network.clone().unwrap_or_default().into_runtime();
    if policy.default == DecisionToml::Prompt {
        policy.default = DecisionToml::Allow;
    }
    policy
}

fn discover(state: &ReplState) -> SkillRegistry {
    let mut registry =
        crate::skills::discover_for_workspace_and_dir(&state.workspace, &state.skills_dir);
    // The bundled review/verify skills live beside the discovered ones so
    // `/skills`, `/skill <name>`, and the commands below share one registry.
    // A skill of the same name on disk wins (`merge_registered` skips the
    // collision and records a warning).
    registry.merge_registered(&crate::bundled_skills::registered());
    registry
}

/// `/skills [prefix]` — local list; `--remote` browses the community
/// registry; `sync` refreshes the local registry cache.
pub async fn list_skills(state: &ReplState, arg: Option<&str>) {
    let arg = arg.map(str::trim).unwrap_or("");
    if arg == "--remote" {
        list_remote(state).await;
        return;
    }
    if arg == "sync" {
        sync_registry_cache(state).await;
        return;
    }
    let mut prefix: Option<String> = None;
    if !arg.is_empty() {
        if arg.starts_with('-') || arg.split_whitespace().count() > 1 {
            println!("{} usage: /skills [<name-prefix>|--remote|sync]", "✗".red());
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

/// `/skill <name> [args]` — activate a skill; the reserved management
/// subcommands (`install` / `update` / `uninstall` / `trust`) drive the
/// community-skill install library instead.
pub async fn run_skill(state: &mut ReplState, name: Option<&str>) {
    let Some(raw) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        println!(
            "{} usage: /skill <name> | install <source> | update <name> | uninstall <name> | trust <name> (skills live under {})",
            "✗".red(),
            state.skills_dir.display()
        );
        return;
    };
    let (first, rest) = raw.split_once(char::is_whitespace).unwrap_or((raw, ""));
    match first.to_ascii_lowercase().as_str() {
        "install" => return install_skill(state, rest.trim()).await,
        "update" => return update_skill(state, rest.trim()).await,
        "uninstall" => return uninstall_skill(state, rest.trim()),
        "trust" => return trust_skill(state, rest.trim()),
        _ => {}
    }
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

/// Activate one of the bundled review/verify skills with a target.
///
/// The skills are prompt workflows (see `crate::bundled_skills`), so
/// activating one is the same call `/skill <name>` makes: the body goes into
/// the conversation as the instruction for this turn.
pub async fn activate(state: &mut ReplState, name: &str, target: &str) {
    let registry = discover(state);
    let Some(skill) = registry.get(name) else {
        println!(
            "{} the {name} skill is missing from this build — /skills lists what is available",
            "✗".red()
        );
        return;
    };
    let body = crate::skills::preprocess_skill_body_for_activation(skill);
    let instruction = format!(
        "You are now using a skill. Follow these instructions:\n\n# Skill: {}\n\n{}\n\n---\n\nNow respond to the user's request following the above skill instructions.",
        skill.name, body
    );
    let request = if target.trim().is_empty() {
        instruction
    } else {
        format!("{instruction}\n\nTarget: {}", target.trim())
    };
    let _ = super::super::submit(state, request).await;
}

/// Parse `/code-review [low|medium|high|xhigh|max] [--fix] [target]`.
///
/// The level and the flag are Claude Code's; this REPL has no separate model
/// switch per level, so the level is carried into the prompt as the review's
/// depth instruction rather than silently dropped.
pub async fn code_review(state: &mut ReplState, args: &str) {
    let mut depth = String::new();
    let mut fix = false;
    let mut target: Vec<&str> = Vec::new();
    for part in args.split_whitespace() {
        match part {
            "low" | "medium" | "high" | "xhigh" | "max" => depth = part.to_string(),
            "--fix" | "fix" => fix = true,
            other => target.push(other),
        }
    }
    let mut request = String::new();
    if !depth.is_empty() {
        request.push_str(&format!(
            "Review depth: {depth}. Spend effort accordingly.\n"
        ));
    }
    if fix {
        request.push_str(
            "Apply the fixes you are confident about after reporting them, then say what you changed.\n",
        );
    }
    request.push_str(&target.join(" "));
    activate(state, "code-review", &request).await;
}

/// `/simplify [target]`
pub async fn simplify(state: &mut ReplState, target: &str) {
    activate(state, "simplify", target).await;
}

/// `/security-review [target]`
pub async fn security_review(state: &mut ReplState, target: &str) {
    activate(state, "security-review", target).await;
}

/// `/run [target]`
pub async fn run_app(state: &mut ReplState, target: &str) {
    activate(state, "run", target).await;
}

/// `/skills --remote` — browse the community registry index.
async fn list_remote(state: &ReplState) {
    let url = skills_config(&state.config).registry_url();
    let policy = user_initiated_policy(&state.config);
    println!("{}", format!("registry: {url}").dimmed());
    match install::fetch_registry(&policy, &url).await {
        Ok(RegistryFetchResult::Loaded(doc)) => {
            if doc.skills.is_empty() {
                println!("registry index is empty");
                return;
            }
            println!("registry skills ({}):", doc.skills.len());
            for (name, entry) in &doc.skills {
                println!(
                    "  /{name} — {}",
                    entry
                        .description
                        .as_deref()
                        .unwrap_or(entry.source.as_str())
                );
            }
            println!(
                "{}",
                "install one with /skill install <name|source>; /skills sync refreshes the local cache".dimmed()
            );
        }
        Ok(RegistryFetchResult::NeedsApproval(host)) => println!(
            "{}",
            format!("network policy needs approval for {host} — allow the host under [network] in config.toml").yellow()
        ),
        Ok(RegistryFetchResult::Denied(host)) => {
            println!("{} network policy denies {host}", "✗".red());
        }
        Err(err) => println!("{} registry fetch failed: {err}", "✗".red()),
    }
}

/// `/skills sync` — refresh the local registry cache (~/.codesmith/cache/skills).
async fn sync_registry_cache(state: &ReplState) {
    let config = skills_config(&state.config);
    let url = config.registry_url();
    let max = config.max_install_size_bytes();
    let policy = user_initiated_policy(&state.config);
    let cache_dir = install::default_cache_skills_dir();
    println!(
        "{}",
        format!("syncing registry {url} → {}", cache_dir.display()).dimmed()
    );
    match install::sync_registry(&policy, &url, &cache_dir, max).await {
        Ok(SyncResult::Done { outcomes }) => {
            let (mut downloaded, mut fresh, mut failed) = (0u32, 0u32, 0u32);
            for outcome in &outcomes {
                match outcome {
                    SkillSyncOutcome::Downloaded { name, .. } => {
                        downloaded += 1;
                        println!("  ↓ {name}");
                    }
                    SkillSyncOutcome::Fresh { name } => {
                        fresh += 1;
                        println!("{}", format!("  = {name} (fresh)").dimmed());
                    }
                    SkillSyncOutcome::Failed { name, reason } => {
                        failed += 1;
                        println!("  ✗ {name}: {reason}");
                    }
                    SkillSyncOutcome::Denied { name, host } => {
                        failed += 1;
                        println!("  ✗ {name}: network policy denies {host}");
                    }
                    SkillSyncOutcome::NeedsApproval { name, host } => {
                        failed += 1;
                        println!("  ✗ {name}: network policy needs approval for {host}");
                    }
                }
            }
            println!(
                "synced: {downloaded} downloaded, {fresh} fresh, {failed} failed (cache: {})",
                cache_dir.display()
            );
        }
        Ok(SyncResult::RegistryDenied(host)) => {
            println!("{} network policy denies registry host {host}", "✗".red());
        }
        Ok(SyncResult::RegistryNeedsApproval(host)) => println!(
            "{}",
            format!("network policy needs approval for registry host {host}").yellow()
        ),
        Err(err) => println!("{} sync failed: {err}", "✗".red()),
    }
}

/// `/skill install <source>` — github:<owner/repo> | bare GitHub URL |
/// direct tarball URL | registry key.
async fn install_skill(state: &ReplState, source: &str) {
    if source.is_empty() {
        println!(
            "{} usage: /skill install <github:owner/repo | URL | registry-key>",
            "✗".red()
        );
        return;
    }
    let parsed = match install::InstallSource::parse(source) {
        Ok(parsed) => parsed,
        Err(err) => {
            println!("{} {err}", "✗".red());
            return;
        }
    };
    let config = skills_config(&state.config);
    let url = config.registry_url();
    let max = config.max_install_size_bytes();
    let policy = user_initiated_policy(&state.config);
    let dir = state.skills_dir.clone();
    println!(
        "{}",
        format!("installing {source} → {}", dir.display()).dimmed()
    );
    match install::install_with_registry(parsed, &dir, max, &policy, false, &url).await {
        Ok(InstallOutcome::Installed(skill)) => {
            println!("installed {} → {}", skill.name, skill.path.display());
            if !skill.path.join(install::TRUSTED_MARKER).is_file() {
                println!(
                    "{}",
                    "untrusted: shell snippets stay disabled until /skill trust <name>".yellow()
                );
            }
        }
        Ok(InstallOutcome::NeedsApproval(host)) => println!(
            "{}",
            format!("network policy needs approval for {host} — allow the host under [network] in config.toml").yellow()
        ),
        Ok(InstallOutcome::NetworkDenied(host)) => {
            println!("{} network policy denies {host}", "✗".red());
        }
        Err(err) if err.to_string().contains("already installed") => println!(
            "{} skill already installed — /skill update <name> refreshes it",
            "✗".red()
        ),
        Err(err) => println!("{} install failed: {err}", "✗".red()),
    }
}

/// `/skill update <name>` — refresh from the recorded install source.
async fn update_skill(state: &ReplState, name: &str) {
    if name.is_empty() {
        println!("{} usage: /skill update <name>", "✗".red());
        return;
    }
    let config = skills_config(&state.config);
    let url = config.registry_url();
    let max = config.max_install_size_bytes();
    let policy = user_initiated_policy(&state.config);
    let dir = state.skills_dir.clone();
    let name = name.to_string();
    match install::update_with_registry(&name, &dir, max, &policy, &url).await {
        Ok(UpdateResult::NoChange) => println!("{name} is already up to date"),
        Ok(UpdateResult::Updated(skill)) => {
            println!("updated {} → {}", skill.name, skill.path.display());
        }
        Ok(UpdateResult::NeedsApproval(host)) => println!(
            "{}",
            format!("network policy needs approval for {host} — allow the host under [network] in config.toml").yellow()
        ),
        Ok(UpdateResult::NetworkDenied(host)) => {
            println!("{} network policy denies {host}", "✗".red());
        }
        Err(err) => println!("{} update failed: {err}", "✗".red()),
    }
}

/// `/skill uninstall <name>` — remove an installed skill (refuses skills
/// without an install marker).
fn uninstall_skill(state: &ReplState, name: &str) {
    if name.is_empty() {
        println!("{} usage: /skill uninstall <name>", "✗".red());
        return;
    }
    match install::uninstall(name, &state.skills_dir) {
        Ok(()) => println!("uninstalled {name}"),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

/// `/skill trust <name>` — drop the `.trusted` marker, unlocking the
/// skill's shell snippets.
fn trust_skill(state: &ReplState, name: &str) {
    if name.is_empty() {
        println!("{} usage: /skill trust <name>", "✗".red());
        return;
    }
    match install::trust(name, &state.skills_dir) {
        Ok(()) => println!("{name} is now trusted — its shell snippets can run"),
        Err(err) => println!("{} {err}", "✗".red()),
    }
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
