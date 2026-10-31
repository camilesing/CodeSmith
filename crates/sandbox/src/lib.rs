//! Execution-policy and sandbox plane — the DSH `sandbox/*` +
//! policy-family counterpart, split out of `codesmith-agent-runtime` in
//! round 9.
//!
//! - [`runtime`]: portable policy/backend types (`SandboxPolicy`,
//!   `SandboxBackend` trait) and the platform executors — Seatbelt (macOS),
//!   Landlock / bwrap (Linux), the Windows contract stub — plus
//!   `SandboxManager`.
//! - [`command_safety`] / [`execpolicy`] / [`bash_ast`]: the command
//!   classifier (trust prefixes, danger patterns, tree-sitter-bash segment
//!   facts) and the exec-policy pattern language.
//! - [`network_policy`]: managed-domain network allowlisting.
//! - [`sanitization`]: untrusted-content demotion wrappers
//!   (`<external_content>`) and unicode sanitization.
//! - [`shell_dispatcher`]: shell dialect detection (bash / zsh / pwsh / cmd).
//!
//! The mode-coupled default policy (`sandbox_policy_for_mode`) stays with
//! the engine (it takes `AppMode`); everything here is mode-free.

pub mod bash_ast;
pub mod child_env;
pub mod command_safety;
pub mod execpolicy;
pub mod network_policy;
pub mod runtime;

pub mod sanitization;
pub mod shell_dispatcher;
#[cfg(test)]
mod test_support;

pub use runtime::{
    CommandSpec, ExecEnv, SandboxBackend, SandboxDecision, SandboxExecRequest, SandboxKind,
    SandboxManager, SandboxOutput, SandboxPolicy, SandboxRuntimeConfig, SandboxType, WritableRoot,
};
