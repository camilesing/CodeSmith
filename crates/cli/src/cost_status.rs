//! Re-export of [`codesmith_agent_runtime::cost_status`].
//!
//! Canonical home is now `codesmith_agent_runtime`. This shim keeps the
//! `crate::cost_status::report` calls in seam-manager working; new code
//! should import from the runtime crate directly.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::cost_status` directly.
pub use codesmith_agent_runtime::cost_status::report;
