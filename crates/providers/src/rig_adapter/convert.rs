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
//! The shaper restores the ones that matter for a given provider. User-side
//! `Image` blocks are the one wire-visible addition: they map to rig
//! `UserContent::Image` so the provider layer serializes the OpenAI-compatible
//! `image_url` form. File-backed images are read on the blocking pool first
//! ([`resolve_file_images`]); the conversions themselves never touch the disk.

use anyhow::{Result, anyhow};
use base64::Engine as _;
use codesmith_agent::models::{ContentBlock, ImageSource, Message, MessageResponse, Tool, Usage};
use rig_core::OneOrMany;
use rig_core::completion::message::{
    AssistantContent, DocumentSourceKind, Image as RigImage, ImageDetail, ImageMediaType, MimeType,
    ToolCall as RigToolCall, ToolFunction, ToolResult as RigToolResult, ToolResultContent,
    UserContent,
};
use rig_core::completion::{Message as RigMessage, ToolDefinition, Usage as RigUsage};
use rig_core::message::ToolChoice;

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
            let items: Vec<AssistantContent> = msg
                .content
                .iter()
                .filter_map(assistant_block_to_rig)
                .collect();
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
        ContentBlock::ToolUse {
            id, name, input, ..
        } => Some(AssistantContent::ToolCall(RigToolCall::new(
            id.clone(),
            ToolFunction::new(name.clone(), input.clone()),
        ))),
        // Server-side / result blocks never appear in an assistant turn we send
        // back to a provider; ignore them.
        _ => None,
    }
}

/// Map a single CodeSmith content block (inside a user message) to rig
/// `UserContent`. Tool results become rig `ToolResult` text content; `Image`
/// blocks become rig `UserContent::Image`.
fn user_block_to_rig(block: &ContentBlock) -> Option<UserContent> {
    match block {
        ContentBlock::Text { text, .. } => Some(UserContent::text(text.clone())),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => {
            // rig's ToolResult has no `is_error` slot, so surface the failure
            // in the text itself: without it, a failed tool result is
            // reconstructed as an ordinary success and the model may trust
            // garbage output or repeat the failing call. When the transcript
            // carries the payload in `content_blocks` with `content` only a
            // short fallback (compaction records it that way), flatten the
            // text blocks so the substance is not dropped on the floor.
            let body = if content.trim().is_empty() {
                content_blocks
                    .iter()
                    .flatten()
                    .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                content.clone()
            };
            let text = if is_error.unwrap_or(false) {
                format!("[tool error] {body}")
            } else {
                body
            };
            Some(UserContent::ToolResult(RigToolResult {
                id: tool_use_id.clone(),
                call_id: None,
                content: OneOrMany::one(ToolResultContent::text(text)),
            }))
        }
        ContentBlock::Image { source } => match source {
            ImageSource::Base64 { media_type, data } => image_block_to_rig(media_type, data),
            // `resolve_file_images` has already rewritten every readable file
            // into a base64 source; a path still here could not be resolved
            // and is dropped.
            ImageSource::File { .. } => None,
        },
        _ => None,
    }
}

/// Map a user-message `Image` block to rig `UserContent::Image`, from a
/// base64 source: the rig provider layer assembles the `data:<mime>;base64,…`
/// URI for the OpenAI-compatible `image_url` form from the media type.
/// Returns `None` when the media type has no rig analogue, so the block is
/// dropped — the user text still carries the
/// `[Attached image: … at <path>]` reference as fallback.
fn image_block_to_rig(media_type: &str, data: &str) -> Option<UserContent> {
    let media_type = ImageMediaType::from_mime_type(media_type)?;
    Some(UserContent::Image(RigImage {
        data: DocumentSourceKind::Base64(data.to_string()),
        media_type: Some(media_type),
        detail: Some(ImageDetail::Auto),
        additional_params: None,
    }))
}

/// Rewrite every file-backed image in `messages` into a base64 source, reading
/// the files on the blocking pool. Runs before [`message_to_rig`], which is
/// synchronous and polled on the runtime's worker thread: an inline
/// `std::fs::read` plus base64 encode there stalled that worker on every
/// request while the image stayed in history (up to eight 10 MiB attachments
/// per message). Each distinct path is read once per request; a file that
/// cannot be read stays a file source and is dropped by
/// [`user_block_to_rig`], as it always was.
pub(crate) async fn resolve_file_images(messages: &mut [Message]) {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for message in messages.iter() {
        for block in &message.content {
            if let ContentBlock::Image {
                source: ImageSource::File { path, media_type },
            } = block
                && file_image_media_type(path, media_type.as_deref()).is_some()
                && !paths.contains(path)
            {
                paths.push(path.clone());
            }
        }
    }
    if paths.is_empty() {
        return;
    }
    // A join error (pool panic, runtime shutdown) leaves every entry
    // unread: the images are then dropped like any unreadable file.
    let read: std::collections::HashMap<std::path::PathBuf, String> =
        tokio::task::spawn_blocking(move || {
            paths
                .into_iter()
                .filter_map(|path| {
                    std::fs::read(&path).ok().map(|bytes| {
                        (
                            path,
                            base64::engine::general_purpose::STANDARD.encode(bytes),
                        )
                    })
                })
                .collect()
        })
        .await
        .unwrap_or_default();

    for message in messages {
        for block in &mut message.content {
            let ContentBlock::Image { source } = block else {
                continue;
            };
            let ImageSource::File { path, media_type } = &*source else {
                continue;
            };
            let resolved = read
                .get(path)
                .cloned()
                .zip(file_image_media_type(path, media_type.as_deref()));
            if let Some((data, media_type)) = resolved {
                *source = ImageSource::Base64 { media_type, data };
            }
        }
    }
}

/// The wire media type of a file-backed image: the explicit one, else the one
/// its extension implies. `None` means no rig-serializable type.
fn file_image_media_type(path: &std::path::Path, explicit: Option<&str>) -> Option<String> {
    explicit.map(str::to_string).or_else(|| {
        image_media_type_from_path(path).map(|media_type| media_type.to_mime_type().to_string())
    })
}

/// Media types the rig providers can serialize. Anything else (tif, ppm, …)
/// has no `image_url` representation and is dropped by
/// [`image_block_to_rig`].
fn image_media_type_from_path(path: &std::path::Path) -> Option<ImageMediaType> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some(ImageMediaType::PNG),
        Some("jpg" | "jpeg") => Some(ImageMediaType::JPEG),
        Some("gif") => Some(ImageMediaType::GIF),
        Some("webp") => Some(ImageMediaType::WEBP),
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
    OneOrMany::many(items).unwrap_or_else(|_| OneOrMany::one(AssistantContent::text(String::new())))
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
            // Map the object `type` explicitly BEFORE falling through:
            // OpenAI `{"type":"none"}` (disable all tool calls) and
            // Anthropic `{"type":"any"}` (force tool use) are recognized
            // shapes with direct rig equivalents — silently converting
            // them to the provider default changed request semantics.
            match map.get("type").and_then(|t| t.as_str()) {
                Some("none") => return Some(ToolChoice::None),
                Some("any" | "required") => return Some(ToolChoice::Required),
                Some("auto") => return Some(ToolChoice::Auto),
                _ => {}
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

/// Derive an Anthropic-style `stop_reason` for the terminal `MessageDelta`.
///
/// rig-core 0.39 collapses the wire `finish_reason` before it crosses its
/// public API (OpenAI-compat compresses it to a crate-private enum, Anthropic
/// reads it and drops the string), so the real value cannot be passed
/// through. What CAN be proven at this layer, in priority order:
///
/// 1. `"max_tokens"` — the billed output reached the output cap we requested.
///    Providers stop at exactly `max_tokens` when the output limit truncates
///    the turn, so hitting the cap is proof of truncation even without the
///    wire field. This arms the P0-2 truncation gate on real traffic. The
///    rare false positive (a turn that happened to end naturally at exactly
///    the cap) is harmless there: the gate only refuses tool calls whose
///    arguments failed to parse.
/// 2. `"tool_use"` — a tool block was committed; the model stopped to call a
///    tool (OpenAI `tool_calls` normalizes to this).
/// 3. `"end_turn"` — proof of a clean finish, value indistinguishable at this
///    layer (stop vs. anything below the cap).
///
/// # Known limitations (deliberate)
///
/// This is a billed-tokens heuristic, not the wire signal, and the error
/// direction is chosen to be safe (over-reporting truncation):
///
/// - A turn that ended with a complete, parseable tool call exactly at the
///   cap reports `max_tokens`, not `tool_use`. The P0-2 gate tolerates this
///   (it only blocks unparseable arguments), but UI badges and telemetry
///   cannot distinguish such a healthy turn from a truncated one.
/// - On reasoning models, billed `output_tokens` includes thinking tokens,
///   so "billed output == cap" is not always proof that the visible content
///   was truncated.
///
/// Capturing the wire `finish_reason` in the shaper/stream layer before rig
/// collapses it and preferring it here is the eventual fix; it requires
/// touching every provider shaper, so the heuristic stays as the fallback.
pub(crate) fn derive_stop_reason(saw_tool_use: bool, usage: &Usage, max_tokens: u32) -> String {
    if max_tokens > 0 && usage.output_tokens >= max_tokens {
        "max_tokens".to_string()
    } else if saw_tool_use {
        "tool_use".to_string()
    } else {
        "end_turn".to_string()
    }
}

/// Build a `MessageResponse` from the non-streaming completion pieces. Used by
/// `RigLlmClient::create_message`.
pub(crate) fn build_message_response(
    provider_msg_id: Option<String>,
    model: String,
    choice: OneOrMany<AssistantContent>,
    usage: &RigUsage,
    max_tokens: u32,
) -> Result<MessageResponse> {
    let items: Vec<AssistantContent> = choice.into_iter().collect();
    let saw_tool_use = items
        .iter()
        .any(|item| matches!(item, AssistantContent::ToolCall(_)));
    let content: Vec<ContentBlock> = items
        .into_iter()
        .filter_map(|item| assistant_content_to_block(&item))
        .collect();
    if content.is_empty() {
        return Err(anyhow!("completion returned no assistant content blocks"));
    }
    let cs_usage = usage_to_codesmith(usage);
    Ok(MessageResponse {
        id: provider_msg_id.unwrap_or_else(crate::rig_adapter::synth_message_id),
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content,
        model,
        stop_reason: Some(derive_stop_reason(saw_tool_use, &cs_usage, max_tokens)),
        stop_sequence: None,
        container: None,
        usage: cs_usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn stop_reason_derivation_priority() {
        let usage = |output| Usage {
            output_tokens: output,
            ..Usage::default()
        };
        // Billed output reaching the requested cap outranks everything —
        // that's the case the P0-2 truncation gate exists for.
        assert_eq!(derive_stop_reason(false, &usage(100), 100), "max_tokens");
        assert_eq!(derive_stop_reason(true, &usage(100), 100), "max_tokens");
        // Tool rounds report tool_use; natural stops report end_turn.
        assert_eq!(derive_stop_reason(true, &usage(50), 100), "tool_use");
        assert_eq!(derive_stop_reason(false, &usage(50), 100), "end_turn");
        // No cap known (0) → the derivation never fires.
        assert_eq!(derive_stop_reason(false, &usage(5_000), 0), "end_turn");
    }

    fn user_message(content: Vec<ContentBlock>) -> Message {
        Message {
            role: "user".to_string(),
            content,
        }
    }

    fn temp_image_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codesmith-convert-{label}-{}.png",
            std::process::id()
        ))
    }

    /// The file is read and encoded by the pre-pass; the conversion itself
    /// never touches the disk.
    #[tokio::test]
    async fn file_image_block_maps_to_rig_image() {
        let path = temp_image_path("img");
        std::fs::write(&path, b"fake-png-bytes").expect("write temp png");
        let mut msg = user_message(vec![ContentBlock::Image {
            source: ImageSource::File {
                path: path.clone(),
                media_type: None,
            },
        }]);
        resolve_file_images(std::slice::from_mut(&mut msg)).await;
        let converted = message_to_rig(&msg).expect("convert");
        let _ = std::fs::remove_file(&path);

        let RigMessage::User { content } = converted else {
            panic!("expected user message");
        };
        let item = content.into_iter().next().expect("one content item");
        let UserContent::Image(image) = item else {
            panic!("expected image content, got {item:?}");
        };
        assert_eq!(image.media_type, Some(ImageMediaType::PNG));
        let DocumentSourceKind::Base64(data) = &image.data else {
            panic!("expected base64 source");
        };
        assert_eq!(
            data.as_str(),
            base64::engine::general_purpose::STANDARD.encode(b"fake-png-bytes")
        );
    }

    #[test]
    fn base64_image_block_passes_through() {
        let msg = user_message(vec![ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/jpeg".to_string(),
                data: "aGVsbG8=".to_string(),
            },
        }]);
        let converted = message_to_rig(&msg).expect("convert");
        let RigMessage::User { content } = converted else {
            panic!("expected user message");
        };
        let UserContent::Image(image) = content.into_iter().next().expect("one item") else {
            panic!("expected image content");
        };
        assert_eq!(image.media_type, Some(ImageMediaType::JPEG));
        assert!(matches!(image.data, DocumentSourceKind::Base64(_)));
    }

    #[tokio::test]
    async fn unreadable_or_unsupported_image_blocks_are_dropped() {
        // A missing file survives the pre-pass as a file source and falls
        // back to the empty-text placeholder rather than failing the whole
        // request.
        let mut missing = user_message(vec![ContentBlock::Image {
            source: ImageSource::File {
                path: PathBuf::from("/nonexistent/codesmith-convert-test.png"),
                media_type: None,
            },
        }]);
        resolve_file_images(std::slice::from_mut(&mut missing)).await;
        let converted = message_to_rig(&missing).expect("convert");
        let RigMessage::User { content } = converted else {
            panic!("expected user message");
        };
        assert!(
            content
                .into_iter()
                .all(|item| { matches!(item, UserContent::Text(_)) })
        );

        // Unsupported media type on an explicit base64 source is dropped too.
        let unsupported = user_message(vec![ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/tiff".to_string(),
                data: "aGVsbG8=".to_string(),
            },
        }]);
        let converted = message_to_rig(&unsupported).expect("convert");
        let RigMessage::User { content } = converted else {
            panic!("expected user message");
        };
        assert!(
            content
                .into_iter()
                .all(|item| matches!(item, UserContent::Text(_)))
        );
    }

    /// Round-3 TODO-41: the encoder and the shared `WIRE_IMAGE_EXTENSIONS`
    /// constant (which user-facing intake — `/attach`, `@path` — rejects on)
    /// must stay in lockstep: every listed extension encodes, and a sample
    /// of unlisted ones (tif, ppm, video containers) do not.
    #[test]
    fn encoder_accepts_exactly_the_wire_image_extensions() {
        for ext in codesmith_agent::models::WIRE_IMAGE_EXTENSIONS {
            assert!(
                super::image_media_type_from_path(&PathBuf::from(format!("file.{ext}"))).is_some(),
                "WIRE_IMAGE_EXTENSIONS lists .{ext} but the encoder drops it"
            );
        }
        for ext in ["tif", "tiff", "ppm", "bmp", "mp4", "mov", "mkv"] {
            assert!(
                super::image_media_type_from_path(&PathBuf::from(format!("file.{ext}"))).is_none(),
                ".{ext} must stay unlisted — the wire layer has no image_url form for it"
            );
        }
    }

    /// The file read runs on the blocking pool, not on the worker thread
    /// that polls the request: with the pool's only thread pinned, the image
    /// cannot be read until that thread is released.
    #[test]
    fn file_images_are_read_on_the_blocking_pool() {
        use std::time::Duration;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let path = temp_image_path("pool");
            std::fs::write(&path, b"fake-png-bytes").expect("write temp png");
            let mut msg = user_message(vec![ContentBlock::Image {
                source: ImageSource::File {
                    path: path.clone(),
                    media_type: None,
                },
            }]);

            // Pin the pool's single thread.
            let (pinned, pinned_rx) = tokio::sync::oneshot::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let pin = tokio::task::spawn_blocking(move || {
                let _ = pinned.send(());
                let _ = released.recv();
            });
            pinned_rx.await.expect("pool thread pinned");

            let blocked = tokio::time::timeout(
                Duration::from_millis(200),
                resolve_file_images(std::slice::from_mut(&mut msg)),
            )
            .await
            .is_err();

            drop(release);
            pin.await.expect("pinned task");
            assert!(
                blocked,
                "the file was read inline on the async worker: the pass never waited for the pool"
            );
            resolve_file_images(std::slice::from_mut(&mut msg)).await;
            assert!(matches!(
                msg.content[0],
                ContentBlock::Image {
                    source: ImageSource::Base64 { .. }
                }
            ));
            let _ = std::fs::remove_file(&path);
        });
    }
}
