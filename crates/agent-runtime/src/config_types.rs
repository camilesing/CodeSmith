//! Configuration value types used by `EngineConfig`.
//!
//! Extracted from `crates/tui/src/config.rs` and `tools/large_output_router.rs`
//! so `EngineConfig` can live in agent-runtime without a tui dependency.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_MAX_SUBAGENTS: usize = 10;

/// Search provider enumeration — selects which backend `web_search` uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    /// Bing HTML scraping. No API key needed.
    Bing,
    /// DuckDuckGo HTML scraping with Bing fallback. No API key needed.
    #[default]
    #[serde(alias = "duckduckgo")]
    DuckDuckGo,
    /// Tavily AI Search API (<https://tavily.com>). Requires api_key.
    Tavily,
    /// Bocha AI Search API (<https://bochaai.com>). Requires api_key.
    Bocha,
    /// Metaso AI Search API (<https://metaso.cn>). Uses built-in default key
    /// or `METASO_API_KEY` env var; configurable via `[search] api_key`.
    #[serde(alias = "metaso")]
    Metaso,
    /// Baidu AI Search API (<https://qianfan.baidubce.com>). Requires api_key.
    #[serde(
        alias = "baidu-search",
        alias = "baidu_ai_search",
        alias = "baidu_search",
        alias = "baidu-ai-search"
    )]
    Baidu,
    /// Volcengine Ark web_search via Responses API. Requires api_key.
    /// Free tier: 20K queries/month per API key. Falls back to
    /// `VOLCENGINE_API_KEY` / `VOLCENGINE_ARK_API_KEY` / `ARK_API_KEY`
    /// env vars when `[search] api_key` is not set.
    #[serde(
        alias = "volcengine",
        alias = "ark",
        alias = "volc",
        alias = "volcengine-ark",
        alias = "volcengine_ark",
        alias = "volc-ark"
    )]
    Volcengine,
}

impl SearchProvider {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bing" => Some(Self::Bing),
            "duckduckgo" | "duck-duck-go" | "duck_duck_go" | "ddg" => Some(Self::DuckDuckGo),
            "tavily" => Some(Self::Tavily),
            "bocha" => Some(Self::Bocha),
            "metaso" => Some(Self::Metaso),
            "baidu" | "baidu-search" | "baidu_search" | "baidu-ai-search" | "baidu_ai_search" => {
                Some(Self::Baidu)
            }
            "volcengine" | "ark" | "volc" | "volcengine-ark" => Some(Self::Volcengine),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bing => "bing",
            Self::DuckDuckGo => "duckduckgo",
            Self::Tavily => "tavily",
            Self::Bocha => "bocha",
            Self::Metaso => "metaso",
            Self::Baidu => "baidu",
            Self::Volcengine => "volcengine",
        }
    }
}

/// Vision model configuration for the `image_analyze` tool.
/// Uses an OpenAI-compatible vision model API.
#[derive(Debug, Clone, Deserialize)]
pub struct VisionModelConfig {
    /// Model identifier (e.g., "gemini-3.1-flash-lite-preview").
    pub model: String,
    /// API key for the vision model. Inherits from main config if not specified.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL for the vision model API. Defaults to OpenAI.
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Default token threshold above which a tool result is routed through the
/// workshop. Matches the issue spec of 4 096 tokens.
pub const DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS: usize = 4_096;

/// `[workshop]` section in `config.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkshopConfig {
    /// Token threshold above which tool results are routed through the workshop
    /// synthesis sub-agent. Default: [`DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS`].
    #[serde(default)]
    pub large_output_threshold_tokens: Option<usize>,

    /// Per-tool threshold overrides (tool name → token limit). A tool whose
    /// name appears here uses this limit instead of
    /// `large_output_threshold_tokens`.
    #[serde(default)]
    pub per_tool_thresholds: Option<HashMap<String, usize>>,
}

impl WorkshopConfig {
    /// Resolve the effective threshold for the given tool name.
    #[must_use]
    pub fn threshold_for(&self, tool_name: &str) -> usize {
        if let Some(per_tool) = self.per_tool_thresholds.as_ref()
            && let Some(&limit) = per_tool.get(tool_name)
        {
            return limit;
        }
        self.large_output_threshold_tokens
            .unwrap_or(DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS)
    }
}

/// How a user wants to replace or disable a built-in tool.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOverride {
    /// Run a local script file. The script receives the tool's JSON input
    /// on stdin and must return a JSON `ToolResult` on stdout.
    Script {
        /// Path to the script (absolute, or relative to `~/.codesmith/tools/`).
        path: String,
        /// Optional static arguments prepended before the tool's JSON input.
        #[serde(default)]
        args: Option<Vec<String>>,
    },
    /// Run an external command. The command receives the tool's JSON input
    /// on stdin and must return a JSON `ToolResult` on stdout.
    Command {
        /// The command to run (binary name or absolute path).
        command: String,
        /// Optional static arguments prepended before the tool's JSON input.
        #[serde(default)]
        args: Option<Vec<String>>,
    },
    /// Completely disable a built-in tool. The tool will not appear in the
    /// model-visible catalog and cannot be called.
    Disabled,
}

/// Model-visible tool catalog controls (`[tools]` table in config.toml).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolsConfig {
    /// Native tool names to keep loaded even when they are outside the small
    /// default core catalog. Unknown names are harmless and simply never match.
    #[serde(default)]
    pub always_load: Vec<String>,

    /// Optional directory to scan for plugin tool scripts. Scripts with a
    /// frontmatter header (`# name:`, `# description:`, `# schema:`) are
    /// auto-discovered and registered as tools.
    ///
    /// Defaults to `~/.codesmith/tools/` when `None`.
    #[serde(default)]
    pub plugin_dir: Option<String>,

    /// Per-tool overrides keyed by built-in tool name.
    /// Each override replaces or disables the named tool.
    #[serde(default)]
    pub overrides: Option<HashMap<String, ToolOverride>>,
}

/// Default per-step DeepSeek API timeout for sub-agent requests, in seconds.
/// Matches the legacy hardcoded value so existing configs keep their old
/// behavior when `[subagents] api_timeout_secs` is unset (#1806, #1808).
pub const DEFAULT_SUBAGENT_API_TIMEOUT_SECS: u64 = 120;
