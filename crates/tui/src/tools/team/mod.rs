//! Agent Teams — multi-agent coordination with shared task lists and
//! inter-teammate messaging.
//!
//! Ports the Claude Code TypeScript Agent Teams system into CodeSmith's
//! Rust architecture. Key components:
//! - Team file persistence (config.json)
//! - File-based teammate mailbox (flock-concurrent inbox)
//! - Team lifecycle tools (create, delete, send_message)
//! - In-process teammate lifecycle (idle/shutdown protocol)

pub mod backend;
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

pub use codesmith_agent_runtime::tool_state::team::*;
pub use inbox_poller::run_leader_inbox_poller;
pub use protocol_handlers::new_shared_permission_registry;
pub use send_message::SendMessageTool;
pub use team_create::TeamCreateTool;
pub use team_delete::TeamDeleteTool;
pub use team_discovery::{add_member, read_team_config, set_member_inactive, write_team_config};
pub use team_file::{
    TeamFile, TeamMember, active_teammate_count, active_teammates, create_team_file,
    delete_team_directories, find_member_by_name, format_lead_agent_id, read_team_file,
    remove_member_by_name, sanitize_name, team_config_path, team_lead_name,
};
#[allow(unused_imports)]
pub use team_memory::{
    TeamMemoryMemberSync, TeamMemorySyncManifest, build_team_memory_sync_manifest,
    read_team_memory_sync_manifest, team_memory_sync_path, write_team_memory_sync_manifest,
};
pub use teammate_lifecycle::{TeammateRuntime, process_inbox_messages, run_teammate_loop};
pub use teammate_mailbox::{
    IdleReason, StructuredProtocolMessage, TeammateMessage, is_structured_protocol_message,
    mark_messages_as_read, parse_structured_protocol, read_unread_messages, write_to_mailbox,
};
