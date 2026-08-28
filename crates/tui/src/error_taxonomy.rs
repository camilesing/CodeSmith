//! Error taxonomy re-exported from `codesmith-agent-runtime`.
//!
//! Types and logic live in agent-runtime; this module re-exports them so
//! existing `crate::error_taxonomy::` references keep resolving.

pub use codesmith_agent_runtime::error_taxonomy::*;
