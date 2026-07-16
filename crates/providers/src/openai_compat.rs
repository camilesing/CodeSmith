//! OpenAI-compatible provider family — one factory per builtin kind.
//!
//! Every provider kind whose API is the OpenAI `/chat/completions` shape and
//! which doesn't have a dedicated rig provider in this crate is served by an
//! [`OpenAiCompatFactory`]: it builds an `openai::CompletionsClient` pointed at
//! the host-resolved `base_url` and tags it with the provider's own name (so
//! `LlmClient::provider_name` reports e.g. `"openrouter"`, not `"openai"`).
//!
//! The catalog of kinds this family serves is declared declaratively in
//! `providers.toml` (one `[[providers]]` entry with `backend = "openai-compat"`
//! per kind); `default_registry` constructs an [`OpenAiCompatFactory`] for each
//! such entry, so the [`ProviderRegistry`] resolves any of these kinds by
//! `cfg.provider` directly — no host-side mapping from kind to factory. The
//! dedicated `openai` / `deepseek` / `anthropic` features own their kinds; this
//! feature owns the rest.

use std::sync::Arc;

use anyhow::{Context, Result};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};
use rig_core::providers::openai;

use crate::rig_adapter::{GenericShaper, RigLlmClient, build_header_map};

/// Factory for a single OpenAI-compatible builtin provider kind. Identical
/// construction for every kind; only the `id` (for registry resolution) and
/// `name` (for `provider_name`) differ — both sourced from the provider's
/// `providers.toml` entry.
pub struct OpenAiCompatFactory {
    id: ProviderId,
    name: &'static str,
}

impl OpenAiCompatFactory {
    /// Construct the factory for a single OpenAI-compatible kind. `id` is the
    /// registry key (parsed via `ProviderId::from`, resolving to the matching
    /// `Builtin(ProviderKind)`); `name` is surfaced through
    /// `LlmClient::provider_name`. Called by `default_registry` once per
    /// `openai-compat` entry in `providers.toml`.
    pub(crate) fn new(id: ProviderId, name: &'static str) -> Self {
        Self { id, name }
    }
}

impl ProviderFactory for OpenAiCompatFactory {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        let mut builder = openai::CompletionsClient::builder().api_key(cfg.api_key.clone());
        if !cfg.base_url.is_empty() {
            builder = builder.base_url(&cfg.base_url);
        }
        builder = builder.http_headers(build_header_map(&cfg.http_headers));
        let client = builder
            .build()
            .with_context(|| format!("failed to build rig openai-compat '{}' client", self.name))?;

        let adapter = RigLlmClient::new(
            client,
            cfg.default_model.clone(),
            GenericShaper::new(self.name),
            cfg.base_url.clone(),
            Some(cfg.api_key.clone()),
            None,
        );
        Ok(Arc::new(adapter) as LlmClientHandle)
    }
}

