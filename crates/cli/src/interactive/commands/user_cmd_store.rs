//! User-defined slash-command loading and templates (moved from the
//! retired `commands/user_commands.rs`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Scan every candidate commands directory and return merged
/// `(name, content, from_user_global)` triples. Workspace-local directories
/// shadow user-global by name — the first occurrence of a name wins.
///
/// `from_user_global` marks the entry's scope so trust decisions (the
/// `allowed-tools` frontmatter grants) can distinguish the user's own
/// `~/.codesmith/commands` from files that shipped with a possibly
/// untrusted repository.
///
/// Pass `None` for the workspace to scan only the global directory
/// (backward-compatible with callers that don't have workspace context).
pub(crate) fn load_user_commands(workspace: Option<&Path>) -> Vec<(String, String, bool)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut commands: Vec<(String, String, bool)> = Vec::new();

    for (dir, from_user_global) in commands_dirs(workspace) {
        for (name, content, dir_is_user_global) in load_commands_from_dir(&dir, from_user_global) {
            if seen.insert(name.clone()) {
                commands.push((name, content, dir_is_user_global));
            }
        }
    }

    // Sort by name for deterministic ordering.
    commands.sort_by(|a, b| a.0.cmp(&b.0));
    commands
}

/// Just the command names, for `/help` and Tab completion (the loader above
/// is the single scanner for these directories).
pub(crate) fn user_command_names(workspace: Option<&Path>) -> Vec<String> {
    load_user_commands(workspace)
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

/// Return all candidate commands directories in precedence order, with
/// whether each is user-global (trusted) scope.
fn commands_dirs(workspace: Option<&Path>) -> Vec<(PathBuf, bool)> {
    let mut dirs = Vec::new();
    if let Some(ws) = workspace {
        dirs.push((ws.join(".codesmith").join("commands"), false));
        dirs.push((ws.join(".claude").join("commands"), false));
        dirs.push((ws.join(".cursor").join("commands"), false));
    }
    // `~` is a shell expansion `std::fs` never performs: the literal-tilde
    // fallback silently resolved to a directory named `~` in the CWD. No
    // resolvable home means no global commands, loudly skipped.
    if let Some(dir) = global_commands_dir() {
        dirs.push((dir, true));
    }
    dirs
}

/// Path to the global user commands directory: `~/.codesmith/commands/`.
fn global_commands_dir() -> Option<PathBuf> {
    codesmith_config::codesmith_home()
        .ok()
        .map(|home| home.join("commands"))
}

/// Scan a single commands directory for `.md` files and return
/// `(name, content, from_user_global)` triples. Unreadable directories and
/// files are logged and skipped — a command that silently vanished from
/// `/help` otherwise had nothing to debug with.
fn load_commands_from_dir(dir: &Path, from_user_global: bool) -> Vec<(String, String, bool)> {
    let mut commands: Vec<(String, String, bool)> = Vec::new();

    if !dir.is_dir() {
        return commands;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            crate::logging::warn(format!(
                "user commands: cannot scan {}: {err}",
                dir.display()
            ));
            return commands;
        }
    };

    // Deterministic order: same-directory case-collisions (`Foo.md` vs
    // `foo.md` map to one command name) otherwise resolved by whatever
    // order `read_dir` happened to return.
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) => stem.to_lowercase(),
            None => continue,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(err) => {
                crate::logging::warn(format!(
                    "user commands: cannot read {}: {err}",
                    path.display()
                ));
                continue;
            }
        };
        commands.push((stem, content, from_user_global));
    }

    commands
}

pub(crate) fn parse_frontmatter(content: &str) -> (Vec<(String, String)>, &str) {
    let Some(first_line_end) = content.find('\n') else {
        return (Vec::new(), content);
    };
    let first = content[..first_line_end].trim_end_matches('\r');

    if first.trim().chars().all(|ch| ch == '-') && first.trim().len() >= 3 {
        let mut metadata = Vec::new();
        let mut offset = first_line_end + 1;
        for raw_line in content[offset..].split_inclusive('\n') {
            let line = raw_line.trim_end_matches(['\r', '\n']);
            offset += raw_line.len();
            let trimmed = line.trim();
            if trimmed.chars().all(|ch| ch == '-') && trimmed.len() >= 3 {
                let body = content[offset..].trim_start_matches(['\r', '\n']);
                return (metadata, body);
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_ascii_lowercase();
                let raw_value = value.trim();
                let value = if key == "allowed-tools" {
                    raw_value.to_string()
                } else {
                    strip_matched_quotes(raw_value).to_string()
                };
                if !key.is_empty() {
                    metadata.push((key, value));
                    continue;
                }
            }
            // A non-empty line that is neither `key: value` nor the closing
            // fence means the opening `---` was never properly closed: this
            // is not valid frontmatter, and the whole document is the
            // command body instead of silently becoming metadata.
            if !trimmed.is_empty() {
                return (Vec::new(), content);
            }
        }
        // Every line parsed as metadata but the fence never closed: same
        // verdict — not valid frontmatter, whole document is the body.
        return (Vec::new(), content);
    }

    (Vec::new(), content)
}

pub(crate) fn parse_allowed_tools(value: &str) -> Vec<String> {
    // The whole value is unquoted first so `"Bash, Read"` splits cleanly;
    // per-item stripping then also handles `"Bash", "Read"`.
    strip_matched_quotes(value.trim())
        .split(',')
        .map(|tool| {
            strip_matched_quotes(tool.trim())
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|tool| !tool.is_empty())
        .collect()
}

/// Substitute `$ARGUMENTS` (the raw argument string), `$1`, `$2`, …
/// (whitespace-separated positional words), and — for skill bodies, via
/// `skill_name` — `$SKILL_NAME` in a template.
///
/// One left-to-right pass, not sequential `str::replace`s: replacing `$1`
/// before `$10` corrupted ten-plus-argument templates, and each `replace`
/// rescans text inserted from user args, so an argument containing
/// `$ARGUMENTS` or `$2` was substituted again (user data must not be
/// re-parsed as template syntax).
pub(crate) fn expand_template(template: &str, args: &str, skill_name: Option<&str>) -> String {
    let positional: Vec<&str> = args.split_whitespace().collect();
    let mut out = String::with_capacity(template.len() + args.len());
    let mut rest = template;
    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        let tail = &rest[dollar + 1..];
        if let Some(after) = tail.strip_prefix("ARGUMENTS") {
            out.push_str(args);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("SKILL_NAME") {
            match skill_name {
                // User command templates have no skill name; the token stays
                // literal rather than being eaten.
                Some(name) => {
                    out.push_str(name);
                    rest = after;
                }
                None => {
                    out.push('$');
                    rest = tail;
                }
            }
        } else {
            let digits = tail.chars().take_while(char::is_ascii_digit).count();
            let positional_arg = (digits > 0)
                .then(|| tail[..digits].parse::<usize>().ok())
                .flatten()
                // `$0` is not a positional slot; it stays literal.
                .and_then(|index| index.checked_sub(1).and_then(|i| positional.get(i)));
            match positional_arg {
                // The longest digit run matches at once: `$10` never
                // collides with `$1`, and inserted arg text is never
                // rescanned, so args cannot inject new tokens.
                Some(arg) => {
                    out.push_str(arg);
                    rest = &tail[digits..];
                }
                None => {
                    out.push('$');
                    rest = tail;
                }
            }
        }
    }
    out.push_str(rest);
    out
}

/// Substitute `$ARGUMENTS` and `$1`, `$2`, ... placeholders in a user
/// command template.
pub(crate) fn apply_template(template: &str, args: &str) -> String {
    expand_template(template, args, None)
}

fn strip_matched_quotes(value: &str) -> &str {
    if let Some(stripped) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        return stripped;
    }
    if let Some(stripped) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return stripped;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_placeholders_take_the_longest_digit_run() {
        // `$1`-then-`$10` sequential replacement corrupted the tenth slot
        // into `<arg1>0`.
        let out = expand_template("run $1 $2 $10", "a b c d e f g h i j", None);
        assert_eq!(out, "run a b j");
        // Out-of-range and non-placeholder dollars stay literal.
        assert_eq!(expand_template("$3 $0 $x $$", "only", None), "$3 $0 $x $$");
    }

    #[test]
    fn arguments_are_not_reparsed_as_template_syntax() {
        // An argument containing `$ARGUMENTS` or `$2` must not be expanded
        // again after insertion.
        let out = expand_template("echo $1 and $2", "$ARGUMENTS $2", None);
        assert_eq!(out, "echo $ARGUMENTS and $2");
        let out = expand_template("$ARGUMENTS", "pay $100 now", None);
        assert_eq!(out, "pay $100 now");
    }

    #[test]
    fn skill_names_substitute_and_user_templates_keep_the_token() {
        assert_eq!(expand_template("$SKILL_NAME", "", Some("review")), "review");
        assert_eq!(expand_template("$SKILL_NAME", "", None), "$SKILL_NAME");
    }

    #[test]
    fn unclosed_frontmatter_is_the_whole_body() {
        let content = "---\ndescription: half open\nRun: this is body\n";
        let (metadata, body) = parse_frontmatter(content);
        assert!(metadata.is_empty(), "no closing fence means no metadata");
        assert_eq!(body, content);
    }

    #[test]
    fn quoted_tool_lists_lose_their_quotes() {
        assert_eq!(
            parse_allowed_tools("\"Bash, Read\""),
            vec!["bash".to_string(), "read".to_string()]
        );
        assert_eq!(
            parse_allowed_tools("\"Bash\", 'read', Write"),
            vec!["bash".to_string(), "read".to_string(), "write".to_string()]
        );
    }
}
