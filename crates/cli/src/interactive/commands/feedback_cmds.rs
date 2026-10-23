//! `/feedback up|down [N] [comment]` — rate one of this session's replies.
//!
//! The rating is a human signal about the model's output, and the rule is
//! that it **never reaches the model**: the handler borrows `ReplState`
//! immutably, so it cannot touch `api_messages`, and nothing reads the record
//! back into a request. Each rating is appended as one JSON line to
//! `~/.codesmith/feedback/feedback.jsonl` (the directory comes from
//! [`codesmith_config::ensure_state_dir`]) carrying the session id, the rated
//! message's index in the transcript, the model, the rating, the comment, and
//! a timestamp. A failed write is reported, not swallowed.
//!
//! `N` addresses a reply the way `/copy N` does: 1 (the default) is the most
//! recent assistant reply with text, 2 the one before it. Both commands count
//! over [`crate::interactive::assistant_replies`].
//!
//! Replaces the `misc_cmds::feedback` that printed the GitHub issue template
//! URLs; the issue chooser now closes the usage text.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Local only. There is no upload and no remote endpoint; the file is for
//!   the user and whatever offline analysis they run over it.
//! - A comment that starts with a bare number is read as `N`: write
//!   `/feedback down 1 3 tests failed` to say "3 tests failed" about the
//!   latest reply.
//! - `message_index` is the position in the live transcript at rating time.
//!   Compaction rewrites the transcript, so the index is stable only against
//!   the session file as saved then; `excerpt` is what keeps the line
//!   readable on its own.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use colored::Colorize;
use serde::{Deserialize, Serialize};

use super::ReplState;

/// Longest excerpt kept in a record, in characters.
const EXCERPT_CHARS: usize = 120;

/// Thumbs up or down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Rating {
    Up,
    Down,
}

impl Rating {
    fn glyph(self) -> &'static str {
        match self {
            Rating::Up => "👍",
            Rating::Down => "👎",
        }
    }
}

/// One rating, one JSON line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FeedbackRecord {
    /// RFC 3339, UTC.
    at: String,
    /// `None` until the session has been saved once.
    session_id: Option<String>,
    model: String,
    /// Position of the rated message in the transcript at rating time.
    message_index: usize,
    /// How the user addressed it: 1 is the most recent reply.
    nth_from_last: usize,
    rating: Rating,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    /// Opening of the rated reply, so the line reads without the session file.
    excerpt: String,
}

/// What `/feedback` was asked to do.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    rating: Rating,
    nth_from_last: usize,
    comment: Option<String>,
}

/// Parse `up|down [N] [comment]`. `None` means "print the usage".
fn parse(arg: &str) -> Option<Request> {
    let arg = arg.trim();
    let (word, rest) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
    let rating = match word.to_ascii_lowercase().as_str() {
        "up" => Rating::Up,
        "down" => Rating::Down,
        _ => return None,
    };
    let rest = rest.trim_start();
    let (head, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let (nth_from_last, comment) = match head.parse::<usize>() {
        Ok(0) => return None,
        Ok(n) => (n, tail.trim()),
        Err(_) => (1, rest),
    };
    Some(Request {
        rating,
        nth_from_last,
        comment: (!comment.is_empty()).then(|| comment.to_string()),
    })
}

/// The opening of a reply on one line.
fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > EXCERPT_CHARS {
        flat.chars().take(EXCERPT_CHARS).collect::<String>() + "…"
    } else {
        flat
    }
}

/// Where ratings land. Resolved per call so a retargeted `CODESMITH_HOME`
/// is honoured.
fn ledger_path() -> anyhow::Result<PathBuf> {
    Ok(codesmith_config::ensure_state_dir("feedback")?.join("feedback.jsonl"))
}

/// Append one record as a JSON line; errors name the path.
fn append_to(path: &Path, record: &FeedbackRecord) -> anyhow::Result<()> {
    let line = serde_json::to_string(record).context("serialize feedback record")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("write {}", path.display()))
}

fn print_usage() {
    println!("usage: /feedback up|down [N] [comment]");
    println!(
        "{}",
        "  rate the most recent reply, or the Nth-from-last the way /copy N counts; add a comment if you like"
            .dimmed()
    );
    println!(
        "{}",
        "  recorded locally under ~/.codesmith/feedback/ — never added to the conversation, never sent anywhere"
            .dimmed()
    );
    println!(
        "{}",
        "  bug reports and feature requests: https://github.com/camilesing/CodeSmith/issues/new/choose"
            .dimmed()
    );
}

/// Run `/feedback`: parse, address the reply, append the record, say where.
///
/// Borrows the state immutably on purpose — see the module note.
pub fn feedback(state: &ReplState, arg: &str) {
    let Some(Request {
        rating,
        nth_from_last,
        comment,
    }) = parse(arg)
    else {
        print_usage();
        return;
    };
    let replies = crate::interactive::assistant_replies(&state.api_messages);
    let Some((message_index, text)) = replies.iter().rev().nth(nth_from_last - 1) else {
        println!(
            "{} no reply #{nth_from_last} in this session ({} assistant repl(ies) so far)",
            "✗".red(),
            replies.len()
        );
        return;
    };
    let has_comment = comment.is_some();
    let record = FeedbackRecord {
        at: chrono::Utc::now().to_rfc3339(),
        session_id: state.current_session_id.clone(),
        model: state.model.clone(),
        message_index: *message_index,
        nth_from_last,
        rating,
        comment,
        excerpt: excerpt(text),
    };
    let written = ledger_path().and_then(|path| append_to(&path, &record).map(|()| path));
    match written {
        Ok(path) => println!(
            "{}",
            format!(
                "recorded {} for reply #{nth_from_last}{} → {}",
                rating.glyph(),
                if has_comment { " with comment" } else { "" },
                path.display()
            )
            .dimmed()
        ),
        Err(err) => println!("{} feedback not recorded: {err:#}", "✗".red()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentBlock, Message};

    fn text(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_only_assistant() -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({}),
                caller: None,
            }],
        }
    }

    fn sample(rating: Rating, comment: Option<&str>) -> FeedbackRecord {
        FeedbackRecord {
            at: "2026-10-21T00:00:00+00:00".to_string(),
            session_id: Some("sess-1".to_string()),
            model: "test-model".to_string(),
            message_index: 4,
            nth_from_last: 1,
            rating,
            comment: comment.map(str::to_string),
            excerpt: "an answer".to_string(),
        }
    }

    #[test]
    fn parse_reads_rating_index_and_comment() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("meh"), None);
        assert_eq!(parse("up 0"), None, "reply #0 does not exist");
        assert_eq!(
            parse("up"),
            Some(Request {
                rating: Rating::Up,
                nth_from_last: 1,
                comment: None,
            })
        );
        assert_eq!(
            parse("DOWN 2"),
            Some(Request {
                rating: Rating::Down,
                nth_from_last: 2,
                comment: None,
            })
        );
        assert_eq!(
            parse("up 3  nice and  tight "),
            Some(Request {
                rating: Rating::Up,
                nth_from_last: 3,
                comment: Some("nice and  tight".to_string()),
            }),
            "the comment keeps its own spacing"
        );
        assert_eq!(
            parse("down wrong file"),
            Some(Request {
                rating: Rating::Down,
                nth_from_last: 1,
                comment: Some("wrong file".to_string()),
            }),
            "no number means the latest reply"
        );
    }

    /// The Nth-from-last count is `/copy N`'s: assistant messages with text,
    /// tool-call-only turns skipped, indices into the full transcript.
    #[test]
    fn nth_from_last_counts_text_replies_like_copy() {
        let messages = vec![
            text("user", "q1"),
            text("assistant", "a1"),
            text("user", "q2"),
            tool_only_assistant(),
            text("assistant", "a2"),
            text("user", "q3"),
            text("assistant", "a3"),
        ];
        let replies = crate::interactive::assistant_replies(&messages);
        assert_eq!(
            replies,
            vec![
                (1, "a1".to_string()),
                (4, "a2".to_string()),
                (6, "a3".to_string())
            ]
        );
        for (nth_from_last, index, reply) in [(1, 6, "a3"), (2, 4, "a2")] {
            assert_eq!(
                replies.iter().rev().nth(nth_from_last - 1),
                Some(&(index, reply.to_string())),
                "/feedback … {nth_from_last}"
            );
        }
        assert_eq!(replies.iter().rev().nth(3), None);
    }

    #[test]
    fn records_append_as_one_json_line_each() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let first = sample(Rating::Down, None);
        let second = sample(Rating::Up, Some("tight"));
        append_to(&path, &first).unwrap();
        append_to(&path, &second).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "one line per rating:\n{body}");
        assert_eq!(
            serde_json::from_str::<FeedbackRecord>(lines[0]).unwrap(),
            first
        );
        assert_eq!(
            serde_json::from_str::<FeedbackRecord>(lines[1]).unwrap(),
            second
        );

        let shape: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        for key in [
            "at",
            "session_id",
            "model",
            "message_index",
            "nth_from_last",
            "rating",
            "comment",
            "excerpt",
        ] {
            assert!(shape.get(key).is_some(), "record lacks {key:?}: {shape}");
        }
        assert_eq!(shape["rating"], "up");
        let without_comment: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert!(without_comment.get("comment").is_none());
    }

    #[test]
    fn a_failed_write_is_an_error_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let path = blocker.join("feedback.jsonl");
        let err = append_to(&path, &sample(Rating::Up, None)).unwrap_err();
        assert!(format!("{err:#}").contains("feedback.jsonl"), "{err:#}");
    }

    /// The contract the command exists for: rating a reply changes nothing
    /// the model will see, and the record lands under the feedback dir.
    #[test]
    fn a_rating_never_reaches_the_model_context() {
        let _lock = codesmith_agent_runtime::test_support::lock_test_env();
        let home = codesmith_agent_runtime::test_support::ScopedCodeSmithHome::new();
        let options = crate::app_state::InteractiveOptions {
            model: "test-model".to_string(),
            workspace: std::env::temp_dir(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            max_subagents: 2,
            skills_dir: std::env::temp_dir(),
            memory_path: std::env::temp_dir().join("memory.md"),
            mcp_config_path: std::env::temp_dir().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        let mut state = ReplState::new(options, &crate::config::Config::default());
        state.api_messages = vec![
            text("user", "q1"),
            text("assistant", "first answer"),
            text("user", "q2"),
            text("assistant", "second answer"),
        ];
        let before = state.api_messages.clone();

        feedback(&state, "down 2 wrong file");

        assert_eq!(
            state.api_messages, before,
            "feedback must not touch the transcript"
        );
        let ledger = home.path().join("feedback").join("feedback.jsonl");
        let body = std::fs::read_to_string(&ledger).expect("ledger written");
        let record: FeedbackRecord = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(record.rating, Rating::Down);
        assert_eq!(record.nth_from_last, 2);
        assert_eq!(record.message_index, 1);
        assert_eq!(record.comment.as_deref(), Some("wrong file"));
        assert_eq!(record.model, state.model, "the record names the live model");
        assert_eq!(record.excerpt, "first answer");
        assert_eq!(record.session_id, None, "unsaved session has no id yet");
    }
}
