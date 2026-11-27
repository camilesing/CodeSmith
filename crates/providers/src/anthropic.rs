//! Anthropic provider factory — rig-backed.
//!
//! Uses `AnthropicShaper` (`rig_adapter`) so the
//! structured `system` prompt and its per-block `cache_control` breakpoints
//! round-trip through rig's `additional_params` instead of being flattened to a
//! plain preamble string. See the shaper docs for the rig serialization
//! invariant that makes this work.

use std::sync::Arc;

use anyhow::{Context, Result};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};
use rig_core::providers::anthropic;

use crate::rig_adapter::{
    AnthropicShaper, H2FallbackClient, RigLlmClient, build_header_map, header_pairs,
};

/// Factory for the Anthropic (Claude) provider. Carries the manifest-sourced
/// `base_url` / `model` defaults it falls back to when the host passes an empty
/// `ProviderConfig` value.
pub struct AnthropicFactory {
    id: ProviderId,
    base_url: Option<String>,
    model: Option<String>,
}

impl AnthropicFactory {
    /// Construct the factory with its manifest id plus the manifest
    /// defaults for `base_url` / `model`. The manifest id is the
    /// authoritative registration key (an override entry must register
    /// under its own id, not clobber the builtin name).
    pub(crate) fn new(id: ProviderId, base_url: Option<String>, model: Option<String>) -> Self {
        Self {
            id,
            base_url,
            model,
        }
    }
}

impl ProviderFactory for AnthropicFactory {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        // Auth is derived from `api_key` only (managed-header guard below);
        // an empty key would build fine and then fail at request time with
        // an opaque 401/403 — fail loud at build instead. Last resort, the
        // well-known env vars supply the key (mirroring the CLI config
        // layer's resolution order) so cross-provider utility setups work
        // with keys that only exist in the environment.
        let api_key = if !cfg.api_key.trim().is_empty() {
            cfg.api_key.clone()
        } else {
            // Emptiness is checked inside the candidate loop: a set-but-empty
            // `ANTHROPIC_API_KEY` used to end the search before
            // `CLAUDE_API_KEY` was ever read.
            ["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"]
                .iter()
                .find_map(|var| std::env::var(var).ok().filter(|key| !key.trim().is_empty()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "anthropic provider requires an api_key; set it in \
                         ProviderConfig::api_key (or the ANTHROPIC_API_KEY / \
                         CLAUDE_API_KEY environment variable)"
                    )
                })?
        };
        // Host value wins; empty falls back to the manifest default; both empty
        // keeps the rig compile-time default.
        let base_url =
            crate::resolve_with_manifest_default(&cfg.base_url, self.base_url.as_deref());
        let default_model =
            crate::resolve_with_manifest_default(&cfg.default_model, self.model.as_deref());
        // rig's AnthropicBuilder defaults to `ANTHROPIC_VERSION_LATEST` and
        // normalises the base_url (strips trailing `/v1`/`/messages`), so we
        // only need to set key + base_url + extra headers.
        let mut builder = anthropic::Client::builder().api_key(api_key);
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
            // Report the endpoint the rig client actually requests: the
            // builder normalises the base_url (see the comment above), so the
            // raw resolved value would diverge from it.
            anthropic::client::normalize_anthropic_base_url(&base_url),
            // Anthropic has no FIM/translate endpoints behind LlmClient, so
            // the adapter needs neither the http shim nor a copy of the api
            // key (all api_key readers are gated on the shim being present).
            None,
            None,
        );
        Ok(Arc::new(adapter.with_headers(header_pairs(&cfg.http_headers))) as LlmClientHandle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent::llm_client::RetryConfig;
    use codesmith_agent::provider::{ProviderConfig, ProviderId};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Serializes this module's environment edits; no other test in the crate
    /// reads these variables (the registry tests pass a non-empty key).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Restores the previous values when dropped.
    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            // SAFETY: ENV_LOCK is held for the guard's whole life.
            unsafe {
                for (key, previous) in self.0.drain(..) {
                    match previous {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    fn no_key_cfg() -> ProviderConfig {
        ProviderConfig {
            provider: ProviderId::from("anthropic"),
            api_key: String::new(),
            base_url: String::new(),
            default_model: String::new(),
            retry: RetryConfig::disabled(),
            http_headers: HashMap::new(),
        }
    }

    /// A set-but-empty `ANTHROPIC_API_KEY` used to end the fallback search
    /// before `CLAUDE_API_KEY` was read, although the host accepts either.
    #[test]
    fn an_empty_first_env_key_falls_through_to_the_second() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let keys = ["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"];
        let _restore = EnvRestore(
            keys.iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect(),
        );
        // SAFETY: under ENV_LOCK, restored on drop.
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "");
            std::env::set_var("CLAUDE_API_KEY", "sk-ant-test");
        }

        AnthropicFactory::new(ProviderId::from("anthropic"), None, None)
            .build(&no_key_cfg())
            .expect("CLAUDE_API_KEY supplies the key");
    }

    #[test]
    fn no_env_key_at_all_fails_loud() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let keys = ["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"];
        let _restore = EnvRestore(
            keys.iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect(),
        );
        // SAFETY: under ENV_LOCK, restored on drop.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("CLAUDE_API_KEY");
        }

        let err = AnthropicFactory::new(ProviderId::from("anthropic"), None, None)
            .build(&no_key_cfg())
            .err()
            .expect("no key anywhere fails loud");
        assert!(err.to_string().contains("CLAUDE_API_KEY"), "{err:#}");
    }
}
