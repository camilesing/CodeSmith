//! Re-export of [`codesmith_agent_runtime::session_store`].
//!
//! Session persistence (record types, format-governance decode pipeline,
//! the `SessionStore` seam, and the file provider) moved to the framework
//! crate so the app-server thread runtime can resume REPL sessions through
//! the same seam. This shim keeps every historical `crate::session_manager`
//! path working; new code should import from the runtime crate directly.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::session_store` directly.
pub use codesmith_agent_runtime::session_store::{
    FileSessionStore as SessionManager, SavedSession, SessionMetadata, create_saved_session,
    create_saved_session_with_id_and_mode, create_saved_session_with_mode, default_sessions_dir,
    format_session_line, prune_workspace_snapshots, truncate_id, update_session,
    workspace_scope_matches,
};
