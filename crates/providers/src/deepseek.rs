//! DeepSeek provider factory — rig-backed, with a direct-HTTP FIM/translate
//! shim.
//!
//! rig's `deepseek::Client` covers `/chat/completions` (and streaming).
//! DeepSeek also exposes a FIM endpoint (`/beta/completions`) and the engine
//! uses a one-shot translate call that returns a plain string — neither is a
//! chat completion rig models. Those two go through a thin reqwest shim
//! ([`fim_translate`](crate::rig_adapter::fim_translate)) that reuses the
//! workspace `reqwest 0.13.x` rig already pulls in, so there is exactly one
//! HTTP stack in the build.
//!
//! `GenericShaper` is sufficient for DeepSeek: system is plain text (rig maps
//! it to a `system` message), and `reasoning_effort` / `thinking` / `metadata`
//! pass through `additional_params` (DeepSeek flattens it onto the request
//! body). Behaviour parity for the legacy `reasoning_effort` → model-tier
//! translation is tracked as a Step 7 refinement.

use std::sync::Arc;

use anyhow::{Context, Result};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};
use rig_core::providers::deepseek;

use crate::rig_adapter::{GenericShaper, RigLlmClient, build_header_map};

/// Factory for the DeepSeek provider. Carries the manifest-sourced `base_url`
/// / `model` defaults it falls back to when the host passes an empty
/// `ProviderConfig` value.
pub struct DeepSeekFactory {
    base_url: Option<String>,
    model: Option<String>,
}

impl DeepSeekFactory {
    /// Construct the factory with the manifest defaults for `base_url` /
    /// `model`. Called by `default_registry` from the `deepseek` entry in
    /// `providers.toml`.
    pub(crate) fn new(base_url: Option<String>, model: Option<String>) -> Self {
        Self { base_url, model }
    }
}

impl ProviderFactory for DeepSeekFactory {
    fn id(&self) -> ProviderId {
        ProviderId::from("deepseek")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        // Host value wins; empty falls back to the manifest default; both empty
        // keeps the rig compile-time default.
        let base_url =
            crate::resolve_with_manifest_default(&cfg.base_url, self.base_url.as_deref());
        let default_model =
            crate::resolve_with_manifest_default(&cfg.default_model, self.model.as_deref());
        let mut builder = deepseek::Client::builder().api_key(cfg.api_key.clone());
        if !base_url.is_empty() {
            builder = builder.base_url(&base_url);
        }
        builder = builder.http_headers(build_header_map(&cfg.http_headers));
        let client = builder
            .build()
            .context("failed to build rig deepseek client")?;

        // Separate reqwest client for the FIM/translate shim. Honours the same
        // extra headers as the chat client; the shim adds bearer auth + JSON
        // body per call.
        let http = reqwest::Client::builder()
            .default_headers(build_header_map(&cfg.http_headers))
            .build()
            .context("failed to build deepseek FIM/translate http client")?;

        // Defense-in-depth for the FIM/translate shim: the manifest now fills
        // the deepseek base_url, but if both the host and manifest are empty,
        // `resolve_base_url` still falls back to DeepSeek's built-in default so
        // the shim always has an absolute base.
        let resolved_base_url =
            crate::rig_adapter::resolve_base_url(&base_url).to_string();
        let adapter = RigLlmClient::new(
            client,
            default_model,
            GenericShaper::new("deepseek"),
            resolved_base_url,
            Some(cfg.api_key.clone()),
            Some(http),
        );
        Ok(Arc::new(adapter) as LlmClientHandle)
    }
}
