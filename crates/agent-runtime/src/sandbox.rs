//! Runtime sandbox configuration types.
//!
//! Extracted from `crates/tui/src/sandbox/runtime.rs`. `SandboxDecision` stays
//! in tui because it references `SandboxPolicy`/`SandboxType` from the sandbox
//! backend layer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

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
