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

/// Failures of [`list`] and [`restore`], kept typed so the UI layer can
/// distinguish "this workspace has no snapshots" (grey out or hide the
/// command) from a git failure (surface the message; after a failed
/// restore the tree may be mid-restore) without parsing strings.
#[derive(Debug, thiserror::Error)]
pub enum RewindError {
    /// No snapshot repo for this workspace (snapshots disabled, a home
    /// directory, an oversized workspace) — read-intent, nothing to act on.
    #[error("no snapshots for this workspace: {0}")]
    NoSnapshots(#[source] std::io::Error),
    /// The side repo exists but could not be opened.
    #[error("could not open the snapshot repository: {0}")]
    Open(#[source] std::io::Error),
    /// The id was not a hex object name (option-injection guard).
    #[error("{0}")]
    InvalidId(#[source] std::io::Error),
    /// A git operation failed; `op` names the step that failed.
    #[error("{op} failed: {source}")]
    Git {
        op: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The blocking task panicked or was cancelled.
    #[error("{op} failed: {source}")]
    Task {
        op: &'static str,
        #[source]
        source: tokio::task::JoinError,
    },
}

/// List the workspace's snapshots, newest first.
///
/// Runs the (blocking) git history read under `spawn_blocking`.
pub async fn list(workspace: &std::path::Path) -> Result<Vec<SnapshotRow>, RewindError> {
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = SnapshotRepo::try_open(&workspace).map_err(classify_open_error)?;
        let snapshots = repo.list(LIST_LIMIT).map_err(|source| RewindError::Git {
            op: "snapshot listing",
            source,
        })?;
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
    .map_err(|source| RewindError::Task {
        op: "snapshot listing",
        source,
    })?
}

/// Restore the workspace to one snapshot.
pub async fn restore(workspace: &std::path::Path, id: String) -> Result<(), RewindError> {
    // `restore` feeds the id into `git checkout <id> -- :/` and deletes
    // working-tree paths — a destructive whole-tree operation.
    // `SnapshotId::from_hex` is the type's validating constructor: anything
    // that is not a hex object name (including a leading `-`, which git
    // parses as an option) is rejected before the repo is even opened.
    let id = SnapshotId::from_hex(id).map_err(RewindError::InvalidId)?;
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = SnapshotRepo::try_open(&workspace).map_err(classify_open_error)?;
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
        repo.restore(&id).map_err(|source| RewindError::Git {
            op: "restore",
            source,
        })
    })
    .await
    .map_err(|source| RewindError::Task {
        op: "restore",
        source,
    })?
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

/// Turn the side repo's refusal into the matching typed variant: the
/// runtime signals "this workspace has no snapshot repo" with
/// `ErrorKind::InvalidInput`; every other open failure is a real repo
/// problem the user should see verbatim.
fn classify_open_error(err: std::io::Error) -> RewindError {
    match err.kind() {
        std::io::ErrorKind::InvalidInput => RewindError::NoSnapshots(err),
        _ => RewindError::Open(err),
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

    /// The UI greys out `NoSnapshots` and alarms on everything else, so the
    /// `InvalidInput` mapping and the rendered git phrasing are contracts.
    #[test]
    fn open_errors_classify_and_render_by_kind() {
        let no_repo = classify_open_error(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "snapshots disabled",
        ));
        assert!(matches!(no_repo, RewindError::NoSnapshots(_)));
        assert_eq!(
            no_repo.to_string(),
            "no snapshots for this workspace: snapshots disabled"
        );

        let broken = classify_open_error(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "config missing",
        ));
        assert!(matches!(broken, RewindError::Open(_)));

        let git = RewindError::Git {
            op: "restore",
            source: std::io::Error::other("bad object"),
        };
        assert_eq!(git.to_string(), "restore failed: bad object");
    }
}
