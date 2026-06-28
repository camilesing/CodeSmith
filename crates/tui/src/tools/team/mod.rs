//! Agent Teams — multi-agent coordination with shared task lists and
//! inter-teammate messaging.
//!
//! Ports the Claude Code TypeScript Agent Teams system into CodeSmith's
//! Rust architecture. Key components:
//! - Team file persistence (config.json)
//! - File-based teammate mailbox (flock-concurrent inbox)
//! - Team lifecycle tools (create, delete, send_message)
//! - In-process teammate lifecycle (idle/shutdown protocol)

mod inbox_poller;
mod protocol_handlers;
mod send_message;
mod team_create;
mod team_delete;
mod team_discovery;
mod team_file;
mod team_memory;
mod teammate_lifecycle;
mod teammate_mailbox;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

pub use inbox_poller::{
    InboxClassification, InboxDispatch, TeamInboxRx, TeamInboxTx, classify_inbox_messages,
    run_leader_inbox_poller,
};
pub use protocol_handlers::{
    PermissionDecision, PermissionRequestRegistry, SharedPermissionRequestRegistry,
    handle_permission_request as proto_permission_request,
    handle_permission_response as proto_permission_response,
    handle_plan_approval_auto_approve as proto_plan_approve,
    handle_plan_approval_rejection as proto_plan_reject,
    handle_shutdown_approval as proto_shutdown_approval,
    handle_shutdown_rejection as proto_shutdown_rejection,
    handle_shutdown_request as proto_shutdown_request, new_shared_permission_registry,
};
pub use send_message::SendMessageTool;
pub use team_create::TeamCreateTool;
pub use team_delete::TeamDeleteTool;
pub use team_discovery::{
    add_member, find_member, find_member_by_id, read_team_config, remove_member, set_member_active,
    set_member_inactive, write_team_config,
};
pub use team_file::{
    TeamAllowedPath, TeamFile, TeamMember, active_teammate_count, active_teammates,
    create_team_file, delete_team_directories, find_member_by_agent_id, find_member_by_name,
    format_lead_agent_id, read_team_file, remove_member_by_name, sanitize_name, team_config_path,
    team_dir, team_lead_name, team_task_dir, write_team_file,
};
#[allow(unused_imports)]
pub use team_memory::{
    TeamMemoryMemberSync, TeamMemorySyncManifest, build_team_memory_sync_manifest,
    read_team_memory_sync_manifest, team_memory_sync_path, write_team_memory_sync_manifest,
};
pub use teammate_lifecycle::{
    TeammateResult, TeammateRuntime, TeammateState, handle_shutdown_request, poll_leader_inbox,
    process_inbox_messages, run_teammate_loop, send_idle_notification,
};
pub use teammate_mailbox::{
    IdleReason, StructuredProtocolMessage, TeammateMessage, clear_mailbox,
    is_structured_protocol_message, mark_messages_as_read, parse_structured_protocol, read_mailbox,
    read_unread_messages, write_to_mailbox,
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
    pub task_v2_manager: crate::tools::task_v2::SharedTaskV2Manager,
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
