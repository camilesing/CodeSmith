use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use serde::Serialize;

use super::Decision;
use super::Policy;
use super::PolicyParser;
use super::RuleMatch;

/// Arguments for evaluating a command against one or more execpolicy files.
#[derive(Debug, Parser, Clone)]
pub struct ExecPolicyCheckCommand {
    /// Paths to execpolicy rule files to evaluate (repeatable).
    #[arg(short = 'r', long = "rules", value_name = "PATH", required = true)]
    pub rules: Vec<PathBuf>,

    /// Pretty-print the JSON output.
    #[arg(long)]
    pub pretty: bool,

    /// Command tokens to check against the policy. All options (e.g.
    /// `--pretty`) must appear before the command; anything after the first
    /// command token — including option-like strings such as `--force` — is
    /// absorbed into the command. Use `--` before commands that contain
    /// option-like tokens.
    #[arg(
        value_name = "COMMAND",
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub command: Vec<String>,
}

impl ExecPolicyCheckCommand {
    /// Load the policies for this command, evaluate the command, and render JSON output.
    ///
    /// This is a raw-match diagnostic tool, not a full evaluation: the
    /// heuristics fallback is disabled, so an unmatched command yields an
    /// empty `matchedRules` list and a `null` `decision` (unlike
    /// [`Policy::check`], which always produces a decision).
    pub fn run(&self) -> Result<()> {
        let policy = load_policies(&self.rules)?;
        // `None` disables the heuristics fallback: this tool reports raw
        // rule matches only, so an unmatched command yields no decision.
        let matched_rules = policy.matches_for_command(&self.command, None);

        let json = format_matches_json(&matched_rules, self.pretty)?;
        println!("{json}");

        Ok(())
    }
}

/// Render matched rules as the tool's JSON output.
///
/// Contract: `matchedRules` is always present, and `decision` is always
/// *serialized* — as the most restrictive matched decision, or `null` when
/// no rules match — so machine consumers never have to treat a missing key
/// as "no match".
pub fn format_matches_json(matched_rules: &[RuleMatch], pretty: bool) -> Result<String> {
    let output = ExecPolicyCheckOutput {
        matched_rules,
        decision: matched_rules
            .iter()
            .map(RuleMatch::decision)
            .reduce(Decision::most_restrictive),
    };

    if pretty {
        serde_json::to_string_pretty(&output).map_err(Into::into)
    } else {
        serde_json::to_string(&output).map_err(Into::into)
    }
}

pub fn load_policies(policy_paths: &[PathBuf]) -> Result<Policy> {
    let mut parser = PolicyParser::new();

    for policy_path in policy_paths {
        let policy_file_contents = fs::read_to_string(policy_path)
            .with_context(|| format!("failed to read policy at {}", policy_path.display()))?;
        let policy_identifier = policy_path.to_string_lossy().to_string();
        parser
            .parse(&policy_identifier, &policy_file_contents)
            .with_context(|| format!("failed to parse policy at {}", policy_path.display()))?;
    }

    Ok(parser.build())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecPolicyCheckOutput<'a> {
    #[serde(rename = "matchedRules")]
    matched_rules: &'a [RuleMatch],
    /// Serialized as `null` when no rules match, so consumers can rely on
    /// the key always being present.
    decision: Option<Decision>,
}
