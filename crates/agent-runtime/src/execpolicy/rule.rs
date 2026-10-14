use super::decision::Decision;
use super::error::Error;
use super::error::Result;
use serde::Deserialize;
use serde::Serialize;
use shlex::try_join;
use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

/// Matches a single command token, either a fixed string or one of several allowed alternatives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatternToken {
    Single(String),
    Alts(Vec<String>),
}

impl PatternToken {
    fn matches(&self, token: &str) -> bool {
        match self {
            Self::Single(expected) => expected == token,
            Self::Alts(alternatives) => alternatives.iter().any(|alt| alt == token),
        }
    }

    pub fn alternatives(&self) -> &[String] {
        match self {
            Self::Single(expected) => std::slice::from_ref(expected),
            Self::Alts(alternatives) => alternatives,
        }
    }
}

/// Prefix matcher for commands with support for alternative match tokens.
/// First token is fixed since we key by the first token in policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrefixPattern {
    first: Arc<str>,
    rest: Arc<[PatternToken]>,
}

impl PrefixPattern {
    /// Build a pattern, rejecting states that could never match a
    /// shell-split command: an empty first token, an empty alternative
    /// list, or empty/blank alternatives would silently fail closed at
    /// match time, so they are surfaced as `Error::InvalidPattern` here.
    pub fn new(first: &str, rest: impl Into<Arc<[PatternToken]>>) -> Result<Self> {
        if first.is_empty() {
            return Err(Error::InvalidPattern(
                "pattern first token cannot be empty".to_string(),
            ));
        }
        let rest = rest.into();
        for token in rest.iter() {
            if let PatternToken::Alts(alternatives) = token
                && (alternatives.is_empty()
                    || alternatives
                        .iter()
                        .any(|alternative| alternative.trim().is_empty()))
            {
                return Err(Error::InvalidPattern(
                    "pattern alternatives cannot be empty or blank".to_string(),
                ));
            }
        }
        Ok(Self {
            first: Arc::from(first),
            rest,
        })
    }

    pub fn matches_prefix(&self, cmd: &[String]) -> Option<Vec<String>> {
        if !self.is_prefix_match(cmd) {
            return None;
        }
        Some(cmd[..self.rest.len() + 1].to_vec())
    }

    /// [`PrefixPattern::matches_prefix`] as a membership check, without
    /// building the matched-prefix `Vec` — for callers that only need the
    /// boolean (example validation).
    pub fn is_prefix_match(&self, cmd: &[String]) -> bool {
        let pattern_length = self.rest.len() + 1;
        if cmd.len() < pattern_length || cmd[0] != self.first.as_ref() {
            return false;
        }
        self.rest
            .iter()
            .zip(&cmd[1..pattern_length])
            .all(|(pattern_token, cmd_token)| pattern_token.matches(cmd_token))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleMatch {
    PrefixRuleMatch {
        #[serde(rename = "matchedPrefix")]
        matched_prefix: Vec<String>,
        decision: Decision,
        /// Optional rationale for why this rule exists.
        ///
        /// This can be supplied for any decision and may be surfaced in different contexts
        /// (e.g., prompt reasons or rejection messages).
        #[serde(skip_serializing_if = "Option::is_none")]
        justification: Option<String>,
    },
    HeuristicsRuleMatch {
        command: Vec<String>,
        decision: Decision,
    },
}

impl RuleMatch {
    pub fn decision(&self) -> Decision {
        match self {
            Self::PrefixRuleMatch { decision, .. } => *decision,
            Self::HeuristicsRuleMatch { decision, .. } => *decision,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrefixRule {
    pub pattern: PrefixPattern,
    pub decision: Decision,
    pub justification: Option<String>,
}

pub trait Rule: Any + Debug + Send + Sync {
    fn program(&self) -> &str;

    fn matches(&self, cmd: &[String]) -> Option<RuleMatch>;

    /// Membership check for callers that discard the [`RuleMatch`]: the
    /// default still allocates via [`Rule::matches`]; implementations with
    /// a cheaper check (e.g. [`PrefixRule`]) should override it.
    fn is_match(&self, cmd: &[String]) -> bool {
        self.matches(cmd).is_some()
    }
}

pub type RuleRef = Arc<dyn Rule>;

impl Rule for PrefixRule {
    fn program(&self) -> &str {
        self.pattern.first.as_ref()
    }

    fn matches(&self, cmd: &[String]) -> Option<RuleMatch> {
        self.pattern
            .matches_prefix(cmd)
            .map(|matched_prefix| RuleMatch::PrefixRuleMatch {
                matched_prefix,
                decision: self.decision,
                justification: self.justification.clone(),
            })
    }

    fn is_match(&self, cmd: &[String]) -> bool {
        self.pattern.is_prefix_match(cmd)
    }
}

/// Count how many rules match each provided example and error if any example is unmatched.
pub fn validate_match_examples(rules: &[RuleRef], matches: &[Vec<String>]) -> Result<()> {
    let mut unmatched_examples = Vec::new();

    for example in matches {
        if rules.iter().any(|rule| rule.is_match(example)) {
            continue;
        }

        unmatched_examples.push(
            try_join(example.iter().map(String::as_str))
                .unwrap_or_else(|_| "unable to render example".to_string()),
        );
    }

    if unmatched_examples.is_empty() {
        Ok(())
    } else {
        Err(Error::ExampleDidNotMatch {
            // Rule program keys only (bounded to the first few plus a
            // count): the full `Debug` payload embedded pattern tokens and
            // multi-line justifications, dumping the entire policy into
            // logs for a large ruleset.
            rules: bounded_rule_summaries(rules),
            examples: unmatched_examples,
        })
    }
}

/// Render a bounded summary of the rule set: the first few program keys and
/// the count of the remainder.
fn bounded_rule_summaries(rules: &[RuleRef]) -> Vec<String> {
    const MAX_LISTED: usize = 8;
    let mut summaries: Vec<String> = rules
        .iter()
        .take(MAX_LISTED)
        .map(|rule| rule.program().to_string())
        .collect();
    let remaining = rules.len().saturating_sub(MAX_LISTED);
    if remaining > 0 {
        summaries.push(format!("(+{remaining} more rules)"));
    }
    summaries
}

/// Ensure that no rule matches any provided negative example.
pub fn validate_not_match_examples(rules: &[RuleRef], not_matches: &[Vec<String>]) -> Result<()> {
    for example in not_matches {
        if let Some(rule) = rules.iter().find(|rule| rule.is_match(example)) {
            return Err(Error::ExampleDidMatch {
                rule: format!("{rule:?}"),
                example: try_join(example.iter().map(String::as_str))
                    .unwrap_or_else(|_| "unable to render example".to_string()),
            });
        }
    }

    Ok(())
}
