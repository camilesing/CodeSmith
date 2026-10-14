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

use crate::rig_adapter::{AnthropicShaper, H2FallbackClient, RigLlmClient, build_header_map};

/// Factory for the Anthropic (Claude) provider. Carries the manifest-sourced
/// `base_url` / `model` defaults it falls back to when the host passes an empty
/// `ProviderConfig` value.
pub struct AnthropicFactory {
    base_url: Option<String>,
    model: Option<String>,
}

impl AnthropicFactory {
    /// Construct the factory with the manifest defaults for `base_url` /
    /// `model`. Called by `default_registry` from the `anthropic` entry in
    /// `providers.toml`.
    pub(crate) fn new(base_url: Option<String>, model: Option<String>) -> Self {
        Self { base_url, model }
    }
}

impl ProviderFactory for AnthropicFactory {
    fn id(&self) -> ProviderId {
        ProviderId::from("anthropic")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        // Auth is derived from `api_key` only (managed-header guard below);
        // an empty key would build fine and then fail at request time with
        // an opaque 401/403 — fail loud at build instead.
        if cfg.api_key.trim().is_empty() {
            anyhow::bail!(
                "anthropic provider requires an api_key; set it in \
                 ProviderConfig::api_key (or the ANTHROPIC_API_KEY \
                 environment variable)"
            );
        }
        // Host value wins; empty falls back to the manifest default; both empty
        // keeps the rig compile-time default.
        let base_url =
            crate::resolve_with_manifest_default(&cfg.base_url, self.base_url.as_deref());
        let default_model =
            crate::resolve_with_manifest_default(&cfg.default_model, self.model.as_deref());
        // rig's AnthropicBuilder defaults to `ANTHROPIC_VERSION_LATEST` and
        // normalises the base_url (strips trailing `/v1`/`/messages`), so we
        // only need to set key + base_url + extra headers.
        let mut builder = anthropic::Client::builder().api_key(cfg.api_key.clone());
        if !base_url.is_empty() {
            builder = builder.base_url(&base_url);
        }
        // Host-supplied extra headers must not override the headers rig
        // derives from the credentials: an `x-api-key` / `authorization`
        // entry can silently break or bypass the auth derived from
        // `cfg.api_key`, and `anthropic-version` changes the wire
        // protocol. Misconfiguration fails loud at build, not mid-request.
        let headers = build_header_map(&cfg.http_headers);
        for managed in ["x-api-key", "authorization", "anthropic-version"] {
            if headers.contains_key(managed) {
                anyhow::bail!(
                    "http_headers must not override the auth-managed header \
                     `{managed}`; configure credentials via \
                     ProviderConfig::api_key and the protocol version via the \
                     client, not extra headers"
                );
            }
        }
        builder = builder.http_headers(headers);
        // P0-3: backend with a one-shot HTTP/2 → HTTP/1.1 fallback
        // (`rig_adapter::http_fallback`). `http_client` advances the
        // builder's backend type parameter, so it chains into `build`
        // instead of re-assigning `builder`.
        let client = builder
            .http_client(H2FallbackClient::new())
            .build()
            .context("failed to build rig anthropic client")?;

        let adapter = RigLlmClient::new(
            client,
            default_model,
            AnthropicShaper,
            base_url,
            Some(cfg.api_key.clone()),
            // Anthropic has no FIM/translate endpoints behind LlmClient.
            None,
        );
        Ok(Arc::new(adapter) as LlmClientHandle)
    }
}
