//! Turn context and tracking -- re-exported from agent-runtime.
//!
//! The implementation lives in codesmith_agent_runtime::turn; this
//! module re-exports it so in-tree consumers keep resolving via
//! crate::core::turn.

// Only the TUI test module consumes this re-export today; the cfg gate keeps
// the non-test bin pass from flagging (and `cargo clippy --fix` deleting) it.
#[cfg(test)]
pub use codesmith_agent_runtime::turn::*;
