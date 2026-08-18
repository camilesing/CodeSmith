//! Configuration value types used by `EngineConfig`.
//!
//! Extracted from `crates/tui/src/config.rs` and `tools/large_output_router.rs`
//! so `EngineConfig` can live in agent-runtime without a tui dependency.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_MAX_SUBAGENTS: usize = 10;

/// Hard cap on the number of concurrent sub-agents a single session may spawn.
/// Distinct from [`DEFAULT_MAX_SUBAGENTS`] (the value used when the user leaves
/// `[subagents] max_concurrent` unset): this is the ceiling that user-set
/// values are clamped to. Migrated from the TUI so the sub-agent tool (which
/// moves to `codesmith-tool-impls`) can reference it without a TUI dependency.
pub const MAX_SUBAGENTS: usize = 20;

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

/// Utility model configuration — a cheap/fast secondary LLM for background
/// assists (workshop large-output synthesis #548, auto-route classification,
/// Flash seams). When the table is absent every assist falls back to the main
/// model and behaviour is unchanged.
#[derive(Debug, Clone, Deserialize)]
pub struct UtilityModelConfig {
    /// Model identifier (e.g., "deepseek-v4-flash"). Setting the table
    /// enables the utility model.
    pub model: String,
    /// Provider for the utility model. Inherits the main provider when unset;
    /// a different provider here builds a dedicated second client.
    #[serde(default)]
    pub provider: Option<ApiProvider>,
    /// API key for the utility model. Inherits from main config if not specified.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL for the utility model. Inherits from main config if not specified.
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

// ============================================================================
// Provider Capability Matrix — migrated from the TUI (`crates/tui/src/config.rs`)
// as part of the engine-closure extraction. The engine body references these
// via `crate::config::{ApiProvider, provider_capability}`; the `crate::models::`
// paths resolve here too (agent-runtime re-exports `codesmith_agent::models`).
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiProvider {
    // §B3 slice 52 — `DeepseekCN` folded onto `Deepseek`; the `deepseek-cn` family
    // (incl. the old `deepseek_cn` snake_case rename of the deleted variant) now
    // collapses here, mirroring `codesmith_config::ProviderKind`.
    #[serde(
        alias = "deepseek-cn",
        alias = "deepseek_china",
        alias = "deepseekcn",
        alias = "deepseek-china",
        alias = "deepseek_cn"
    )]
    Deepseek,
    NvidiaNim,
    Openai,
    Atlascloud,
    WanjieArk,
    Volcengine,
    Openrouter,
    XiaomiMimo,
    Novita,
    Fireworks,
    Siliconflow,
    Moonshot,
    Sglang,
    Vllm,
    Ollama,
    Anthropic,
}

impl ApiProvider {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deepseek" | "deep-seek" | "deepseek-cn" | "deepseek_china" | "deepseekcn"
            | "deepseek-china" | "deepseek_cn" => Some(Self::Deepseek),
            "nvidia" | "nvidia-nim" | "nvidia_nim" | "nim" => Some(Self::NvidiaNim),
            "openai" | "open-ai" => Some(Self::Openai),
            "atlascloud" | "atlas-cloud" | "atlas_cloud" | "atlas" => Some(Self::Atlascloud),
            "wanjie" | "wanjie-ark" | "wanjie_ark" | "ark-wanjie" | "ark_wanjie" | "wanjieark"
            | "wanjie-maas" | "wanjie_maas" | "wanjiemaas" => Some(Self::WanjieArk),
            "volcengine" | "volcengine-ark" | "volcengine_ark" | "ark" | "volc-ark"
            | "volcengineark" => Some(Self::Volcengine),
            "openrouter" | "open_router" => Some(Self::Openrouter),
            "xiaomi-mimo" | "xiaomi_mimo" | "xiaomimimo" | "mimo" | "xiaomi" => {
                Some(Self::XiaomiMimo)
            }
            "novita" => Some(Self::Novita),
            "fireworks" | "fireworks-ai" => Some(Self::Fireworks),
            "siliconflow" | "silicon-flow" | "silicon_flow" => Some(Self::Siliconflow),
            "moonshot" | "moonshot-ai" | "kimi" | "kimi-k2" => Some(Self::Moonshot),
            "sglang" | "sg-lang" => Some(Self::Sglang),
            "vllm" | "v-llm" => Some(Self::Vllm),
            "ollama" | "ollama-local" => Some(Self::Ollama),
            "anthropic" | "claude" | "anthropic-claude" | "claude-ai" => Some(Self::Anthropic),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::NvidiaNim => "nvidia-nim",
            Self::Openai => "openai",
            Self::Atlascloud => "atlascloud",
            Self::WanjieArk => "wanjie-ark",
            Self::Volcengine => "volcengine",
            Self::Openrouter => "openrouter",
            Self::XiaomiMimo => "xiaomi-mimo",
            Self::Novita => "novita",
            Self::Fireworks => "fireworks",
            Self::Siliconflow => "siliconflow",
            Self::Moonshot => "moonshot",
            Self::Sglang => "sglang",
            Self::Vllm => "vllm",
            Self::Ollama => "ollama",
            Self::Anthropic => "anthropic",
        }
    }

    /// Human-friendly label for picker UIs / status chips.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Deepseek => "DeepSeek",
            Self::NvidiaNim => "NVIDIA NIM",
            Self::Openai => "OpenAI-compatible",
            Self::Atlascloud => "AtlasCloud",
            Self::WanjieArk => "Wanjie Ark",
            Self::Volcengine => "Volcengine Ark",
            Self::Openrouter => "OpenRouter",
            Self::XiaomiMimo => "Xiaomi MiMo",
            Self::Novita => "Novita AI",
            Self::Fireworks => "Fireworks AI",
            Self::Siliconflow => "SiliconFlow",
            Self::Moonshot => "Moonshot/Kimi",
            Self::Sglang => "SGLang",
            Self::Vllm => "vLLM",
            Self::Ollama => "Ollama",
            Self::Anthropic => "Anthropic Claude",
        }
    }

    /// All providers, in the order shown in the picker.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Deepseek,
            Self::NvidiaNim,
            Self::Openai,
            Self::Atlascloud,
            Self::WanjieArk,
            Self::Volcengine,
            Self::Openrouter,
            Self::XiaomiMimo,
            Self::Novita,
            Self::Fireworks,
            Self::Siliconflow,
            Self::Moonshot,
            Self::Sglang,
            Self::Vllm,
            Self::Ollama,
            Self::Anthropic,
        ]
    }
}

// ============================================================================
// Provider Capability Matrix
// ============================================================================

/// Known capabilities for a provider + resolved-model combination.
///
/// Returned by [`provider_capability`] to describe what a given provider
/// supports for the resolved model string.  All fields are derived from
/// static knowledge (release docs, API guides) rather than live API probes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ProviderCapability {
    /// Canonical provider identifier.
    pub provider: ApiProvider,
    /// Resolved model identifier that will be sent in the API payload.
    pub resolved_model: String,
    /// Context window in tokens (the maximum input the model can accept).
    pub context_window: u32,
    /// Official maximum output tokens for this combo.
    ///
    /// This is model metadata for diagnostics and CI policy. Normal turns use
    /// a separate, more conservative request cap in the engine.
    pub max_output: u32,
    /// Whether the provider+model supports thinking/reasoning mode.
    pub thinking_supported: bool,
    /// Whether the provider returns prompt-cache telemetry fields.
    pub cache_telemetry_supported: bool,
    /// Which request-payload dialect the provider uses.
    pub request_payload_mode: RequestPayloadMode,
    /// Deprecation metadata for compatibility aliases that are still accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_deprecation: Option<ModelAliasDeprecation>,
}

pub const DEEPSEEK_ALIAS_RETIREMENT_DATE: &str = "2026-07-24";
pub const DEEPSEEK_ALIAS_RETIREMENT_UTC: &str = "2026-07-24T15:59:00Z";
pub const DEEPSEEK_ALIAS_REPLACEMENT: &str = "deepseek-v4-flash";

/// Upstream retirement metadata for a model alias that remains compatible.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ModelAliasDeprecation {
    pub alias: String,
    pub replacement: String,
    pub retirement_date: String,
    pub retirement_utc: String,
    pub notice: String,
}

/// Which request-payload dialect the provider speaks.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum RequestPayloadMode {
    /// Standard OpenAI-compatible `/v1/chat/completions` payload.
    ChatCompletions,
    /// Anthropic-native `/v1/messages` payload.
    Messages,
}

/// Resolve the provider capability for a given [`ApiProvider`] and resolved
/// model string.
///
/// The `resolved_model` should be the final model identifier that will appear
/// in the API payload (after normalization / provider-specific mapping).
#[must_use]
pub fn provider_capability(provider: ApiProvider, resolved_model: &str) -> ProviderCapability {
    if matches!(
        provider,
        ApiProvider::Openai | ApiProvider::Atlascloud | ApiProvider::Moonshot
    ) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: crate::models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS,
            max_output: 4096,
            thinking_supported: false,
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ApiProvider::XiaomiMimo) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: 1_000_000,
            max_output: 128_000,
            thinking_supported: true,
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ApiProvider::Ollama) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: 8192,
            max_output: 4096,
            thinking_supported: false,
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ApiProvider::Anthropic) {
        // Capabilities are inferred from the model name; we don't bundle a
        // catalog. Recent Claude families (3.5+, 4+) all support thinking
        // and prompt-caching telemetry, but we conservatively gate thinking
        // behind a name match so older snapshots don't lie.
        let model_lower = resolved_model.to_ascii_lowercase();
        let thinking_supported = model_lower.contains("claude-")
            && (model_lower.contains("opus-4")
                || model_lower.contains("sonnet-4")
                || model_lower.contains("3-7")
                || model_lower.contains("3.7")
                || model_lower.contains("haiku-4"));
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: 200_000,
            max_output: 8_192,
            thinking_supported,
            cache_telemetry_supported: true,
            request_payload_mode: RequestPayloadMode::Messages,
            alias_deprecation: None,
        };
    }

    let model_lower = resolved_model.to_ascii_lowercase();
    let alias_deprecation = if matches!(provider, ApiProvider::Deepseek) {
        deepseek_alias_deprecation(&model_lower)
    } else {
        None
    };
    let is_v4_pro = model_lower.contains("v4-pro") || model_lower == "deepseek-v4pro";
    let is_v4_flash = model_lower.contains("v4-flash")
        || model_lower == "deepseek-v4flash"
        || model_lower == "deepseek-v4"
        || alias_deprecation.is_some();
    let is_reasoner = matches!(provider, ApiProvider::WanjieArk)
        && (model_lower.contains("reasoner") || model_lower.contains("r1"));

    // Context window: V4-class models get 1M, everything else falls through
    // to the model's own lookup or a default.
    let context_window = if is_v4_pro || is_v4_flash {
        crate::models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
    } else {
        crate::models::context_window_for_model(resolved_model)
            .unwrap_or(crate::models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS)
    };

    // Max output tokens: official DeepSeek V4 API metadata lists 384K;
    // runtime request caps remain separate and more conservative.
    let max_output = if is_v4_pro || is_v4_flash {
        384_000
    } else {
        crate::models::max_output_tokens_for_model(resolved_model).unwrap_or(4096)
    };

    // Thinking support: V4 models support thinking on all providers, but
    // only when the model name matches the V4 family.
    let thinking_supported = is_v4_pro
        || is_v4_flash
        || is_reasoner
        || crate::models::model_supports_reasoning(resolved_model);

    // Cache telemetry: returned only by DeepSeek-native and NVIDIA NIM endpoints.
    let cache_telemetry_supported = matches!(
        provider,
        ApiProvider::Deepseek | ApiProvider::NvidiaNim | ApiProvider::Volcengine
    );

    // Request payload mode: all current providers use chat completions.
    let request_payload_mode = RequestPayloadMode::ChatCompletions;

    ProviderCapability {
        provider,
        resolved_model: resolved_model.to_string(),
        context_window,
        max_output,
        thinking_supported,
        cache_telemetry_supported,
        request_payload_mode,
        alias_deprecation,
    }
}

fn deepseek_alias_deprecation(model_lower: &str) -> Option<ModelAliasDeprecation> {
    match model_lower {
        "deepseek-chat" | "deepseek-reasoner" => Some(ModelAliasDeprecation {
            alias: model_lower.to_string(),
            replacement: DEEPSEEK_ALIAS_REPLACEMENT.to_string(),
            retirement_date: DEEPSEEK_ALIAS_RETIREMENT_DATE.to_string(),
            retirement_utc: DEEPSEEK_ALIAS_RETIREMENT_UTC.to_string(),
            notice: format!(
                "{model_lower} is a compatibility alias for {DEEPSEEK_ALIAS_REPLACEMENT} and is scheduled to retire on {DEEPSEEK_ALIAS_RETIREMENT_DATE}."
            ),
        }),
        _ => None,
    }
}
