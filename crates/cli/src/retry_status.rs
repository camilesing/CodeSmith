//! Re-export of [`codesmith_agent_runtime::retry_status`].
//!
//! Canonical home is now `codesmith_agent_runtime`. This shim keeps the
//! `crate::retry_status::<item>` paths that remain in the CLI working; new
//! code should import from the runtime crate directly.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::retry_status` directly.
pub use codesmith_agent_runtime::retry_status::clear;
