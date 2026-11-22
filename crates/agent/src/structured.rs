//! # Structured output
//!
//! The LangChain `with_structured_output` analog (tool strategy): extract a
//! typed `T` from one model call by registering `T`'s JSON schema as the
//! single available tool, forcing tool use (`tool_choice: "required"`, the
//! wire convention the strict-tool path already uses), and validating the
//! tool input by deserializing it into `T`. A validation failure is fed back
//! as an error `ToolResult` and retried, up to `max_attempts`.
//!
//! Known limitations (deliberate):
//! - `tool_choice` is provider-dependent. `"required"` forces *a* tool call
//!   on OpenAI-compatible backends; backends that ignore it free-choose, and
//!   the validation + feedback-retry loop below is the backstop — a model
//!   that never calls the tool surfaces as a loud error after
//!   `max_attempts`.
//! - No provider-native JSON mode (`response_format`): this is the tool
//!   emulation only. A provider strategy belongs to the provider layer.
//! - `T`'s schema is `schemars::schema_for!` output as-is; root alternatives
//!   (untagged enums) render as `anyOf` and are weakly constrained.

use std::marker::PhantomData;

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::llm_client::LlmClientHandle;
use crate::models::{ContentBlock, Message, MessageRequest, MessageResponse, SystemPrompt, Tool};

/// The tool name the answer is submitted through. Fixed so transcripts are
/// greppable and providers can cache the tool schema.
pub const STRUCTURED_OUTPUT_TOOL_NAME: &str = "submit_structured_output";

/// Run one structured-output exchange: `prompt` in, typed `T` out.
///
/// `max_attempts` bounds the validation-feedback retries (`1` = no retry).
#[allow(clippy::needless_pass_by_value)] // docs/examples pass an owned Arc
pub async fn structured_output<T: DeserializeOwned + JsonSchema>(
    client: LlmClientHandle,
    prompt: &str,
    max_attempts: usize,
) -> Result<T> {
    StructuredOutput::<T>::new(client)
        .run(prompt, max_attempts)
        .await
}

/// Extract the raw tool-input [`serde_json::Value`] from a response — the
/// untyped half of the loop, public for callers that validate themselves.
#[must_use]
pub fn structured_output_value(response: &MessageResponse) -> Option<&serde_json::Value> {
    response.content.iter().find_map(|block| match block {
        ContentBlock::ToolUse { name, input, .. } if name == STRUCTURED_OUTPUT_TOOL_NAME => {
            Some(input)
        }
        _ => None,
    })
}

struct StructuredOutput<T> {
    client: LlmClientHandle,
    schema: serde_json::Value,
    _marker: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned + JsonSchema> StructuredOutput<T> {
    fn new(client: LlmClientHandle) -> Self {
        Self {
            client,
            // `schema_for!` output serializes straight into the wire
            // `input_schema` slot; preserve_order keeps field order stable.
            schema: serde_json::to_value(schemars::schema_for!(T))
                .unwrap_or_else(|err| panic!("JSON schema for T must serialize: {err}")),
            _marker: PhantomData,
        }
    }

    async fn run(&self, prompt: &str, max_attempts: usize) -> Result<T> {
        if max_attempts == 0 {
            bail!("max_attempts must be at least 1");
        }
        let mut messages = vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
                cache_control: None,
            }],
        }];

        let mut last_error = String::new();
        for attempt in 1..=max_attempts {
            let response = self.request(messages.clone()).await?;
            // Keep the assistant turn for the retry transcript regardless of
            // whether it called the tool.
            messages.push(Message {
                role: "assistant".to_string(),
                content: response.content.clone(),
            });

            let Some(input) = structured_output_value(&response) else {
                last_error = format!(
                    "model answered without calling the '{STRUCTURED_OUTPUT_TOOL_NAME}' tool"
                );
                if attempt == max_attempts {
                    break;
                }
                messages.push(feedback(
                    &last_error,
                    format!("Call the '{STRUCTURED_OUTPUT_TOOL_NAME}' tool with your answer."),
                ));
                continue;
            };

            match serde_json::from_value::<T>(input.clone()) {
                Ok(value) => return Ok(value),
                Err(err) => {
                    last_error = format!("validation failed: {err}");
                    if attempt == max_attempts {
                        break;
                    }
                    messages.push(feedback(
                        &last_error,
                        format!(
                            "Resubmit via '{STRUCTURED_OUTPUT_TOOL_NAME}' with the violation fixed."
                        ),
                    ));
                }
            }
        }
        bail!(
            "structured output did not produce a valid value after {max_attempts} attempt(s): \
             {last_error}"
        )
    }

    async fn request(&self, messages: Vec<Message>) -> Result<MessageResponse> {
        let request = MessageRequest {
            model: self.client.model().to_string(),
            messages,
            max_tokens: 4096,
            system: Some(SystemPrompt::Text(format!(
                "You answer exclusively through the '{STRUCTURED_OUTPUT_TOOL_NAME}' tool."
            ))),
            tools: Some(vec![self.tool()]),
            // The wire convention for "a tool call is required" — the same
            // value the strict-tool path sends. See the module doc for the
            // provider-dependence caveat.
            tool_choice: Some(json!("required")),
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: Some(false),
            temperature: None,
            top_p: None,
        };
        self.client.create_message(request).await
    }

    fn tool(&self) -> Tool {
        Tool {
            tool_type: None,
            name: STRUCTURED_OUTPUT_TOOL_NAME.to_string(),
            description: "Submit the final answer. The input must satisfy the JSON schema."
                .to_string(),
            input_schema: self.schema.clone(),
            output_schema: None,
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }
}

/// The error-feedback turn: a `role: "user"` message carrying the failed
/// `ToolResult` (is_error) plus a plain-text instruction block.
fn feedback(error: &str, instruction: String) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![
            ContentBlock::ToolResult {
                // No real tool_use_id exists for a never-made call; a stable
                // marker keeps the pairing predicate honest for providers
                // that check it.
                tool_use_id: STRUCTURED_OUTPUT_TOOL_NAME.to_string(),
                content: format!("Error: {error}"),
                is_error: Some(true),
                content_blocks: None,
            },
            ContentBlock::Text {
                text: instruction,
                cache_control: None,
            },
        ],
    }
}

// === Tests ==================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::LlmClient;
    use crate::models::Usage;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema, PartialEq)]
    struct Answer {
        city: String,
        population: u64,
    }

    /// Pops one canned response per call, mirroring the executor-test mock.
    struct ScriptedClient {
        calls: Mutex<std::collections::VecDeque<MessageResponse>>,
    }

    fn tool_response(input: serde_json::Value) -> MessageResponse {
        MessageResponse {
            id: "r".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "t1".to_string(),
                name: STRUCTURED_OUTPUT_TOOL_NAME.to_string(),
                input,
                caller: None,
            }],
            model: "mock".to_string(),
            stop_reason: Some("tool_use".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        }
    }

    fn text_response(text: &str) -> MessageResponse {
        MessageResponse {
            id: "r".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            model: "mock".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        }
    }

    impl LlmClient for ScriptedClient {
        fn provider_name(&self) -> &'static str {
            "scripted"
        }
        fn model(&self) -> &str {
            "scripted-v0"
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
            let next = self.calls.lock().unwrap().pop_front();
            Box::pin(
                async move { next.ok_or_else(|| anyhow::anyhow!("scripted client exhausted")) },
            )
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<crate::llm_client::StreamEventBox>> + Send + '_>>
        {
            Box::pin(async { bail!("scripted client does not stream") })
        }
    }

    fn client(responses: Vec<MessageResponse>) -> LlmClientHandle {
        Arc::new(ScriptedClient {
            calls: Mutex::new(responses.into()),
        })
    }

    #[tokio::test]
    async fn valid_first_attempt_returns_typed_value() {
        let handle = client(vec![tool_response(
            serde_json::json!({"city": "Tokyo", "population": 37_400_000}),
        )]);
        let answer: Answer = structured_output(handle, "biggest city?", 1)
            .await
            .expect("valid input parses");
        assert_eq!(
            answer,
            Answer {
                city: "Tokyo".to_string(),
                population: 37_400_000,
            }
        );
    }

    #[tokio::test]
    async fn validation_failure_feeds_back_and_retries() {
        // Attempt 1: wrong types. Attempt 2: valid.
        let handle = client(vec![
            tool_response(serde_json::json!({"city": "Tokyo", "population": "lots"})),
            tool_response(serde_json::json!({"city": "Tokyo", "population": 37400000})),
        ]);
        let answer: Answer = structured_output(handle, "biggest city?", 2)
            .await
            .expect("second attempt validates");
        assert_eq!(answer.population, 37_400_000);
    }

    #[tokio::test]
    async fn exhausted_attempts_fail_loud_naming_the_error() {
        let handle = client(vec![
            tool_response(serde_json::json!({"city": "Tokyo"})),
            tool_response(serde_json::json!({"city": "Tokyo"})),
        ]);
        let err = structured_output::<Answer>(handle, "biggest city?", 2)
            .await
            .expect_err("both attempts invalid");
        let msg = format!("{err:#}");
        assert!(msg.contains("after 2 attempt(s)"), "{msg}");
        assert!(msg.contains("validation failed"), "{msg}");
    }

    #[tokio::test]
    async fn text_answer_without_tool_call_retries_then_fails() {
        let handle = client(vec![
            text_response("Tokyo, obviously"),
            tool_response(serde_json::json!({"city": "Tokyo", "population": 1})),
        ]);
        let answer: Answer = structured_output(handle, "biggest city?", 2)
            .await
            .expect("tool call on retry parses");
        assert_eq!(answer.city, "Tokyo");

        let handle = client(vec![text_response("no tools here")]);
        let err = structured_output::<Answer>(handle, "biggest city?", 1)
            .await
            .expect_err("no tool call and no retries");
        assert!(
            format!("{err:#}").contains("without calling"),
            "names the failure mode"
        );
    }
}
