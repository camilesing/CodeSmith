//! Coherence ladder re-exported from `codesmith-agent-runtime`.
//!
//! Types and logic live in agent-runtime; this module re-exports them so
//! existing `crate::core::coherence::` references keep resolving.

pub use codesmith_agent_runtime::coherence::*;
