//! Tab completion and the inline hint for the line REPL.
//!
//! rustyline consults a helper only for Tab (`Cmd::Complete`), and both
//! callbacks run synchronously on the render thread: everything here has to
//! be cheap and local. The helper therefore reads the static
//! [`registry`](super::commands::registry) table plus the user command names
//! loaded once at REPL start — never [`ReplState`](super::state::ReplState),
//! the filesystem, or the network.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - No popup menu while typing. Feedback is the dim inline hint; one Tab
//!   completes to the longest common prefix, and a second Tab lists the
//!   candidates in columns (rustyline's own display).
//! - `Shift+Tab` no longer walks the candidates backwards: the REPL binds
//!   BackTab to its run-mode cycler, shadowing rustyline's default
//!   `Cmd::CompleteBackward` for it. `Tab`'s forward completion and the
//!   candidate list are unchanged.
//! - Argument completion covers only closed, static vocabularies
//!   (`/mode`, `/provider`, `/verbose`). Model names, sessions, skills,
//!   presets, and task ids are not completed — those need a network call or
//!   a directory walk, which cannot run in this callback.
//! - Aliases are neither offered nor hinted (they still dispatch), and
//!   `@file` mentions are extracted after Enter, not completed here.

use rustyline::Context;
use rustyline::Helper;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;

use crate::config::ApiProvider;

use super::commands::registry::{Command, CommandInfo};

/// The helper attached to the REPL's rustyline editor.
pub struct ReplHelper {
    /// Names from `.codesmith/commands/*.md` (workspace, then user-global),
    /// loaded once when the REPL starts.
    user_commands: Vec<String>,
}

impl ReplHelper {
    pub fn new(user_commands: Vec<String>) -> Self {
        Self { user_commands }
    }
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        Ok(complete_at(line, pos, &self.user_commands))
    }
}

impl Hinter for ReplHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        hint_at(line, pos, &self.user_commands)
    }
}

// The helper exists for completion and the hint; the line is not
// re-highlighted and there is no multiline validation.
impl Highlighter for ReplHelper {}
impl Validator for ReplHelper {}
// rustyline 14 has no blanket impl, so the alias trait is spelled out.
impl Helper for ReplHelper {}

/// The word under the cursor: its byte offset in `line`, and its text.
fn word_at(line: &str, pos: usize) -> (usize, &str) {
    let head = &line[..pos.min(line.len())];
    let start = head.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    (start, &line[start..head.len()])
}

/// The argument vocabulary of a command, when it is closed and static.
/// Empty means "not completed" rather than "no arguments".
fn arg_vocab(command: &str) -> Vec<&'static str> {
    match command {
        "mode" => vec!["agent", "plan", "yolo", "coordinator"],
        "verbose" => vec!["on", "off", "toggle"],
        "provider" => ApiProvider::all().iter().map(|p| p.as_str()).collect(),
        _ => Vec::new(),
    }
}

/// Completion candidates for the cursor position, as `(replacement start,
/// candidates)`. Pure, so the behaviour is testable without a terminal.
pub(crate) fn complete_at(line: &str, pos: usize, user_cmds: &[String]) -> (usize, Vec<Pair>) {
    let (start, word) = word_at(line, pos);

    if start == 0 {
        // First word: a slash-command name. Canonical names only — aliases
        // dispatch but would triple the candidate list.
        let Some(partial) = word.strip_prefix('/') else {
            return (0, Vec::new());
        };
        let mut names: Vec<String> = Command::ALL
            .iter()
            .map(|info| info.name)
            .chain(user_cmds.iter().map(String::as_str))
            .filter(|name| name.starts_with(partial))
            .map(|name| format!("/{name}"))
            .collect();
        names.sort_unstable();
        names.dedup();
        // A unique match takes a trailing space so arguments can follow;
        // a list leaves the word alone for further typing.
        let unique = names.len() == 1;
        let candidates = names
            .into_iter()
            .map(|display| {
                let replacement = if unique {
                    format!("{display} ")
                } else {
                    display.clone()
                };
                Pair {
                    display,
                    replacement,
                }
            })
            .collect();
        return (0, candidates);
    }

    // Later words: only the second word of a command with a closed
    // vocabulary is completed.
    if line[..start].split_whitespace().count() != 1 {
        return (start, Vec::new());
    }
    let command = line[..start]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('/');
    let candidates = arg_vocab(command)
        .into_iter()
        .filter(|value| value.starts_with(word))
        .map(|value| Pair {
            display: value.to_string(),
            replacement: format!("{value} "),
        })
        .collect();
    (start, candidates)
}

/// The dim inline hint rendered after the cursor, or `None` when the line
/// carries nothing to say (plain prompts are never annotated).
pub(crate) fn hint_at(line: &str, pos: usize, user_cmds: &[String]) -> Option<String> {
    if !line.starts_with('/') {
        return None;
    }
    let (start, word) = word_at(line, pos);

    if start == 0 {
        let partial = word.trim_start_matches('/');
        let builtin: Vec<&CommandInfo> = Command::ALL
            .iter()
            .filter(|info| info.name.starts_with(partial))
            .collect();
        let user: Vec<&str> = user_cmds
            .iter()
            .map(String::as_str)
            .filter(|name| name.starts_with(partial))
            .collect();
        let total = builtin.len() + user.len();
        if total == 0 {
            return Some("  no command matches — /help lists what exists".to_string());
        }
        if total == 1 {
            // Ghost text: the remainder of the name, then what it does.
            if let Some(info) = builtin.first() {
                return Some(format!("{} — {}", &info.name[partial.len()..], info.help));
            }
            if let Some(name) = user.first() {
                return Some(format!("{} — user command", &name[partial.len()..]));
            }
        }
        return Some(format!("  {total} matches · Tab Tab lists all"));
    }

    if line[..start].split_whitespace().count() != 1 {
        return None;
    }
    let command = line[..start]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('/');
    let matches: Vec<&str> = arg_vocab(command)
        .into_iter()
        .filter(|value| value.starts_with(word))
        .collect();
    match matches.as_slice() {
        [] => None,
        [only] if word.len() < only.len() => Some(only[word.len()..].to_string()),
        many => Some(format!("  {}", many.join(" "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn displays(pairs: &[Pair]) -> Vec<&str> {
        pairs.iter().map(|pair| pair.display.as_str()).collect()
    }

    #[test]
    fn completes_command_names_after_a_slash() {
        let (start, pairs) = complete_at("/mo", 3, &[]);
        assert_eq!(start, 0);
        assert_eq!(displays(&pairs), ["/mode", "/model", "/models", "/mods"]);
        // Many matches: the word is left as typed, no trailing space.
        assert_eq!(pairs[0].replacement, "/mode");

        let (start, pairs) = complete_at("/models", 7, &[]);
        assert_eq!(start, 0);
        assert_eq!(pairs[0].replacement, "/models ");
    }

    #[test]
    fn completes_loaded_user_commands() {
        let user = vec!["zz-deploy".to_string()];
        let (_, pairs) = complete_at("/zz", 3, &user);
        assert_eq!(displays(&pairs), ["/zz-deploy"]);
        assert_eq!(pairs[0].replacement, "/zz-deploy ");
    }

    #[test]
    fn completes_only_closed_argument_vocabularies() {
        let (start, pairs) = complete_at("/mode ", 6, &[]);
        assert_eq!(start, 6);
        assert_eq!(
            displays(&pairs),
            ["agent", "plan", "yolo", "coordinator"],
            "offered in the documented order"
        );

        let (_, pairs) = complete_at("/model ", 7, &[]);
        assert!(pairs.is_empty(), "live model names need the network");
    }

    #[test]
    fn leaves_plain_text_alone() {
        assert!(complete_at("hello ", 6, &[]).1.is_empty());
        assert!(complete_at("/mode plan extra", 16, &[]).1.is_empty());
        assert!(hint_at("hello /mo", 9, &[]).is_none());
    }

    #[test]
    fn hint_ghosts_a_unique_match_and_counts_many() {
        assert_eq!(hint_at("/hel", 4, &[]).as_deref(), Some("p — this list"));
        assert_eq!(hint_at("/mode p", 7, &[]).as_deref(), Some("lan"));
        assert_eq!(
            hint_at("/", 1, &[]).as_deref(),
            Some(format!("  {} matches · Tab Tab lists all", Command::ALL.len()).as_str())
        );
        assert_eq!(
            hint_at("/zzz", 4, &[]).as_deref(),
            Some("  no command matches — /help lists what exists")
        );
    }
}
