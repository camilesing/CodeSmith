//! Provider implementations for the CodeSmith framework.
//!
//! This crate is the "Lego box" of LLM providers: each provider lives behind
//! a Cargo feature, so a host pulls in only what it needs. The framework
//! abstractions ([`codesmith_agent::llm_client::LlmClient`],
//! [`codesmith_agent::provider::ProviderFactory`],
//! [`codesmith_agent::provider::ProviderRegistry`]) live in `codesmith-agent`;
//! concrete clients live here, assembled à la carte.
//!
//! # Available providers
//!
//! | Feature  | Provider                          | Network |
//! |----------|-----------------------------------|---------|
//! | `mock`   | [`mock::MockClient`] — echoes the last user message | none |
//!
//! Upcoming (tracked in `ROADMAP.md`): `openai-compat`, `anthropic`.
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

use codesmith_agent::provider::ProviderRegistry;

/// Build a [`ProviderRegistry`] pre-populated with every provider whose Cargo
/// feature is enabled.
///
/// This is the Lego assembly point: the host gets a registry containing all
/// compiled-in providers, then optionally registers its own factories (which
/// upsert, so a host can replace any default). Mirrors pi-ai's `MutableModels`
/// instance seeded with its built-in providers. With no provider features
/// enabled, the returned registry is empty.
#[must_use]
pub fn default_registry() -> ProviderRegistry {
    let registry = ProviderRegistry::new();
    // Each enabled provider feature rebinds `registry` to a registered copy.
    // Shadowing (rather than `let mut`) keeps this warning-free even when no
    // provider feature is enabled (the `--no-default-features` Lego build).
    #[cfg(feature = "mock")]
    let registry = {
        let mut r = registry;
        r.register(std::sync::Arc::new(mock::MockProviderFactory::default()));
        r
    };
    registry
}
