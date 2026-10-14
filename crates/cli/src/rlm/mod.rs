//! Recursive Language Model (RLM) loop — paper-spec Algorithm 1.
//!
//! Implements Zhang, Kraska & Khattab (arXiv:2512.24601, §2 Algorithm 1).
//!
//! Migrated to `codesmith_agent_runtime::rlm`; re-exported here so the
//! historical `crate::rlm::session::<item>` paths keep resolving for the
//! TUI-side callers (`runtime_threads`, `core/engine/tool_setup`). The rest
//! of the runtime `rlm` surface is no longer re-exported — import it from
//! `codesmith_agent_runtime::rlm` directly.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::rlm` directly.
pub use codesmith_agent_runtime::rlm::session;
