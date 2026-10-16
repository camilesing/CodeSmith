//! Re-export of [`codesmith_agent_runtime::child_env`].
//!
//! Canonical home is now `codesmith_agent_runtime`. This shim keeps the
//! `crate::child_env::<item>` paths that remain in the CLI working; new
//! code should import from the runtime crate directly.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::child_env` directly.
pub use codesmith_agent_runtime::child_env::{apply_to_command, string_map_env};
