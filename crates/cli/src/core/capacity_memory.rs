//! Capacity memory persistence re-exported from `codesmith-agent-runtime`.
//!
//! Types and logic live in agent-runtime; this module re-exports them so
//! existing `crate::core::capacity_memory::` references keep resolving.

// Only the TUI test module consumes this re-export today; the cfg gate keeps
// the non-test bin pass from flagging (and `cargo clippy --fix` deleting) it.
#[cfg(test)]
pub use codesmith_agent_runtime::capacity_memory::*;
