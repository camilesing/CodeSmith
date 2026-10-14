//! Mock (echo) provider — a no-network [`LlmClient`] for tests and demos.
//!
//! The mock echoes the last user message back as the assistant response. It is
//! the reference [`ProviderFactory`](codesmith_agent::provider::ProviderFactory)
//! sample: the smallest self-contained provider that compiles, registers, and
//! answers — the LangChain `FakeListLLM` analog. No API key, no network, no
//! global state, so it is safe to build and exercise in any context (including
//! `--no-default-features` hosts that pull it in via `--features mock` alone).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;

use codesmith_agent::llm_client::{LlmClient, LlmClientHandle, StreamEventBox};
use codesmith_agent::models::{
    ContentBlock, ContentBlockStart, Delta, Message, MessageDelta, MessageRequest, MessageResponse,
    StreamEvent, Usage,
};
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};

/// Mock LLM client that echoes the last user message.
///
/// Built by [`MockProviderFactory`] from a [`ProviderConfig`]; the
/// `default_model` becomes [`LlmClient::model`]. No network calls are made.
pub struct MockClient {
    name: &'static str,
    model: String,
}

impl MockClient {
    /// A non-streaming response whose single text block is `text`.
    fn text_response(model: &str, text: &str) -> MessageResponse {
        MessageResponse {
            id: String::from("mock-msg"),
            r#type: String::from("message"),
            role: String::from("assistant"),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            model: model.to_string(),
            stop_reason: Some(String::from("end_turn")),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        }
    }

    /// Concatenated text of the last `user` message, if any.
    fn last_user_text(messages: &[Message]) -> Option<String> {
        messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .and_then(|m| {
                let text: String = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() { None } else { Some(text) }
            })
    }

    /// Echo text for a request, distinguishing "no user message at all"
    /// from "the last user message carried no text" (e.g. an image-only
    /// turn) — conflating them distorts test assertions built on the mock.
    fn echo_text(messages: &[Message]) -> String {
        match messages.iter().rev().find(|m| m.role == "user") {
            None => String::from("mock: no user message to echo"),
            Some(_) => Self::last_user_text(messages)
                .unwrap_or_else(|| String::from("mock: last user message contained no text")),
        }
    }
}

impl LlmClient for MockClient {
    fn provider_name(&self) -> &'static str {
        self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn create_message(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
        let text = Self::echo_text(&request.messages);
        let model = self.model.clone();
        Box::pin(async move { Ok(Self::text_response(&model, &text)) })
    }

    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
        let text = Self::echo_text(&request.messages);
        let model = self.model.clone();
        Box::pin(async move {
            // MessageStart carries an empty content list; the echoed text is
            // delivered via a content_block_delta, matching the wire shape of a
            // real streaming completion.
            let start = MessageResponse {
                id: String::from("mock-msg"),
                r#type: String::from("message"),
                role: String::from("assistant"),
                content: Vec::new(),
                model,
                stop_reason: None,
                stop_sequence: None,
                container: None,
                usage: Usage::default(),
            };
            let events = vec![
                StreamEvent::MessageStart { message: start },
                StreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: ContentBlockStart::Text {
                        text: String::new(),
                    },
                },
                StreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: Delta::TextDelta { text },
                },
                StreamEvent::ContentBlockStop { index: 0 },
                StreamEvent::MessageDelta {
                    delta: MessageDelta {
                        stop_reason: Some(String::from("end_turn")),
                        stop_sequence: None,
                    },
                    usage: Some(Usage::default()),
                },
                StreamEvent::MessageStop,
            ];
            let stream = futures_util::stream::iter(events.into_iter().map(Ok));
            Ok(Box::pin(stream) as StreamEventBox)
        })
    }
}

/// Factory that builds [`MockClient`]s. Registers under the `mock` provider id.
pub struct MockProviderFactory {
    id: ProviderId,
    name: &'static str,
}

impl Default for MockProviderFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl MockProviderFactory {
    /// Create a factory registering under the default `mock` id.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: ProviderId::from("mock"),
            name: "mock",
        }
    }

    /// Create a factory registering under a custom id, for hosts that want to
    /// register the mock under their own name. The built client's
    /// `provider_name` follows the id, so routing and telemetry see the name
    /// the factory was registered under.
    #[must_use]
    pub fn with_id(id: ProviderId) -> Self {
        Self {
            name: static_provider_name(&id),
            id,
        }
    }
}

/// Resolve a `&'static str` provider name for a [`ProviderId`] so
/// [`LlmClient::provider_name`] (which returns `&'static str`) can follow a
/// custom id. Builtin ids map to their stable `as_str`; a custom id leaks one
/// copy, bounded by the small set of factory constructions (the registry's
/// `leak_str` precedent).
fn static_provider_name(id: &ProviderId) -> &'static str {
    match id {
        ProviderId::Builtin(kind) => kind.as_str(),
        ProviderId::Custom(name) => Box::leak(name.clone().into_boxed_str()),
    }
}

impl ProviderFactory for MockProviderFactory {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        Ok(Arc::new(MockClient {
            name: self.name,
            model: cfg.default_model.clone(),
        }) as LlmClientHandle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent::llm_client::RetryConfig;
    use codesmith_agent::models::{Message, MessageRequest};
    use futures_util::StreamExt;
    use std::collections::HashMap;

    fn request_with_user(text: &str) -> MessageRequest {
        MessageRequest {
            model: String::from("mock-model"),
            messages: vec![Message {
                role: String::from("user"),
                content: vec![ContentBlock::Text {
                    text: text.to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 128,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: None,
            top_p: None,
        }
    }

    fn cfg_for_mock() -> ProviderConfig {
        ProviderConfig {
            provider: ProviderId::from("mock"),
            api_key: String::new(),
            base_url: String::new(),
            default_model: String::from("mock-model"),
            retry: RetryConfig::disabled(),
            http_headers: HashMap::new(),
            on_retry: None,
        }
    }

    #[test]
    fn factory_registers_under_mock_id() {
        let factory = MockProviderFactory::new();
        assert_eq!(factory.id(), ProviderId::from("mock"));
    }

    #[test]
    fn with_id_factory_reports_custom_provider_name() {
        let factory = MockProviderFactory::with_id(ProviderId::from("acme-mock"));
        assert_eq!(factory.id(), ProviderId::from("acme-mock"));
        let client = factory
            .build(&cfg_for_mock())
            .expect("mock factory should build a client");
        assert_eq!(client.provider_name(), "acme-mock");
        // Builtin ids keep their stable name.
        let factory = MockProviderFactory::with_id(ProviderId::from("mock"));
        let client = factory
            .build(&cfg_for_mock())
            .expect("mock factory should build a client");
        assert_eq!(client.provider_name(), "mock");
    }

    #[test]
    fn default_registry_resolves_mock() {
        let registry = crate::default_registry();
        assert!(
            registry.resolve(&ProviderId::from("mock")).is_some(),
            "default_registry should contain the mock factory"
        );
    }

    #[test]
    fn build_via_registry_returns_mock_client() {
        let registry = crate::default_registry();
        let client = registry
            .build(&cfg_for_mock())
            .expect("mock factory should build a client");
        assert_eq!(client.provider_name(), "mock");
        assert_eq!(client.model(), "mock-model");
    }

    #[tokio::test]
    async fn create_message_echoes_last_user_text() {
        let client = MockClient {
            name: "mock",
            model: String::from("mock-model"),
        };
        let response = client
            .create_message(request_with_user("hello mock"))
            .await
            .expect("mock create_message should succeed");
        let text = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .next()
            .expect("response should carry a text block");
        assert_eq!(text, "hello mock");
        assert_eq!(response.model, "mock-model");
        assert_eq!(response.role, "assistant");
    }

    #[tokio::test]
    async fn create_message_stream_yields_text_delta_and_stops() {
        let client = MockClient {
            name: "mock",
            model: String::from("mock-model"),
        };
        let stream = client
            .create_message_stream(request_with_user("streamed hello"))
            .await
            .expect("mock stream should start");
        let events: Vec<StreamEvent> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .expect("no stream errors expected");
        let has_delta = events.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockDelta {
                    delta: Delta::TextDelta { text },
                    ..
                } if text.as_str() == "streamed hello"
            )
        });
        assert!(has_delta, "stream should carry the echoed text delta");
        assert!(
            events.iter().any(|e| matches!(e, StreamEvent::MessageStop)),
            "stream should end with MessageStop"
        );
    }
}
