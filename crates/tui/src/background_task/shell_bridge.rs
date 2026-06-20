//! Shell-to-registry bridge and stall watchdog.
//!
//! Provides default stall detection patterns (mirrors Claude Code's
//! `PROMPT_PATTERNS` in LocalShellTask.tsx) and utility functions
//! for bridging ShellManager background shells into the unified registry.

use regex::Regex;

use super::StallPattern;

/// Default stall patterns — mirrors Claude Code's `PROMPT_PATTERNS`.
///
/// These match shell output that suggests the command is blocked waiting
/// for keyboard input. Used to gate the stall notification: we stay silent
/// on commands that are merely slow (git log -S, long builds) and only
/// notify when the tail looks like an interactive prompt.
pub fn default_stall_patterns() -> Vec<StallPattern> {
    let patterns: &[(&str, &str)] = &[
        (r"(?i)\(y/n\)", "yes/no confirmation (y/n)"),
        (r"(?i)\[y/n\]", "yes/no confirmation [y/n]"),
        (r"(?i)\(yes/no\)", "yes/no confirmation"),
        (r"(?i)password\s*:", "password prompt"),
        (r"(?i)enter\s+password", "password entry"),
        (
            r"(?i)\b(?:do you|would you|shall i|are you sure|ready to)\b.*\?\s*$",
            "directed question",
        ),
        (r"(?i)press\s+(any key|enter)", "press key prompt"),
        (r"(?i)continue\?", "continue prompt"),
        (r"(?i)overwrite\?", "overwrite prompt"),
        (r"(?i)confirm\s*\?", "confirmation prompt"),
        (r"(?i)login\s*:", "login prompt"),
        (r"Username\s*:", "username prompt"),
    ];
    patterns
        .iter()
        .filter_map(|(pat, desc)| {
            Regex::new(pat).ok().map(|r| StallPattern {
                pattern: r,
                description: desc.to_string(),
            })
        })
        .collect()
}

/// Check if the tail of shell output looks like an interactive prompt.
/// Extracted as a standalone function so it can be used without the registry.
pub fn looks_like_prompt(tail: &str, patterns: &[StallPattern]) -> bool {
    let last_line = tail.trim_end().lines().last().unwrap_or("");
    patterns.iter().any(|p| p.pattern.is_match(last_line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn make_pattern(pat: &str) -> StallPattern {
        StallPattern {
            pattern: Regex::new(pat).expect("valid regex"),
            description: pat.to_string(),
        }
    }

    #[test]
    fn default_stall_patterns_compiles_all_regexes() {
        let patterns = default_stall_patterns();
        assert_eq!(patterns.len(), 12, "all stall patterns should compile");
    }

    #[test]
    fn looks_like_prompt_true_for_yes_no() {
        let patterns = default_stall_patterns();
        assert!(looks_like_prompt(
            "Do you want to continue? (y/n)",
            &patterns
        ));
    }

    #[test]
    fn looks_like_prompt_true_for_password_prompt() {
        let patterns = default_stall_patterns();
        assert!(looks_like_prompt("Password:", &patterns));
    }

    #[test]
    fn looks_like_prompt_false_for_normal_output() {
        let patterns = default_stall_patterns();
        assert!(!looks_like_prompt("Build complete", &patterns));
    }

    #[test]
    fn looks_like_prompt_false_for_empty_string() {
        let patterns = default_stall_patterns();
        assert!(!looks_like_prompt("", &patterns));
    }

    #[test]
    fn looks_like_prompt_uses_last_line_only() {
        let patterns = vec![make_pattern(r"\(y/n\)")];
        // (y/n) on first line, normal text on last line => false
        assert!(!looks_like_prompt("(y/n)\nBuild complete", &patterns));
        // normal text on first line, (y/n) on last line => true
        assert!(looks_like_prompt("Building...\nContinue? (y/n)", &patterns));
    }
}
