use super::decision::Decision;
use super::error::Error;
use super::error::Result;
use super::rule::PatternToken;
use super::rule::PrefixPattern;
use super::rule::PrefixRule;
use super::rule::RuleMatch;
use super::rule::RuleRef;
use multimap::MultiMap;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;

type HeuristicsFallback<'a> = Option<&'a dyn Fn(&[String]) -> Decision>;

#[derive(Clone, Debug)]
pub struct Policy {
    rules_by_program: MultiMap<String, RuleRef>,
}

impl Policy {
    pub fn new(rules_by_program: MultiMap<String, RuleRef>) -> Self {
        Self { rules_by_program }
    }

    pub fn empty() -> Self {
        Self::new(MultiMap::new())
    }

    pub fn rules(&self) -> &MultiMap<String, RuleRef> {
        &self.rules_by_program
    }

    pub fn add_prefix_rule(&mut self, prefix: &[String], decision: Decision) -> Result<()> {
        let (first_token, rest) = prefix
            .split_first()
            .ok_or_else(|| Error::InvalidPattern("prefix cannot be empty".to_string()))?;

        let rule: RuleRef = Arc::new(PrefixRule {
            pattern: PrefixPattern::new(
                first_token.as_str(),
                rest.iter()
                    .map(|token| PatternToken::Single(token.clone()))
                    .collect::<Vec<_>>(),
            )?,
            decision,
            justification: None,
        });

        self.rules_by_program.insert(first_token.clone(), rule);
        Ok(())
    }

    pub fn check<F>(&self, cmd: &[String], heuristics_fallback: &F) -> Evaluation
    where
        F: Fn(&[String]) -> Decision,
    {
        let matched_rules = self.matches_for_command(cmd, Some(heuristics_fallback));
        Evaluation::from_matches(matched_rules)
    }

    /// Checks multiple commands and aggregates the results.
    pub fn check_multiple<Commands, F>(
        &self,
        commands: Commands,
        heuristics_fallback: &F,
    ) -> Evaluation
    where
        Commands: IntoIterator,
        Commands::Item: AsRef<[String]>,
        F: Fn(&[String]) -> Decision,
    {
        let matched_rules: Vec<RuleMatch> = commands
            .into_iter()
            .flat_map(|command| {
                self.matches_for_command(command.as_ref(), Some(heuristics_fallback))
            })
            .collect();

        Evaluation::from_matches(matched_rules)
    }

    /// Returns matching rules for the given command. If no rules match and
    /// `heuristics_fallback` is provided, returns a single
    /// `HeuristicsRuleMatch` with the decision rendered by
    /// `heuristics_fallback`.
    ///
    /// If `heuristics_fallback.is_some()`, then the returned vector is
    /// guaranteed to be non-empty.
    pub fn matches_for_command(
        &self,
        cmd: &[String],
        heuristics_fallback: HeuristicsFallback<'_>,
    ) -> Vec<RuleMatch> {
        let matched_rules: Vec<RuleMatch> = match cmd.first() {
            Some(first) => self
                .rules_by_program
                .get_vec(first)
                .map(|rules| rules.iter().filter_map(|rule| rule.matches(cmd)).collect())
                .unwrap_or_default(),
            None => Vec::new(),
        };

        if matched_rules.is_empty()
            && let Some(heuristics_fallback) = heuristics_fallback
        {
            vec![RuleMatch::HeuristicsRuleMatch {
                command: cmd.to_vec(),
                decision: heuristics_fallback(cmd),
            }]
        } else {
            matched_rules
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub decision: Decision,
    #[serde(rename = "matchedRules")]
    pub matched_rules: Vec<RuleMatch>,
}

impl Evaluation {
    pub fn is_match(&self) -> bool {
        self.matched_rules
            .iter()
            .any(|rule_match| !matches!(rule_match, RuleMatch::HeuristicsRuleMatch { .. }))
    }

    fn from_matches(matched_rules: Vec<RuleMatch>) -> Self {
        // `check_multiple` aggregates per-command matches and cannot
        // uphold a non-empty invariant for an empty command iterator:
        // fail closed instead of panicking on ordinary input.
        let decision = matched_rules
            .iter()
            .map(RuleMatch::decision)
            .reduce(Decision::most_restrictive)
            .unwrap_or(Decision::Forbidden);

        Self {
            decision,
            matched_rules,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_multiple_with_no_commands_fails_closed() {
        // An empty command iterator aggregates zero matches; the reducer
        // used to `expect` and panic on this ordinary input.
        let policy = Policy::empty();
        let evaluation = policy.check_multiple(Vec::<Vec<String>>::new(), &|_| Decision::Allow);
        assert_eq!(evaluation.decision, Decision::Forbidden);
    }
}
