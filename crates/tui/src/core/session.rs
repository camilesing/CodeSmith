//! Session state management for the core engine.
//!
//! Re-exported from `codesmith_agent_runtime::session` so the engine and TUI
//! share the canonical `Session` / `SessionUsage` / `RecentReadFile` types
//! and the `Session::new` / `add_message` / `rebuild_working_set` /
//! `record_read_file_result` constructors. The implementation lives in the
//! runtime; the TUI only consumes it.
pub use codesmith_agent_runtime::session::*;
