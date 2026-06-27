use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

use super::{SandboxPolicy, SandboxType};

/// Runtime sandbox configuration after merging legacy top-level keys, the
/// `[sandbox]` table, environment overrides, and per-mode policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRuntimeConfig {
    pub enabled: bool,
    pub fail_if_unavailable: bool,
    pub enabled_platforms: Vec<String>,
    pub excluded_commands: Vec<String>,
    pub auto_allow_bash_if_sandboxed: bool,
    pub prefer_bwrap: bool,
    pub backend: SandboxBackendKind,
    pub filesystem: SandboxFilesystemConfig,
    pub network: SandboxNetworkConfig,
}

impl Default for SandboxRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fail_if_unavailable: false,
            enabled_platforms: Vec::new(),
            excluded_commands: Vec::new(),
            auto_allow_bash_if_sandboxed: false,
            prefer_bwrap: false,
            backend: SandboxBackendKind::Local,
            filesystem: SandboxFilesystemConfig::default(),
            network: SandboxNetworkConfig::default(),
        }
    }
}

impl SandboxRuntimeConfig {
    #[must_use]
    pub fn platform_enabled(&self) -> bool {
        if self.enabled_platforms.is_empty() {
            return true;
        }
        let current = current_platform();
        self.enabled_platforms
            .iter()
            .any(|platform| platform.trim().eq_ignore_ascii_case(current))
    }

    #[must_use]
    pub fn command_is_excluded(&self, program: &str, command_line: &str) -> bool {
        self.excluded_commands.iter().any(|entry| {
            let trimmed = entry.trim();
            !trimmed.is_empty()
                && (trimmed.eq_ignore_ascii_case(program)
                    || command_line.trim_start().starts_with(trimmed))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SandboxFilesystemConfig {
    pub mode: Option<String>,
    pub writable_roots: Vec<PathBuf>,
    pub allow_read: Vec<PathBuf>,
    pub deny_read: Vec<PathBuf>,
    pub allow_write: Vec<PathBuf>,
    pub deny_write: Vec<PathBuf>,
    pub exclude_tmpdir: Option<bool>,
    pub exclude_slash_tmp: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SandboxNetworkConfig {
    pub enabled: Option<bool>,
    pub allow_managed_domains_only: bool,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackendKind {
    #[default]
    Local,
    OpenSandbox,
}

impl SandboxBackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::OpenSandbox => "opensandbox",
        }
    }
}

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

#[must_use]
pub fn current_platform() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "unknown"
    }
}

#[must_use]
pub fn managed_domains() -> BTreeSet<String> {
    [
        "api.deepseek.com",
        "chat.deepseek.com",
        "deepseek.com",
        "api.anthropic.com",
        "api.openai.com",
        "openrouter.ai",
        "api.tavily.com",
        "api.bochaai.com",
        "metaso.cn",
        "www.googleapis.com",
        "github.com",
        "raw.githubusercontent.com",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[must_use]
pub fn is_managed_domain(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    managed_domains()
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}
