//! Re-export of [`codesmith_agent_runtime::command_safety`].
//!
//! Canonical home is now `codesmith_agent_runtime`. This shim keeps the
//! `crate::command_safety::<item>` paths that remain in the TUI working;
//! new code should import from the runtime crate directly.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::command_safety` directly.
pub use codesmith_agent_runtime::command_safety::{SafetyLevel, analyze_command};
