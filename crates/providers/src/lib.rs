//! Provider implementations for the CodeSmith framework.
//!
//! This crate is the "Lego box" of LLM providers: each provider lives behind
//! a Cargo feature, so a host pulls in only what it needs. The framework
//! abstractions ([`codesmith_agent::llm_client::LlmClient`],
//! [`codesmith_agent::provider::ProviderFactory`],
//! [`codesmith_agent::provider::ProviderRegistry`]) live in `codesmith-agent`;
//! concrete clients live here, assembled à la carte. Every networked provider
//! is a thin factory that constructs a rig client and wraps it in the
//! [`rig_adapter`] — rig does the HTTP, CodeSmith keeps its `LlmClient` seam.
//!
//! # Available providers
//!
//! | Feature        | Provider(s)                                              | Network |
//! |----------------|----------------------------------------------------------|---------|
//! | `mock`         | [`mock::MockClient`] — echoes the last user message      | none    |
//! | `openai`       | [`openai::OpenAiFactory`] — official OpenAI Chat API     | yes     |
//! | `anthropic`    | [`anthropic::AnthropicFactory`] — Claude                 | yes     |
//! | `deepseek`     | [`deepseek::DeepSeekFactory`] — DeepSeek + FIM/translate | yes     |
//! | `openai-compat`| [`openai_compat`] — OpenRouter, vLLM, Ollama, … (×13)    | yes     |
//!
//! # Registering providers
//!
//! A host seeds a [`ProviderRegistry`] from [`default_registry`] and may then
//! register its own factories (which upsert, so a host can replace any
//! default) — the pi-mono-style "freely replace the implementation" seam.
//!
//! ```ignore
//! use codesmith_agent::provider::ProviderConfig;
//!
//! let cfg: ProviderConfig = todo!();
//! let mut registry = codesmith_providers::default_registry();
//! // optionally: registry.register(std::sync::Arc::new(my_factory));
//! let _client = registry.build(&cfg);
//! ```

#[cfg(feature = "mock")]
pub mod mock;

/// rig-backed adapter implementing `LlmClient` by delegating to a rig
/// `CompletionClient`. Compiled whenever any rig-backed provider feature
/// (`openai` / `anthropic` / `deepseek` / `openai-compat`) is enabled via the
/// internal `rig` aggregate feature.
#[cfg(feature = "rig")]
pub(crate) mod rig_adapter;

#[cfg(feature = "openai")]
pub mod openai;
#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "deepseek")]
pub mod deepseek;
#[cfg(feature = "openai-compat")]
pub mod openai_compat;

use codesmith_agent::provider::ProviderRegistry;

/// Build a [`ProviderRegistry`] pre-populated with every provider whose Cargo
/// feature is enabled.
///
/// This is the Lego assembly point: the host gets a registry containing all
/// compiled-in providers, then optionally registers its own factories (which
/// upsert, so a host can replace any default). Mirrors pi-ai's `MutableModels`
/// instance seeded with its built-in providers. With no provider features
/// enabled, the returned registry is empty.
///
/// `#[allow(unused_mut)]` covers the `--no-default-features` Lego build where no
/// `register` call survives cfg-expansion and `registry` would otherwise read
/// as never mutated.
#[must_use]
#[cfg_attr(
    not(any(
        feature = "mock",
        feature = "openai",
        feature = "anthropic",
        feature = "deepseek",
        feature = "openai-compat"
    )),
    allow(unused_mut)
)]
pub fn default_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    #[cfg(feature = "mock")]
    registry.register(std::sync::Arc::new(mock::MockProviderFactory::default()));
    #[cfg(feature = "openai")]
    registry.register(std::sync::Arc::new(openai::OpenAiFactory));
    #[cfg(feature = "anthropic")]
    registry.register(std::sync::Arc::new(anthropic::AnthropicFactory));
    #[cfg(feature = "deepseek")]
    registry.register(std::sync::Arc::new(deepseek::DeepSeekFactory));
    #[cfg(feature = "openai-compat")]
    openai_compat::register(&mut registry);
    registry
}

#[cfg(all(test, feature = "rig"))]
mod rig_registry_tests {
    //! Registry-level tests for the rig-backed factories. These build clients
    //! (no network — rig's `ClientBuilder::build` only constructs the client
    //! struct; the `VERIFY_PATH` probe is never called) and assert each
    //! provider resolves to a handle with the right `provider_name`.

    use super::*;
    use codesmith_agent::llm_client::RetryConfig;
    use codesmith_agent::provider::{ProviderConfig, ProviderId};
    use std::collections::HashMap;

    fn cfg_for(id: ProviderId) -> ProviderConfig {
        ProviderConfig {
            provider: id,
            api_key: String::from("dummy-key"),
            base_url: String::from("https://example.test/v1"),
            default_model: String::from("m"),
            retry: RetryConfig::disabled(),
            http_headers: HashMap::new(),
            on_retry: None,
        }
    }

    #[cfg(feature = "deepseek")]
    #[test]
    fn default_registry_builds_deepseek() {
        let registry = default_registry();
        let handle = registry
            .build(&cfg_for(ProviderId::from("deepseek")))
            .expect("deepseek factory should be registered");
        assert_eq!(handle.provider_name(), "deepseek");
        assert_eq!(handle.model(), "m");
    }

    #[cfg(feature = "openai")]
    #[test]
    fn default_registry_builds_openai() {
        let registry = default_registry();
        let handle = registry
            .build(&cfg_for(ProviderId::from("openai")))
            .expect("openai factory should be registered");
        assert_eq!(handle.provider_name(), "openai");
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn default_registry_builds_anthropic() {
        let registry = default_registry();
        let handle = registry
            .build(&cfg_for(ProviderId::from("anthropic")))
            .expect("anthropic factory should be registered");
        assert_eq!(handle.provider_name(), "anthropic");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn default_registry_builds_openai_compat_family() {
        let registry = default_registry();
        // Spot-check the head and tail of the compat family.
        for name in ["openrouter", "ollama"] {
            let handle = registry
                .build(&cfg_for(ProviderId::from(name)))
                .unwrap_or_else(|e| panic!("'{name}' should resolve: {e}"));
            assert_eq!(handle.provider_name(), name);
        }
    }

    #[test]
    fn default_registry_errors_on_unregistered_provider() {
        let registry = default_registry();
        // `.err()` (not `expect_err`) — `LlmClientHandle` is `Arc<dyn LlmClient>`
        // and the trait has no `Debug` supertrait, so the `Ok` variant isn't Debug.
        let err = registry
            .build(&cfg_for(ProviderId::from("acme-llm")))
            .err()
            .expect("an unregistered provider should not build");
        let msg = format!("{err}");
        assert!(
            msg.contains("no provider factory registered for 'acme-llm'"),
            "unexpected error: {msg}"
        );
    }
}
