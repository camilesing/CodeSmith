#![allow(dead_code)]

//! Sandbox module for secure command execution (re-export shim).
//!
//! The sandbox data types, `SandboxManager`, the platform detection helpers
//! (`get_platform_sandbox` / `is_sandbox_available`), and the per-platform
//! executors (seatbelt / landlock / seccomp / bwrap / windows /
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
#[cfg(target_os = "macos")]
pub use codesmith_agent_runtime::sandbox::seatbelt;
#[cfg(target_os = "windows")]
pub use codesmith_agent_runtime::sandbox::windows;
#[cfg(target_os = "linux")]
pub use codesmith_agent_runtime::sandbox::{bwrap, landlock, seccomp};

pub use backend::SandboxExecRequest;
pub use codesmith_agent_runtime::sandbox::{
    CommandSpec, ExecEnv, SandboxManager, SandboxType, get_platform_sandbox, is_sandbox_available,
};
pub use policy::SandboxPolicy;
pub use runtime::{
    SandboxBackendKind, SandboxDecision, SandboxFilesystemConfig, SandboxNetworkConfig,
    SandboxRuntimeConfig,
};
