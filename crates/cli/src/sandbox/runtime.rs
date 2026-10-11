//! Runtime sandbox configuration re-exported from `codesmith-agent-runtime`.
//!
//! `SandboxDecision` and the runtime-config types now live in
//! `codesmith-agent-runtime`'s `sandbox` module so they can cross the
//! `Arc<dyn HostServices>` boundary. This file re-exports them so existing
//! `crate::sandbox::runtime::*` paths keep resolving.

pub use codesmith_agent_runtime::sandbox::{
    SandboxBackendKind, SandboxFilesystemConfig, SandboxNetworkConfig, SandboxRuntimeConfig,
    managed_domains,
};
