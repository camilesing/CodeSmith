//! Auto-model routing + config persistence (moved from the retired
//! `commands/config.rs` when the TUI command tree was removed).
//!
//! `resolve_auto_route` picks the model + reasoning effort per request
//! when the model selection is `auto`: a keyword heuristic first, then a
//! router-model recommendation. The persist helpers round-trip config.toml
//! without disturbing unknown keys.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::app_state::ReasoningEffort;
use crate::config::{effective_home_dir, expand_path};
use crate::models::{ContentBlock, Message, MessageRequest, MessageResponse, SystemPrompt};

pub fn persist_root_string_key(
    config_path: Option<&Path>,
    key: &str,
    value: &str,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    use std::fs;

    let path = config_toml_path(config_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    let mut doc: toml::Value = if path.exists() {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        toml::from_str(&raw)
            .with_context(|| format!("failed to parse config at {}", path.display()))?
    } else {
        toml::Value::Table(toml::value::Table::new())
    };
    let table = doc
        .as_table_mut()
        .context("config.toml root must be a table")?;
    table.insert(key.to_string(), toml::Value::String(value.to_string()));
    let body = toml::to_string_pretty(&doc).context("failed to serialize config.toml")?;
    crate::utils::write_atomic(&path, body.as_bytes())
        .with_context(|| format!("failed to write config at {}", path.display()))?;
    Ok(path)
}

/// Resolve the path to `~/.codesmith/config.toml` (or
/// `$CODESMITH_CONFIG_PATH`). Mirrors what `Config::load` accepts so we
/// never write to a different file than the one we read.
pub(crate) fn config_toml_path(config_path: Option<&Path>) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    if let Some(path) = config_path {
        return Ok(expand_path(path.to_string_lossy().as_ref()));
    }
    if let Ok(env) = std::env::var("CODESMITH_CONFIG_PATH") {
        let trimmed = env.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    let home =
        effective_home_dir().context("failed to resolve home directory for config.toml path")?;
    Ok(home.join(".codesmith").join("config.toml"))
}

/// Deterministic tier-level heuristic for callers that resolve the tier
/// against their own model pair (sub-agent runtimes carry an
/// `AutoRouteContext`, not the full `Config`).
#[must_use]
pub fn auto_model_heuristic_tier(input: &str, cost_saving: bool) -> crate::config::ModelTier {
    auto_model_heuristic_selection_with_bias(input, cost_saving).tier
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoModelHeuristicConfidence {
    Decisive,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoModelHeuristicSelection {
    tier: crate::config::ModelTier,
    confidence: AutoModelHeuristicConfidence,
}

fn auto_model_heuristic_selection_with_bias(
    input: &str,
    cost_saving: bool,
) -> AutoModelHeuristicSelection {
    let len = input.chars().count();
    let lower = input.to_lowercase();
    let borderline_pro_keywords: &[&str] = &[
        "implement",
        "analyze",
        "\u{5b9e}\u{73b0}", // 实现
        "\u{5206}\u{6790}", // 分析
        "\u{5be6}\u{73fe}", // 實現
    ];
    let strong_match = COMPLEX_KEYWORDS
        .iter()
        .any(|kw| !borderline_pro_keywords.contains(kw) && lower.contains(kw));
    let borderline_match = borderline_pro_keywords.iter().any(|kw| lower.contains(kw));
    let heavy_match = strong_match || (!cost_saving && borderline_match);
    if heavy_match {
        return AutoModelHeuristicSelection {
            tier: crate::config::ModelTier::Heavy,
            confidence: AutoModelHeuristicConfidence::Decisive,
        };
    }
    // Short messages → light tier
    if len < 100 {
        return AutoModelHeuristicSelection {
            tier: crate::config::ModelTier::Light,
            confidence: AutoModelHeuristicConfidence::Decisive,
        };
    }
    // Long complex requests → heavy tier. Cost-saving raises the threshold
    // so that long-but-routine requests (pasted logs, CSV-style data) don't
    // escalate.
    let long_threshold = if cost_saving { 1_000 } else { 500 };
    if len > long_threshold {
        return AutoModelHeuristicSelection {
            tier: crate::config::ModelTier::Heavy,
            confidence: AutoModelHeuristicConfidence::Decisive,
        };
    }
    // Grey-zone default branch: light is the deterministic fallback, but the
    // router can still add value here because there was no strong local
    // signal.
    AutoModelHeuristicSelection {
        tier: crate::config::ModelTier::Light,
        confidence: AutoModelHeuristicConfidence::Ambiguous,
    }
}

/// Keywords that escalate `auto`-mode model selection to the heavy tier.
/// The Latin entries are lowercase (the caller lowercases the message); CJK
/// has no case so the literal form matches as-is.
///
/// Without the CJK entries, a Chinese-speaking user typing
/// "帮我重构这个模块" or "审计安全漏洞" silently fell through to the
/// short/long-message threshold and usually landed on the light tier even
/// for tasks that obviously need heavy-grade reasoning.
const COMPLEX_KEYWORDS: &[&str] = &[
    // English (unchanged from the original list).
    "refactor",
    "architecture",
    "design",
    "debug",
    "security",
    "review",
    "audit",
    "migrate",
    "optimize",
    "rewrite",
    "implement",
    "analyze",
    // Simplified Chinese.
    "\u{91cd}\u{6784}", // 重构
    "\u{67b6}\u{6784}", // 架构
    "\u{8bbe}\u{8ba1}", // 设计
    "\u{8c03}\u{8bd5}", // 调试
    "\u{5b89}\u{5168}", // 安全
    "\u{5ba1}\u{67e5}", // 审查
    "\u{5ba1}\u{8ba1}", // 审计
    "\u{8fc1}\u{79fb}", // 迁移
    "\u{4f18}\u{5316}", // 优化
    "\u{91cd}\u{5199}", // 重写
    "\u{5b9e}\u{73b0}", // 实现
    "\u{5206}\u{6790}", // 分析
    // Traditional Chinese variants where they differ.
    "\u{91cd}\u{69cb}", // 重構
    "\u{67b6}\u{69cb}", // 架構
    "\u{8a2d}\u{8a08}", // 設計
    "\u{8abf}\u{8a66}", // 調試
    "\u{5be9}\u{67e5}", // 審查
    "\u{5be9}\u{8a08}", // 審計
    "\u{9077}\u{79fb}", // 遷移
    "\u{512a}\u{5316}", // 優化
    "\u{91cd}\u{5beb}", // 重寫
    "\u{5be6}\u{73fe}", // 實現
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoRouteRecommendation {
    pub tier: crate::config::ModelTier,
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoRouteSource {
    Router,
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoRouteSelection {
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub source: AutoRouteSource,
}

pub const AUTO_MODEL_ROUTER_SYSTEM_PROMPT: &str = "\
You are the codesmith auto-routing classifier. Return only compact JSON: \
{\"tier\":\"light|heavy\",\"thinking\":\"off|high|max\"}. \
Use the light tier for trivial, conversational, status, or single-step work. \
Use the heavy tier for coding, debugging, release work, multi-step tasks, high-risk decisions, \
tool-heavy work, ambiguous requests, or anything that benefits from deeper reasoning. \
Use thinking off only for trivial no-tool answers, high for ordinary reasoning, and max for \
agentic, coding, multi-file, release, architecture, debugging, security, tool-heavy, or uncertain work.";

/// Bias appended to the auto-router's system prompt when the user opts in to
/// `[auto] cost_saving = true` (#1207). Reverses the default tie-breaker for
/// genuinely ambiguous requests so the heavy tier is reserved for tasks that
/// clearly require it; ordinary tweaks, config edits, and short reads stay on
/// the light tier.
pub const AUTO_MODEL_ROUTER_COST_SAVING_ADDENDUM: &str = "\
\n\nCost-saving mode is ON. Prefer the light tier for any request that is \
not unmistakably agentic, multi-step, architecture/design, security review, \
debugging, or otherwise clearly beyond the light tier's capability. Resolve \
ambiguous cases in favour of light, not heavy.";

/// Parse the router's JSON-only response.
///
/// The runtime treats classifier output as untrusted: only the two routing
/// tiers and supported reasoning tiers are accepted. Anything else falls
/// back to the deterministic heuristic. Responses from routers prompted
/// before the tier vocabulary still parse — the legacy `model` field
/// ("deepseek-v4-pro" / "deepseek-v4-flash") maps onto tiers.
pub fn parse_auto_route_recommendation(raw: &str) -> Option<AutoRouteRecommendation> {
    let json = extract_first_json_object(raw)?;
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tier = value
        .get("tier")
        .or_else(|| value.get("model"))
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_auto_route_tier)?;
    let reasoning_effort = value
        .get("thinking")
        .or_else(|| value.get("reasoning_effort"))
        .or_else(|| value.get("effort"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_auto_route_reasoning_effort);

    Some(AutoRouteRecommendation {
        tier,
        reasoning_effort,
    })
}

fn extract_first_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end >= start).then_some(&raw[start..=end])
}

/// Whitelist for the router's tier answer. The tier names plus the legacy
/// DeepSeek model vocabulary (kept so pre-tier router responses and their
/// "pro"/"flash" shorthands still parse) are the only accepted values —
/// a classifier can never name a concrete model ID into the turn.
fn normalize_auto_route_tier(value: &str) -> Option<crate::config::ModelTier> {
    use crate::config::ModelTier;
    match value.trim().to_ascii_lowercase().as_str() {
        "heavy" | "deepseek-v4-pro" | "v4-pro" | "pro" => Some(ModelTier::Heavy),
        "light" | "deepseek-v4-flash" | "v4-flash" | "flash" => Some(ModelTier::Light),
        _ => None,
    }
}

fn parse_auto_route_reasoning_effort(effort: &str) -> Option<ReasoningEffort> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" => Some(ReasoningEffort::Off),
        "low" | "minimal" | "medium" | "mid" => Some(ReasoningEffort::High),
        "high" => Some(ReasoningEffort::High),
        "max" | "maximum" | "xhigh" => Some(ReasoningEffort::Max),
        _ => None,
    }
}

#[must_use]
pub fn normalize_auto_route_effort(effort: ReasoningEffort) -> ReasoningEffort {
    match effort {
        ReasoningEffort::Low | ReasoningEffort::Medium => ReasoningEffort::High,
        other => other,
    }
}

pub async fn resolve_auto_route(
    config: &crate::config::Config,
    latest_request: &str,
    recent_context: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
) -> AutoRouteSelection {
    let cost_saving = config.auto_cost_saving();
    let heuristic = auto_model_heuristic_selection_with_bias(latest_request, cost_saving);
    if heuristic.confidence == AutoModelHeuristicConfidence::Decisive {
        return auto_route_from_heuristic(config, latest_request, heuristic);
    }

    match auto_route_model_recommendation(
        config,
        latest_request,
        recent_context,
        selected_model_mode,
        selected_thinking_mode,
    )
    .await
    {
        Ok(Some(recommendation)) => AutoRouteSelection {
            model: config.resolve_model_tier(recommendation.tier),
            reasoning_effort: recommendation.reasoning_effort,
            source: AutoRouteSource::Router,
        },
        Ok(None) => auto_route_from_heuristic(config, latest_request, heuristic),
        Err(ref err) => {
            // The sub-agent router logs this same fallback; without the log
            // here a misconfigured `[auto] router_model` no-ops invisibly.
            tracing::warn!(
                target: "codesmith_auto_route",
                "model router failed; falling back to heuristic: {err}"
            );
            auto_route_from_heuristic(config, latest_request, heuristic)
        }
    }
}

fn auto_route_from_heuristic(
    config: &crate::config::Config,
    latest_request: &str,
    heuristic: AutoModelHeuristicSelection,
) -> AutoRouteSelection {
    AutoRouteSelection {
        model: config.resolve_model_tier(heuristic.tier),
        reasoning_effort: Some(normalize_auto_route_effort(crate::auto_reasoning::select(
            false,
            latest_request,
        ))),
        source: AutoRouteSource::Heuristic,
    }
}

async fn auto_route_model_recommendation(
    config: &crate::config::Config,
    latest_request: &str,
    recent_context: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
) -> Result<Option<AutoRouteRecommendation>> {
    if cfg!(test) {
        return Ok(None);
    }

    // The classifier brain. Quality-first default: the heavy tier — the
    // strongest model the active provider serves — decides who does the
    // work. A misroute wastes the whole turn, which costs far more than one
    // tiny heavy call on a six-line input. `[auto] cost_saving = true`
    // restores the cheap brain: the configured `[utility_model]`, falling
    // back to the main model when absent. (Both brain paths previously
    // pinned to deepseek-v4-flash, which errored — and silently fell back
    // to the heuristic — on non-DeepSeek providers.)
    let main_client = crate::core::engine::resolve_llm_client(config)?;
    let (client, model) = if let Some(model) = config.auto_router_model_override() {
        (main_client, model)
    } else if config.auto_cost_saving() {
        // Bind first: the scrutinee's borrow of `main_client` must end before
        // the None arm may move it.
        let utility = crate::core::engine::resolve_utility_llm(config, Some(&main_client));
        match utility {
            Some(utility) => (utility.client, utility.model),
            None => {
                let model = main_client.model().to_string();
                (main_client, model)
            }
        }
    } else {
        let heavy = config.resolve_model_tier(crate::config::ModelTier::Heavy);
        (main_client, heavy)
    };
    let mut router_system = AUTO_MODEL_ROUTER_SYSTEM_PROMPT.to_string();
    if config.auto_cost_saving() {
        router_system.push_str(AUTO_MODEL_ROUTER_COST_SAVING_ADDENDUM);
    }
    let request = MessageRequest {
        model,
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: auto_route_prompt(
                    latest_request,
                    recent_context,
                    selected_model_mode,
                    selected_thinking_mode,
                ),
                cache_control: None,
            }],
        }],
        max_tokens: 96,
        system: Some(SystemPrompt::Text(router_system)),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: Some("off".to_string()),
        stream: Some(false),
        temperature: Some(0.0),
        top_p: None,
    };

    let response =
        tokio::time::timeout(Duration::from_secs(4), client.create_message(request)).await??;
    Ok(parse_auto_route_recommendation(&message_response_text(
        &response,
    )))
}

fn auto_route_prompt(
    latest_request: &str,
    recent_context: &str,
    selected_model_mode: &str,
    selected_thinking_mode: &str,
) -> String {
    format!(
        "Session mode: agent\nSelected model mode: {}\nSelected thinking mode: {}\n\nRecent context:\n{}\n\nLatest user request:\n{}\n\nReturn JSON only.",
        selected_model_mode,
        selected_thinking_mode,
        if recent_context.trim().is_empty() {
            "No prior context."
        } else {
            recent_context
        },
        truncate_for_auto_router(latest_request, 4_000)
    )
}

fn message_response_text(response: &MessageResponse) -> String {
    let mut out = String::new();
    for block in &response.content {
        match block {
            ContentBlock::Text { text, .. } | ContentBlock::ToolResult { content: text, .. } => {
                append_router_text(&mut out, text);
            }
            ContentBlock::Thinking { thinking } => {
                append_router_text(&mut out, thinking);
            }
            ContentBlock::ToolUse { name, .. } => {
                append_router_text(&mut out, &format!("[tool call: {name}]"));
            }
            _ => {}
        }
    }
    out
}

fn append_router_text(out: &mut String, text: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(text);
}

fn truncate_for_auto_router(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}
