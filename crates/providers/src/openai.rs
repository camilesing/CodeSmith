//! OpenAI provider factory — rig-backed, Chat Completions API.
//!
//! CodeSmith speaks Chat Completions (`/chat/completions`), not OpenAI's newer
//! Responses API. rig's `openai::Client` defaults to the Responses API; the
//! `openai::CompletionsClient` is the Chat Completions client. We build it
//! directly via `CompletionsClient::builder()` rather than
//! `Client::new(..).completions_api()` so the type is monomorphic from the
//! start — no Responses-era state to carry through the conversion.

use std::sync::Arc;

use anyhow::{Context, Result};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};
use rig_core::providers::openai;

use crate::rig_adapter::{GenericShaper, RigLlmClient, build_header_map};

/// Factory for the official OpenAI provider (Chat Completions API).
pub struct OpenAiFactory;

impl ProviderFactory for OpenAiFactory {
    fn id(&self) -> ProviderId {
        // `ProviderId::from` parses via `codesmith_config::ProviderKind::parse`,
        // so "openai" resolves to `Builtin(Openai)` — matching the id the host
        // builds from its config. String here keeps this crate off the
        // `codesmith-config` dep edge (provider box ↔ abstraction only).
        ProviderId::from("openai")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        let mut builder = openai::CompletionsClient::builder().api_key(cfg.api_key.clone());
        // Only override the rig default (api.openai.com/v1) when the host
        // resolved a non-empty base_url — an empty string would otherwise
        // clobber it.
        if !cfg.base_url.is_empty() {
            builder = builder.base_url(&cfg.base_url);
        }
        builder = builder.http_headers(build_header_map(&cfg.http_headers));
        let client = builder
            .build()
            .context("failed to build rig openai (completions) client")?;

        let adapter = RigLlmClient::new(
            client,
            cfg.default_model.clone(),
            GenericShaper::new("openai"),
            cfg.base_url.clone(),
            Some(cfg.api_key.clone()),
            // OpenAI has no FIM/translate endpoints behind LlmClient.
            None,
        );
        Ok(Arc::new(adapter) as LlmClientHandle)
    }
}
