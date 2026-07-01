//! API request/response models for `DeepSeek` and OpenAI-compatible endpoints.

use serde::{Deserialize, Serialize};

/// Context window used only for legacy CodeSmith model IDs that do not name a
/// newer V4 alias and do not carry an explicit `*k` suffix.
pub const LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS: u32 = 128_000;
pub const DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS: u32 = 1_000_000;
/// Last-resort compaction trigger when [`context_window_for_model`] returns
/// `None` (an unrecognised model id). Keep this provider-neutral and sized for
/// common 128K deployments rather than DeepSeek V4 cache economics.
pub const DEFAULT_COMPACTION_TOKEN_THRESHOLD: usize = 95_000;
const AUTO_COMPACT_SUMMARY_RESERVE_TOKENS: u32 = 20_000;
const AUTO_COMPACT_BUFFER_TOKENS: u32 = 13_000;

// === Core Message Types ===

/// Request payload for sending a message to the API.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// DeepSeek reasoning-effort tier: "off" | "low" | "medium" | "high" | "max".
    /// Translated by the client into DeepSeek's `reasoning_effort` + `thinking` fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
}

/// System prompt representation (plain text or structured blocks).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

/// A structured system prompt block.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// A chat message with role and content blocks.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

/// A single content block inside a message.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        caller: Option<ToolCaller>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_blocks: Option<Vec<serde_json::Value>>,
    },
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_search_tool_result")]
    ToolSearchToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
    #[serde(rename = "code_execution_tool_result")]
    CodeExecutionToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
}

/// Cache control metadata for tool definitions and blocks.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
}

/// Metadata describing who invoked a tool call.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ToolCaller {
    #[serde(rename = "type")]
    pub caller_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
}

/// Tool definition exposed to the model.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Tool {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_callers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_examples: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Container metadata for code-execution style server tools.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContainerInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Server-side tool usage counters.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct ServerToolUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution_requests: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_search_requests: Option<u32>,
}

/// Response payload for a message request.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageResponse {
    pub id: String,
    pub r#type: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerInfo>,
    pub usage: Usage,
}

/// Token usage metadata for a response.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// Approximate input tokens spent re-sending prior `reasoning_content`
    /// across user-message boundaries in DeepSeek V4 thinking-mode tool-calling
    /// turns (V4 §5.1.1 "Interleaved Thinking"). Estimated client-side at
    /// ~4 chars/token from the outgoing request body, before the model sees it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_replay_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<ServerToolUsage>,
}

/// Map known models to their approximate context window sizes.
///
/// Lookup order:
/// 1. An explicit `_Nk` suffix in the model name, for **any** vendor. This
///    lets self-hosted deployments advertise their window through the served
///    model name (e.g. a vLLM `--served-model-name qwen3-32b-256k`), which is
///    the only signal we have for non-DeepSeek/Claude models. The 1000-token
///    approximation is fine for compaction-threshold math.
/// 2. DeepSeek vendor heuristics (V4 family -> 1M, legacy -> 128K).
/// 3. Claude -> 200K.
#[must_use]
pub fn context_window_for_model(model: &str) -> Option<u32> {
    let lower = model.to_lowercase();
    if let Some(explicit_window) = explicit_context_window_hint(&lower) {
        return Some(explicit_window);
    }
    if lower.contains("deepseek") {
        if lower.contains("v4") {
            return Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS);
        }
        return Some(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS);
    }
    if let Some(window) = known_context_window_for_model(&lower) {
        return Some(window);
    }
    if lower.contains("claude") {
        return Some(200_000);
    }
    None
}

fn known_context_window_for_model(model_lower: &str) -> Option<u32> {
    match model_lower {
        "arcee-ai/trinity-large-thinking" => Some(262_144),
        "google/gemma-4-31b-it"
        | "google/gemma-4-31b-it:free"
        | "google/gemma-4-26b-a4b-it"
        | "google/gemma-4-26b-a4b-it:free"
        | "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free"
        | "qwen/qwen3.6-35b-a3b"
        | "qwen/qwen3.6-27b"
        | "tencent/hy3-preview"
        | "moonshotai/kimi-k2.6"
        | "moonshotai/kimi-k2.6:free" => Some(262_144),
        "z-ai/glm-5.1" | "z-ai/glm-5v-turbo" => Some(202_752),
        "qwen/qwen3.7-max" | "xiaomi/mimo-v2.5-pro" | "xiaomi/mimo-v2.5" | "qwen/qwen3.6-plus" => {
            Some(1_000_000)
        }
        _ => None,
    }
}

#[must_use]
pub fn max_output_tokens_for_model(model: &str) -> Option<u32> {
    let lower = model.to_lowercase();
    if lower.contains("deepseek") && lower.contains("v4") {
        return Some(384_000);
    }
    match lower.as_str() {
        "arcee-ai/trinity-large-thinking" | "moonshotai/kimi-k2.6" => Some(262_144),
        "qwen/qwen3.6-35b-a3b" | "qwen/qwen3.6-27b" => Some(262_140),
        "xiaomi/mimo-v2.5-pro" | "xiaomi/mimo-v2.5" => Some(131_072),
        "qwen/qwen3.7-max" | "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free" => Some(65_536),
        "google/gemma-4-31b-it" => Some(16_384),
        "google/gemma-4-31b-it:free" | "google/gemma-4-26b-a4b-it:free" => Some(32_768),
        _ => None,
    }
}

#[must_use]
pub fn model_supports_reasoning(model: &str) -> bool {
    let lower = model.to_lowercase();
    if lower.contains("deepseek") && lower.contains("v4") {
        return true;
    }
    matches!(
        lower.as_str(),
        "arcee-ai/trinity-large-thinking"
            | "google/gemma-4-31b-it"
            | "google/gemma-4-31b-it:free"
            | "google/gemma-4-26b-a4b-it"
            | "google/gemma-4-26b-a4b-it:free"
            | "moonshotai/kimi-k2.6"
            | "moonshotai/kimi-k2.6:free"
            | "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free"
            | "qwen/qwen3.7-max"
            | "qwen/qwen3.6-35b-a3b"
            | "qwen/qwen3.6-27b"
            | "tencent/hy3-preview"
            | "xiaomi/mimo-v2.5-pro"
            | "xiaomi/mimo-v2.5"
            | "z-ai/glm-5.1"
    )
}

/// Parse an explicit `_Nk` context-window hint from a model name (vendor
/// agnostic). Returns the window in tokens for `N` in `8..=1024`.
fn explicit_context_window_hint(model_lower: &str) -> Option<u32> {
    let bytes = model_lower.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'k' {
                continue;
            }

            let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
            let after_ok = i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_alphanumeric();
            if !before_ok || !after_ok {
                continue;
            }

            if let Ok(kilo_tokens) = model_lower[start..i].parse::<u32>()
                && (8..=1024).contains(&kilo_tokens)
            {
                return Some(kilo_tokens.saturating_mul(1000));
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Effective input window used by automatic compaction.
///
/// Automatic compaction needs room for the summary response itself. Claude Code
/// reserves at most 20K output tokens for that summary; using the same policy
/// here keeps the trigger provider-neutral instead of tuning around one large
/// DeepSeek V4 window.
#[must_use]
pub fn auto_compact_effective_window_for_model(model: &str) -> Option<u32> {
    let window = context_window_for_model(model)?;
    let reserve = max_output_tokens_for_model(model)
        .unwrap_or(AUTO_COMPACT_SUMMARY_RESERVE_TOKENS)
        .min(AUTO_COMPACT_SUMMARY_RESERVE_TOKENS);
    Some(window.saturating_sub(reserve))
}

/// Derive the automatic compaction threshold from the effective context window.
///
/// Trigger when the summarizable portion approaches the effective input window,
/// leaving a 13K safety buffer for turn metadata, tool schemas, and estimator
/// drift. Unknown models use a conservative 128K-class fallback.
#[must_use]
pub fn compaction_threshold_for_model(model: &str) -> usize {
    let Some(effective_window) = auto_compact_effective_window_for_model(model) else {
        return DEFAULT_COMPACTION_TOKEN_THRESHOLD;
    };
    usize::try_from(effective_window.saturating_sub(AUTO_COMPACT_BUFFER_TOKENS))
        .unwrap_or(DEFAULT_COMPACTION_TOKEN_THRESHOLD)
}

/// Compaction threshold keyed by model and caller-supplied effort tier.
///
/// Replacement-style compaction is no longer tuned only around DeepSeek V4
/// prefix-cache economics. All providers use the same effective-window policy;
/// explicit user settings remain the way to disable automatic compaction.
#[must_use]
pub fn compaction_threshold_for_model_and_effort(
    model: &str,
    _reasoning_effort: Option<&str>,
) -> usize {
    compaction_threshold_for_model(model)
}

// === Streaming Structures ===

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
/// Streaming event types for SSE responses.
pub enum StreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: MessageResponse },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockStart,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u32, delta: Delta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDelta,
        usage: Option<Usage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
/// Content block types used in streaming starts.
pub enum ContentBlockStart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value, // usually empty or partial
        #[serde(skip_serializing_if = "Option::is_none")]
        caller: Option<ToolCaller>,
    },
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

// Variant names match legacy streaming spec, suppressing style warning
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
/// Delta events emitted during streaming responses.
pub enum Delta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
/// Delta payload for message-level updates.
pub struct MessageDelta {
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_snapshots_preserve_context_window() {
        // v-series snapshots get 1M context since they contain "v4"
        assert_eq!(
            context_window_for_model("deepseek-v4-flash-20260423"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
        assert_eq!(
            context_window_for_model("deepseek-v4-pro-20260423"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
    }

    #[test]
    fn unknown_legacy_deepseek_models_map_to_128k_context_window() {
        assert_eq!(
            context_window_for_model("deepseek-coder"),
            Some(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS)
        );
        assert_eq!(
            context_window_for_model("deepseek-v3.2-0324"),
            Some(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS)
        );
    }

    #[test]
    fn deepseek_v4_models_map_to_1m_context_window() {
        assert_eq!(
            context_window_for_model("deepseek-v4-pro"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
        assert_eq!(
            context_window_for_model("deepseek-v4-flash"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
        assert_eq!(
            context_window_for_model("deepseek-ai/deepseek-v4-pro"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
    }

    #[test]
    fn recent_openrouter_large_models_have_static_windows() {
        for (model, expected_window) in [
            ("arcee-ai/trinity-large-thinking", 262_144),
            (concat!("qwen/", "qwen3.7-max"), 1_000_000),
            (concat!("qwen/", "qwen3.6-35b-a3b"), 262_144),
            (concat!("xiaomi/", "mimo-v2.5-pro"), 1_000_000),
            ("moonshotai/kimi-k2.6", 262_144),
            ("google/gemma-4-31b-it", 262_144),
            ("z-ai/glm-5.1", 202_752),
        ] {
            assert_eq!(context_window_for_model(model), Some(expected_window));
            assert!(model_supports_reasoning(model));
        }
    }

    #[test]
    fn recent_openrouter_large_models_have_known_output_caps() {
        assert_eq!(
            max_output_tokens_for_model("arcee-ai/trinity-large-thinking"),
            Some(262_144)
        );
        assert_eq!(
            max_output_tokens_for_model(concat!("qwen/", "qwen3.7-max")),
            Some(65_536)
        );
        assert_eq!(
            max_output_tokens_for_model(concat!("xiaomi/", "mimo-v2.5-pro")),
            Some(131_072)
        );
    }

    #[test]
    fn deepseek_models_with_k_suffix_use_hint() {
        assert_eq!(context_window_for_model("deepseek-v3.2-32k"), Some(32_000));
        assert_eq!(
            context_window_for_model("deepseek-v3.2-256k-preview"),
            Some(256_000)
        );
        assert_eq!(
            context_window_for_model("deepseek-v3.2-2k-preview"),
            Some(LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS)
        );
    }

    #[test]
    fn compaction_threshold_reserves_summary_and_buffer() {
        assert_eq!(
            auto_compact_effective_window_for_model("deepseek-v3.2-128k"),
            Some(108_000)
        );
        assert_eq!(compaction_threshold_for_model("deepseek-v3.2-128k"), 95_000);
        assert_eq!(compaction_threshold_for_model("unknown-model"), 95_000);
    }

    #[test]
    fn compaction_scales_for_deepseek_v4_1m_context() {
        assert_eq!(
            auto_compact_effective_window_for_model("deepseek-v4-pro"),
            Some(980_000)
        );
        assert_eq!(compaction_threshold_for_model("deepseek-v4-pro"), 967_000);
    }

    #[test]
    fn replacement_compaction_uses_provider_neutral_threshold_for_effort() {
        assert_eq!(
            compaction_threshold_for_model_and_effort("deepseek-v4-pro", Some("off")),
            967_000
        );
        assert_eq!(
            compaction_threshold_for_model_and_effort("deepseek-v4-pro", Some("high")),
            967_000
        );
        assert_eq!(
            compaction_threshold_for_model_and_effort("deepseek-v4-pro", Some("max")),
            967_000
        );
    }

    #[test]
    fn provider_neutral_thresholds_apply_to_non_v4_models() {
        assert_eq!(
            compaction_threshold_for_model_and_effort("deepseek-v3.2-128k", Some("max")),
            95_000
        );
        assert_eq!(
            compaction_threshold_for_model_and_effort("claude-sonnet-4-20250514", Some("max")),
            167_000
        );
        assert_eq!(
            compaction_threshold_for_model_and_effort("unknown-model", Some("max")),
            95_000
        );
    }

    #[test]
    fn replacement_compaction_defaults_to_effective_window_when_effort_unknown() {
        assert_eq!(
            compaction_threshold_for_model_and_effort("deepseek-v4-pro", None),
            967_000
        );
        assert_eq!(
            compaction_threshold_for_model_and_effort("deepseek-v4-pro", Some("unknown")),
            967_000
        );
    }
}
