#![allow(dead_code)]

//! Sandbox module for secure command execution (re-export shim).
//!
//! The sandbox data types, `SandboxManager`, the platform detection helpers
//! (`get_platform_sandbox` / `is_sandbox_available`), and the per-platform
//! executors (seatbelt / landlock / bwrap / windows /
//! process_hardening) now live in `codesmith_agent_runtime::sandbox`. This
//! module keeps the TUI-local `backend` / `opensandbox` / `policy` /
//! `runtime` submodules (which depend on `crate::config::Config` and
//! `crate::command_safety`) and re-exports everything else so historical
//! `crate::sandbox::*` paths keep resolving.

pub mod backend;
pub mod opensandbox;
pub mod policy;
pub mod runtime;

pub use codesmith_agent_runtime::sandbox::process_hardening;

pub use codesmith_agent_runtime::sandbox::{
    CommandSpec, ExecEnv, SandboxManager, SandboxType, get_platform_sandbox,
};
pub use policy::SandboxPolicy;
pub use runtime::{
    SandboxBackendKind, SandboxFilesystemConfig, SandboxNetworkConfig, SandboxRuntimeConfig,
};
