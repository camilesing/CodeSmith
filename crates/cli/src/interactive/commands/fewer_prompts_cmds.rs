//! `/fewer-permission-prompts` — turn approval friction into allow rules.
//!
//! Claude Code's version scans your transcripts for common read-only Bash and
//! MCP calls and writes an allowlist into the project's settings. This is the
//! same idea against the stores this REPL has: it reads the saved sessions
//! ([`super::usage_scan`]), counts which shell commands you actually run,
//! keeps the ones `command_safety::analyze_command` classifies as
//! [`SafetyLevel::Safe`] (read-only — the same authority the approval prompt
//! consults), and proposes one rule per command, most frequent first.
//!
//! Nothing is written without `--yes`, and what it writes is exactly what
//! `/permissions allow <prefix>` writes: a grant in
//! `~/.codesmith/approvals.toml`, keyed by the arity dictionary
//! (`shell_grant_key`), so a rule for `git status` covers `git status -s` and
//! not `git push`. The dry run is the default because widening what runs
//! unattended is a decision about risk, not a formatting choice.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Read-only commands only. A `WorkspaceSafe` command (one that modifies
//!   files) is deliberately not proposed: those are the approvals worth
//!   answering one at a time. Use `/permissions allow` to grant one on
//!   purpose.
//! - The transcript it reads is this machine's saved sessions, newest first
//!   and capped; a command from a session saved elsewhere is not counted.
//! - "Frequently prompted" is measured as "frequently run and not yet
//!   granted": the approval prompt itself is not recorded in the stores, so
//!   the count is usage, not a prompt count.
//! - MCP tools are not proposed. Their read-only-ness is a property of a
//!   server this command cannot ask, and a wrong guess here is a rule that
//!   runs someone else's tool unattended.

use std::collections::HashMap;

use colored::Colorize;

use super::{ReplState, permission_cmds, usage_scan};
use codesmith_agent_runtime::command_safety::{SafetyLevel, analyze_command};

/// How many session files to read.
const SESSION_LIMIT: usize = 200;
/// A command must be seen at least this often to be worth a rule.
const MIN_RUNS: u32 = 3;
/// How many rules to offer at most.
const MAX_RULES: usize = 12;

/// One candidate rule.
#[derive(Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The command prefix `/permissions allow` takes (`git status`).
    pub prefix: String,
    /// The grant key the approval path matches and the store holds
    /// (`shell:git status`), which is what the report prints and what an
    /// existing grant is compared against.
    pub key: String,
    /// A full command line that produced this key, for the report.
    pub example: String,
    pub runs: u32,
}

/// Count shell commands across the newest `limit` saved sessions.
///
/// Separated from the printing so the counting can be tested without a REPL
/// or a home directory.
pub fn tally(commands: &[String]) -> HashMap<String, (Vec<String>, u32)> {
    let mut counts: HashMap<String, (Vec<String>, u32)> = HashMap::new();
    for command in commands {
        let key = permission_cmds::shell_grant_key(command);
        // `shell:<empty>` is what an all-whitespace command keys to; it
        // cannot be granted and must not be proposed.
        if key.is_empty() || key == "shell:<empty>" {
            continue;
        }
        // Every observed variant of the family is kept: the rule written
        // with `--yes` covers the whole family, so safety must hold for all
        // of them, not for whichever example happened to come first.
        let entry = counts.entry(key).or_insert_with(|| (Vec::new(), 0));
        entry.0.push(command.clone());
        entry.1 += 1;
    }
    counts
}

/// Rank the tally: frequent first, read-only only.
///
/// `granted` is the set of rules already in force, which are dropped — the
/// command exists to remove friction that is still there.
pub fn suggestions(
    counts: HashMap<String, (Vec<String>, u32)>,
    granted: &[String],
) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = counts
        .into_iter()
        .filter(|(_, (_, runs))| *runs >= MIN_RUNS)
        .filter(|(key, _)| !granted.iter().any(|existing| existing == key))
        // The grant covers every variant that keys here, so every observed
        // variant must be read-only — one Safe example must not vouch for a
        // family that also contains an unsafe spelling.
        .filter(|(_, (examples, _))| {
            examples
                .iter()
                .all(|example| analyze_command(example).level == SafetyLevel::Safe)
        })
        .map(|(key, (examples, runs))| Suggestion {
            prefix: key.strip_prefix("shell:").unwrap_or(&key).to_string(),
            key,
            example: examples.first().cloned().unwrap_or_default(),
            runs,
        })
        .collect();
    out.sort_by(|a, b| b.runs.cmp(&a.runs).then_with(|| a.key.cmp(&b.key)));
    out.truncate(MAX_RULES);
    out
}

/// `/fewer-permission-prompts [--yes|--all]`.
pub async fn run(state: &mut ReplState, arg: &str) {
    let apply = arg
        .split_whitespace()
        .any(|word| matches!(word, "--yes" | "-y" | "--all"));

    // Reading the session files is blocking file IO and it is the whole cost
    // of the command.
    let counts = match tokio::task::spawn_blocking(|| {
        let files = usage_scan::session_files(SESSION_LIMIT);
        let mut commands = Vec::new();
        for file in &files {
            commands.extend(usage_scan::shell_commands(file));
        }
        (files.len(), tally(&commands))
    })
    .await
    {
        Ok(counts) => counts,
        Err(err) => {
            // A JoinError is a panic or cancellation in the worker, not an
            // IO failure — do not blame a file read that never failed.
            if err.is_panic() {
                std::panic::resume_unwind(err.into_panic());
            }
            println!(
                "{} reading the saved sessions was cancelled: {err}",
                "✗".red()
            );
            return;
        }
    };
    let (files_read, counts) = counts;

    if files_read == 0 {
        println!(
            "{}",
            "no saved sessions to learn from — this command reads your own history, so run a few turns first"
                .dimmed()
        );
        return;
    }

    let key = state.workspace.to_string_lossy().to_string();
    let granted = state.approval_grants.grants_for(&key);
    let mut granted: Vec<String> = granted.to_vec();
    granted.extend(state.approval_session_approved.iter().cloned());
    let rules = suggestions(counts, &granted);

    if rules.is_empty() {
        println!(
            "{}",
            format!(
                "nothing to propose — read {files_read} session(s): every read-only command run \
                 {MIN_RUNS}+ times is already allowed, and nothing else reached {MIN_RUNS} runs"
            )
            .dimmed()
        );
        println!(
            "{}",
            "  /permissions list shows what is in force · /permissions allow <prefix> adds one by hand"
                .dimmed()
        );
        return;
    }

    println!(
        "{}",
        format!(
            "{} rule(s) from {files_read} session(s) — read-only commands you run often (same key \
             the approval prompt uses):",
            rules.len()
        )
        .bold()
    );
    for suggestion in &rules {
        println!(
            "  {:<26} {:>4} run(s)   e.g. {}",
            suggestion.key,
            suggestion.runs,
            crate::tool_summary::summarize_tool_output(&suggestion.example)
        );
    }

    if !apply {
        println!(
            "{}",
            format!(
                "nothing written — re-run `/fewer-permission-prompts --yes` to save all {} as \
                 approval grants in ~/.codesmith/approvals.toml, or /permissions allow one prefix",
                rules.len()
            )
            .dimmed()
        );
        return;
    }

    let prefixes: Vec<&str> = rules.iter().map(|rule| rule.prefix.as_str()).collect();
    permission_cmds::allow_many(state, &prefixes);
}
#[cfg(test)]
mod tests {
    use super::*;

    fn runs(commands: &[&str]) -> HashMap<String, (Vec<String>, u32)> {
        tally(
            &commands
                .iter()
                .map(|c| (*c).to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn proposals_follow_frequency_and_the_safety_authority() {
        let counts = tally(&[
            // Read-only and frequent: the point of the command.
            "git status".to_string(),
            "git status -s".to_string(),
            "git status".to_string(),
            // Read-only but rare: not worth a rule yet.
            "ls -la".to_string(),
            // Frequent, and whatever the safety authority says it is. The
            // assertion below derives the expectation from that authority
            // rather than from a second opinion written here.
            "cargo test".to_string(),
            "cargo test".to_string(),
            "cargo test --lib".to_string(),
            // Catastrophic, and frequent: never proposed.
            "rm -rf build".to_string(),
            "rm -rf build".to_string(),
            "rm -rf build".to_string(),
        ]);

        let proposed = suggestions(counts.clone(), &[]);
        let keys: Vec<&str> = proposed.iter().map(|s| s.key.as_str()).collect();
        assert!(keys.contains(&"shell:git status"), "{keys:?}");
        assert!(
            !keys.contains(&"shell:ls -la"),
            "a command seen once is not friction worth a rule: {keys:?}"
        );

        for (key, (examples, runs)) in &counts {
            let expected = *runs >= MIN_RUNS
                && examples
                    .iter()
                    .all(|example| analyze_command(example).level == SafetyLevel::Safe);
            assert_eq!(
                keys.contains(&key.as_str()),
                expected,
                "{key} (ran {runs}x, examples {examples:?})"
            );
        }

        let git = proposed
            .iter()
            .find(|s| s.key == "shell:git status")
            .expect("the git rule");
        assert_eq!(git.prefix, "git status", "what /permissions allow takes");
        assert_eq!(git.runs, 3);
        assert_eq!(git.example, "git status");
    }

    #[test]
    fn what_is_already_granted_is_not_proposed_again() {
        let counts = runs(&["git status", "git status", "git status"]);
        assert_eq!(suggestions(counts.clone(), &[]).len(), 1);
        assert!(
            suggestions(counts, &["shell:git status".to_string()]).is_empty(),
            "an existing grant is why there is no prompt left to remove"
        );
    }

    /// A family whose first-seen example is read-only but that also
    /// contains a non-Safe spelling must not be proposed: the `--yes` rule
    /// covers every variant, so the first example must not vouch for the
    /// rest. This is the shape the old single-example tally got wrong.
    #[test]
    fn one_unsafe_variant_disqualifies_the_family() {
        let mut counts: HashMap<String, (Vec<String>, u32)> = HashMap::new();
        counts.insert(
            "shell:read-only family".to_string(),
            (vec!["ls".to_string(), "rm -rf build".to_string()], 5),
        );
        // Expectation derives from the same authority the filter uses.
        let all_safe = ["ls", "rm -rf build"]
            .iter()
            .all(|example| analyze_command(example).level == SafetyLevel::Safe);
        assert_eq!(
            suggestions(counts, &[]).is_empty(),
            !all_safe,
            "a family with one non-Safe variant must not be proposed"
        );
    }

    #[test]
    fn the_safety_check_is_what_keeps_dangerous_commands_out() {
        let counts = runs(&["git push --force", "git push --force", "git push --force"]);
        // Whether this is proposed must follow the same authority the
        // approval prompt uses, not a second opinion written here.
        let proposed = suggestions(counts, &[]);
        let dangerous_is_safe = analyze_command("git push --force").level == SafetyLevel::Safe;
        assert_eq!(proposed.is_empty(), !dangerous_is_safe);
    }
}
