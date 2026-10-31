//! Append-only session journal — the durable record of a REPL / ACP
//! session's transcript (round-9 TODO-58).
//!
//! Recording-layer role: while the session runs, the
//! [`AppendLog`](crate::prompt_zones::AppendLog) is the authoritative
//! transcript; the journal is its durable, append-only copy — every append
//! and every wholesale replacement, in order — so history that compaction,
//! `/purge`, `/edit`, or a reset removed from the live transcript stays on
//! disk, and the transcript at any point is a fold of a journal prefix. The
//! session snapshot file ([`SavedSession`](crate::session_store::SavedSession))
//! is a load cache of that fold: it records the journal size it was written
//! against, and a load whose journal has grown since (a crash between the
//! journal append and the snapshot write, or an op that changed the
//! transcript after the last snapshot) recovers the transcript from here.
//!
//! Seam: [`SessionJournal`] is the definition the engine consumes
//! (`EngineConfig::session_journal`); [`FileSessionJournal`] is the file
//! provider, kept beside `FileSessionStore` by the provider-in-framework
//! precedent; the CLI REPL and ACP hosts inject it.
//!
//! Format: `<sessions>/<id>.journal.jsonl`, one JSON object per line; the
//! first line is a [`JournalEntry::Header`] naming the format version.
//! Written lines never change. A torn final line (a crash mid-write) is
//! dropped on read; any other unreadable line, or a header newer than this
//! build, fails the read loud.
//!
//! Requests and attempts (round-9 TODO-59): every main-turn model request
//! is journaled where it was sent ([`JournalEntry::Request`] — system prompt
//! and tool catalog only when they change, messages only when they are not
//! the transcript prefix), so [`reconstruct_requests`] rebuilds each request
//! from the journal alone; failed model rounds land as
//! [`JournalEntry::Attempt`]. Retries of one request are not repeated
//! (their failures are the attempt entries), and per-chunk stream timing is
//! not recorded — the committed content is.
//!
//! Known limitations: single writer per session (the snapshot store's own
//! limit); utility calls that share the client (compaction summaries, cycle
//! briefings) are not journaled — their inputs are the journaled transcript
//! plus fixed prompts; appends are not fsynced (a power loss can drop trailing lines,
//! never reorder them); `codesmith exec` runs (their snapshot ids are minted
//! independently of the engine's session id), the runtime API's threads
//! (their own event stream is authoritative) and sub-agent transcripts keep
//! no journal.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::{Message, MessageRequest, SystemPrompt, Tool};
use crate::session_store::AttemptRecord;

/// Version of the journal line format this build writes and reads.
pub const JOURNAL_FORMAT_VERSION: u32 = 1;

/// One journal record. Closed set: a reader meeting an unknown `kind` fails
/// loud rather than folding a transcript it cannot fully interpret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEntry {
    /// The format header — the first line of every journal file.
    Header { version: u32, session_id: String },
    /// Messages appended to the transcript, in order.
    Append { messages: Vec<Message> },
    /// The transcript was replaced wholesale by `messages`. `reason` is the
    /// [`RebuildReason`](crate::prompt_zones::RebuildReason) display text and
    /// `before` the replaced transcript's length — its content is the fold
    /// of every line above this one.
    Rebuild {
        reason: String,
        before: usize,
        messages: Vec<Message>,
    },
    /// A resume re-synced the transcript to exactly the journal's current
    /// fold (`len` messages); written instead of a `Rebuild` that would
    /// repeat a transcript already on record.
    Resume { len: usize },
    /// One main-turn model request (round-9 TODO-59), at the transcript
    /// position it was sent from. `envelope` is the request with `messages`
    /// emptied and `system` / `tools` moved out; those two are recorded in
    /// full only when they changed since the previous request entry.
    /// `messages` is present only when the request's messages were not
    /// exactly the transcript's first `message_count` messages (an
    /// extension rewrote them); otherwise they are that fold prefix.
    Request {
        envelope: Box<MessageRequest>,
        message_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        messages: Option<Vec<Message>>,
        system: Recorded<Option<SystemPrompt>>,
        tools: Recorded<Option<Vec<Tool>>>,
    },
    /// A failed model round (round-9 TODO-59) — the same record the
    /// snapshot's `failed_attempts` keeps, journaled in turn order.
    Attempt { record: AttemptRecord },
}

/// A request field recorded only when it changed since the previous
/// request entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recorded<T> {
    Unchanged,
    Set(T),
}

/// One serialized journal line: the entry plus its write time.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalLine {
    /// RFC 3339 write time.
    at: String,
    #[serde(flatten)]
    entry: JournalEntry,
}

/// Seam definition: where the engine sends a session's journal entries.
pub trait SessionJournal: Send + Sync + std::fmt::Debug {
    /// Append `entries`, in order, to the journal of `session_id`. Blocking
    /// I/O — the engine calls it from `spawn_blocking`.
    fn append(&self, session_id: &str, entries: Vec<JournalEntry>) -> std::io::Result<()>;
}

/// The journal file of `session_id` under `dir`. The id must be a single
/// safe path component (it names a file).
pub fn journal_path(dir: &Path, session_id: &str) -> std::io::Result<PathBuf> {
    let id = session_id.trim();
    if id.is_empty() || id.contains(['/', '\\']) || id == "." || id == ".." || id.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("session id {session_id:?} is not a safe file name"),
        ));
    }
    Ok(dir.join(format!("{id}.journal.jsonl")))
}

/// File provider of [`SessionJournal`]: `<dir>/<id>.journal.jsonl`.
#[derive(Debug, Clone)]
pub struct FileSessionJournal {
    dir: PathBuf,
}

impl FileSessionJournal {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl SessionJournal for FileSessionJournal {
    fn append(&self, session_id: &str, mut entries: Vec<JournalEntry>) -> std::io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let path = journal_path(&self.dir, session_id)?;
        let exists = path.exists() && drop_torn_tail(&path)? > 0;
        if exists
            && let Some(JournalEntry::Rebuild {
                reason, messages, ..
            }) = entries.first()
        {
            let resync = crate::prompt_zones::RebuildReason::SessionSync.to_string();
            if *reason == resync
                && read_journal(&path)
                    .and_then(|on_disk| fold_transcript(&on_disk))
                    .is_ok_and(|folded| folded == *messages)
            {
                entries[0] = JournalEntry::Resume {
                    len: messages.len(),
                };
            }
        }
        fs::create_dir_all(&self.dir)?;
        let mut options = fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut buffer = String::new();
        if !exists {
            entries.insert(
                0,
                JournalEntry::Header {
                    version: JOURNAL_FORMAT_VERSION,
                    session_id: session_id.trim().to_string(),
                },
            );
        }
        for entry in entries {
            let line = JournalLine {
                at: now.clone(),
                entry,
            };
            let json = serde_json::to_string(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            buffer.push_str(&json);
            buffer.push('\n');
        }
        // One write per batch: a crash leaves at most one torn final line.
        file.write_all(buffer.as_bytes())
    }
}

/// Cut a torn final line (a crash mid-write left bytes without the closing
/// newline) so the next append starts on a fresh line — appending after it
/// would fuse the fragment into an unreadable middle line. Only bytes that
/// never formed a complete line are removed. Returns the remaining length.
fn drop_torn_tail(path: &Path) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(0);
    }
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(len);
    }
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut bytes)?;
    let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1) as u64;
    file.set_len(keep)?;
    tracing::warn!(
        target: "codesmith::session",
        path = %path.display(),
        dropped = len - keep,
        "cut a torn final journal line before appending"
    );
    Ok(keep)
}

/// Read every entry of the journal at `path`, in order. A torn final line
/// (no trailing newline and unparseable — a crash mid-write) is dropped;
/// any other unreadable line, or a header newer than this build, is an
/// `InvalidData` error.
pub fn read_journal(path: &Path) -> std::io::Result<Vec<JournalEntry>> {
    let text = fs::read_to_string(path)?;
    let torn_tail_allowed = !text.ends_with('\n');
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut entries = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<JournalLine>(line) {
            Ok(parsed) => {
                if let JournalEntry::Header { version, .. } = &parsed.entry
                    && *version > JOURNAL_FORMAT_VERSION
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "session journal {} is format v{version}, newer than supported v{JOURNAL_FORMAT_VERSION}",
                            path.display()
                        ),
                    ));
                }
                entries.push(parsed.entry);
            }
            Err(err) if torn_tail_allowed && index + 1 == lines.len() => {
                tracing::warn!(
                    target: "codesmith::session",
                    path = %path.display(),
                    %err,
                    "dropping a torn final journal line"
                );
            }
            Err(err) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "session journal {} line {} is unreadable: {err}",
                        path.display(),
                        index + 1
                    ),
                ));
            }
        }
    }
    Ok(entries)
}

/// The transcript the journal `entries` describe.
pub fn fold_transcript(entries: &[JournalEntry]) -> std::io::Result<Vec<Message>> {
    let mut transcript: Vec<Message> = Vec::new();
    for entry in entries {
        match entry {
            JournalEntry::Header { .. } => {}
            JournalEntry::Append { messages } => transcript.extend(messages.iter().cloned()),
            JournalEntry::Rebuild { messages, .. } => transcript.clone_from(messages),
            JournalEntry::Resume { len } => {
                if *len != transcript.len() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "journal resume marker names {len} messages but the fold holds {}",
                            transcript.len()
                        ),
                    ));
                }
            }
            JournalEntry::Request { .. } | JournalEntry::Attempt { .. } => {}
        }
    }
    Ok(transcript)
}

/// Every main-turn model request the journal `entries` record, rebuilt in
/// order: the envelope, the system prompt and tool catalog in force, and
/// the messages (the transcript prefix at that point, or the recorded
/// rewrite). A request recorded before any system/tool entry, or naming
/// more messages than the fold holds, is `InvalidData`.
pub fn reconstruct_requests(entries: &[JournalEntry]) -> std::io::Result<Vec<MessageRequest>> {
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let mut transcript: Vec<Message> = Vec::new();
    let mut system: Option<Option<SystemPrompt>> = None;
    let mut tools: Option<Option<Vec<Tool>>> = None;
    let mut requests = Vec::new();
    for entry in entries {
        match entry {
            JournalEntry::Header { .. } | JournalEntry::Attempt { .. } => {}
            JournalEntry::Append { messages } => transcript.extend(messages.iter().cloned()),
            JournalEntry::Rebuild { messages, .. } => transcript.clone_from(messages),
            JournalEntry::Resume { .. } => {}
            JournalEntry::Request {
                envelope,
                message_count,
                messages,
                system: recorded_system,
                tools: recorded_tools,
            } => {
                if let Recorded::Set(value) = recorded_system {
                    system = Some(value.clone());
                }
                if let Recorded::Set(value) = recorded_tools {
                    tools = Some(value.clone());
                }
                let mut request = (**envelope).clone();
                request.system = system
                    .clone()
                    .ok_or_else(|| invalid("request entry before any system prompt".into()))?;
                request.tools = tools
                    .clone()
                    .ok_or_else(|| invalid("request entry before any tool catalog".into()))?;
                request.messages = match messages {
                    Some(rewritten) => rewritten.clone(),
                    None => transcript
                        .get(..*message_count)
                        .ok_or_else(|| {
                            invalid(format!(
                                "request names {message_count} messages but the fold holds {}",
                                transcript.len()
                            ))
                        })?
                        .to_vec(),
                };
                requests.push(request);
            }
        }
    }
    Ok(requests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ContentBlock;

    fn text(role: &str, body: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: body.to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn append_then_fold_round_trips_appends_and_rebuilds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = FileSessionJournal::new(dir.path());
        journal
            .append(
                "s1",
                vec![JournalEntry::Append {
                    messages: vec![text("user", "one"), text("assistant", "two")],
                }],
            )
            .expect("append");
        journal
            .append(
                "s1",
                vec![
                    JournalEntry::Rebuild {
                        reason: "manual compaction (/compact)".to_string(),
                        before: 2,
                        messages: vec![text("user", "summary")],
                    },
                    JournalEntry::Append {
                        messages: vec![text("user", "three")],
                    },
                ],
            )
            .expect("append");
        let entries = read_journal(&journal_path(dir.path(), "s1").unwrap()).expect("read");
        assert!(matches!(
            entries[0],
            JournalEntry::Header { version: 1, .. }
        ));
        assert_eq!(
            fold_transcript(&entries).expect("fold"),
            vec![text("user", "summary"), text("user", "three")]
        );
        // The replaced history is still on record above the rebuild.
        assert!(matches!(&entries[1], JournalEntry::Append { messages } if messages.len() == 2));
    }

    #[test]
    fn resume_of_the_recorded_transcript_writes_a_marker_not_a_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = FileSessionJournal::new(dir.path());
        let transcript = vec![text("user", "one"), text("assistant", "two")];
        journal
            .append(
                "s2",
                vec![JournalEntry::Append {
                    messages: transcript.clone(),
                }],
            )
            .expect("append");
        let resync = crate::prompt_zones::RebuildReason::SessionSync.to_string();
        journal
            .append(
                "s2",
                vec![JournalEntry::Rebuild {
                    reason: resync,
                    before: 0,
                    messages: transcript.clone(),
                }],
            )
            .expect("append");
        let entries = read_journal(&journal_path(dir.path(), "s2").unwrap()).expect("read");
        assert_eq!(entries.last(), Some(&JournalEntry::Resume { len: 2 }));
        assert_eq!(fold_transcript(&entries).expect("fold"), transcript);
    }

    #[test]
    fn torn_final_line_is_dropped_on_read_and_cut_before_the_next_append() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = FileSessionJournal::new(dir.path());
        journal
            .append(
                "s3",
                vec![JournalEntry::Append {
                    messages: vec![text("user", "one")],
                }],
            )
            .expect("append");
        let path = journal_path(dir.path(), "s3").unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"at\":\"x\",\"kind\":\"app").unwrap();
        let entries = read_journal(&path).expect("torn tail tolerated");
        assert_eq!(
            fold_transcript(&entries).unwrap(),
            vec![text("user", "one")]
        );

        journal
            .append(
                "s3",
                vec![JournalEntry::Append {
                    messages: vec![text("user", "two")],
                }],
            )
            .expect("append after a torn tail");
        let entries = read_journal(&path).expect("journal stays readable");
        assert_eq!(
            fold_transcript(&entries).unwrap(),
            vec![text("user", "one"), text("user", "two")]
        );
    }

    #[test]
    fn an_unreadable_complete_line_fails_the_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s4.journal.jsonl");
        fs::write(&path, "{\"at\":\"x\",\"kind\":\"nope\"}\n").unwrap();
        assert_eq!(
            read_journal(&path).expect_err("unknown kind").kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn unsafe_session_ids_are_refused() {
        let dir = Path::new("/tmp");
        assert!(journal_path(dir, "../evil").is_err());
        assert!(journal_path(dir, "").is_err());
        assert!(journal_path(dir, "ok-id").is_ok());
    }
}
