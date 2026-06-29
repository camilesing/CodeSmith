//! Context compaction for long conversations.
//!
//! The heavy compaction engine (planning, summary generation, the safe retry
//! loop) now lives in `codesmith_agent_runtime::compaction::compact`. This
//! module is a thin re-export shim so historical `crate::compaction::<item>`
//! paths keep resolving; it also owns the compaction *test* module (which
//! depends on TUI-local `MockLlmClient` / `HookExecutor`) and the three
//! prompt-/reinject-style submodules.

pub mod attachment_reinject;
pub mod compact_prompt;
pub mod partial_compact;

// Re-export the full compaction surface — config + token helpers + state
// submodules + the heavy `compact` engine — so `crate::compaction::<item>`
// resolves for the engine, the submodules below, and the tests.
pub use codesmith_agent_runtime::compaction::*;

use anyhow::Result;
use regex::Regex;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::hooks::{HookContext, HookExecutor, HookHost};
use crate::llm_client::LlmClient;
use crate::models::{
    CacheControl, ContentBlock, Message, MessageRequest, SystemBlock, SystemPrompt,
    context_window_for_model,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::mock::MockLlmClient;
    use crate::llm_client::{LlmClient, StreamEventBox};
    use crate::models::{MessageResponse, Usage};
    use serde_json::json;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
            }],
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    /// Test-only convenience wrapper around [`prune_tool_results_until`] with a
    /// never-stop predicate. Relocated from the migrated `compact` engine (where
    /// it was `#[cfg(test)]`); kept here in the test module so AR production stays
    /// free of test-only helpers.
    fn prune_tool_results(messages: &mut [Message], protected_window: usize) -> usize {
        prune_tool_results_until(messages, protected_window, |_, _| false)
    }

    #[test]
    fn anchor_summary_section_is_empty_without_workspace_or_file() {
        assert!(anchor_summary_section(None).is_empty());

        let tmpdir = tempfile::TempDir::new().unwrap();
        assert!(anchor_summary_section(Some(tmpdir.path())).is_empty());
    }

    #[test]
    fn anchor_summary_section_parses_anchor_file_into_bullets() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let deepseek_dir = tmpdir.path().join(".deepseek");
        std::fs::create_dir_all(&deepseek_dir).unwrap();
        std::fs::write(
            deepseek_dir.join("anchors.md"),
            "\n---\nDo not touch .ssh\n---\nStatus field is unreliable\n",
        )
        .unwrap();

        let section = anchor_summary_section(Some(tmpdir.path()));

        assert!(section.contains("## Pinned Facts (User Anchors)"));
        assert!(section.contains("- Do not touch .ssh\n"));
        assert!(section.contains("- Status field is unreliable\n"));
        assert!(!section.contains("\n---\nDo not touch"));
    }

    #[test]
    fn truncate_chars_respects_unicode_boundaries() {
        let text = "abc😀é";
        assert_eq!(truncate_chars(text, 0), "");
        assert_eq!(truncate_chars(text, 1), "a");
        assert_eq!(truncate_chars(text, 3), "abc");
        assert_eq!(truncate_chars(text, 4), "abc😀");
        assert_eq!(truncate_chars(text, 5), "abc😀é");
    }

    #[test]
    fn prune_tool_results_summarizes_old_verbose_outputs() {
        let verbose = "x".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 80);
        let mut messages = vec![
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &verbose),
            msg("user", "recent question"),
            msg("assistant", "recent answer"),
        ];

        let saved = prune_tool_results(&mut messages, 2);

        assert!(saved > 0);
        let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert!(content.contains("[read_file] tool result pruned"));
        assert!(content.contains("Cargo.toml"));
        assert!(content.len() < verbose.len());
    }

    #[test]
    fn prune_tool_results_preserves_protected_tail() {
        let verbose = "x".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 80);
        let mut messages = vec![
            msg("user", "older context"),
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &verbose),
        ];

        let saved = prune_tool_results(&mut messages, 2);

        assert_eq!(saved, 0);
        let ContentBlock::ToolResult { content, .. } = &messages[2].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(content, &verbose);
    }

    #[test]
    fn prune_tool_results_preserves_prefix_bytes_when_reverse_prune_is_enough() {
        let older_verbose = "old ".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 40);
        let newer_verbose = "new ".repeat(SUMMARY_TOOL_RESULT_SNIPPET_CHARS + 40);
        let mut messages = vec![
            tool_use("call-old", "read_file", json!({"path": "old.txt"})),
            tool_result("call-old", &older_verbose),
            tool_use("call-new", "read_file", json!({"path": "new.txt"})),
            tool_result("call-new", &newer_verbose),
            msg("user", "protected tail"),
        ];
        let original = messages.clone();

        // Simulate the caller clearing its token budget after one suffix prune.
        let saved = prune_tool_results_until(&mut messages, 1, |_, saved| saved > 0);

        assert!(saved > 0);
        assert_eq!(&messages[..3], &original[..3]);
        assert_eq!(&messages[4..], &original[4..]);
        let ContentBlock::ToolResult { content, .. } = &messages[3].content[0] else {
            panic!("expected pruned tool result");
        };
        assert!(content.contains("[read_file] tool result pruned"));
        assert!(content.contains("new.txt"));
        assert!(content.len() < newer_verbose.len());
    }

    #[test]
    fn prune_tool_results_stops_after_newest_duplicate_prune() {
        let oldest = "oldest ".repeat(80);
        let middle = "middle ".repeat(80);
        let latest = "latest ".repeat(80);
        let mut messages = vec![
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &oldest),
            tool_use("call-2", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-2", &middle),
            tool_use("call-3", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-3", &latest),
            msg("user", "protected tail"),
        ];
        let original = messages.clone();

        let saved = prune_tool_results_until(&mut messages, 1, |_, saved| saved > 0);

        assert!(saved > 0);
        assert_eq!(&messages[..3], &original[..3]);
        assert_eq!(&messages[4..], &original[4..]);
        let ContentBlock::ToolResult { content, .. } = &messages[3].content[0] else {
            panic!("expected middle duplicate to be pruned");
        };
        assert!(content.contains("[read_file] tool result pruned"));
    }

    #[test]
    fn prune_tool_results_dedupes_identical_reads_but_keeps_latest_full_body() {
        let first = "first ".repeat(80);
        let second = "second ".repeat(80);
        let mut messages = vec![
            tool_use("call-1", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-1", &first),
            tool_use("call-2", "read_file", json!({"path": "Cargo.toml"})),
            tool_result("call-2", &second),
            msg("user", "tail"),
        ];

        let saved = prune_tool_results(&mut messages, 1);

        assert!(saved > 0);
        let ContentBlock::ToolResult { content: older, .. } = &messages[1].content[0] else {
            panic!("expected older tool result");
        };
        assert!(older.contains("tool result pruned"));
        let ContentBlock::ToolResult {
            content: latest, ..
        } = &messages[3].content[0]
        else {
            panic!("expected latest tool result");
        };
        assert_eq!(latest, &second);
    }

    #[test]
    fn is_transient_error_detects_network_issues() {
        let timeout_err = anyhow::anyhow!("Connection timeout");
        assert!(is_transient_error(&timeout_err));

        let rate_limit_err = anyhow::anyhow!("429 Too Many Requests");
        assert!(is_transient_error(&rate_limit_err));

        let service_err = anyhow::anyhow!("503 Service Unavailable");
        assert!(is_transient_error(&service_err));

        let network_err = anyhow::anyhow!("network error: connection refused");
        assert!(is_transient_error(&network_err));
    }

    #[test]
    fn is_transient_error_rejects_permanent_errors() {
        let auth_err = anyhow::anyhow!("401 Unauthorized: Invalid API key");
        assert!(!is_transient_error(&auth_err));

        let parse_err = anyhow::anyhow!("Failed to parse JSON response");
        assert!(!is_transient_error(&parse_err));

        let validation_err = anyhow::anyhow!("Invalid request: missing required field");
        assert!(!is_transient_error(&validation_err));
    }

    #[test]
    fn summary_limits_expand_for_v4_context() {
        let legacy = summary_input_limits_for_model("deepseek-v3.2-128k");
        let v4 = summary_input_limits_for_model("deepseek-v4-pro");

        assert!(v4.input_max_chars > legacy.input_max_chars);
        assert!(v4.tool_result_snippet_chars > legacy.tool_result_snippet_chars);
        assert!(v4.max_tokens > legacy.max_tokens);
    }

    #[test]
    fn cache_aligned_summary_is_used_for_v4_scale_contexts() {
        let messages = vec![msg("user", "Please edit crates/tui/src/compaction.rs")];

        assert!(should_use_cache_aligned_summary(
            "deepseek-v4-flash",
            &messages
        ));
        assert!(!should_use_cache_aligned_summary(
            "deepseek-v3.2-128k",
            &messages
        ));
    }

    /// #584: the summary cache-hit percentage must be computed against
    /// `input_tokens`, not `cache_hit + cache_miss`. Providers that
    /// only populate `prompt_cache_hit_tokens` (and leave the miss
    /// field at `None`) would otherwise be reported as a flat 100%
    /// hit rate even when most of the prompt was uncached.
    #[test]
    fn summary_cache_hit_percent_uses_input_tokens_as_denominator() {
        // Both fields populated and consistent.
        assert!((summary_cache_hit_percent(800, 1000) - 80.0).abs() < f64::EPSILON);
        // No cache hit at all.
        assert!((summary_cache_hit_percent(0, 1000) - 0.0).abs() < f64::EPSILON);
        // Full cache hit.
        assert!((summary_cache_hit_percent(1000, 1000) - 100.0).abs() < f64::EPSILON);
        // Partial-telemetry guard: provider reports `cache_hit` only,
        // miss is unknown (treated as 0 by the caller). Naive
        // `hit / (hit + miss)` would have reported 100%; against
        // `input_tokens` the answer is the real share.
        assert!((summary_cache_hit_percent(200, 1000) - 20.0).abs() < f64::EPSILON);
        // Defensive: zero `input_tokens` short-circuits without a
        // divide-by-zero.
        assert!((summary_cache_hit_percent(0, 0) - 0.0).abs() < f64::EPSILON);
        assert!((summary_cache_hit_percent(50, 0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn context_window_errors_are_detected_for_summary_fallback() {
        for msg in [
            "HTTP 400 Bad Request: maximum context length is 1000000 tokens",
            "invalid_request_error: prompt is too long for the current model",
            "You requested 1000001 tokens but the maximum is 1000000",
            "request exceeds context window",
        ] {
            assert!(
                is_context_window_error(&anyhow::anyhow!(msg)),
                "expected context-window detection for `{msg}`",
            );
        }

        assert!(!is_context_window_error(&anyhow::anyhow!(
            "Invalid request: missing required field"
        )));
        assert!(!is_context_window_error(&anyhow::anyhow!(
            "503 Service Unavailable"
        )));
    }

    #[test]
    fn formatted_summary_request_bounds_large_input() {
        let messages = (0..90)
            .map(|idx| {
                msg(
                    "user",
                    &format!("turn {idx}: {}", "中文上下文 ".repeat(1_000)),
                )
            })
            .collect::<Vec<_>>();
        let limits = summary_input_limits_for_model("deepseek-v4-pro");

        let request = build_formatted_summary_request("deepseek-v4-pro", &messages, limits);

        assert_eq!(request.messages.len(), 1);
        let ContentBlock::Text { text, .. } = &request.messages[0].content[0] else {
            panic!("expected summary text request");
        };
        assert!(text.contains("characters omitted before summary"));
        assert!(text.chars().count() <= limits.input_max_chars + 2_000);
    }

    #[test]
    fn cache_aligned_summary_request_preserves_message_prefix() {
        let messages = vec![
            msg("user", "Please edit crates/tui/src/compaction.rs"),
            msg("assistant", "I will inspect the file."),
        ];
        let limits = summary_input_limits_for_model("deepseek-v4-pro");
        let request = build_cache_aligned_summary_request("deepseek-v4-pro", &messages, limits);

        assert_eq!(request.system, None);
        assert_eq!(&request.messages[..messages.len()], &messages[..]);
        assert_eq!(request.messages.len(), messages.len() + 1);
        let last = request.messages.last().expect("summary instruction");
        assert_eq!(last.role, "user");
        assert!(matches!(
            &last.content[..],
            [ContentBlock::Text { text, .. }] if text.contains("conversation above")
        ));
    }

    #[test]
    fn peel_summary_messages_for_retry_drops_old_messages_and_keeps_recent_tail() {
        let messages = (0..10)
            .map(|idx| {
                msg(
                    if idx % 2 == 0 { "user" } else { "assistant" },
                    &format!("turn {idx}"),
                )
            })
            .collect::<Vec<_>>();

        let peeled = peel_summary_messages_for_retry(&messages);

        assert_eq!(peeled.len(), 8);
        assert_eq!(message_text(&peeled[0]).trim(), "turn 2");
        assert_eq!(message_text(&peeled[4]).trim(), "turn 6");
        assert_eq!(message_text(&peeled[7]).trim(), "turn 9");
    }

    #[test]
    fn peel_summary_messages_for_retry_preserves_recent_tool_pairs() {
        let messages = vec![
            msg("user", "old 0"),
            msg("assistant", "old 1"),
            msg("user", "old 2"),
            tool_use("call_recent", "read_file", json!({ "path": "src/lib.rs" })),
            tool_result("call_recent", "recent result"),
        ];

        let peeled = peel_summary_messages_for_retry(&messages);

        assert_eq!(peeled.len(), 4);
        assert!(matches!(
            &peeled[2].content[..],
            [ContentBlock::ToolUse { id, .. }] if id == "call_recent"
        ));
        assert!(matches!(
            &peeled[3].content[..],
            [ContentBlock::ToolResult { tool_use_id, .. }] if tool_use_id == "call_recent"
        ));
    }

    struct PromptTooLongThenSummaryClient {
        inner: MockLlmClient,
        failed: std::sync::atomic::AtomicBool,
    }

    impl PromptTooLongThenSummaryClient {
        fn new() -> Self {
            let inner = MockLlmClient::new(Vec::new());
            inner.push_message_response(MessageResponse {
                id: "summary".to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "peeled summary".to_string(),
                    cache_control: None,
                }],
                model: "mock-model".to_string(),
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                container: None,
                usage: Usage::default(),
            });
            Self {
                inner,
                failed: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn captured_requests(&self) -> Vec<MessageRequest> {
            self.inner.captured_requests()
        }
    }

    impl LlmClient for PromptTooLongThenSummaryClient {
        fn provider_name(&self) -> &'static str {
            self.inner.provider_name()
        }

        fn model(&self) -> &str {
            self.inner.model()
        }

        fn create_message(
            &self,
            request: MessageRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<crate::models::MessageResponse>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    self.inner.record_request(&request);
                    return Err(anyhow::anyhow!(
                        "invalid_request_error: prompt is too long for the current model"
                    ));
                }
                self.inner.create_message(request).await
            })
        }

        fn create_message_stream(
            &self,
            request: MessageRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<StreamEventBox>> + Send + '_>>
        {
            self.inner.create_message_stream(request)
        }
    }

    #[tokio::test]
    async fn summary_prompt_too_long_peels_old_messages_and_retries() {
        let mock = PromptTooLongThenSummaryClient::new();
        let messages = (0..10)
            .map(|idx| {
                msg(
                    if idx % 2 == 0 { "user" } else { "assistant" },
                    &format!("turn {idx}"),
                )
            })
            .collect::<Vec<_>>();

        let result = create_summary(&mock, &messages, "deepseek-v3.2-128k")
            .await
            .expect("summary should succeed after peel retry");

        assert_eq!(result.text, "peeled summary");
        assert_eq!(result.retries_used, 1);
        let requests = mock.captured_requests();
        assert_eq!(requests.len(), 2);
        let ContentBlock::Text {
            text: retry_text, ..
        } = &requests[1].messages[0].content[0]
        else {
            panic!("expected formatted summary text");
        };
        assert!(!retry_text.contains("turn 0"));
        assert!(!retry_text.contains("turn 1"));
        assert!(retry_text.contains("turn 2"));
        assert!(retry_text.contains("turn 9"));
    }

    #[test]
    fn estimate_tokens_empty_messages() {
        let messages: Vec<Message> = vec![];
        assert_eq!(estimate_tokens(&messages), 0);
    }

    #[test]
    fn estimate_tokens_with_text() {
        let messages = vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello, world!".to_string(), // 13 chars = ~3 tokens
                cache_control: None,
            }],
        }];
        let tokens = estimate_tokens(&messages);
        assert!(tokens > 0 && tokens < 10);
    }

    #[test]
    fn estimate_tokens_counts_tool_round_thinking_across_turns() {
        // Per DeepSeek thinking-mode rules, any assistant message that
        // performed a tool call keeps its reasoning_content in the request
        // forever, including across new user turns. Token estimates must
        // count those bytes.
        let thinking = "reasoning ".repeat(800);
        let current_messages = vec![
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Use a tool".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::Thinking {
                        thinking: thinking.clone(),
                    },
                    ContentBlock::ToolUse {
                        id: "tool-1".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"path": "Cargo.toml"}),
                        caller: None,
                    },
                ],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    content: "manifest".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let historical_messages = {
            let mut messages = current_messages.clone();
            messages.push(Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Done.".to_string(),
                    cache_control: None,
                }],
            });
            messages.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Next question.".to_string(),
                    cache_control: None,
                }],
            });
            messages
        };
        let completed_messages = {
            let mut messages = current_messages.clone();
            messages.push(Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Done.".to_string(),
                    cache_control: None,
                }],
            });
            messages
        };

        let lower_bound = thinking.len() / 5;
        assert!(estimate_tokens(&current_messages) > lower_bound);
        assert!(estimate_tokens(&completed_messages) > lower_bound);
        assert!(estimate_tokens(&historical_messages) > lower_bound);
    }

    #[test]
    fn should_compact_respects_enabled_flag() {
        let config = CompactionConfig {
            enabled: false,
            ..Default::default()
        };
        // Even with many messages, disabled compaction should return false
        let messages: Vec<Message> = (0..100)
            .map(|_| Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "test".to_string(),
                    cache_control: None,
                }],
            })
            .collect();
        assert!(!should_compact(&messages, &config, None, None, None));
    }

    /// v0.8.11: message-count is no longer a compaction trigger. Long
    /// chats of small messages stay uncompacted because rewriting the V4
    /// prefix cache for a tiny budget reclaim is net-negative. Only token
    /// pressure (and the explicit `/compact` slash command) trigger
    /// compaction.
    #[test]
    fn message_count_no_longer_triggers_compaction() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 1_000_000,
            auto_floor_tokens: 0,
            ..Default::default()
        };

        // 200 tiny messages, well above the prior message threshold.
        let many_messages: Vec<Message> = (0..200)
            .map(|_| Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "x".to_string(),
                    cache_control: None,
                }],
            })
            .collect();
        // Token total stays minuscule so the token threshold is not hit;
        // without the prior message-count trigger, no compaction.
        assert!(!should_compact(&many_messages, &config, None, None, None));
    }

    #[test]
    fn plan_compaction_pins_recent_and_working_set_paths() {
        let messages = vec![
            msg("user", "General discussion"),
            msg("assistant", "Unrelated note"),
            msg("user", "Earlier we touched src/core/engine.rs"),
            msg("assistant", "More unrelated chatter"),
            msg("user", "Let's keep working on src/core/engine.rs"),
            msg("assistant", "Tool output mentions src/core/engine.rs too"),
            msg("assistant", "Recent reasoning"),
            msg("user", "Final recent instruction"),
        ];

        let plan = plan_compaction(&messages, None, KEEP_RECENT_MESSAGES, None, None);

        assert!(plan.pinned_indices.contains(&2));
        for idx in 4..messages.len() {
            assert!(plan.pinned_indices.contains(&idx));
        }
        assert!(plan.summarize_indices.contains(&0));
        assert!(plan.summarize_indices.contains(&1));
        assert!(plan.summarize_indices.contains(&3));
    }

    #[test]
    fn plan_compaction_respects_external_pins() {
        let messages = vec![
            msg("user", "noise 0"),
            msg("assistant", "noise 1"),
            msg("user", "noise 2"),
            msg("assistant", "noise 3"),
            msg("user", "recent 4"),
            msg("assistant", "recent 5"),
            msg("assistant", "recent 6"),
            msg("user", "recent 7"),
        ];

        let pins = vec![1usize];
        let plan = plan_compaction(&messages, None, KEEP_RECENT_MESSAGES, Some(&pins), None);

        assert!(plan.pinned_indices.contains(&1));
        assert!(!plan.summarize_indices.contains(&1));
    }

    #[test]
    fn plan_compaction_uses_external_working_set_paths() {
        let mut messages = vec![msg("user", "edit src/core/engine.rs now")];
        messages.extend((1..20).map(|i| msg("assistant", &format!("noise {i}"))));

        let working_set_paths = vec!["src/core/engine.rs".to_string()];
        let plan = plan_compaction(
            &messages,
            None,
            KEEP_RECENT_MESSAGES,
            None,
            Some(&working_set_paths),
        );

        assert!(plan.pinned_indices.contains(&0));
    }

    #[test]
    fn plan_compaction_pins_tool_calls_for_tool_results() {
        let messages = vec![
            msg("user", "noise"),
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "read_file".to_string(),
                    input: json!({"path": "src/main.rs"}),
                    caller: None,
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    content: "ok src/main.rs".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let plan = plan_compaction(&messages, None, 1, None, None);
        assert!(plan.pinned_indices.contains(&2));
        assert!(plan.pinned_indices.contains(&1));
    }

    #[test]
    fn should_compact_ignores_fully_pinned_context() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 10,
            ..Default::default()
        };

        let messages: Vec<Message> = (0..12)
            .map(|_| msg("user", "Work on src/compaction.rs right now"))
            .collect();

        assert!(!should_compact(&messages, &config, None, None, None));
    }

    // v0.8.11: removed `should_compact_counts_only_unpinned_messages` and
    // `should_compact_when_pins_consume_budget` — both tested the
    // message-count compaction trigger that v0.8.11 deleted. The
    // pinned-tokens accounting they exercised is still tested by
    // `should_compact_ignores_fully_pinned_context` below; the rest of
    // their setup has no contemporary contract to pin.

    #[test]
    fn enforce_tool_call_pairs_removes_orphaned_tool_call() {
        // An assistant message with a tool call but no matching result anywhere
        // in the history should be removed from the pinned set.
        let messages = vec![
            msg("user", "noise"),
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: "orphan-call".to_string(),
                    name: "read_file".to_string(),
                    input: json!({"path": "src/main.rs"}),
                    caller: None,
                }],
            },
            msg("assistant", "recent"),
        ];

        let mut pinned = BTreeSet::from([0, 1, 2]);
        enforce_tool_call_pairs(&messages, &mut pinned);

        // The orphaned tool call message (index 1) should be removed.
        assert!(
            !pinned.contains(&1),
            "orphaned tool call should be removed from pinned set"
        );
        // Other messages stay.
        assert!(pinned.contains(&0));
        assert!(pinned.contains(&2));
    }

    #[test]
    fn enforce_tool_call_pairs_removes_orphaned_tool_result() {
        // A tool result whose call doesn't exist anywhere should be removed.
        let messages = vec![
            msg("user", "noise"),
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "orphan-result".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            msg("assistant", "recent"),
        ];

        let mut pinned = BTreeSet::from([0, 1, 2]);
        enforce_tool_call_pairs(&messages, &mut pinned);

        assert!(
            !pinned.contains(&1),
            "orphaned tool result should be removed from pinned set"
        );
        assert!(pinned.contains(&0));
        assert!(pinned.contains(&2));
    }

    #[test]
    fn enforce_tool_call_pairs_preserves_valid_pairs() {
        // A complete call+result pair should remain intact.
        let messages = vec![
            msg("user", "do something"),
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: "tool-ok".to_string(),
                    name: "list_dir".to_string(),
                    input: json!({}),
                    caller: None,
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-ok".to_string(),
                    content: "files here".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            msg("assistant", "done"),
        ];

        let mut pinned = BTreeSet::from([1, 2, 3]);
        enforce_tool_call_pairs(&messages, &mut pinned);

        assert!(pinned.contains(&1), "tool call should stay pinned");
        assert!(pinned.contains(&2), "tool result should stay pinned");
        assert!(pinned.contains(&3));
    }

    #[test]
    fn enforce_tool_call_pairs_pins_transitive_pairs() {
        // If only the result is initially pinned, the call should be pulled in.
        // The call message may also contain another tool call whose result should
        // then be pulled in transitively.
        let messages = vec![
            msg("user", "start"),
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::ToolUse {
                        id: "t1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "a.rs"}),
                        caller: None,
                    },
                    ContentBlock::ToolUse {
                        id: "t2".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "b.rs"}),
                        caller: None,
                    },
                ],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".to_string(),
                    content: "content of a.rs".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t2".to_string(),
                    content: "content of b.rs".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            msg("assistant", "done"),
        ];

        // Only pin the result for t1 initially.
        let mut pinned = BTreeSet::from([2, 4]);
        enforce_tool_call_pairs(&messages, &mut pinned);

        // The call message (index 1) should be pulled in because t1's result is pinned.
        assert!(
            pinned.contains(&1),
            "call message should be transitively pinned"
        );
        // Since the call message also contains t2, t2's result (index 3) should also be pinned.
        assert!(
            pinned.contains(&3),
            "t2 result should be transitively pinned via the call message"
        );
    }

    #[test]
    fn enforce_tool_call_pairs_cascading_removal() {
        // Removing an orphaned call should cascade to remove its result.
        // Message 1: assistant with t1 (call) — t1 has a result at index 2
        // Message 2: user with t1 (result)
        // Message 3: assistant with t2 (call) — t2 has NO result
        // Message 4: user with t2 result referencing the call
        //
        // If t2 has no result in history, message 3 is removed. That's straightforward.
        // Here we test: if a call message is removed because ONE of its calls is orphaned,
        // the result for the other call also gets removed in subsequent iterations.
        let messages = vec![
            msg("user", "start"),
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::ToolUse {
                        id: "good".to_string(),
                        name: "read_file".to_string(),
                        input: json!({}),
                        caller: None,
                    },
                    ContentBlock::ToolUse {
                        id: "orphan".to_string(),
                        name: "shell".to_string(),
                        input: json!({}),
                        caller: None,
                    },
                ],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "good".to_string(),
                    content: "ok".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            // Note: NO result for "orphan" exists anywhere
            msg("assistant", "done"),
        ];

        let mut pinned = BTreeSet::from([1, 2, 3]);
        enforce_tool_call_pairs(&messages, &mut pinned);

        // Message 1 has an orphaned tool call ("orphan"), so it's removed.
        assert!(
            !pinned.contains(&1),
            "message with orphaned call should be removed"
        );
        // Message 2 (result for "good") now has no matching call pinned, so it's also removed.
        assert!(
            !pinned.contains(&2),
            "result whose call was removed should cascade-remove"
        );
        // Message 3 (plain text) stays.
        assert!(pinned.contains(&3));
    }

    #[test]
    fn enforce_tool_call_pairs_converges_long_chain() {
        let mut messages = vec![msg("user", "start")];
        for i in 0..15 {
            messages.push(Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: format!("t{i}"),
                    name: "read_file".to_string(),
                    input: json!({}),
                    caller: None,
                }],
            });
            messages.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: format!("t{i}"),
                    content: format!("result {i}"),
                    is_error: None,
                    content_blocks: None,
                }],
            });
        }
        messages.push(msg("assistant", "done"));

        let mut pinned: BTreeSet<usize> = (0..messages.len()).collect();
        enforce_tool_call_pairs(&messages, &mut pinned);

        // All pairs should remain intact (no orphans)
        assert_eq!(pinned.len(), messages.len());
    }

    #[test]
    fn plan_compaction_keeps_at_least_one_user_text_query() {
        let mut messages = vec![msg(
            "user",
            "This is the original query that started the chain.",
        )];

        for i in 0..10 {
            messages.push(Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: format!("call-{i}"),
                    name: "test_tool".to_string(),
                    input: json!({}),
                    caller: None,
                }],
            });
            messages.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: format!("call-{i}"),
                    content: "tool output".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            });
        }

        let plan = plan_compaction(&messages, None, KEEP_RECENT_MESSAGES, None, None);

        assert!(plan.pinned_indices.contains(&0));
    }

    // ========================================================================
    // Additional Compaction Trigger Tests
    // ========================================================================

    #[test]
    fn test_should_compact_token_threshold_triggers() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 100, // Low threshold for testing
            auto_floor_tokens: 0,
            ..Default::default()
        };

        // Create messages that exceed token threshold
        let messages: Vec<Message> = (0..10)
            .map(|_| msg("user", &"x".repeat(50))) // 50 chars = ~12 tokens each
            .collect();

        // Total tokens: ~120, which exceeds 100
        assert!(should_compact(&messages, &config, None, None, None));
    }

    #[test]
    fn test_should_compact_below_token_threshold() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 1000,
            ..Default::default()
        };

        // Create short messages
        let messages: Vec<Message> = (0..5).map(|_| msg("user", "short")).collect();

        assert!(!should_compact(&messages, &config, None, None, None));
    }

    #[test]
    fn auto_compaction_default_floor_allows_sub_500k_providers() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 100,
            auto_floor_tokens: MINIMUM_AUTO_COMPACTION_TOKENS,
            ..Default::default()
        };

        let messages: Vec<Message> = (0..10).map(|_| msg("user", &"x".repeat(50))).collect();
        assert!(should_compact(&messages, &config, None, None, None));
    }

    #[test]
    fn explicit_auto_compaction_floor_still_blocks_when_requested() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 100,
            auto_floor_tokens: 500_000,
            ..Default::default()
        };

        let messages: Vec<Message> = (0..10).map(|_| msg("user", &"x".repeat(50))).collect();
        assert!(!should_compact(&messages, &config, None, None, None));
    }

    #[test]
    fn explicit_auto_compaction_floor_yields_to_threshold_logic_above_floor() {
        let config = CompactionConfig {
            enabled: true,
            token_threshold: 2_000_000,
            auto_floor_tokens: 500_000,
            ..Default::default()
        };

        let messages: Vec<Message> = (0..1100).map(|_| msg("user", &"x".repeat(2000))).collect();
        assert!(!should_compact(&messages, &config, None, None, None));

        let config_lower = CompactionConfig {
            token_threshold: 100_000,
            ..config
        };
        assert!(should_compact(&messages, &config_lower, None, None, None));
    }

    #[test]
    fn compaction_config_default_has_no_provider_specific_floor() {
        let config = CompactionConfig::default();
        assert_eq!(config.auto_floor_tokens, MINIMUM_AUTO_COMPACTION_TOKENS);
        assert_eq!(config.auto_floor_tokens, 0);
    }

    #[test]
    fn test_plan_compaction_pins_error_messages() {
        let messages = vec![
            msg("user", "normal message"),
            msg("assistant", "error: compilation failed"),
            msg("user", "another message"),
            msg("assistant", "panic at src/main.rs:42"),
            msg("user", "more chat"),
            msg("assistant", "Traceback (most recent call last):"),
            msg("user", "recent 1"),
            msg("assistant", "recent 2"),
        ];

        let plan = plan_compaction(&messages, None, KEEP_RECENT_MESSAGES, None, None);

        // Error messages should be pinned
        assert!(plan.pinned_indices.contains(&1)); // error:
        assert!(plan.pinned_indices.contains(&3)); // panic
        assert!(plan.pinned_indices.contains(&5)); // traceback
    }

    #[test]
    fn test_plan_compaction_pins_patch_messages() {
        let messages = vec![
            msg("user", "normal chat"),
            msg("assistant", "diff --git a/src/main.rs b/src/main.rs"),
            msg("user", "more chat"),
            msg("assistant", "+++ b/src/core.rs"),
            msg("user", "chat"),
            msg("assistant", "```diff\n-some code\n+new code\n```"),
            msg("user", "recent 1"),
            msg("assistant", "recent 2"),
        ];

        let plan = plan_compaction(&messages, None, KEEP_RECENT_MESSAGES, None, None);

        // Patch/diff messages should be pinned
        assert!(plan.pinned_indices.contains(&1)); // diff --git
        assert!(plan.pinned_indices.contains(&3)); // +++ b/
        assert!(plan.pinned_indices.contains(&5)); // ```diff
    }

    #[test]
    fn test_plan_compaction_pins_apply_patch_tool_calls() {
        let messages = vec![
            msg("user", "normal chat"),
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: "patch-1".to_string(),
                    name: "apply_patch".to_string(),
                    input: json!({"patch": "diff content"}),
                    caller: None,
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "patch-1".to_string(),
                    content: "Patch applied successfully".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            msg("assistant", "more chat"),
            msg("user", "even more"),
            msg("assistant", "recent 1"),
            msg("user", "recent 2"),
            msg("assistant", "recent 3"),
        ];

        let plan = plan_compaction(&messages, None, KEEP_RECENT_MESSAGES, None, None);

        // Message 1 contains apply_patch tool call with matching result (message 2)
        // Both should be pinned due to tool call pairing
        // Messages 5, 6, 7, 8 are recent (last 4 messages)
        eprintln!("Pinned indices: {:?}", plan.pinned_indices);

        // apply_patch tool call and its result should be pinned
        assert!(
            plan.pinned_indices.contains(&1),
            "apply_patch tool call should be pinned"
        );
        assert!(
            plan.pinned_indices.contains(&2),
            "apply_patch tool result should be pinned"
        );
    }

    #[test]
    fn test_extract_paths_from_text_finds_various_formats() {
        let text = r#"
            I'm working on src/main.rs
            Also check Cargo.toml
            The error is in src/core/engine.rs:42
            See docs/API.md for details
            Config at config.example.toml
        "#;

        let paths = extract_paths_from_text(text, None);

        assert!(paths.iter().any(|p| p == "src/main.rs"));
        assert!(paths.iter().any(|p| p == "Cargo.toml"));
        assert!(paths.iter().any(|p| p == "src/core/engine.rs"));
        assert!(paths.iter().any(|p| p == "docs/API.md"));
        assert!(paths.iter().any(|p| p == "config.example.toml"));
    }

    #[test]
    fn test_extract_paths_from_tool_input_finds_path_field() {
        let input = json!({
            "path": "src/main.rs",
            "content": "test"
        });

        let paths = extract_paths_from_tool_input(&input, None);
        assert!(paths.iter().any(|p| p == "src/main.rs"));
    }

    #[test]
    fn test_extract_paths_from_tool_input_finds_paths_array() {
        let input = json!({
            "paths": ["src/main.rs", "src/core.rs", "tests/test.rs"]
        });

        let paths = extract_paths_from_tool_input(&input, None);
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p == "src/main.rs"));
        assert!(paths.iter().any(|p| p == "src/core.rs"));
        assert!(paths.iter().any(|p| p == "tests/test.rs"));
    }

    #[test]
    fn test_extract_paths_from_tool_input_finds_cwd() {
        let input = json!({
            "cwd": "src/core",
            "command": "cargo build"
        });

        let paths = extract_paths_from_tool_input(&input, None);
        assert!(paths.iter().any(|p| p == "src/core"));
    }

    #[test]
    fn test_normalize_path_candidate_handles_absolute_paths() {
        use std::env;
        let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Create an absolute path
        let absolute_path = current_dir.join("src/main.rs");
        let absolute_path_str = absolute_path.to_string_lossy();

        let normalized = normalize_path_candidate(&absolute_path_str, Some(&current_dir));

        assert_eq!(normalized, Some("src/main.rs".to_string()));
    }

    #[test]
    fn test_normalize_path_candidate_rejects_parent_refs() {
        let normalized = normalize_path_candidate("../outside/file.rs", Some(&PathBuf::from(".")));
        assert_eq!(normalized, None);
    }

    #[test]
    fn test_normalize_path_candidate_cleans_backslashes() {
        let normalized = normalize_path_candidate("src\\main.rs", Some(&PathBuf::from(".")));
        assert_eq!(normalized, Some("src/main.rs".to_string()));
    }

    #[test]
    fn test_merge_system_prompts_none_none() {
        let result = merge_system_prompts(None, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_merge_system_prompts_some_text_none() {
        let original = Some(SystemPrompt::Text("original".to_string()));
        let result = merge_system_prompts(original.as_ref(), None);
        assert!(matches!(result, Some(SystemPrompt::Text(s)) if s == "original"));
    }

    #[test]
    fn test_merge_system_prompts_none_some_blocks() {
        let summary = Some(SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "summary".to_string(),
            cache_control: None,
        }]));
        let result = merge_system_prompts(None, summary);
        assert!(matches!(result, Some(SystemPrompt::Blocks(b)) if b.len() == 1));
    }

    #[test]
    fn test_merge_system_prompts_text_plus_blocks() {
        let original = Some(SystemPrompt::Text("original".to_string()));
        let summary = Some(SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "summary".to_string(),
            cache_control: None,
        }]));

        let result = merge_system_prompts(original.as_ref(), summary);

        match result {
            Some(SystemPrompt::Blocks(blocks)) => {
                assert_eq!(blocks.len(), 2);
                assert!(matches!(&blocks[0], SystemBlock { text, .. } if text == "original"));
                assert!(matches!(&blocks[1], SystemBlock { text, .. } if text == "summary"));
            }
            _ => panic!("Expected Blocks"),
        }
    }

    #[test]
    fn test_merge_system_prompts_blocks_plus_blocks() {
        let original = Some(SystemPrompt::Blocks(vec![
            SystemBlock {
                block_type: "text".to_string(),
                text: "orig1".to_string(),
                cache_control: None,
            },
            SystemBlock {
                block_type: "text".to_string(),
                text: "orig2".to_string(),
                cache_control: None,
            },
        ]));

        let summary = Some(SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "summary".to_string(),
            cache_control: None,
        }]));

        let result = merge_system_prompts(original.as_ref(), summary);

        match result {
            Some(SystemPrompt::Blocks(blocks)) => {
                assert_eq!(blocks.len(), 3);
                assert!(matches!(&blocks[0], SystemBlock { text, .. } if text == "orig1"));
                assert!(matches!(&blocks[1], SystemBlock { text, .. } if text == "orig2"));
                assert!(matches!(&blocks[2], SystemBlock { text, .. } if text == "summary"));
            }
            _ => panic!("Expected Blocks"),
        }
    }

    #[test]
    fn test_merge_system_prompts_blocks_plus_text() {
        let original = Some(SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "original".to_string(),
            cache_control: None,
        }]));

        let summary = Some(SystemPrompt::Text("summary".to_string()));

        let result = merge_system_prompts(original.as_ref(), summary);

        match result {
            Some(SystemPrompt::Blocks(blocks)) => {
                assert_eq!(blocks.len(), 2);
                assert!(matches!(&blocks[0], SystemBlock { text, .. } if text == "original"));
                assert!(matches!(&blocks[1], SystemBlock { text, .. } if text == "summary"));
            }
            _ => panic!("Expected Blocks"),
        }
    }

    #[test]
    fn test_compaction_result_retries_used() {
        // This test verifies the CompactionResult structure
        let result = CompactionResult {
            messages: vec![],
            summary_prompt: None,
            removed_messages: vec![],
            retries_used: 2,
        };

        assert_eq!(result.retries_used, 2);
        assert!(result.messages.is_empty());
        assert!(result.removed_messages.is_empty());
    }

    #[test]
    fn test_should_compact_with_workspace_path_detection() {
        use std::env;
        let workspace = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let _config = CompactionConfig {
            enabled: true,
            token_threshold: 1000,
            ..Default::default()
        };

        // Create messages mentioning workspace paths
        let messages = vec![
            msg("user", "working on src/main.rs"),
            msg("assistant", "noise 1"),
            msg("user", "noise 2"),
            msg("assistant", "noise 3"),
            msg("user", "noise 4"),
            msg("assistant", "noise 5"),
            msg("user", "recent 1"),
            msg("assistant", "recent 2"),
        ];

        // src/main.rs mention should pin message 0 in the plan.
        let plan = plan_compaction(
            &messages,
            Some(&workspace),
            KEEP_RECENT_MESSAGES,
            None,
            None,
        );
        assert!(plan.pinned_indices.contains(&0)); // src/main.rs mention
    }

    // === PreCompact hook + session-memory-first enhancements (#485) ===

    use crate::compaction::session_memory_compact::SessionMemoryCompactConfig;
    use crate::hooks::{Hook, HookContext, HookEvent, HookExecutor, HooksConfig};

    fn over_threshold_config() -> CompactionConfig {
        CompactionConfig {
            enabled: true,
            token_threshold: 10,
            model: "deepseek-v3.2-128k".to_string(),
            cache_summary: false,
            auto_floor_tokens: 0,
        }
    }

    /// 12 messages, ~400 chars each (~130 tokens) → well over the tiny test
    /// threshold and enough for `plan_compaction` to mark 8 for summarization.
    fn long_conversation() -> Vec<Message> {
        let long = "x".repeat(400);
        (0..12)
            .map(|i| {
                msg(
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("{long} turn {i}"),
                )
            })
            .collect()
    }

    fn collect_summary_text(prompt: &Option<SystemPrompt>) -> String {
        match prompt {
            Some(SystemPrompt::Text(t)) => t.clone(),
            Some(SystemPrompt::Blocks(blocks)) => blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            None => String::new(),
        }
    }

    #[tokio::test]
    async fn compact_messages_safe_session_memory_first_avoids_llm_call() {
        let mock = MockLlmClient::new(Vec::new());
        let config = over_threshold_config();
        let messages = long_conversation();

        let enhancements = CompactionEnhancements {
            hooks: None,
            session_memory: Some(SessionMemorySidecar {
                memory_content: "## Project memory\n- fact A\n- fact B".to_string(),
                config: SessionMemoryCompactConfig {
                    enabled: true,
                    min_retain_tokens: 50,
                    max_retain_tokens: 200,
                },
            }),
        };

        let result = compact_messages_safe(
            &mock,
            &messages,
            &config,
            None,
            None,
            None,
            Some(&enhancements),
        )
        .await
        .expect("session-memory-first compaction should succeed");

        // No LLM call was made — the mock queue is untouched.
        assert_eq!(
            mock.call_count(),
            0,
            "session-memory-first must not hit the LLM"
        );
        assert!(result.messages.len() < messages.len());
        assert!(result.summary_prompt.is_some());
        let prompt_text = collect_summary_text(&result.summary_prompt);
        assert!(prompt_text.contains("Session Memory"));
    }

    #[tokio::test]
    async fn compact_messages_safe_session_memory_falls_through_to_llm_when_nothing_removed() {
        let mock = MockLlmClient::new(Vec::new());
        mock.push_message_response(MessageResponse {
            id: "summary".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "llm summary".to_string(),
                cache_control: None,
            }],
            model: "deepseek-v3.2-128k".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        });

        let config = over_threshold_config();
        let messages = long_conversation();

        // Session-memory applies (memory present, over min_retain) but the
        // huge max_retain means nothing is removed → must fall through to LLM.
        let enhancements = CompactionEnhancements {
            hooks: None,
            session_memory: Some(SessionMemorySidecar {
                memory_content: "## Project memory\n- fact A".to_string(),
                config: SessionMemoryCompactConfig {
                    enabled: true,
                    min_retain_tokens: 50,
                    max_retain_tokens: 10_000_000,
                },
            }),
        };

        let result = compact_messages_safe(
            &mock,
            &messages,
            &config,
            None,
            None,
            None,
            Some(&enhancements),
        )
        .await
        .expect("fall-through LLM compaction should succeed");

        assert!(
            mock.call_count() >= 1,
            "should have fallen through to the LLM"
        );
        let prompt_text = collect_summary_text(&result.summary_prompt);
        assert!(prompt_text.contains("llm summary"));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn compact_messages_safe_pre_compact_hook_merges_preserve_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hook = Hook::new(HookEvent::PreCompact, "printf 'preserve-this-fact'");
        let hooks_config = HooksConfig {
            enabled: true,
            hooks: vec![hook],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(hooks_config, dir.path().to_path_buf());

        let mock = MockLlmClient::new(Vec::new());
        mock.push_message_response(MessageResponse {
            id: "summary".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "llm summary body".to_string(),
                cache_control: None,
            }],
            model: "deepseek-v3.2-128k".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        });

        let config = over_threshold_config();
        let messages = long_conversation();

        let enhancements = CompactionEnhancements {
            hooks: Some((Arc::new(executor), HookContext::new())),
            session_memory: None,
        };

        let result = compact_messages_safe(
            &mock,
            &messages,
            &config,
            None,
            None,
            None,
            Some(&enhancements),
        )
        .await
        .expect("LLM compaction with hook should succeed");

        assert!(mock.call_count() >= 1, "LLM compaction should have run");
        let prompt_text = collect_summary_text(&result.summary_prompt);
        assert!(prompt_text.contains("llm summary body"));
        assert!(prompt_text.contains("preserve-this-fact"));
        assert!(prompt_text.contains("Context to Preserve"));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn compact_messages_safe_pre_compact_hook_merges_into_session_memory_early_return() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hook = Hook::new(HookEvent::PreCompact, "printf 'keep-this-forever'");
        let hooks_config = HooksConfig {
            enabled: true,
            hooks: vec![hook],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(hooks_config, dir.path().to_path_buf());

        let mock = MockLlmClient::new(Vec::new());
        let config = over_threshold_config();
        let messages = long_conversation();

        let enhancements = CompactionEnhancements {
            hooks: Some((Arc::new(executor), HookContext::new())),
            session_memory: Some(SessionMemorySidecar {
                memory_content: "## Project memory\n- fact A".to_string(),
                config: SessionMemoryCompactConfig {
                    enabled: true,
                    min_retain_tokens: 50,
                    max_retain_tokens: 200,
                },
            }),
        };

        let result = compact_messages_safe(
            &mock,
            &messages,
            &config,
            None,
            None,
            None,
            Some(&enhancements),
        )
        .await
        .expect("session-memory-first + hook should succeed");

        // Early return — no LLM call.
        assert_eq!(mock.call_count(), 0);
        let prompt_text = collect_summary_text(&result.summary_prompt);
        assert!(prompt_text.contains("Session Memory"));
        assert!(prompt_text.contains("keep-this-forever"));
    }

    #[test]
    fn merge_preserve_context_leaves_summary_untouched_when_empty() {
        let summary = Some(SystemPrompt::Text("orig".to_string()));
        assert_eq!(merge_preserve_context(summary.clone(), None), summary);
        assert_eq!(
            merge_preserve_context(summary.clone(), Some("   ")),
            summary
        );
    }

    #[test]
    fn merge_preserve_context_appends_block_to_blocks_summary() {
        let summary = Some(SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "orig block".to_string(),
            cache_control: None,
        }]));
        let merged = merge_preserve_context(summary, Some("save me")).expect("Some");
        match merged {
            SystemPrompt::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert!(blocks[1].text.contains("save me"));
                assert!(blocks[1].text.contains("Context to Preserve"));
            }
            _ => panic!("expected Blocks"),
        }
    }

    #[test]
    fn merge_preserve_context_creates_blocks_when_summary_none() {
        let merged = merge_preserve_context(None, Some("brand new")).expect("Some");
        match merged {
            SystemPrompt::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert!(blocks[0].text.contains("brand new"));
            }
            _ => panic!("expected Blocks"),
        }
    }
}
