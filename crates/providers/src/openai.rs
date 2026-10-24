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

use crate::rig_adapter::{
    GenericShaper, H2FallbackClient, RigLlmClient, build_header_map, header_pairs,
};

/// Factory for the official OpenAI provider (Chat Completions API). Carries
/// the manifest-sourced `base_url` / `model` defaults it falls back to when the
/// host passes an empty `ProviderConfig` value.
pub struct OpenAiFactory {
    id: ProviderId,
    base_url: Option<String>,
    model: Option<String>,
}

impl OpenAiFactory {
    /// Construct the factory with its manifest id plus the manifest
    /// defaults for `base_url` / `model`. The manifest id is the
    /// authoritative key: hardcoding "openai" made an override entry like
    /// `id = "my-openai", backend = "openai"` register under `openai` —
    /// unresolvable by its own name and clobbering any genuine `openai`
    /// entry via last-wins register.
    pub(crate) fn new(id: ProviderId, base_url: Option<String>, model: Option<String>) -> Self {
        Self {
            id,
            base_url,
            model,
        }
    }
}

impl ProviderFactory for OpenAiFactory {
    fn id(&self) -> ProviderId {
        // The manifest id, parsed by the registry builder via
        // `ProviderId::from` (which resolves "openai" to `Builtin(Openai)`
        // — matching the id the host builds from its config).
        self.id.clone()
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        // Host value wins; empty falls back to the manifest default; both empty
        // keeps the rig compile-time default (api.openai.com/v1).
        let base_url =
            crate::resolve_with_manifest_default(&cfg.base_url, self.base_url.as_deref());
        let default_model =
            crate::resolve_with_manifest_default(&cfg.default_model, self.model.as_deref());
        let mut builder = openai::CompletionsClient::builder().api_key(cfg.api_key.clone());
        // Only override the rig default (api.openai.com/v1) when resolved to a
        // non-empty base_url — an empty string would otherwise clobber it.
        if !base_url.is_empty() {
            builder = builder.base_url(&base_url);
        }
        builder = builder.http_headers(build_header_map(&cfg.http_headers));
        // P0-3: backend with a one-shot HTTP/2 → HTTP/1.1 fallback
        // (`rig_adapter::http_fallback`). `http_client` advances the
        // builder's backend type parameter, so it chains into `build`
        // instead of re-assigning `builder`.
        let client = builder
            .http_client(H2FallbackClient::new())
            .build()
            .context("failed to build rig openai (completions) client")?;

        let adapter = RigLlmClient::new(
            client,
            default_model,
            GenericShaper::new("openai"),
            base_url,
            // OpenAI has no FIM/translate endpoints behind LlmClient, so the
            // adapter needs neither the http shim nor a copy of the api key
            // (all api_key readers are gated on the shim being present).
            None,
            None,
        );
        Ok(Arc::new(adapter.with_headers(header_pairs(&cfg.http_headers))) as LlmClientHandle)
    }
}
