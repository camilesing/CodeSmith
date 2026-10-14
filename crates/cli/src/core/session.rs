//! Session state management for the core engine.
//!
//! The implementation (the `Session` / `SessionUsage` / `RecentReadFile`
//! types, their constructors, and the free function
//! `rebuild_recent_read_files_from_messages`) lives in
//! `codesmith_agent_runtime::session`. This shim re-exports the `Session`
//! type for `crate::core::engine`'s `use super::session::Session`; every
//! other consumer imports from the runtime crate directly.
// TODO(shim-removal): remove this shim once the engine imports `Session`
// from `codesmith_agent_runtime::session` directly.
pub use codesmith_agent_runtime::session::Session;
