//! Tab completion and the inline hint for the line REPL.
//!
//! rustyline consults a helper only for Tab (`Cmd::Complete`) and for the
//! hint, and both callbacks run synchronously on the render thread: command
//! completion reads the static
//! [`registry`](super::commands::registry) table plus the user command names
//! loaded once at REPL start. The one exception is `@path` completion, which
//! consults a bounded cached workspace index ([`super::path_index`]) — the
//! walk is capped and rebuilt at most once per its TTL, never per keystroke.
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
//! - Argument completion covers the closed vocabularies declared in the
//!   registry's `args` column, plus `/provider` (read off the provider
//!   enum). Session ids, skills, presets, and task ids are not completed —
//!   those need a network call or a directory walk with no cache.
//! - Aliases are neither offered nor hinted (they still dispatch), so a
//!   migrated spelling such as `/new` completes only under its canonical
//!   name. Claude Code hints alias matches; this REPL teaches one name per
//!   command instead.
//! - Command completion in the middle of a prompt (`run the tests then /com`)
//!   matches on a plain name prefix, so a path like `/tmp/notes.md` offers
//!   nothing and keeps no list open.

use std::borrow::Cow;
use std::sync::Mutex;

use rustyline::Context;
use rustyline::Helper;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};

use crate::config::ApiProvider;

use super::bindings::{self, Bindings};
use super::commands::registry::{Command, CommandInfo};
use super::path_index::PathIndex;

/// The helper attached to the REPL's rustyline editor.
///
/// Besides completion it carries the shared [`Bindings`] cell — which it
/// keeps fed with the line text, so a key that discards the buffer can still
/// be recalled — and the session's todo list, so `Ctrl+T` can report a live
/// summary in the hint.
pub struct ReplHelper {
    /// Names from `.codesmith/commands/*.md` (workspace, then user-global),
    /// loaded once when the REPL starts.
    user_commands: Vec<String>,
    /// Canonical builtin names disabled via `[commands] disabled`
    /// (snapshotted once at REPL start). Mod-sourced disables are read
    /// live off the runner — see [`Self::disabled_builtin_names`].
    disabled: Vec<String>,
    /// The extension runner, for live mod-command completion — mods
    /// activate/disable mid-session, so their command names are snapshotted
    /// per keystroke instead of once at startup.
    extension_runner: Option<std::sync::Arc<codesmith_extensions::ExtensionRunner>>,
    /// `@path` candidates. Behind a `Mutex` because completion takes `&self`
    /// and the index rebuilds lazily; the lock is only ever held for one
    /// bounded lookup.
    paths: Mutex<PathIndex>,
    bindings: bindings::Shared,
    todos: Option<codesmith_agent_runtime::tool_state::todo::SharedTodoList>,
}

impl ReplHelper {
    pub fn new(
        user_commands: Vec<String>,
        disabled: Vec<String>,
        extension_runner: Option<std::sync::Arc<codesmith_extensions::ExtensionRunner>>,
        paths: PathIndex,
        bindings: bindings::Shared,
        todos: Option<codesmith_agent_runtime::tool_state::todo::SharedTodoList>,
    ) -> Self {
        Self {
            user_commands,
            disabled,
            extension_runner,
            paths: Mutex::new(paths),
            bindings,
            todos,
        }
    }

    /// User + live mod command names (mod commands dispatch between user
    /// commands and the static table, so they complete too).
    fn command_name_pool(&self) -> Vec<String> {
        let mut names = self.user_commands.clone();
        if let Some(runner) = &self.extension_runner {
            names.extend(runner.bound_command_names());
        }
        names
    }

    /// Builtin names disabled right now: the config snapshot plus live mod
    /// registrations (mods activate/disable mid-session, the same reasoning
    /// as mod-command completion).
    fn disabled_builtin_names(&self) -> Vec<String> {
        let mut names = self.disabled.clone();
        if let Some(runner) = &self.extension_runner {
            names.extend(runner.disabled_builtin_commands());
        }
        names
    }

    /// Completion inside a `!` line: workspace paths first, then the `!`
    /// commands this project has run before.
    ///
    /// Claude Code documents both for shell mode ("live file path
    /// autocomplete" and "history-based autocomplete"), and they answer
    /// different questions: the path list says what exists, the history says
    /// what you ran last time.
    fn complete_shell(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> Option<(usize, Vec<Pair>)> {
        if !line.trim_start().starts_with('!') {
            return None;
        }
        let (start, word) = word_at(line, pos);
        if word.is_empty() {
            return None;
        }
        // A word with a separator is a path (`.` hides it: `!ls .` should
        // still offer history).
        if word.contains('/') {
            let Ok(mut index) = self.paths.lock() else {
                return None;
            };
            let candidates: Vec<Pair> = index
                .complete(word)
                .into_iter()
                .map(|entry| {
                    let replacement = if entry.is_dir {
                        format!("{}/", entry.path)
                    } else {
                        format!("{} ", entry.path)
                    };
                    Pair {
                        display: entry.path,
                        replacement,
                    }
                })
                .collect();
            return (!candidates.is_empty()).then_some((start, candidates));
        }
        let candidates = shell_history_candidates(ctx, word);
        (!candidates.is_empty()).then_some((start, candidates))
    }

    /// `@path` candidates for the word at the cursor, or `None` when the
    /// word is not an `@` mention.
    fn complete_mention(&self, line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
        let (start, word) = word_at(line, pos);
        let partial = word.strip_prefix('@')?;
        let Ok(mut index) = self.paths.lock() else {
            return Some((start, Vec::new()));
        };
        let candidates = index
            .complete(partial)
            .into_iter()
            .map(|entry| {
                // A directory keeps the cursor inside it (`@src/` then Tab
                // again); a file takes a trailing space.
                let (replacement, display) = if entry.is_dir {
                    (format!("@{}/", entry.path), format!("@{}/", entry.path))
                } else {
                    (format!("@{} ", entry.path), format!("@{}", entry.path))
                };
                Pair {
                    display,
                    replacement,
                }
            })
            .collect();
        Some((start, candidates))
    }

    /// `@path` hint: how many candidates the mention has.
    fn mention_hint(&self, line: &str, pos: usize) -> Option<String> {
        let (_, word) = word_at(line, pos);
        let partial = word.strip_prefix('@')?;
        if partial.is_empty() {
            return None;
        }
        let count = self.paths.lock().ok()?.complete(partial).len();
        match count {
            0 => None,
            1 => Some("  Tab completes".to_string()),
            many => Some(format!("  {many} paths · Tab Tab lists them")),
        }
    }
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // A `!` line is owned by shell completion (paths, then history):
        // consulting command completion first made `!ls /mo` offer `/mode`,
        // `/model`, … — Tab inserted a command name into a shell line.
        // `complete_shell` returns `None` immediately for non-`!` lines, so
        // trying it first preserves behavior everywhere else.
        if let Some(shell) = self.complete_shell(line, pos, ctx) {
            return Ok(shell);
        }
        let pool = self.command_name_pool();
        let disabled = self.disabled_builtin_names();
        let (start, pairs) = complete_at(line, pos, &pool, &disabled);
        if !pairs.is_empty() {
            return Ok((start, pairs));
        }
        if let Some(mention) = self.complete_mention(line, pos) {
            return Ok(mention);
        }
        Ok((start, pairs))
    }
}

impl Hinter for ReplHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        // Every keystroke passes through here before the line is redrawn:
        // this is where the shared cell keeps its copy of the buffer.
        Bindings::lock(&self.bindings).note_line(line);
        // `!` lines get no hint: the command hint would ghost command text
        // ("de — …") into `!ls /mo` and mislead with "no command matches"
        // on `!ls /ho`.
        if line.trim_start().starts_with('!') {
            return None;
        }
        let pool = self.command_name_pool();
        let disabled = self.disabled_builtin_names();
        hint_at(line, pos, &pool, &disabled)
            .or_else(|| self.mention_hint(line, pos))
            .or_else(|| self.status_hint(line))
    }
}

impl ReplHelper {
    /// State the prompt can act on that has no other visible affordance
    /// (a stashed draft, a cleared draft, the task list).
    fn status_hint(&self, line: &str) -> Option<String> {
        let summary = self.todos.as_ref().and_then(todo_summary);
        Bindings::lock(&self.bindings).status_hint(line, summary.as_deref())
    }
}

/// `tasks 3/7 done · 1 in progress`, or `None` when there is no list.
///
/// Best-effort by construction: the hint callback runs synchronously on the
/// render thread and the list is guarded by an async mutex, so a busy lock
/// simply yields no summary for that keystroke.
fn todo_summary(
    todos: &codesmith_agent_runtime::tool_state::todo::SharedTodoList,
) -> Option<String> {
    use codesmith_agent_runtime::tool_state::todo::TodoStatus;
    let list = todos.try_lock().ok()?;
    let snapshot = list.snapshot();
    if snapshot.items.is_empty() {
        return None;
    }
    let done = snapshot
        .items
        .iter()
        .filter(|item| item.status == TodoStatus::Completed)
        .count();
    let running = snapshot
        .items
        .iter()
        .filter(|item| item.status == TodoStatus::InProgress)
        .count();
    let total = snapshot.items.len();
    let mut summary = format!("tasks {done}/{total} done");
    if running > 0 {
        summary.push_str(&format!(" · {running} in progress"));
    }
    Some(summary)
}

/// Multiline input rule: a line ending in `\` continues on the next line
/// (Claude Code's quick escape — it works in every terminal, unlike
/// Shift+Enter). Enter then inserts a newline instead of submitting.
///
/// The `\` is a marker, not content: [`strip_line_continuations`] removes it
/// before the message is sent.
#[must_use]
pub(crate) fn validate_input(input: &str) -> ValidationResult {
    if input.ends_with('\\') {
        ValidationResult::Incomplete
    } else {
        ValidationResult::Valid(None)
    }
}

/// Drop the `\` that [`validate_input`] used as a continuation marker.
/// A backslash kept for its own sake survives when it is not at the end of a
/// line, so `C:\path\` mid-message is untouched.
#[must_use]
pub(crate) fn strip_line_continuations(input: &str) -> String {
    // The CRLF form first (a `\` before a pasted `\r\n` pair), else the
    // inner `\r` shields the `\n` from the LF-only replace and the marker
    // backslash survives into the sent message.
    input.replace("\\\r\n", "\n").replace("\\\n", "\n")
}

impl Validator for ReplHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> rustyline::Result<ValidationResult> {
        Ok(validate_input(ctx.input()))
    }
}

// The helper exists for completion, the hint, and multiline validation; the
// line is not re-highlighted.
impl Highlighter for ReplHelper {
    /// The status row is the prompt's first row, so a mode switch redraws it
    /// in place: this is the only hook that sees the prompt string on every
    /// repaint, and the shared cell holds the row plus the mode `Shift+Tab`
    /// has cycled to. A changed prompt comes back owned; everything else,
    /// including rustyline's own one-row reverse-search prompt, is lent.
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        match Bindings::lock(&self.bindings).repainted_prompt(prompt) {
            Some(repainted) => Cow::Owned(repainted),
            None => Cow::Borrowed(prompt),
        }
    }
}
// rustyline 14 has no blanket impl, so the alias trait is spelled out.
impl Helper for ReplHelper {}

/// The word under the cursor: its byte offset in `line`, and its text.
fn word_at(line: &str, pos: usize) -> (usize, &str) {
    // Clamp to a char boundary first: `pos` can sit inside a multi-byte
    // character (CJK input). `rfind` returns the FIRST byte of the matched
    // whitespace, which for multi-byte whitespace (ideographic space
    // U+3000 etc.) is not `i + 1` — slicing there panicked on the render
    // thread.
    let mut end = pos.min(line.len());
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    let head = &line[..end];
    let start = head.rfind(char::is_whitespace).map_or(0, |i| {
        i + head[i..].chars().next().map_or(1, char::len_utf8)
    });
    (start, &line[start..end])
}

/// The argument vocabulary of a command: the registry's `args` column.
///
/// Empty means "not completed" rather than "no arguments". `/provider` is
/// the one vocabulary that is not written in the table — it is read off
/// [`ApiProvider::all`] so a new provider is completable without a second
/// edit.
fn arg_vocab(command: &str) -> Vec<&'static str> {
    if command == "provider" {
        return ApiProvider::all().iter().map(|p| p.as_str()).collect();
    }
    Command::resolve(command).map_or_else(Vec::new, |info| info.args.to_vec())
}

/// Command-name candidates for `partial` (the text after the `/`).
/// Disabled builtins are skipped; user and mod commands are not affected
/// by disables (they shadow the builtin layer, they are not it).
fn command_candidates(partial: &str, user_cmds: &[String], disabled: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Command::ALL
        .iter()
        .map(|info| info.name)
        .filter(|name| !disabled.iter().any(|d| d.as_str() == *name))
        .chain(user_cmds.iter().map(String::as_str))
        .filter(|name| name.starts_with(partial))
        .map(|name| format!("/{name}"))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Completion candidates for the cursor position, as `(replacement start,
/// candidates)`. Pure, so the behaviour is testable without a terminal.
///
/// A `/word` is completed as a command name wherever it sits — at the start
/// of the prompt or after a space mid-prompt, which is Claude Code's rule
/// (a slash inside a path such as `/tmp/notes.md` matches no command and so
/// opens no list).
pub(crate) fn complete_at(
    line: &str,
    pos: usize,
    user_cmds: &[String],
    disabled: &[String],
) -> (usize, Vec<Pair>) {
    let (start, word) = word_at(line, pos);

    if let Some(partial) = word.strip_prefix('/') {
        let names = command_candidates(partial, user_cmds, disabled);
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
        return (start, candidates);
    }

    // Later words: only the second word of a command with a closed
    // vocabulary is completed.
    if start == 0 || line[..start].split_whitespace().count() != 1 {
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

/// `!` commands from the session history whose command line starts with
/// `partial`, newest first and deduplicated.
///
/// Kept free of the helper so a test can pass any `History`.
fn shell_history_candidates(ctx: &Context<'_>, partial: &str) -> Vec<Pair> {
    use rustyline::history::SearchDirection;

    let history = ctx.history();
    let mut seen: Vec<String> = Vec::new();
    for index in (0..history.len()).rev() {
        let Ok(Some(found)) = history.get(index, SearchDirection::Forward) else {
            continue;
        };
        let entry = found.entry.trim();
        let Some(command) = entry.strip_prefix('!') else {
            continue;
        };
        let command = command.trim();
        if !command.starts_with(partial) {
            continue;
        }
        let command = command.to_string();
        if seen.contains(&command) {
            continue;
        }
        seen.push(command);
        if seen.len() >= 20 {
            break;
        }
    }
    seen.into_iter()
        .map(|command| Pair {
            display: command.clone(),
            replacement: format!("{command} "),
        })
        .collect()
}

/// The dim inline hint rendered after the cursor, or `None` when the line
/// carries nothing to say (plain prompts are never annotated).
pub(crate) fn hint_at(
    line: &str,
    pos: usize,
    user_cmds: &[String],
    disabled: &[String],
) -> Option<String> {
    let (start, word) = word_at(line, pos);

    if let Some(partial) = word.strip_prefix('/') {
        let builtin: Vec<&CommandInfo> = Command::ALL
            .iter()
            .filter(|info| !disabled.iter().any(|d| *d == info.name))
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

    if start == 0 || line[..start].split_whitespace().count() != 1 {
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

    fn helper(user_commands: Vec<String>) -> ReplHelper {
        ReplHelper::new(
            user_commands,
            Vec::new(),
            None,
            PathIndex::new(std::env::temp_dir(), 6, 128),
            Bindings::shared(),
            None,
        )
    }

    #[test]
    fn completes_command_names_after_a_slash() {
        let (start, pairs) = complete_at("/mo", 3, &[], &[]);
        assert_eq!(start, 0);
        assert_eq!(displays(&pairs), ["/mode", "/model", "/models", "/mods"]);
        // Many matches: the word is left as typed, no trailing space.
        assert_eq!(pairs[0].replacement, "/mode");

        let (start, pairs) = complete_at("/models", 7, &[], &[]);
        assert_eq!(start, 0);
        assert_eq!(pairs[0].replacement, "/models ");
    }

    #[test]
    fn completes_loaded_user_commands() {
        let user = vec!["zz-deploy".to_string()];
        let (_, pairs) = complete_at("/zz", 3, &user, &[]);
        assert_eq!(displays(&pairs), ["/zz-deploy"]);
        assert_eq!(pairs[0].replacement, "/zz-deploy ");
    }

    #[test]
    fn completes_a_command_mid_prompt() {
        // Claude Code's rule: `/` after a space, name prefix only.
        let line = "run the tests, then /com";
        let (start, pairs) = complete_at(line, line.len(), &[], &[]);
        assert_eq!(start, "run the tests, then ".len());
        assert_eq!(displays(&pairs), ["/compact"]);
        assert_eq!(pairs[0].replacement, "/compact ");

        // A path that is not a command keeps no list open.
        let line = "look at /tmp/notes.md";
        let (_, pairs) = complete_at(line, line.len(), &[], &[]);
        assert!(pairs.is_empty(), "{:?}", displays(&pairs));

        // Several matches mid-prompt leave the word for the list.
        let line = "then /mo";
        let (_, pairs) = complete_at(line, line.len(), &[], &[]);
        assert_eq!(displays(&pairs), ["/mode", "/model", "/models", "/mods"]);
    }

    #[test]
    fn completes_only_closed_argument_vocabularies() {
        let (start, pairs) = complete_at("/mode ", 6, &[], &[]);
        assert_eq!(start, 6);
        assert_eq!(
            displays(&pairs),
            ["agent", "plan", "yolo", "coordinator"],
            "offered in the documented order"
        );

        let (_, pairs) = complete_at("/model ", 7, &[], &[]);
        assert!(pairs.is_empty(), "live model names need the network");

        // The vocabularies come from the registry's `args` column.
        let (_, pairs) = complete_at("/trust ad", 9, &[], &[]);
        assert_eq!(displays(&pairs), ["add"]);
    }

    #[test]
    fn leaves_plain_text_alone() {
        assert!(complete_at("hello ", 6, &[], &[]).1.is_empty());
        assert!(complete_at("/mode plan extra", 16, &[], &[]).1.is_empty());
        assert!(hint_at("hello world", 11, &[], &[]).is_none());
    }

    #[test]
    fn hint_ghosts_a_unique_match_and_counts_many() {
        assert_eq!(
            hint_at("/hel", 4, &[], &[]).as_deref(),
            Some("p — this list")
        );
        assert_eq!(hint_at("/mode p", 7, &[], &[]).as_deref(), Some("lan"));
        assert_eq!(
            hint_at("/", 1, &[], &[]).as_deref(),
            Some(format!("  {} matches · Tab Tab lists all", Command::ALL.len()).as_str())
        );
        assert_eq!(
            hint_at("/zzz", 4, &[], &[]).as_deref(),
            Some("  no command matches — /help lists what exists")
        );
        // Mid-prompt hints follow the same policy.
        assert_eq!(
            hint_at("then /hel", 9, &[], &[]).as_deref(),
            Some("p — this list")
        );
    }

    #[test]
    fn disabled_builtins_leave_completion_and_hints() {
        let disabled = vec!["model".to_string(), "recap".to_string()];
        let (_, pairs) = complete_at("/mo", 3, &[], &disabled);
        assert_eq!(
            displays(&pairs),
            ["/mode", "/models", "/mods"],
            "disabled builtins are not candidates"
        );
        // /model itself is gone, so the unique-match ghost falls through to
        // /models (its own ghost text) instead of /model's.
        assert_eq!(
            hint_at("/model", 6, &[], &disabled).as_deref(),
            Some("s — list live models from the active endpoint")
        );
        // A prefix only the disabled command matches: nothing offered, the
        // same hint as an unknown word.
        let (_, pairs) = complete_at("/rec", 4, &[], &disabled);
        assert!(pairs.is_empty(), "{:?}", displays(&pairs));
        assert_eq!(
            hint_at("/recap", 6, &[], &disabled).as_deref(),
            Some("  no command matches — /help lists what exists")
        );
        // User commands of the same name are a different layer — unaffected.
        let user = vec!["model".to_string()];
        let (_, pairs) = complete_at("/mod", 4, &user, &disabled);
        assert_eq!(displays(&pairs), ["/mode", "/model", "/models", "/mods"]);
    }

    #[test]
    fn mention_completion_keeps_the_marker_and_flags_directories() {
        let dir = tempfile::tempdir().expect("temp workspace");
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        let helper = ReplHelper::new(
            Vec::new(),
            Vec::new(),
            None,
            PathIndex::new(dir.path().to_path_buf(), 6, 128),
            Bindings::shared(),
            None,
        );

        let line = "read @src/ma";
        let (start, pairs) = helper.complete_mention(line, line.len()).expect("mention");
        assert_eq!(start, "read ".len());
        assert_eq!(displays(&pairs), ["@src/main.rs"]);
        assert_eq!(pairs[0].replacement, "@src/main.rs ");

        let line = "read @sr";
        let (_, pairs) = helper.complete_mention(line, line.len()).expect("mention");
        assert_eq!(
            displays(&pairs),
            ["@src/", "@src/main.rs", "@src/nested/"],
            "a directory keeps the cursor inside it; files take a space"
        );
        assert_eq!(pairs[0].replacement, "@src/");
        assert_eq!(pairs[1].replacement, "@src/main.rs ");

        // Not a mention at all: the Completer must not offer paths.
        assert!(helper.complete_mention("read src/ma", 11).is_none());
    }

    #[test]
    fn completer_falls_through_to_mentions_but_not_the_other_way() {
        let helper = helper(vec!["zz-deploy".to_string()]);
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);

        let (_, pairs) = helper.complete("/mo", 3, &ctx).expect("command");
        assert_eq!(displays(&pairs), ["/mode", "/model", "/models", "/mods"]);

        let (_, pairs) = helper.complete("plain text", 10, &ctx).expect("none");
        assert!(pairs.is_empty());
    }

    #[test]
    fn shell_lines_complete_from_history_newest_first() {
        use rustyline::history::{DefaultHistory, History};

        let mut history = DefaultHistory::new();
        history.add("!cargo check").expect("add");
        history.add("!git status").expect("add");
        history.add("please run the tests").expect("add");
        history.add("!git status").expect("add");
        let ctx = Context::new(&history);

        let pairs = shell_history_candidates(&ctx, "git");
        assert_eq!(
            displays(&pairs),
            ["git status"],
            "duplicates collapse to one candidate"
        );
        assert_eq!(pairs[0].replacement, "git status ");

        let pairs = shell_history_candidates(&ctx, "");
        assert_eq!(
            displays(&pairs),
            ["git status", "cargo check"],
            "newest first"
        );
        assert!(
            shell_history_candidates(&ctx, "npm").is_empty(),
            "a command never run offers nothing"
        );
    }

    #[test]
    fn a_trailing_backslash_continues_the_prompt() {
        assert!(matches!(
            validate_input("explain this:\\"),
            ValidationResult::Incomplete
        ));
        assert!(matches!(
            validate_input("explain this"),
            ValidationResult::Valid(None)
        ));
        // Only the end of the whole input continues; an inner backslash is
        // content.
        assert!(matches!(
            validate_input("a\\b"),
            ValidationResult::Valid(None)
        ));
    }

    #[test]
    fn continuation_markers_are_stripped_before_sending() {
        assert_eq!(
            strip_line_continuations("first line\\\nsecond line"),
            "first line\nsecond line"
        );
        assert_eq!(
            strip_line_continuations("C:\\path\\file"),
            "C:\\path\\file",
            "backslashes inside a line are content"
        );
        assert_eq!(strip_line_continuations("no markers"), "no markers");
    }
}
