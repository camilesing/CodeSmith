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

use crate::rig_adapter::{
    GenericShaper, H2FallbackClient, RigLlmClient, build_header_map, header_pairs,
};

/// Factory for a single OpenAI-compatible builtin provider kind. Identical
/// construction for every kind; only the `id` (for registry resolution),
/// `name` (for `provider_name`), and the manifest-sourced `base_url` / `model`
/// defaults differ — all sourced from the provider's `providers.toml` entry.
pub struct OpenAiCompatFactory {
    id: ProviderId,
    name: &'static str,
    base_url: Option<String>,
    model: Option<String>,
}

impl OpenAiCompatFactory {
    /// Construct the factory for a single OpenAI-compatible kind. `id` is the
    /// registry key (parsed via `ProviderId::from`, resolving to the matching
    /// `Builtin(ProviderKind)`); `name` is surfaced through
    /// `LlmClient::provider_name`. `base_url` / `model` are the manifest
    /// defaults the factory falls back to when the host passes an empty
    /// `ProviderConfig` value. Called by `default_registry` once per
    /// `openai-compat` entry in `providers.toml`.
    pub(crate) fn new(
        id: ProviderId,
        name: &'static str,
        base_url: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            id,
            name,
            base_url,
            model,
        }
    }
}

impl ProviderFactory for OpenAiCompatFactory {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        // Host value wins; empty falls back to the manifest default. Both
        // empty is a configuration error: every kind in this family is a
        // non-OpenAI endpoint, and silently keeping rig's compile-time
        // endpoint (api.openai.com/v1) would route requests — and the
        // provider's own Bearer credential — to a different vendor's
        // service instead of failing at the configuration site.
        let base_url =
            crate::resolve_with_manifest_default(&cfg.base_url, self.base_url.as_deref())
                .trim()
                .to_string();
        if base_url.is_empty() {
            anyhow::bail!(
                "provider '{}' ({}) has no base_url configured (host value \
                 and manifest default are both empty); refusing to fall back \
                 to api.openai.com — set the provider's base_url",
                self.id.as_str(),
                self.name
            );
        }
        // Fail loud at the configuration site (not as an opaque reqwest
        // failure at the first request), and never send the provider's
        // Bearer credential in plaintext to a non-loopback host.
        let parsed_url = reqwest::Url::parse(&base_url).with_context(|| {
            format!(
                "provider '{}' ({}) base_url is not a valid URL: '{base_url}'",
                self.id.as_str(),
                self.name
            )
        })?;
        let host = parsed_url.host_str().unwrap_or("");
        let host_unbracketed = host.trim_start_matches('[').trim_end_matches(']');
        let is_loopback = host_unbracketed == "localhost"
            || host_unbracketed
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|a| a.is_loopback())
            || host_unbracketed
                .parse::<std::net::Ipv6Addr>()
                .is_ok_and(|a| a.is_loopback());
        if !matches!(parsed_url.scheme(), "http" | "https") {
            anyhow::bail!(
                "provider '{}' ({}) base_url must use http(s): '{base_url}'",
                self.id.as_str(),
                self.name
            );
        }
        if parsed_url.scheme() == "http" && !is_loopback {
            anyhow::bail!(
                "provider '{}' ({}) base_url must use https (or a loopback \
                 host) — refusing to send the provider credential in \
                 plaintext to '{base_url}'",
                self.id.as_str(),
                self.name
            );
        }
        // Same resolution for the model: an empty model id would otherwise
        // defer to an obscure provider-side failure at the first request.
        let default_model =
            crate::resolve_with_manifest_default(&cfg.default_model, self.model.as_deref());
        if default_model.is_empty() {
            anyhow::bail!(
                "provider '{}' ({}) has no model configured (host value and \
                 manifest default are both empty); set the provider's model",
                self.id.as_str(),
                self.name
            );
        }
        let mut builder = openai::CompletionsClient::builder().api_key(cfg.api_key.clone());
        builder = builder.base_url(&base_url);
        // Host-supplied extra headers must not override the auth rig derives
        // from `cfg.api_key`: an `authorization` entry can silently break or
        // bypass the Bearer credential (cf. the anthropic factory's guard
        // over its auth-managed headers). Fails loud at build, mid-request
        // never happens.
        let headers = build_header_map(&cfg.http_headers);
        if headers.contains_key(reqwest::header::AUTHORIZATION) {
            anyhow::bail!(
                "provider '{}' ({}): http_headers must not override the \
                 auth-managed `authorization` header; configure credentials \
                 via ProviderConfig::api_key",
                self.id.as_str(),
                self.name
            );
        }
        builder = builder.http_headers(headers);
        // P0-3: backend with a one-shot HTTP/2 → HTTP/1.1 fallback
        // (`rig_adapter::http_fallback`). `http_client` advances the
        // builder's backend type parameter, so it chains into `build`
        // instead of re-assigning `builder`.
        let client = builder
            .http_client(H2FallbackClient::new())
            .build()
            .with_context(|| format!("failed to build rig openai-compat '{}' client", self.name))?;

        let adapter = RigLlmClient::new(
            client,
            default_model,
            GenericShaper::new(self.name),
            base_url,
            // No FIM/translate endpoints behind LlmClient for this family,
            // so the adapter needs neither the http shim nor a copy of the
            // api key (all api_key readers are gated on the shim).
            None,
            None,
        );
        Ok(Arc::new(adapter.with_headers(header_pairs(&cfg.http_headers))) as LlmClientHandle)
    }
}
