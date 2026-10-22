//! `/permissions [list|allow|deny|remove]` — what runs without asking, and
//! who decided it.
//!
//! Every surface here already existed; nothing new decides anything:
//!
//! - **Session grants** (`state.approval_session_approved` / `_denied`) are
//!   what the approval prompt's `[a]` and `[e]` fill.
//! - **Persisted grants** (`~/.codesmith/approvals.toml`) are per workspace
//!   and per grouping key, and the approval path has always *read* them —
//!   this command is the writer the store was designed for (`grants_for` was
//!   marked as "future management command").
//! - **Trusted paths** (`~/.codesmith/workspace-trust.json`) are `/trust`'s
//!   store; this command prints them, `/trust` edits them.
//! - **`[sandbox] auto_allow`** is the config-file rule set: command
//!   prefixes that never prompt. It is read here and merged into the grant
//!   set at startup, so the documented key finally does something.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - A persisted *deny* has no store. `deny` is session-scoped, and the
//!   command says so rather than writing a rule nothing reads.
//! - Grants are per workspace (the key is the canonical workspace path), so
//!   `allow` covers the project you are in and not its siblings.
//! - The keys are the engine's grouping keys: for a shell command that is
//!   `shell:<canonical prefix>` computed by the same arity dictionary the
//!   approval path uses, so `allow "git status"` covers `git status -s` and
//!   not `git push`. `allow git commit` and `allow "git commit"` are the
//!   same rule; `/permissions list` prints the stored key so there is no
//!   guessing.

use colored::Colorize;

use super::ReplState;
use crate::approval_grants::approvals_toml_path;

/// The command a subcommand argument names: surrounding whitespace and one
/// pair of quotes removed.
///
/// Quoting exists so a multi-word prefix survives the command parser
/// (`/permissions allow "git status"`); the quotes are the shell's business,
/// not part of the command, and leaving them in produced a grant keyed
/// `shell:"git` that could never match anything.
#[must_use]
pub fn normalize_rule(argument: &str) -> String {
    let trimmed = argument.trim();
    for quote in ['"', '\''] {
        if trimmed.len() >= 2 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }
    trimmed.to_string()
}

/// The canonical grant key for a shell command prefix.
///
/// Delegates to the engine's own classifier, so a key written here is the
/// key the approval path will look up.
#[must_use]
pub fn shell_grant_key(command: &str) -> String {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return "shell:<empty>".to_string();
    }
    format!(
        "shell:{}",
        codesmith_agent_runtime::command_safety::classify_command(&tokens)
    )
}

/// The workspace key grants are filed under.
fn workspace_key(state: &ReplState) -> String {
    state.workspace.to_string_lossy().into_owned()
}

/// `/permissions` and `/permissions list`.
pub fn list(state: &ReplState) {
    let key = workspace_key(state);
    println!("{}", "permissions".bold());
    println!("  workspace:        {}", state.workspace.display());
    println!(
        "  approval mode:    {}",
        state.approval_mode.label().to_ascii_lowercase()
    );

    println!("{}", "  session grants:".dimmed());
    if state.approval_session_approved.is_empty() {
        println!("    (none — the approval prompt's [a] fills these)");
    } else {
        let mut granted: Vec<&String> = state.approval_session_approved.iter().collect();
        granted.sort();
        for grant in granted {
            println!("    {grant}");
        }
    }
    if !state.approval_session_denied.is_empty() {
        let mut denied: Vec<&String> = state.approval_session_denied.iter().collect();
        denied.sort();
        println!("{}", "  session denials:".dimmed());
        for denial in denied {
            println!("    {denial}");
        }
    }

    println!("{}", "  persisted grants:".dimmed());
    match approvals_toml_path() {
        Some(path) => {
            let grants = state.approval_grants.grants_for(&key);
            if grants.is_empty() {
                println!("    (none — /permissions allow \"git status\" writes one)");
            } else {
                let mut grants = grants;
                grants.sort();
                for grant in grants {
                    println!("    {grant}");
                }
            }
            println!("    file: {}", path.display());
        }
        None => println!("    (unavailable — home directory not resolvable)"),
    }

    let trusted = crate::workspace_trust::WorkspaceTrust::load_for(&state.workspace);
    let paths = trusted.paths();
    println!("{}", "  trusted paths:".dimmed());
    if paths.is_empty() {
        println!("    (none — /trust add <path> adds one)");
    } else {
        for path in paths {
            println!("    {}", path.display());
        }
    }

    let config_rules = auto_allow_rules(state);
    println!("{}", "  [sandbox] auto_allow:".dimmed());
    if config_rules.is_empty() {
        println!(
            "    (none — config.toml [sandbox] auto_allow = [\"git status\"], read at startup)"
        );
    } else {
        for rule in config_rules {
            println!("    {rule}  →  {}", shell_grant_key(&rule));
        }
    }
}

/// `/permissions allow <command prefix>` — persist a grant.
pub fn allow(state: &mut ReplState, command: &str) {
    let rule = normalize_rule(command);
    if rule.is_empty() {
        println!("{} usage: /permissions allow <command prefix>", "✗".red());
        return;
    }
    let key = workspace_key(state);
    let grant = shell_grant_key(&rule);
    state.approval_grants.grant(&key, &grant);
    // The approval path reads the in-memory store, so this session is
    // covered immediately; the file is what later sessions read.
    match approvals_toml_path() {
        Some(path) => match state.approval_grants.save(&path) {
            Ok(()) => println!(
                "{}",
                format!("allowed {grant} — saved to {}", path.display()).green()
            ),
            Err(err) => println!(
                "{}",
                format!(
                    "allowed {grant} for this session, but saving failed: {err} \
                     (the grant is in memory only)"
                )
                .yellow()
            ),
        },
        None => println!(
            "{}",
            format!("allowed {grant} for this session — no home directory to persist it in")
                .yellow()
        ),
    }
}

/// `/permissions remove <command prefix>` — forget a persisted grant.
pub fn remove(state: &mut ReplState, command: &str) {
    let rule = normalize_rule(command);
    if rule.is_empty() {
        println!("{} usage: /permissions remove <command prefix>", "✗".red());
        return;
    }
    let key = workspace_key(state);
    let grant = shell_grant_key(&rule);
    let existed = state.approval_grants.revoke(&key, &grant);
    state.approval_session_approved.remove(&grant);
    match approvals_toml_path() {
        Some(path) => match state.approval_grants.save(&path) {
            Ok(()) => println!(
                "{}",
                if existed {
                    format!("removed {grant}")
                } else {
                    format!("{grant} was not granted (it is now recorded as removed)")
                }
                .dimmed()
            ),
            Err(err) => println!("{} could not save the removal: {err}", "✗".red()),
        },
        None => println!("{}", format!("cleared {grant} for this session").dimmed()),
    }
}

/// `/permissions deny <command>` — refuse it for the rest of the session.
///
/// Session-scoped by design: a persisted deny would need a store, and none
/// exists (see the module note). The denial is keyed like `/permissions
/// allow` — the engine's grouping key (`shell:<family>`), so `deny git
/// status` refuses that family wherever it appears in a later command. The
/// approval gate also honors the exact per-call keys the prompt's `[e]`
/// records; the two shapes meet in `approval_session_denied`.
pub fn deny(state: &mut ReplState, command: &str) {
    let rule = normalize_rule(command);
    if rule.is_empty() {
        println!("{} usage: /permissions deny <command>", "✗".red());
        return;
    }
    let grant = shell_grant_key(&rule);
    state.approval_session_denied.insert(grant.clone());
    println!(
        "{}",
        format!(
            "denied {grant} for this session — a persisted deny has no store yet; \
             run `/permissions list` to see the key"
        )
        .dimmed()
    );
}

/// The config-file rules, when the key is present.
fn auto_allow_rules(state: &ReplState) -> Vec<String> {
    state
        .config
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.auto_allow.clone())
        .unwrap_or_default()
}

/// Merge `[sandbox] auto_allow` into the grant set at startup.
///
/// Config rules are the user's standing decision, so they are grants for
/// every session — merged in memory rather than written to the grant file,
/// so removing a config rule does not leave a stale grant behind.
pub fn apply_config_rules(state: &mut ReplState) {
    let key = workspace_key(state);
    for rule in auto_allow_rules(state) {
        state.approval_grants.grant(&key, &shell_grant_key(&rule));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_keys_use_the_engine_classifier() {
        // The arity dictionary's documented invariant: a prefix covers its
        // own options and not its siblings.
        assert_eq!(shell_grant_key("git status"), "shell:git status");
        assert_ne!(shell_grant_key("git status"), shell_grant_key("git push"));
        // Extra whitespace and quoting do not change the key.
        assert_eq!(shell_grant_key("  git   status  "), "shell:git status");
        assert_eq!(shell_grant_key(""), "shell:<empty>");
    }

    use crate::approval_grants::ApprovalGrants;

    #[test]
    fn quoted_rules_lose_their_quotes() {
        assert_eq!(normalize_rule("\"git status\""), "git status");
        assert_eq!(normalize_rule("'cargo check'"), "cargo check");
        assert_eq!(normalize_rule("  git push  "), "git push");
        // A lone quote is part of the command, not a wrapper.
        assert_eq!(normalize_rule("\"git"), "\"git");
        assert_eq!(
            shell_grant_key(&normalize_rule("\"git status\"")),
            "shell:git status"
        );
    }

    #[test]
    fn grants_round_trip_through_the_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("approvals.toml");
        let mut store = ApprovalGrants::default();
        store.grant("/w/a", "shell:git status");
        store.save(&path).expect("save");

        let reloaded = ApprovalGrants::load(&path);
        assert!(reloaded.is_granted("/w/a", "shell:git status"));
        assert_eq!(
            reloaded.grants_for("/w/a"),
            vec!["shell:git status".to_string()]
        );
        assert!(
            !reloaded.is_granted("/w/b", "shell:git status"),
            "grants stay inside their workspace"
        );

        // A removal survives the union merge with the on-disk copy.
        let mut second = ApprovalGrants::load(&path);
        second.revoke("/w/a", "shell:git status");
        second.save(&path).expect("save removal");
        let after = ApprovalGrants::load(&path);
        assert!(!after.is_granted("/w/a", "shell:git status"));

        // Re-granting clears the removal instead of being dropped by it.
        let mut third = ApprovalGrants::load(&path);
        third.grant("/w/a", "shell:cargo check");
        third.save(&path).expect("save");
        third.revoke("/w/a", "shell:cargo check");
        third.grant("/w/a", "shell:cargo check");
        third.save(&path).expect("save after re-grant");
        let final_state = ApprovalGrants::load(&path);
        assert!(final_state.is_granted("/w/a", "shell:cargo check"));
    }
}
