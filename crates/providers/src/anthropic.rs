//! Anthropic provider factory — rig-backed.
//!
//! Uses [`AnthropicShaper`](crate::rig_adapter::AnthropicShaper) so the
//! structured `system` prompt and its per-block `cache_control` breakpoints
//! round-trip through rig's `additional_params` instead of being flattened to a
//! plain preamble string. See the shaper docs for the rig serialization
//! invariant that makes this work.

use std::sync::Arc;

use anyhow::{Context, Result};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};
use rig_core::providers::anthropic;

use crate::rig_adapter::{AnthropicShaper, RigLlmClient, build_header_map};

/// Factory for the Anthropic (Claude) provider.
pub struct AnthropicFactory;

impl ProviderFactory for AnthropicFactory {
    fn id(&self) -> ProviderId {
        ProviderId::from("anthropic")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        // rig's AnthropicBuilder defaults to `ANTHROPIC_VERSION_LATEST` and
        // normalises the base_url (strips trailing `/v1`/`/messages`), so we
        // only need to set key + base_url + extra headers.
        let mut builder = anthropic::Client::builder().api_key(cfg.api_key.clone());
        if !cfg.base_url.is_empty() {
            builder = builder.base_url(&cfg.base_url);
        }
        builder = builder.http_headers(build_header_map(&cfg.http_headers));
        let client = builder
            .build()
            .context("failed to build rig anthropic client")?;

        let adapter = RigLlmClient::new(
            client,
            cfg.default_model.clone(),
            AnthropicShaper,
            cfg.base_url.clone(),
            Some(cfg.api_key.clone()),
            // Anthropic has no FIM/translate endpoints behind LlmClient.
            None,
        );
        Ok(Arc::new(adapter) as LlmClientHandle)
    }
}
