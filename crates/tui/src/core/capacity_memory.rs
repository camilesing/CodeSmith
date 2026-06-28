//! Capacity memory persistence re-exported from `codesmith-agent-runtime`.
//!
//! Types and logic live in agent-runtime; this module re-exports them so
//! existing `crate::core::capacity_memory::` references keep resolving.

pub use codesmith_agent_runtime::capacity_memory::*;
