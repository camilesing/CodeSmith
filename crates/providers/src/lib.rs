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
//! [`default_registry`] returns a process-wide cached `&'static`
//! [`ProviderRegistry`] (built once from the declarative `providers.toml`
//! catalog, ROADMAP §E4). The common path never mutates it — a host just
//! resolves a client:
//!
//! ```ignore
//! use codesmith_agent::provider::ProviderConfig;
//! let cfg: ProviderConfig = todo!();
//! let _client = codesmith_providers::default_registry().build(&cfg);
//! ```
//!
//! To replace a default or add your own factory, clone the cached registry
//! (a shallow `Arc`-map copy) and `register` on your own mutable copy — the
//! pi-mono-style "freely replace the implementation" seam:
//!
//! ```ignore
//! use std::sync::Arc;
//! let mut registry = codesmith_providers::default_registry().clone();
//! registry.register(Arc::new(my_factory)); // upserts, so defaults are replaceable
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

use std::sync::{Arc, OnceLock};

use anyhow::{Result, bail};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::provider::{
    ProviderConfig, ProviderFactory, ProviderId, ProviderRegistry,
};
use codesmith_config::{FactoryBackend, ProvidersManifest};

/// Process-wide cached [`ProviderRegistry`] built from the declarative
/// `providers.toml` catalog (ROADMAP §E4).
///
/// Built once — the `OnceLock` is the "registry built once" half of E4's lazy
/// loading (the manifest "read once" half lives in
/// [`codesmith_config::providers_manifest`]). Iterating the manifest's
/// `[[providers]]` entries, each entry's `backend` selects the factory whose
/// Cargo feature matches; this is the Lego assembly point, mirroring pi-ai's
/// `MutableModels` instance seeded with its built-in providers.
///
/// The catalog is the [`providers.toml`](crate) bundled with this crate,
/// unless `CODESMITH_PROVIDERS_MANIFEST` points at an override file — a
/// non-empty override **replaces** the bundled catalog (ship a custom provider
/// set without recompiling); a failed override load is logged and falls back to
/// the bundled catalog.
///
/// An entry whose `backend` Cargo feature is not compiled in is registered as
/// an [`UncompiledBackendFactory`] stub whose `build` errors clearly (naming
/// the missing feature): a runtime manifest can only select among factories
/// that are compiled in. With no provider features enabled, every entry is a
/// stub.
///
/// Returns `&'static` so the sole production caller (`resolve_llm_client` in
/// the tui engine) stops rebuilding the registry per request. To customize,
/// clone the cached registry and [`ProviderRegistry::register`] on your own
/// mutable copy.
#[must_use]
pub fn default_registry() -> &'static ProviderRegistry {
    static REGISTRY: OnceLock<ProviderRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| build_registry_from(active_manifest()))
}

/// The active manifest: the `CODESMITH_PROVIDERS_MANIFEST` override if it is
/// non-empty, else the bundled [`providers.toml`](crate). A non-empty override
/// replaces the bundled catalog.
fn active_manifest() -> &'static ProvidersManifest {
    let override_manifest = codesmith_config::providers_manifest();
    if !override_manifest.providers.is_empty() {
        override_manifest
    } else {
        bundled_manifest()
    }
}

/// The shipped `providers.toml`, parsed + validated once and cached for the
/// process. A parse/validate failure is a build-time bug — the file is
/// `include_str!`'d and kept in sync with `FactoryBackend` — so it panics
/// rather than silently yielding a partial registry.
fn bundled_manifest() -> &'static ProvidersManifest {
    static BUNDLED: OnceLock<ProvidersManifest> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        let manifest = ProvidersManifest::parse(include_str!("../providers.toml"))
            .expect("bundled providers.toml must parse (kept in sync with FactoryBackend)");
        manifest
            .validate()
            .expect("bundled providers.toml must validate (no empty/duplicate ids)");
        manifest
    })
}

/// Build a fresh registry from a manifest, registering one factory per entry.
fn build_registry_from(manifest: &ProvidersManifest) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    for desc in &manifest.providers {
        let id = ProviderId::from(desc.id.as_str());
        // Each backend's factory is behind its Cargo feature; statement-cfg
        // (not a `match`) keeps this compiling for any feature subset, with no
        // `unreachable_patterns` warning when every feature is on.
        #[cfg(feature = "mock")]
        if desc.backend == FactoryBackend::Mock {
            registry.register(Arc::new(mock::MockProviderFactory::default()));
            continue;
        }
        #[cfg(feature = "openai")]
        if desc.backend == FactoryBackend::Openai {
            registry.register(Arc::new(openai::OpenAiFactory::new(
                desc.base_url.clone(),
                desc.model.clone(),
            )));
            continue;
        }
        #[cfg(feature = "anthropic")]
        if desc.backend == FactoryBackend::Anthropic {
            registry.register(Arc::new(anthropic::AnthropicFactory::new(
                desc.base_url.clone(),
                desc.model.clone(),
            )));
            continue;
        }
        #[cfg(feature = "deepseek")]
        if desc.backend == FactoryBackend::Deepseek {
            registry.register(Arc::new(deepseek::DeepSeekFactory::new(
                desc.base_url.clone(),
                desc.model.clone(),
            )));
            continue;
        }
        #[cfg(feature = "openai-compat")]
        if desc.backend == FactoryBackend::OpenaiCompat {
            let name = leak_str(&desc.id);
            registry.register(Arc::new(openai_compat::OpenAiCompatFactory::new(
                id.clone(),
                name,
                desc.base_url.clone(),
                desc.model.clone(),
            )));
            continue;
        }
        // No arm matched: the entry's backend feature isn't compiled in.
        // Register a diagnostic stub so resolving this id errors clearly
        // (naming the missing feature) rather than the generic "not registered".
        registry.register(Arc::new(UncompiledBackendFactory::new(id, desc.backend)));
    }
    registry
}

/// Leak a `&str` to `&'static str` for the `GenericShaper`'s `provider_name`.
/// Bounded: called only inside the registry's `OnceLock` init, once per
/// `openai-compat` entry. The registry lives for the process, so the leaked
/// name lives exactly as long.
#[cfg(feature = "openai-compat")]
fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// Resolve a `ProviderConfig` string field against the manifest default: a
/// non-empty host value wins; an empty host value falls back to the manifest
/// default the entry declares (if any); both empty yields an empty string,
/// letting the rig builder keep its compile-time default. Shared by every
/// rig-backed factory so the fallback rule lives in one place (ROADMAP §E4 —
/// the manifest as a source of per-provider `base_url`/`model` defaults).
#[cfg(feature = "rig")]
pub(crate) fn resolve_with_manifest_default(
    cfg_val: &str,
    manifest: Option<&str>,
) -> String {
    if !cfg_val.is_empty() {
        cfg_val.to_string()
    } else if let Some(m) = manifest {
        m.to_string()
    } else {
        String::new()
    }
}

/// Diagnostic factory registered for a manifest entry whose `FactoryBackend`
/// Cargo feature is not compiled in. `build` errors clearly so a resolve
/// attempt points at the missing feature rather than the generic
/// "not registered" message.
struct UncompiledBackendFactory {
    id: ProviderId,
    backend: FactoryBackend,
}

impl UncompiledBackendFactory {
    fn new(id: ProviderId, backend: FactoryBackend) -> Self {
        Self { id, backend }
    }
}

impl ProviderFactory for UncompiledBackendFactory {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn build(&self, _cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        let feature = self.backend.as_str();
        bail!(
            "provider '{}' is declared in providers.toml with backend='{}', \
             but the '{}' Cargo feature of codesmith-providers is not enabled; \
             rebuild with --features {}",
            self.id.as_str(),
            feature,
            feature,
            feature
        )
    }
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

#[cfg(test)]
mod manifest_tests {
    //! Manifest-driven `default_registry` tests that don't need rig (the stub
    //! path never builds a rig client), so they run in every feature config —
    //! including the mock-only Lego build.

    use super::*;
    use codesmith_agent::llm_client::RetryConfig;
    use codesmith_agent::provider::{ProviderConfig, ProviderId};
    use codesmith_config::FactoryBackend;
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

    #[test]
    fn bundled_manifest_has_full_catalog() {
        let manifest = bundled_manifest();
        // 4 dedicated factories + 13 openai-compat kinds = 17 entries.
        assert_eq!(
            manifest.providers.len(),
            17,
            "bundled providers.toml should list 17 entries"
        );
        let ids: Vec<&str> = manifest.providers.iter().map(|d| d.id.as_str()).collect();
        for dedicated in ["mock", "openai", "anthropic", "deepseek"] {
            assert!(
                ids.contains(&dedicated),
                "missing dedicated entry '{dedicated}'"
            );
        }
        for compat in ["openrouter", "ollama"] {
            assert!(ids.contains(&compat), "missing compat entry '{compat}'");
        }
        // `bundled_manifest` already validates; assert again here as a guard.
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn bundled_manifest_populates_base_url_and_model() {
        // ROADMAP §E4: every non-mock entry declares the per-provider
        // `base_url` / `model` defaults (mirroring codesmith-config's
        // `DEFAULT_*` constants), which the factories now consume as a fallback
        // when the host passes an empty `ProviderConfig` value.
        let manifest = bundled_manifest();

        // `mock` carries neither — it needs no endpoint.
        let mock = manifest
            .providers
            .iter()
            .find(|d| d.id == "mock")
            .expect("mock entry");
        assert!(mock.base_url.is_none(), "mock should not declare a base_url");
        assert!(mock.model.is_none(), "mock should not declare a model");

        // Spot-check across backends: the dedicated factories plus the head and
        // tail of the openai-compat family.
        #[allow(clippy::type_complexity)]
        let cases: &[(&str, &str, &str)] = &[
            ("openai", "https://api.openai.com/v1", "gpt-5"),
            ("anthropic", "https://api.anthropic.com/v1", "claude-sonnet-4-5"),
            ("deepseek", "https://api.deepseek.com/beta", "deepseek-v4-pro"),
            ("openrouter", "https://openrouter.ai/api/v1", "deepseek/deepseek-v4-pro"),
            ("ollama", "http://localhost:11434/v1", "deepseek-coder:1.3b"),
        ];
        for &(id, expected_base_url, expected_model) in cases {
            let desc = manifest
                .providers
                .iter()
                .find(|d| d.id.as_str() == id)
                .unwrap_or_else(|| panic!("missing entry '{id}'"));
            assert_eq!(desc.base_url.as_deref(), Some(expected_base_url), "{id} base_url");
            assert_eq!(desc.model.as_deref(), Some(expected_model), "{id} model");
        }
    }

    #[test]
    fn default_registry_is_cached() {
        // Same `&'static` ref across calls — the OnceLock half of E4 lazy
        // loading.
        assert!(std::ptr::eq(default_registry(), default_registry()));
    }

    #[test]
    fn uncompiled_backend_factory_errors_clearly() {
        let factory = UncompiledBackendFactory::new(
            ProviderId::from("deepseek"),
            FactoryBackend::Deepseek,
        );
        let err = factory
            .build(&cfg_for(ProviderId::from("deepseek")))
            .err()
            .expect("the stub should error");
        let msg = format!("{err}");
        assert!(
            msg.contains("'deepseek'") && msg.contains("--features deepseek"),
            "stub error should name the id and the missing feature: {msg}"
        );
    }

    // End-to-end: a manifest entry whose backend feature is off is registered
    // as the stub, so resolving it errors clearly. Only runs when `deepseek`
    // is not compiled in — the bundled catalog's deepseek entry then misses its
    // factory arm and falls through to the stub.
    #[cfg(not(feature = "deepseek"))]
    #[test]
    fn uncompiled_backend_resolves_to_stub() {
        let registry = default_registry();
        let err = registry
            .build(&cfg_for(ProviderId::from("deepseek")))
            .err()
            .expect("deepseek (backend not compiled in) should resolve to the stub");
        let msg = format!("{err}");
        assert!(
            msg.contains("--features deepseek"),
            "stub error should name the missing feature: {msg}"
        );
    }
}

#[cfg(all(test, feature = "rig"))]
mod manifest_default_tests {
    //! Tests for the manifest-as-default-source wiring (ROADMAP §E4): the
    //! [`resolve_with_manifest_default`] fallback rule, and that a factory's
    //! `build()` consumes the manifest default when the host passes an empty
    //! `ProviderConfig` value.

    use super::*;
    use codesmith_agent::llm_client::RetryConfig;
    use codesmith_agent::provider::{ProviderConfig, ProviderId};
    use std::collections::HashMap;

    fn empty_cfg(id: ProviderId) -> ProviderConfig {
        ProviderConfig {
            provider: id,
            api_key: String::from("dummy-key"),
            base_url: String::new(),
            default_model: String::new(),
            retry: RetryConfig::disabled(),
            http_headers: HashMap::new(),
            on_retry: None,
        }
    }

    // === resolve_with_manifest_default unit tests ===

    #[test]
    fn resolve_prefers_non_empty_host_value() {
        assert_eq!(
            resolve_with_manifest_default("https://host.example/v1", Some("https://manifest/v1")),
            "https://host.example/v1"
        );
    }

    #[test]
    fn resolve_falls_back_to_manifest_default_when_host_empty() {
        assert_eq!(
            resolve_with_manifest_default("", Some("https://manifest/v1")),
            "https://manifest/v1"
        );
    }

    #[test]
    fn resolve_yields_empty_when_both_empty() {
        assert_eq!(resolve_with_manifest_default("", None), "");
        // An empty manifest default (`Some("")`) is treated as no default.
        assert_eq!(resolve_with_manifest_default("", Some("")), "");
    }

    // === factory build() integration (openai-compat family) ===

    #[cfg(feature = "openai-compat")]
    fn openrouter_factory() -> openai_compat::OpenAiCompatFactory {
        openai_compat::OpenAiCompatFactory::new(
            ProviderId::from("openrouter"),
            "openrouter",
            Some(String::from("https://openrouter.ai/api/v1")),
            Some(String::from("deepseek/deepseek-v4-pro")),
        )
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn factory_falls_back_to_manifest_default_when_host_empty() {
        let factory = openrouter_factory();
        let handle = factory
            .build(&empty_cfg(ProviderId::from("openrouter")))
            .expect("build should succeed (rig constructs the client with no network)");
        assert_eq!(handle.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(handle.model(), "deepseek/deepseek-v4-pro");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn factory_host_value_overrides_manifest_default() {
        let factory = openrouter_factory();
        let cfg = ProviderConfig {
            provider: ProviderId::from("openrouter"),
            api_key: String::from("dummy-key"),
            base_url: String::from("https://custom.example/v1"),
            default_model: String::from("custom-model"),
            retry: RetryConfig::disabled(),
            http_headers: HashMap::new(),
            on_retry: None,
        };
        let handle = factory.build(&cfg).expect("build should succeed");
        assert_eq!(handle.base_url(), "https://custom.example/v1");
        assert_eq!(handle.model(), "custom-model");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn factory_empty_cfg_and_no_manifest_default_falls_through() {
        // No manifest default + empty host → the resolved value stays empty,
        // so the rig builder keeps its compile-time default. The adapter still
        // surfaces the (empty) resolved value as before.
        let factory = openai_compat::OpenAiCompatFactory::new(
            ProviderId::from("acme-llm"),
            "acme-llm",
            None,
            None,
        );
        let handle = factory
            .build(&empty_cfg(ProviderId::from("acme-llm")))
            .expect("build should succeed");
        assert_eq!(handle.base_url(), "");
        assert_eq!(handle.model(), "");
    }
}
