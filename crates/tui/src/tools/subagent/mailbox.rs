//! Mailbox abstraction for sub-agent runtime coordination.
//!
//! Re-exported from `codesmith_agent_runtime::mailbox` so the engine and
//! TUI share the same sub-agent mailbox types and coordination primitives.
//! The implementation (channel, sequence counter, close-as-cancel) lives in
//! the runtime; the TUI only consumes it.
pub use codesmith_agent_runtime::mailbox::*;
