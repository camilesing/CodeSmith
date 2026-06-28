//! Re-export of the LSP language registry.
//!
//! The canonical home is [`codesmith_agent_runtime::lsp_registry`]; this
//! module re-exports it at the historical `crate::lsp::registry` path so
//! existing call sites (`registry::server_for`, `registry::detect_language`,
//! `registry::Language`) keep resolving.

pub use codesmith_agent_runtime::lsp_registry::*;
