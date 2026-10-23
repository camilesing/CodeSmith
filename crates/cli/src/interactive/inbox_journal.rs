//! Durable inbox journal — the persistence half of the REPL's queued-message
//! queue (round-7 TODO-48).
//!
//! [`super::state::ReplState::queued_messages`] is in-memory only: text typed
//! while a turn runs is steered into the turn as tool calls finish and the
//! leftovers are sent as the next turn, but nothing about the queue survives
//! a crash — the REPL's persistence points (turn-start checkpoint,
//! turn-boundary autosave) both fire outside the window in which queued text
//! exists. The journal is an append-only `<sessions>/<id>.inbox.jsonl`
//! written the moment text is queued and rewritten to mirror the queue after
//! every successful turn-boundary persist, so at every crash point it holds
//! exactly what the queue held as of the last session persist — the same
//! recovery contract as the transcript itself.
//!
//! Delivery semantics: an entry survives until the turn that consumed it
//! (steered mid-turn, or sent as the next turn's seed) is durably persisted.
//! A crash mid-turn therefore resurrects the entry as the next turn's user
//! message — a deliberate steer→next-turn downgrade, not a loss; the turn it
//! was steered into is itself lost to the crash. Text taken back into the
//! composer (Up on an empty prompt, the take-back key) and never resent is
//! resurrected the same way, up to the last persist.
//!
//! Known limitations: text only (the mid-turn composer attaches no images);
//! corrupt lines are skipped with a loud count, never silently; a rewrite
//! failure keeps the superseded entries (duplicates on restore, never
//! loss); orphan journals whose session file is gone are swept at REPL
//! startup — entries from never-persisted sessions are offered into the
//! fresh queue first (24h age gate, mirroring the crash-recovery
//! checkpoint), older orphans are deleted.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use colored::Colorize;

/// One journal line. `enqueued_at` is unix milliseconds, kept for forensics
/// only — ordering is file order.
#[derive(serde::Serialize, serde::Deserialize)]
struct InboxEntry {
    content: String,
    enqueued_at: i64,
}

/// What [`InboxJournal::load`] found on disk.
pub(crate) struct JournalContents {
    pub entries: Vec<String>,
    pub corrupt: usize,
}

/// The append-only queue journal for one session.
pub(crate) struct InboxJournal {
    path: PathBuf,
}

impl InboxJournal {
    /// Journal for `id` under `sessions_dir`, validated like the session
    /// file name (`checkpoints` is reserved by the store).
    pub(crate) fn for_session(sessions_dir: &Path, id: &str) -> Option<Self> {
        let trimmed = id.trim();
        if trimmed.is_empty()
            || trimmed == "checkpoints"
            || !trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }
        Some(Self {
            path: sessions_dir.join(format!("{trimmed}.inbox.jsonl")),
        })
    }

    pub(crate) fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Append one queued message, flushed and synced — this is the
    /// durability point for text the user has already handed over.
    pub(crate) fn append(&self, content: &str) -> io::Result<()> {
        let line = InboxEntry {
            content: content.to_string(),
            enqueued_at: chrono::Utc::now().timestamp_millis(),
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, &line)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()
    }

    /// Read the journal; corrupt lines are skipped and counted, never fatal.
    pub(crate) fn load(&self) -> JournalContents {
        let mut contents = JournalContents {
            entries: Vec::new(),
            corrupt: 0,
        };
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return contents; // no journal yet — nothing was ever queued
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<InboxEntry>(line) {
                Ok(entry) => contents.entries.push(entry.content),
                Err(_) => contents.corrupt += 1,
            }
        }
        contents
    }

    /// Rewrite the journal to mirror `queued` (atomic tmp+rename). An empty
    /// queue removes the file. On failure the previous entries survive — a
    /// later restore may see duplicates, never losses.
    pub(crate) fn rewrite(&self, queued: &[String]) -> io::Result<()> {
        if queued.is_empty() {
            return self.remove();
        }
        let mut body = String::new();
        for content in queued {
            let line = InboxEntry {
                content: content.clone(),
                enqueued_at: chrono::Utc::now().timestamp_millis(),
            };
            body.push_str(&serde_json::to_string(&line)?);
            body.push('\n');
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, body.as_bytes())?;
        // fsync the temp before the rename (a crash must not publish a
        // torn journal), then fsync the parent so the rename is durable.
        if let Ok(file) = std::fs::File::open(&tmp) {
            let _ = file.sync_all();
        }
        match std::fs::rename(&tmp, &self.path) {
            Ok(()) => {
                if let Some(parent) = self.path.parent()
                    && let Ok(dir) = std::fs::File::open(parent)
                {
                    let _ = dir.sync_all();
                }
                Ok(())
            }
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                Err(err)
            }
        }
    }

    /// Delete the journal if present (idempotent).
    pub(crate) fn remove(&self) -> io::Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

/// Append one message to the current session's journal, minting the session
/// id if the queue outlives the first persist (`saved_session_from_state`
/// adopts the same id, so journal and session file stay paired). A write
/// failure is loud but never blocks the in-memory queue.
pub(crate) fn journal_enqueue(state: &mut super::state::ReplState, text: &str) {
    let id = state
        .current_session_id
        .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
        .clone();
    let Some(journal) = default_journal(&id) else {
        return;
    };
    if let Err(err) = journal.append(text) {
        println!(
            "{}",
            format!("warning: could not persist the queued message to the inbox journal: {err}")
                .yellow()
        );
    }
}

/// Mirror the live queue into the journal after a successful session
/// persist: entries the finished turn consumed are now durable in the
/// session file, so only what is still queued keeps its journal line.
pub(crate) fn mirror_after_persist(state: &super::state::ReplState, sessions_dir: &Path) {
    let Some(id) = state.current_session_id.as_deref() else {
        return;
    };
    let Some(journal) = InboxJournal::for_session(sessions_dir, id) else {
        return;
    };
    if !journal.exists() && state.queued_messages.is_empty() {
        return;
    }
    if let Err(err) = journal.rewrite(&state.queued_messages) {
        println!(
            "{}",
            format!("warning: could not update the inbox journal: {err}").yellow()
        );
    }
}

/// Seed the queue from a resumed session's journal. Returns the number of
/// restored entries.
pub(crate) fn adopt_for_resume(
    state: &mut super::state::ReplState,
    sessions_dir: &Path,
    session_id: &str,
) -> usize {
    let Some(journal) = InboxJournal::for_session(sessions_dir, session_id) else {
        return 0;
    };
    let contents = journal.load();
    if contents.corrupt > 0 {
        println!(
            "{}",
            format!(
                "warning: skipped {} corrupt inbox journal line(s) for session {}",
                contents.corrupt,
                crate::session_manager::truncate_id(session_id),
            )
            .yellow()
        );
    }
    let count = contents.entries.len();
    if count > 0 {
        state.queued_messages = contents.entries;
    }
    count
}

/// Move the journal when the engine reports a session id different from the
/// one the REPL minted for the queue, so journal and session file stay
/// paired. A failure is logged and leaves the old file for the startup
/// sweep.
pub(crate) fn rekey(sessions_dir: &Path, old_id: &str, new_id: &str) {
    let (Some(old), Some(new)) = (
        InboxJournal::for_session(sessions_dir, old_id),
        InboxJournal::for_session(sessions_dir, new_id),
    ) else {
        return;
    };
    if !old.exists() || old.path == new.path {
        return;
    }
    if let Err(err) = std::fs::rename(&old.path, &new.path) {
        tracing::warn!(target: "codesmith::inbox", %err, "inbox journal rekey failed");
    }
}

/// Best-effort journal removal where the session it belonged to was deleted
/// (the REPL startup sweep is the backstop for stores that are not the
/// default file store).
pub(crate) fn remove_at_default_location(id: &str) {
    let Some(sessions_dir) = crate::session_manager::SessionManager::default_location()
        .ok()
        .map(|manager| manager.sessions_dir().to_path_buf())
    else {
        return;
    };
    if let Some(journal) = InboxJournal::for_session(&sessions_dir, id) {
        let _ = journal.remove();
    }
}

/// Age gate for harvesting orphaned journals into a fresh session — mirrors
/// the 24h gate on the crash-recovery checkpoint (`load_recent_checkpoint`).
const ORPHAN_HARVEST_WINDOW: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Harvest orphan journals — those with no sibling session file. A recent
/// orphan (a session that crashed before its first persist) yields its
/// entries for the fresh queue; an old one (its session file was evicted or
/// deleted) is just deleted. Journals whose session file still exists are
/// left alone — they belong to a session the user can resume.
pub(crate) fn sweep_orphans(sessions_dir: &Path) -> Vec<String> {
    let Ok(read_dir) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    let mut harvested = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".inbox.jsonl") else {
            continue;
        };
        if sessions_dir.join(format!("{stem}.json")).exists() {
            continue; // live session — recovery rides resume
        }
        let mtime = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        // An unreadable clock is treated as recent: when in doubt,
        // harvest rather than destroy.
        let recent = mtime
            .elapsed()
            .map(|age| age < ORPHAN_HARVEST_WINDOW)
            .unwrap_or(true);
        if recent {
            harvested.extend(
                InboxJournal::for_session(sessions_dir, stem)
                    .map(|journal| journal.load().entries)
                    .unwrap_or_default(),
            );
        }
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!(target: "codesmith::inbox", %err, "orphan inbox journal cleanup failed");
        }
    }
    harvested
}

/// Startup sweep at the default location: harvest recent orphan entries into
/// the live session's queue, re-journaled under the current id so they keep
/// the same crash-durability contract. Returns the harvested count.
pub(crate) fn harvest_orphans_at_default_location(state: &mut super::state::ReplState) -> usize {
    let manager = match crate::session_manager::SessionManager::default_location() {
        Ok(manager) => manager,
        Err(err) => {
            println!(
                "{}",
                format!("warning: could not open the sessions directory for inbox recovery: {err}")
                    .yellow()
            );
            return 0;
        }
    };
    let harvested = sweep_orphans(manager.sessions_dir());
    let count = harvested.len();
    for text in harvested {
        journal_enqueue(state, &text);
        state.queued_messages.push(text);
    }
    count
}

fn default_journal(id: &str) -> Option<InboxJournal> {
    let sessions_dir = crate::session_manager::SessionManager::default_location()
        .ok()
        .map(|manager| manager.sessions_dir().to_path_buf());
    let sessions_dir = sessions_dir?;
    InboxJournal::for_session(&sessions_dir, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_load_roundtrip_preserves_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let journal = InboxJournal::for_session(tmp.path(), "abc-123").unwrap();
        journal.append("first").unwrap();
        journal.append("second").unwrap();
        let contents = journal.load();
        assert_eq!(contents.corrupt, 0);
        assert_eq!(
            contents.entries,
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn load_skips_corrupt_lines_and_counts_them() {
        let tmp = tempfile::TempDir::new().unwrap();
        let journal = InboxJournal::for_session(tmp.path(), "s1").unwrap();
        journal.append("good").unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path().join("s1.inbox.jsonl"))
            .unwrap();
        file.write_all(b"{not json}\n").unwrap();
        let contents = journal.load();
        assert_eq!(contents.entries, vec!["good".to_string()]);
        assert_eq!(contents.corrupt, 1);
    }

    #[test]
    fn rewrite_mirrors_the_queue_and_empty_removes_the_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let journal = InboxJournal::for_session(tmp.path(), "s1").unwrap();
        journal.append("a").unwrap();
        journal.append("b").unwrap();
        journal.rewrite(&["b".to_string()]).unwrap();
        assert_eq!(journal.load().entries, vec!["b".to_string()]);
        journal.rewrite(&[]).unwrap();
        assert!(!journal.exists(), "an empty queue removes the journal");
    }

    #[test]
    fn for_session_rejects_reserved_and_invalid_ids() {
        let dir = Path::new("/nonexistent");
        assert!(InboxJournal::for_session(dir, "checkpoints").is_none());
        assert!(InboxJournal::for_session(dir, "../escape").is_none());
        assert!(InboxJournal::for_session(dir, "").is_none());
        assert!(InboxJournal::for_session(dir, "with space").is_none());
        assert!(InboxJournal::for_session(dir, "uuid-style_1").is_some());
    }

    #[test]
    fn rekey_moves_the_journal_and_ignores_missing_sources() {
        let tmp = tempfile::TempDir::new().unwrap();
        let old = InboxJournal::for_session(tmp.path(), "old-id").unwrap();
        old.append("entry").unwrap();
        rekey(tmp.path(), "old-id", "new-id");
        assert!(!old.exists());
        let new = InboxJournal::for_session(tmp.path(), "new-id").unwrap();
        assert_eq!(new.load().entries, vec!["entry".to_string()]);
        // A missing source is a no-op, not an error.
        rekey(tmp.path(), "never-existed", "another-id");
    }

    #[test]
    fn sweep_harvests_recent_orphans_deletes_stale_ones_leaves_live_ones() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        let fresh = InboxJournal::for_session(dir, "fresh-orphan").unwrap();
        fresh.append("typed during the crash").unwrap();

        let stale = InboxJournal::for_session(dir, "stale-orphan").unwrap();
        stale.append("long gone").unwrap();
        let stale_path = dir.join("stale-orphan.inbox.jsonl");
        std::fs::File::options()
            .write(true)
            .open(&stale_path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600))
            .unwrap();

        let live = InboxJournal::for_session(dir, "live-session").unwrap();
        live.append("waits for resume").unwrap();
        std::fs::write(dir.join("live-session.json"), "{}").unwrap();

        let harvested = sweep_orphans(dir);
        assert_eq!(harvested, vec!["typed during the crash".to_string()]);
        assert!(!dir.join("fresh-orphan.inbox.jsonl").exists());
        assert!(!stale_path.exists(), "the stale orphan is deleted");
        assert!(
            dir.join("live-session.inbox.jsonl").exists(),
            "a journal whose session file exists is left for resume"
        );
    }
}
