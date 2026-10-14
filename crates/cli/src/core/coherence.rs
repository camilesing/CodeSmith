//! Coherence ladder re-exported from `codesmith-agent-runtime`.
//!
//! Types and logic live in agent-runtime; this module re-exports them so
//! existing `crate::core::coherence::` references keep resolving.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::coherence` directly.
pub use codesmith_agent_runtime::coherence::CoherenceState;
