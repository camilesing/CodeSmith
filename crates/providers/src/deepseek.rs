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

/// Factory for the DeepSeek provider.
pub struct DeepSeekFactory;

impl ProviderFactory for DeepSeekFactory {
    fn id(&self) -> ProviderId {
        ProviderId::from("deepseek")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        let mut builder = deepseek::Client::builder().api_key(cfg.api_key.clone());
        if !cfg.base_url.is_empty() {
            builder = builder.base_url(&cfg.base_url);
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

        // Defense-in-depth: the host normally resolves an empty base_url to
        // DeepSeek's default, but if it doesn't, the FIM/translate shim would
        // POST to a relative URL. `resolve_base_url` falls back to the default
        // so the shim always has an absolute base.
        let resolved_base_url = crate::rig_adapter::resolve_base_url(&cfg.base_url).to_string();
        let adapter = RigLlmClient::new(
            client,
            cfg.default_model.clone(),
            GenericShaper::new("deepseek"),
            resolved_base_url,
            Some(cfg.api_key.clone()),
            Some(http),
        );
        Ok(Arc::new(adapter) as LlmClientHandle)
    }
}
