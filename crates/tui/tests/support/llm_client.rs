//! Test-only mirror of the production `llm_client` module surface.
//!
//! The integration test under `tests/integration_mock_llm.rs` includes this
//! file as `mod llm_client` and `mock.rs` as the nested submodule. Doing it
//! this way means `mock.rs`'s `super::{LlmClient, StreamEventBox}` paths
//! resolve cleanly — they refer to the trait + alias declared right here.
//!
//! The trait shape MUST stay 1:1 with the real one in
//! `crates/tui/src/llm_client/mod.rs`. If the production trait grows a method,
//! mirror it here so `mock.rs` (the same source file shipped in the binary)
//! still satisfies it.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::models::{MessageRequest, MessageResponse, StreamEvent};

pub type StreamEventBox =
    Pin<Box<dyn futures_util::Stream<Item = Result<StreamEvent>> + Send + 'static>>;

pub type LlmClientHandle = Arc<dyn LlmClient>;

#[allow(dead_code)]
pub trait LlmClient: Send + Sync {
    fn provider_name(&self) -> &'static str;
    fn model(&self) -> &str;
    fn create_message(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>>;
    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>>;
    fn health_check(&self) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>> {
        Box::pin(async { Ok(true) })
    }
    fn fim_completion(
        &self,
        model: String,
        prompt: String,
        suffix: String,
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "FIM completion not supported by provider '{}'",
                self.provider_name()
            ))
        })
    }
    fn translate(
        &self,
        text: String,
        model: String,
        target_language: String,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "Translation not supported by provider '{}'",
                self.provider_name()
            ))
        })
    }
    fn base_url(&self) -> &str {
        ""
    }
    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[path = "../../src/llm_client/mock.rs"]
pub mod mock;
