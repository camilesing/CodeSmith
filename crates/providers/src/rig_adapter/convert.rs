//! Pure conversions between CodeSmith's wire model and rig's provider-agnostic
//! completion model.
//!
//! CodeSmith speaks an Anthropic-style wire format (`MessageRequest` /
//! `MessageResponse` / `ContentBlock`) because its first providers were
//! DeepSeek and Anthropic. rig speaks its own role-tagged `Message` /
//! `AssistantContent`. These functions are the bidirectional seam the
//! [`crate::rig_adapter::RigLlmClient`] adapter drives — they take no
//! provider-specific decisions, so a `RequestShaper` is layered on top for
//! fields rig has no first-class slot for (cache_control, reasoning_effort,
//! …).
//!
//! Lossy by design: CodeSmith content blocks that have no rig analogue
//! (`ServerToolUse`, `ToolSearchToolResult`, `CodeExecutionToolResult`,
//! assistant `Image`) are dropped, and provider-only fields (`caller`,
//! `is_error`, `content_blocks`, `cache_control`) are not round-tripped here.
//! The shaper restores the ones that matter for a given provider.

use anyhow::{Result, anyhow};
use codesmith_agent::models::{
    ContentBlock, Message, MessageResponse, Tool, Usage,
};
use rig_core::completion::message::{
    AssistantContent, ToolCall as RigToolCall, ToolFunction, ToolResult as RigToolResult,
    ToolResultContent, UserContent,
};
use rig_core::completion::{Message as RigMessage, ToolDefinition, Usage as RigUsage};
use rig_core::message::ToolChoice;
use rig_core::OneOrMany;

// === Request direction: CodeSmith -> rig =====================================

/// Convert a CodeSmith chat message into a rig provider-agnostic message.
///
/// System `cache_control` blocks are intentionally not handled here — the
/// `RequestShaper` owns provider-specific system shaping (Anthropic forwards
/// structured system through `additional_params`).
pub(crate) fn message_to_rig(msg: &Message) -> Result<RigMessage> {
    match msg.role.as_str() {
        "system" => {
            let text = collect_text_blocks(&msg.content);
            Ok(RigMessage::System {
                content: text.join("\n\n"),
            })
        }
        "assistant" => {
            let items: Vec<AssistantContent> =
                msg.content.iter().filter_map(assistant_block_to_rig).collect();
            Ok(RigMessage::Assistant {
                id: None,
                content: one_or_many_assistant(items),
            })
        }
        // "user" and any unrecognized role land in the user bucket; CodeSmith
        // carries tool results inside user-role messages (Anthropic convention).
        _ => {
            let items: Vec<UserContent> =
                msg.content.iter().filter_map(user_block_to_rig).collect();
            Ok(RigMessage::User {
                content: one_or_many_user(items),
            })
        }
    }
}

/// Map a single CodeSmith content block (inside an assistant message) to rig
/// `AssistantContent`. Returns `None` for blocks with no assistant-side rig
/// analogue so they can be filtered out.
fn assistant_block_to_rig(block: &ContentBlock) -> Option<AssistantContent> {
    match block {
        ContentBlock::Text { text, .. } => Some(AssistantContent::text(text.clone())),
        ContentBlock::Thinking { thinking } => Some(AssistantContent::reasoning(thinking)),
        ContentBlock::ToolUse { id, name, input, .. } => Some(AssistantContent::ToolCall(
            RigToolCall::new(id.clone(), ToolFunction::new(name.clone(), input.clone())),
        )),
        // Server-side / result blocks never appear in an assistant turn we send
        // back to a provider; ignore them.
        _ => None,
    }
}

/// Map a single CodeSmith content block (inside a user message) to rig
/// `UserContent`. Tool results become rig `ToolResult` text content.
fn user_block_to_rig(block: &ContentBlock) -> Option<UserContent> {
    match block {
        ContentBlock::Text { text, .. } => Some(UserContent::text(text.clone())),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } => Some(UserContent::ToolResult(RigToolResult {
            id: tool_use_id.clone(),
            call_id: None,
            content: OneOrMany::one(ToolResultContent::text(content.clone())),
        })),
        _ => None,
    }
}

/// Gather the `text` field of every `Text` block in a content list. Used to
/// flatten system prompts that arrived as multiple text blocks.
fn collect_text_blocks(content: &[ContentBlock]) -> Vec<String> {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// `OneOrMany` cannot be empty; if filtering dropped every block, substitute a
/// single empty text block so the message is still valid on the wire.
fn one_or_many_user(items: Vec<UserContent>) -> OneOrMany<UserContent> {
    OneOrMany::many(items).unwrap_or_else(|_| OneOrMany::one(UserContent::text(String::new())))
}

fn one_or_many_assistant(items: Vec<AssistantContent>) -> OneOrMany<AssistantContent> {
    OneOrMany::many(items)
        .unwrap_or_else(|_| OneOrMany::one(AssistantContent::text(String::new())))
}

/// Map CodeSmith tool definitions to rig `ToolDefinition`s (name / description /
/// JSON-schema parameters). Extra CodeSmith fields (`cache_control`, `strict`,
/// `output_schema`, …) are provider-specific and are reattached by the shaper.
pub(crate) fn tools_to_rig(tools: &[Tool]) -> Vec<ToolDefinition> {
    tools
        .iter()
        .map(|t| ToolDefinition {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        })
        .collect()
}

/// Best-effort mapping of CodeSmith's free-form `tool_choice` JSON to rig's
/// typed `ToolChoice`. Handles the common string form and the OpenAI / Anthropic
/// object shapes; unrecognised shapes fall back to `None` (let the provider
/// default, usually `Auto`).
pub(crate) fn tool_choice_to_rig(value: &serde_json::Value) -> Option<ToolChoice> {
    match value {
        serde_json::Value::String(s) => match s.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" => Some(ToolChoice::Required),
            _ => None,
        },
        serde_json::Value::Object(map) => {
            // OpenAI: {"type":"function","function":{"name":"..."}}
            if let Some(name) = map
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
            {
                return Some(ToolChoice::Specific {
                    function_names: vec![name.to_string()],
                });
            }
            // Anthropic: {"type":"tool","name":"..."}
            if let Some(name) = map.get("name").and_then(|n| n.as_str()) {
                return Some(ToolChoice::Specific {
                    function_names: vec![name.to_string()],
                });
            }
            if matches!(map.get("type").and_then(|t| t.as_str()), Some("required")) {
                return Some(ToolChoice::Required);
            }
            None
        }
        _ => None,
    }
}

// === Response direction: rig -> CodeSmith ====================================

/// Map a rig `AssistantContent` item (from a non-streaming completion
/// `choice`) to a CodeSmith `ContentBlock`. Returns `None` for rig content
/// with no CodeSmith analogue (e.g. assistant `Image`).
pub(crate) fn assistant_content_to_block(item: &AssistantContent) -> Option<ContentBlock> {
    match item {
        AssistantContent::Text(text) => Some(ContentBlock::Text {
            text: text.text.clone(),
            cache_control: None,
        }),
        AssistantContent::ToolCall(tc) => Some(ContentBlock::ToolUse {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            input: tc.function.arguments.clone(),
            caller: None,
        }),
        AssistantContent::Reasoning(reasoning) => Some(ContentBlock::Thinking {
            thinking: reasoning.display_text(),
        }),
        AssistantContent::Image(_) => None,
    }
}

/// Convert rig's token `Usage` into CodeSmith's. rig uses `u64` and a zero
/// sentinel for "not reported"; CodeSmith uses `u32` with `Option` fields that
/// are omitted when absent. We only populate the optional cache/reasoning
/// counters when the provider actually reported them (non-zero).
pub(crate) fn usage_to_codesmith(usage: &RigUsage) -> Usage {
    Usage {
        input_tokens: u32::try_from(usage.input_tokens).unwrap_or(u32::MAX),
        output_tokens: u32::try_from(usage.output_tokens).unwrap_or(u32::MAX),
        prompt_cache_hit_tokens: nonzero(usage.cached_input_tokens),
        prompt_cache_miss_tokens: nonzero(usage.cache_creation_input_tokens),
        reasoning_tokens: nonzero(usage.reasoning_tokens),
        reasoning_replay_tokens: None,
        server_tool_use: None,
    }
}

fn nonzero(v: u64) -> Option<u32> {
    (v != 0).then(|| u32::try_from(v).unwrap_or(u32::MAX))
}

/// Build a `MessageResponse` from the non-streaming completion pieces. Used by
/// `RigLlmClient::create_message`.
pub(crate) fn build_message_response(
    provider_msg_id: Option<String>,
    model: String,
    choice: OneOrMany<AssistantContent>,
    usage: &RigUsage,
) -> Result<MessageResponse> {
    let content: Vec<ContentBlock> = choice
        .into_iter()
        .filter_map(|item| assistant_content_to_block(&item))
        .collect();
    if content.is_empty() {
        return Err(anyhow!("completion returned no assistant content blocks"));
    }
    Ok(MessageResponse {
        id: provider_msg_id.unwrap_or_else(crate::rig_adapter::synth_message_id),
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content,
        model,
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        container: None,
        usage: usage_to_codesmith(usage),
    })
}
