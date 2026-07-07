//! Prompt-building and cache-inspection helpers for the OpenAI-compatible
//! chat-completions surface.
//!
//! DeepSeek traffic now routes through the rig-based provider adapter
//! (`resolve_llm_client`); this module retains only the prompt-construction
//! primitives shared with that adapter (`to_api_tool_name`,
//! `system_to_instructions`) plus the `chat` submodule's cache-inspection and
//! cache-warmup entry points.

use crate::models::{MessageRequest, SystemPrompt};

pub(super) fn to_api_tool_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if ch == '-' {
            out.push_str("--");
        } else {
            out.push_str("-x");
            out.push_str(&format!("{:06X}", ch as u32));
            out.push('-');
        }
    }
    out
}
pub(super) fn system_to_instructions(system: Option<SystemPrompt>) -> Option<String> {
    match system {
        Some(SystemPrompt::Text(text)) => Some(text),
        Some(SystemPrompt::Blocks(blocks)) => {
            let joined = blocks
                .into_iter()
                .map(|b| b.text)
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");
            if joined.trim().is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        None => None,
    }
}
mod chat;

pub(crate) use chat::{CacheWarmupKey, PromptInspection};

pub(crate) fn inspect_prompt_for_request(request: &MessageRequest) -> PromptInspection {
    chat::inspect_prompt_for_request(request)
}

pub(crate) fn build_cache_warmup_request(request: &MessageRequest) -> MessageRequest {
    chat::build_cache_warmup_request(request)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::chat::build_chat_messages_for_request;
    use crate::models::{ContentBlock, Message, MessageRequest, SystemPrompt, Tool};
    use serde_json::{json, Value};

    fn test_tool(name: &str) -> Tool {
        Tool {
            tool_type: None,
            name: name.to_string(),
            description: format!("{name} test tool"),
            input_schema: json!({
                "type": "object",
                "properties": {},
            }),
            output_schema: None,
            allowed_callers: None,
            defer_loading: Some(false),
            input_examples: None,
            strict: Some(true),
            cache_control: None,
        }
    }

    #[test]
    fn prompt_inspect_reports_stable_layers_and_dynamic_user_task() {
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![
                Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Prior answer".to_string(),
                        cache_control: None,
                    }],
                },
                Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Current task".to_string(),
                        cache_control: None,
                    }],
                },
            ],
            max_tokens: 1024,
            system: Some(SystemPrompt::Text(
                "Base policy\n\n<project_instructions source=\"AGENTS.md\">\nRules\n</project_instructions>\n\n## Project Context Pack\n\n<project_context_pack>\n{}\n</project_context_pack>\n\n## Environment\n\n- lang: en"
                    .to_string(),
            )),
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("max".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        };

        let inspection = inspect_prompt_for_request(&request);

        assert_eq!(inspection.base_static_prefix_hash.len(), 64);
        assert_eq!(inspection.full_request_prefix_hash.len(), 64);
        assert!(inspection.layers.iter().any(|layer| {
            layer.name == "Global system prefix"
                && layer.stability.label() == "static"
                && layer.char_len == "Base policy".chars().count()
                && layer.sha256.len() == 64
        }));
        assert!(inspection.layers.iter().any(|layer| {
            layer.name == "Project context" && layer.stability.label() == "static"
        }));
        assert!(inspection.layers.iter().any(|layer| {
            layer.name == "Project context pack" && layer.stability.label() == "static"
        }));
        assert!(inspection.layers.iter().any(|layer| {
            layer.name == "Message #1 assistant" && layer.stability.label() == "history"
        }));
        assert!(
            inspection.layers.last().is_some_and(
                |layer| layer.name == "User task" && layer.stability.label() == "dynamic"
            )
        );
    }

    #[test]
    fn prompt_inspect_keeps_static_base_hash_across_different_user_tasks() {
        fn request_with_user_task(task: &str) -> MessageRequest {
            MessageRequest {
                model: "deepseek-v4-pro".to_string(),
                messages: vec![
                    Message {
                        role: "assistant".to_string(),
                        content: vec![ContentBlock::Text {
                            text: "Prior answer".to_string(),
                            cache_control: None,
                        }],
                    },
                    Message {
                        role: "user".to_string(),
                        content: vec![ContentBlock::Text {
                            text: task.to_string(),
                            cache_control: None,
                        }],
                    },
                ],
                max_tokens: 1024,
                system: Some(SystemPrompt::Text(
                    "Base policy\n\n## Environment\n\n- shell: powershell\n\n## Skills\n\n- rust\n\n## Context Management\n\nKeep concise\n\n## Compact\n\nTemplate"
                        .to_string(),
                )),
                tools: None,
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: Some("max".to_string()),
                stream: None,
                temperature: None,
                top_p: None,
            }
        }

        let first = inspect_prompt_for_request(&request_with_user_task("First task"));
        let second = inspect_prompt_for_request(&request_with_user_task("Second task"));
        let mut changed_history_request = request_with_user_task("Second task");
        changed_history_request.messages[0] = Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "Different prior answer".to_string(),
                cache_control: None,
            }],
        };
        let changed_history = inspect_prompt_for_request(&changed_history_request);

        assert_eq!(
            first.base_static_prefix_hash,
            second.base_static_prefix_hash
        );
        assert_eq!(
            first.full_request_prefix_hash, second.full_request_prefix_hash,
            "full request prefix excludes the final dynamic user task"
        );
        assert_ne!(
            second.full_request_prefix_hash, changed_history.full_request_prefix_hash,
            "full request prefix can change when session history changes"
        );
        assert!(
            second.layers.last().is_some_and(
                |layer| layer.name == "User task" && layer.stability.label() == "dynamic"
            ),
            "current user task must remain the final layer"
        );
        assert!(second.layers.iter().any(|layer| {
            layer.name == "Message #1 assistant" && layer.stability.label() == "history"
        }));
        assert!(!second.layers.iter().any(
            |layer| layer.name.starts_with("Message #") && layer.stability.label() == "static"
        ));
    }

    #[test]
    fn prompt_inspect_tracks_tool_catalog_in_static_prefix_hash() {
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Current task".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 1024,
            system: Some(SystemPrompt::Text("Base policy".to_string())),
            tools: Some(vec![test_tool("read_file")]),
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("max".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        };

        let first = inspect_prompt_for_request(&request);
        let mut changed_tools = request.clone();
        changed_tools.tools = Some(vec![test_tool("read_file"), test_tool("grep_files")]);
        let second = inspect_prompt_for_request(&changed_tools);

        assert!(
            first.layers.iter().any(|layer| {
                layer.name == "Tool catalog" && layer.stability.label() == "static"
            })
        );
        assert_ne!(
            first.base_static_prefix_hash, second.base_static_prefix_hash,
            "tool schema changes must be visible to cache-inspect base prefix diagnostics"
        );
        assert_ne!(
            first.full_request_prefix_hash, second.full_request_prefix_hash,
            "tool schema changes must be visible to full reusable-prefix diagnostics"
        );
    }

    #[test]
    fn cache_warmup_request_reuses_stable_prefix_and_fixed_user_tail() {
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![
                Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Stable prior answer".to_string(),
                        cache_control: None,
                    }],
                },
                Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Dynamic latest user task".to_string(),
                        cache_control: None,
                    }],
                },
            ],
            max_tokens: 1024,
            system: Some(SystemPrompt::Text(
                "Base policy\n\n<project_instructions source=\"AGENTS.md\">\nStable project rules\n</project_instructions>\n\n## Previous Session Relay\n\nDynamic relay"
                    .to_string(),
            )),
            tools: Some(vec![test_tool("read_file")]),
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("max".to_string()),
            stream: Some(true),
            temperature: Some(0.7),
            top_p: None,
        };

        let warmup = build_cache_warmup_request(&request);

        assert_eq!(warmup.max_tokens, 8);
        assert_eq!(warmup.temperature, Some(0.0));
        assert_eq!(warmup.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(warmup.tools.as_ref().map(Vec::len), Some(1));
        assert_eq!(warmup.tool_choice, Some(json!("none")));
        assert_eq!(warmup.messages.len(), 2);
        assert_eq!(warmup.messages[0].role, "assistant");
        assert_eq!(warmup.messages[1].role, "user");
        assert_eq!(
            warmup.messages[1].content,
            vec![ContentBlock::Text {
                text: "请只回复 OK".to_string(),
                cache_control: None,
            }]
        );

        let wire = build_chat_messages_for_request(&warmup);
        let system = wire
            .first()
            .and_then(|value| value.get("content"))
            .and_then(Value::as_str)
            .expect("warmup system prompt");
        assert!(system.contains("Stable project rules"));
        assert!(!system.contains("Dynamic relay"));
        assert!(
            !wire
                .iter()
                .any(|value| value.to_string().contains("Dynamic latest user task")),
            "warmup must not include the dynamic latest user task"
        );
    }

}
