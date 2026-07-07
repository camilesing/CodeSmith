//! OpenAI-compatible provider family — one factory per builtin kind.
//!
//! Every provider kind whose API is the OpenAI `/chat/completions` shape and
//! which doesn't have a dedicated rig provider in this crate is served by an
//! [`OpenAiCompatFactory`]: it builds an `openai::CompletionsClient` pointed at
//! the host-resolved `base_url` and tags it with the provider's own name (so
//! `LlmClient::provider_name` reports e.g. `"openrouter"`, not `"openai"`).
//!
//! Registering them all in one place means the [`ProviderRegistry`] resolves
//! any of these kinds by `cfg.provider` directly — no host-side mapping from
//! kind to factory. The dedicated `openai` / `deepseek` / `anthropic` features
//! own their kinds; this feature owns the rest.

use std::sync::Arc;

use anyhow::{Context, Result};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId, ProviderRegistry};
use rig_core::providers::openai;

use crate::rig_adapter::{GenericShaper, RigLlmClient, build_header_map};

/// Factory for a single OpenAI-compatible builtin provider kind. Identical
/// construction for every kind in [`COMPAT_KINDS`]; only the `id` (for registry
/// resolution) and `name` (for `provider_name`) differ.
pub struct OpenAiCompatFactory {
    id: ProviderId,
    name: &'static str,
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

/// The OpenAI-compatible builtin family: providers whose API is the
/// `/chat/completions` shape and which don't have a dedicated factory elsewhere
/// in this crate. Each string is both the registry id (parsed via
/// `ProviderId::from`, which resolves to the matching `Builtin(ProviderKind)`)
/// and the name surfaced through `LlmClient::provider_name`.
const COMPAT_KINDS: &[&str] = &[
    "openrouter",
    "nvidia-nim",
    "volcengine",
    "wanjie-ark",
    "atlascloud",
    "xiaomi-mimo",
    "novita",
    "fireworks",
    "siliconflow",
    "moonshot",
    "sglang",
    "vllm",
    "ollama",
];

/// Register a factory for every OpenAI-compatible builtin kind. Called by the
/// crate's [`default_registry`](crate::default_registry) when the
/// `openai-compat` feature is enabled.
pub fn register(registry: &mut ProviderRegistry) {
    for &name in COMPAT_KINDS {
        registry.register(Arc::new(OpenAiCompatFactory {
            id: ProviderId::from(name),
            name,
        }));
    }
}

