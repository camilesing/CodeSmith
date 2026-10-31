//! Session persistence — re-exported from [`codesmith_session::store`]
//! (round-9 split: the session data plane moved to its own framework crate).
//! Workspace snapshot pruning stays here because the side-git snapshot code
//! lives in this crate.

pub use codesmith_session::store::*;

use std::path::Path;

/// Prune snapshots older than `max_age` for `workspace`.
///
/// Always non-fatal. Returns silently — callers don't need the count
/// (the underlying repo logs at WARN if anything blew up).
pub fn prune_workspace_snapshots(workspace: &Path, max_age: std::time::Duration) {
    match crate::snapshot::prune_older_than(workspace, max_age) {
        Ok(0) => {}
        Ok(n) => {
            tracing::debug!(target: "snapshot", "boot prune removed {n} snapshot(s)");
        }
        Err(e) => {
            tracing::warn!(target: "snapshot", "boot prune failed: {e}");
        }
    }
}
