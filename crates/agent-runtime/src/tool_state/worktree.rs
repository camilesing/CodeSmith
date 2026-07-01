//! State types extracted from `crates/tui/src/tools/worktree/mod.rs`.
//! Tool implementations stay in tui.

use std::path::PathBuf;
use std::sync::Arc;

/// Session-level worktree state, stored in EngineConfig via `Arc<Mutex>`.
/// `EnterWorktreeTool` writes to this; `ExitWorktreeTool` clears it.
/// The engine reads it when building `ToolContext` per turn to set `cwd`.
#[derive(Debug, Clone, Default)]
pub struct WorktreeSessionState {
    /// Whether a worktree session is currently active.
    pub active: bool,
    /// Path to the worktree directory (e.g. `.codesmith/worktrees/<slug>`).
    pub worktree_path: Option<PathBuf>,
    /// Branch name for the worktree (e.g. `worktree-<slug>`).
    pub worktree_branch: Option<String>,
    /// Name/slug used to create the worktree.
    pub worktree_name: Option<String>,
    /// Original CWD before entering the worktree (the main repo root).
    pub original_cwd: Option<PathBuf>,
    /// HEAD commit SHA at the time the worktree was created.
    /// Used by `ExitWorktreeTool` to detect new commits (safety check).
    pub original_head_commit: Option<String>,
    /// Session ID that created this worktree.
    pub session_id: Option<String>,
}

pub type SharedWorktreeSessionState = Arc<std::sync::Mutex<WorktreeSessionState>>;

pub fn new_shared_worktree_session_state() -> SharedWorktreeSessionState {
    Arc::new(std::sync::Mutex::new(WorktreeSessionState::default()))
}
