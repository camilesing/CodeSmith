use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

use super::{SandboxPolicy, SandboxType};
pub use codesmith_agent_runtime::sandbox::{SandboxBackendKind, SandboxFilesystemConfig, SandboxNetworkConfig, SandboxRuntimeConfig, current_platform, is_managed_domain, managed_domains};


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDecision {
    pub sandbox_requested: bool,
    pub sandbox_effective: bool,
    pub sandbox_policy: String,
    pub sandbox_backend: Option<String>,
    pub sandbox_unavailable_reason: Option<String>,
    pub sandbox_fallback_allowed: bool,
    pub sandbox_excluded_command: Option<String>,
    pub sandbox_fail_closed: bool,
}

impl SandboxDecision {
    #[must_use]
    pub fn unsandboxed(policy: &SandboxPolicy) -> Self {
        Self {
            sandbox_requested: false,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: false,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn enforcing(policy: &SandboxPolicy, backend: SandboxType) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: true,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: Some(backend.to_string()),
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: false,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn unavailable(
        policy: &SandboxPolicy,
        reason: impl Into<String>,
        fail_closed: bool,
    ) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: Some(reason.into()),
            sandbox_fallback_allowed: !fail_closed,
            sandbox_excluded_command: None,
            sandbox_fail_closed: fail_closed,
        }
    }

    #[must_use]
    pub fn disabled(policy: &SandboxPolicy, reason: impl Into<String>) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: Some(reason.into()),
            sandbox_fallback_allowed: true,
            sandbox_excluded_command: None,
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn excluded(policy: &SandboxPolicy, command: impl Into<String>) -> Self {
        Self {
            sandbox_requested: true,
            sandbox_effective: false,
            sandbox_policy: policy.name().to_string(),
            sandbox_backend: None,
            sandbox_unavailable_reason: None,
            sandbox_fallback_allowed: true,
            sandbox_excluded_command: Some(command.into()),
            sandbox_fail_closed: false,
        }
    }

    #[must_use]
    pub fn allows_execution(&self) -> bool {
        self.sandbox_effective || !self.sandbox_fail_closed
    }
}


