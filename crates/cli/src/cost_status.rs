//! Re-export of [`codesmith_agent_runtime::cost_status`] (Phase 6 §6b-1).
//!
//! Canonical home is now `codesmith_agent_runtime`. This glob shim flattens
//! the runtime module's public items so `crate::cost_status::<item>` paths in
//! the TUI keep working until later steps rewire them onto the runtime crate
//! directly.
pub use codesmith_agent_runtime::cost_status::*;
