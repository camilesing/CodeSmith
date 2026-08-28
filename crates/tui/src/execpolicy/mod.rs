//! Re-export of [`codesmith_agent_runtime::execpolicy`] (Phase 6 §6b-1).
//!
//! Canonical home is now `codesmith_agent_runtime`. This glob shim flattens
//! the runtime module's public items (including public submodules) so
//! `crate::execpolicy::<item>` and nested `crate::execpolicy::<sub>::…` paths
//! in the TUI keep working until later steps rewire them onto the runtime
//! crate directly.
pub use codesmith_agent_runtime::execpolicy::*;
