//! Mailbox abstraction for sub-agent runtime coordination.
//!
//! Re-exported from `codesmith_agent_runtime::mailbox` so the engine and
//! CLI share the same sub-agent mailbox types and coordination primitives.
//! The implementation (channel, sequence counter, close-as-cancel) lives in
//! the runtime; the CLI only consumes it.
pub use codesmith_agent_runtime::mailbox::*;
