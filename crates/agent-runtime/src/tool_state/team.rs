//! State types extracted from `crates/tui/src/tools/team/mod.rs`.
//! Tool implementations stay in tui.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Runtime info about a teammate tracked in the session-level TeamContext.
#[derive(Debug, Clone)]
pub struct TeammateInfo {
    pub name: String,
    pub agent_type: String,
    pub color: Option<String>,
    pub cwd: PathBuf,
    pub spawned_at: i64,
}

/// Shared, mutable team context for the current session.
///
/// Stored in Engine and propagated via RuntimeToolServices. When
/// TeamCreateTool executes, it writes the TeamContext into this slot.
#[derive(Debug)]
pub struct TeamContext {
    pub team_name: String,
    pub team_file_path: PathBuf,
    pub lead_agent_id: String,
    pub task_v2_manager: crate::tool_state::task_v2::SharedTaskV2Manager,
    /// Active teammates keyed by agent ID.
    pub teammates: HashMap<String, TeammateInfo>,
    /// Cancellation tokens for active in-process teammates keyed by agent name.
    pub teammate_cancel_tokens: HashMap<String, tokio_util::sync::CancellationToken>,
}

/// Thread-safe shared reference to optional TeamContext.
pub type SharedTeamContext = Arc<Mutex<Option<TeamContext>>>;

/// Create a new empty SharedTeamContext.
pub fn new_shared_team_context() -> SharedTeamContext {
    Arc::new(Mutex::new(None))
}

