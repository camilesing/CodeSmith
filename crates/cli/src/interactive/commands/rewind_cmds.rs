//! `/rewind [n|list]` — restore the workspace to one of this session's
//! snapshots.
//!
//! The engine snapshots the workspace around every file-mutating turn (the
//! side-git repo in `~/.codesmith/snapshots/<project>`), so the history is
//! already recorded; this command lists it and checks one back out. Both
//! paths open the side repo with `SnapshotRepo::try_open`: reading never
//! initializes anything, and a workspace with no repo yet is simply
//! "nothing to rewind to".
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - **Files only.** Claude Code's `/rewind` also rewinds the conversation,
//!   with a picker that restores code, conversation, or both. Here the
//!   conversation half is `/undo` (drop the last exchange) and `/retry`
//!   (drop and re-send); this command touches the working tree and says so,
//!   rather than half-rewinding both and leaving them inconsistent.
//! - Restoring is a checkout of the whole tree: files the snapshot does not
//!   contain are removed from the working tree (`git checkout … -- :/` plus
//!   the repo's own reconciliation). It is the engine's own restore path, so
//!   the behaviour matches `/restore`'s engine-side equivalent.
//! - Snapshots are off when `[snapshots] enabled = false`, and a workspace
//!   the side repo refuses (a home directory, for instance) has none at all;
//!   both cases are reported as "nothing to rewind to" with the reason.
//! - The list is capped at [`LIST_LIMIT`] entries.

use codesmith_agent_runtime::snapshot::repo::{SnapshotId, SnapshotRepo};
use colored::Colorize;

/// How many snapshots `/rewind` lists.
pub const LIST_LIMIT: usize = 25;

/// List the workspace's snapshots, newest first.
///
/// Runs the (blocking) git history read under `spawn_blocking`.
pub async fn list(workspace: &std::path::Path) -> Result<Vec<SnapshotRow>, String> {
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = SnapshotRepo::try_open(&workspace).map_err(describe_open_error)?;
        let snapshots = repo.list(LIST_LIMIT).map_err(|err| err.to_string())?;
        Ok(snapshots
            .into_iter()
            .map(|snapshot| SnapshotRow {
                id: snapshot.id.as_str().to_string(),
                label: snapshot.label,
                timestamp: snapshot.timestamp,
            })
            .collect())
    })
    .await
    .map_err(|err| format!("snapshot listing failed: {err}"))?
}

/// Restore the workspace to one snapshot.
pub async fn restore(workspace: &std::path::Path, id: String) -> Result<(), String> {
    // `restore` feeds the id into `git checkout <id> -- :/` and deletes
    // working-tree paths — a destructive whole-tree operation.
    // `SnapshotId::from_hex` is the type's validating constructor: anything
    // that is not a hex object name (including a leading `-`, which git
    // parses as an option) is rejected before the repo is even opened.
    let id = SnapshotId::from_hex(id).map_err(|err| err.to_string())?;
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = SnapshotRepo::try_open(&workspace).map_err(describe_open_error)?;
        // Safety net: the checkout deletes working-tree files the target
        // snapshot does not contain, so manual edits made outside an engine
        // turn exist in no snapshot — capture the pre-restore state so
        // `/rewind` is itself rewindable. A failed safety-net snapshot is a
        // warning, not a blocker (the restore is the user's explicit ask).
        if let Err(err) = repo.snapshot("before /rewind") {
            println!(
                "{}",
                format!("warning: could not snapshot the current state before restoring: {err}")
                    .yellow()
            );
        }
        repo.restore(&id).map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("restore failed: {err}"))?
}

/// One row of the `/rewind` list.
pub struct SnapshotRow {
    pub id: String,
    pub label: String,
    /// Unix seconds.
    pub timestamp: i64,
}

impl SnapshotRow {
    /// `2026-10-13 14:22` in local time, or the raw epoch when it cannot be
    /// formatted.
    #[must_use]
    pub fn when(&self) -> String {
        chrono::DateTime::from_timestamp(self.timestamp, 0).map_or_else(
            || self.timestamp.to_string(),
            |utc| {
                utc.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            },
        )
    }

    /// Short id for the list: enough of the SHA to copy out of the terminal.
    #[must_use]
    pub fn short_id(&self) -> &str {
        // `id` is a public, unvalidated String (rows can be built with
        // non-SHA ids — the unit tests do), so the slice index must be
        // floored to a UTF-8 char boundary or a multi-byte character
        // spanning byte 8 panics.
        let mut end = self.id.len().min(8);
        while end > 0 && !self.id.is_char_boundary(end) {
            end -= 1;
        }
        &self.id[..end]
    }
}

/// Turn the side repo's refusal into something the user can act on.
fn describe_open_error(err: std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::InvalidInput => {
            format!("no snapshots for this workspace: {err}")
        }
        _ => format!("could not open the snapshot repository: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_render_a_time_and_a_copyable_id() {
        let row = SnapshotRow {
            id: "0123456789abcdef".to_string(),
            label: "turn 3".to_string(),
            timestamp: 1_700_000_000,
        };
        assert_eq!(row.short_id(), "01234567");
        // Local time makes the exact string timezone-dependent, so assert the
        // shape rather than the value.
        let when = row.when();
        assert_eq!(when.len(), 16, "{when}");
        assert_eq!(when.chars().nth(4), Some('-'));
        assert_eq!(when.chars().nth(13), Some(':'));
    }

    #[test]
    fn an_unrepresentable_timestamp_falls_back_to_the_epoch() {
        let row = SnapshotRow {
            id: "abc".to_string(),
            label: "old".to_string(),
            timestamp: i64::MAX,
        };
        assert_eq!(row.when(), i64::MAX.to_string());
    }

    /// The boundary walk exists so a multi-byte character spanning byte 8
    /// cannot panic the slice; a regression to `&self.id[..8]` would pass
    /// every clean-ascii test and only blow up here.
    #[test]
    fn short_id_stops_at_a_char_boundary() {
        let row = SnapshotRow {
            id: "0123456\u{e9}abcdef".to_string(),
            label: "turn 1".to_string(),
            timestamp: 0,
        };
        assert_eq!(row.short_id(), "0123456");
    }
}
