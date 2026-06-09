//! Agent Teams — multi-agent coordination with shared task lists and
//! inter-teammate messaging.
//!
//! Ports the Claude Code TypeScript Agent Teams system into CodeSmith's
//! Rust architecture. Key components:
//! - Team file persistence (config.json)
//! - File-based teammate mailbox (flock-concurrent inbox)
//! - Team lifecycle tools (create, delete, send_message)
//! - In-process teammate lifecycle (idle/shutdown protocol)

mod team_file;
mod teammate_mailbox;
mod team_create;
mod team_delete;
mod send_message;
mod teammate_lifecycle;
mod team_discovery;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

pub use team_file::{
    TeamFile, TeamMember, TeamAllowedPath,
    sanitize_name, format_lead_agent_id, team_lead_name,
    team_dir, team_config_path, team_task_dir,
    create_team_file, read_team_file, write_team_file,
    delete_team_directories,
    find_member_by_name, find_member_by_agent_id, remove_member_by_name,
    active_teammates, active_teammate_count,
};
pub use teammate_mailbox::{
    TeammateMessage, StructuredProtocolMessage,
    read_mailbox, write_to_mailbox, read_unread_messages,
    mark_messages_as_read, clear_mailbox, parse_structured_protocol,
    is_structured_protocol_message,
};
pub use team_create::TeamCreateTool;
pub use team_delete::TeamDeleteTool;
pub use send_message::SendMessageTool;
pub use teammate_lifecycle::{
    TeammateState, TeammateResult, TeammateRuntime,
    process_inbox_messages, send_idle_notification,
    handle_shutdown_request, run_teammate_loop, poll_leader_inbox,
};
pub use team_discovery::{
    read_team_config, write_team_config,
    find_member, find_member_by_id, remove_member,
    add_member, set_member_inactive, set_member_active,
};

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
    /// Active teammates keyed by agent ID.
    pub teammates: HashMap<String, TeammateInfo>,
}

/// Thread-safe shared reference to optional TeamContext.
pub type SharedTeamContext = Arc<Mutex<Option<TeamContext>>>;

/// Create a new empty SharedTeamContext.
pub fn new_shared_team_context() -> SharedTeamContext {
    Arc::new(Mutex::new(None))
}