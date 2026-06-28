//! Re-export of [`codesmith_agent::models`] (extracted in Phase 6 §6a).
//!
//! The canonical home for API request/response models is now the
//! `codesmith_agent` crate. This shim keeps `crate::models::*` paths in the
//! TUI working until later steps rewire them onto `codesmith_agent` directly.

pub use codesmith_agent::models::*;
