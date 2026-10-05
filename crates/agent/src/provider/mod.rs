//! Provider pluggability core: identify, configure, and build LLM clients
//! without depending on any concrete client implementation.
//!
//! This is the CodeSmith framework's provider seam — the Rust analog of
//! pi-ai's `MutableModels` registry + `createProvider()` factory. The
//! `codesmith-agent` crate holds only this abstraction; concrete provider
//! implementations live in `codesmith-providers` (or a user's own crate) and
//! register an [`Arc<dyn ProviderFactory>`] into a [`ProviderRegistry`].
//!
//! # Adding a provider
//!
//! ```ignore
//! use codesmith_agent::llm_client::LlmClientHandle;
//! use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId};
//!
//! struct AcmeFactory;
//! impl ProviderFactory for AcmeFactory {
//!     fn id(&self) -> ProviderId { ProviderId::from("acme") }
//!     fn build(&self, cfg: &ProviderConfig) -> anyhow::Result<LlmClientHandle> {
//!         // construct your client from cfg.api_key / cfg.base_url / ...
//!         todo!()
//!     }
//! }
//! ```
//!
//! The host then calls `registry.build(&cfg)` — it never names a concrete
//! client type, so the implementation is freely replaceable.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

use crate::llm_client::{LlmClientHandle, LlmError, RetryConfig};
use codesmith_config::ProviderKind;

// === ProviderId ===

/// Open provider identifier: a known builtin or a custom string.
///
/// Mirrors pi-ai's `KnownProvider | string` open-union: built-ins get IDE
/// autocomplete + exhaustiveness, while [`Custom`](Self::Custom) lets any
/// extension register a brand-new provider id without modifying the core
/// enum.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProviderId {
    /// One of the providers `codesmith-config` knows about.
    Builtin(ProviderKind),
    /// An arbitrary provider id registered by an extension.
    Custom(String),
}

impl ProviderId {
    /// Stable string key used by the registry and for diagnostics.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Builtin(kind) => kind.as_str(),
            Self::Custom(name) => name.as_str(),
        }
    }
}

impl From<ProviderKind> for ProviderId {
    fn from(kind: ProviderKind) -> Self {
        Self::Builtin(kind)
    }
}

impl From<&str> for ProviderId {
    fn from(s: &str) -> Self {
        match ProviderKind::parse(s) {
            Some(kind) => Self::Builtin(kind),
            None => Self::Custom(s.to_string()),
        }
    }
}

// === ProviderConfig ===

/// Neutral construction input for any provider. Carries exactly what a
/// provider needs to build an [`LlmClientHandle`], with no dependency on the
/// TUI `Config`. Built by the host (TUI/app-server) from its own config.
/// Host-injected retry-notification closure. Kept as a named alias so the
/// `ProviderConfig.on_retry` field stays readable; providers compiled into
/// `codesmith-providers` receive this without any terminal/UI coupling.
pub type RetryHook = Arc<dyn Fn(&LlmError, u32, Duration) + Send + Sync>;

#[derive(Clone)]
pub struct ProviderConfig {
    /// Which provider this config is for.
    pub provider: ProviderId,
    /// Resolved API key (already env/keyring-expanded by the host).
    pub api_key: String,
    /// Provider base URL (validated HTTPS/loopback by the host or provider).
    pub base_url: String,
    /// Default model id to use when a request omits one.
    pub default_model: String,
    /// Retry / backoff policy.
    pub retry: RetryConfig,
    /// Extra HTTP headers (e.g. `X-Model-Provider-Id`).
    pub http_headers: HashMap<String, String>,
    /// Optional retry-notification hook. Replaces the TUI's global
    /// `retry_status` UI: the host injects a closure, so a provider compiled
    /// into `codesmith-providers` stays free of terminal/UI coupling.
    pub on_retry: Option<RetryHook>,
}

// === ProviderFactory ===

/// A factory that builds an LLM client for a given provider.
///
/// Implement this in `codesmith-providers` (or your own crate) to add a
/// provider — no `codesmith-tui` dependency required. Register an
/// `Arc<dyn ProviderFactory>` into a [`ProviderRegistry`].
pub trait ProviderFactory: Send + Sync {
    /// The provider this factory builds clients for.
    fn id(&self) -> ProviderId;
    /// Build a client from the neutral [`ProviderConfig`].
    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle>;
}

// === ProviderRegistry ===

/// Instance-based provider registry. Mirrors pi-ai's `MutableModels`:
/// `HashMap<ProviderId, Arc<dyn ProviderFactory>>`;
/// [`build`](Self::build) resolves the factory by `cfg.provider` and
/// delegates. Last-registered factory for an id wins (upsert), matching
/// pi-ai's `setProvider`.
///
/// `Clone` is a shallow Arc-map copy, so a host that wants to customize the
/// cached [`codesmith_providers::default_registry`](../../codesmith_providers/fn.default_registry.html)
/// (returned as `&'static`, hence immutable) clones it and then calls
/// [`register`](Self::register) on its own mutable copy.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    factories: HashMap<ProviderId, Arc<dyn ProviderFactory>>,
}

impl ProviderRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register (or replace) a provider factory.
    pub fn register(&mut self, factory: Arc<dyn ProviderFactory>) {
        self.factories.insert(factory.id(), factory);
    }

    /// Look up the factory registered for `id`, if any.
    #[must_use]
    pub fn resolve(&self, id: &ProviderId) -> Option<Arc<dyn ProviderFactory>> {
        self.factories.get(id).cloned()
    }

    /// Resolve the factory for `cfg.provider` and build a client.
    ///
    /// Returns an error if no factory is registered for the provider, naming
    /// the registered ids to aid diagnosis.
    pub fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        match self.factories.get(&cfg.provider) {
            Some(factory) => factory.build(cfg),
            None => {
                let registered = self
                    .ids()
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "no provider factory registered for '{}'; registered: [{}]",
                    cfg.provider.as_str(),
                    registered
                )
            }
        }
    }

    /// All registered provider ids (unordered).
    #[must_use]
    pub fn ids(&self) -> Vec<ProviderId> {
        self.factories.keys().cloned().collect()
    }

    /// Remove the factory registered for `id`, returning it (upsert's
    /// inverse — used by [`SharedProviderRegistry`] registration guards).
    pub fn remove(&mut self, id: &ProviderId) -> Option<Arc<dyn ProviderFactory>> {
        self.factories.remove(id)
    }

    /// All registered factories (for seeding a [`SharedProviderRegistry`]).
    #[must_use]
    pub fn snapshot(&self) -> Vec<Arc<dyn ProviderFactory>> {
        self.factories.values().cloned().collect()
    }
}

// === SharedProviderRegistry (route A: extension-registered providers) =====

/// Session-shared, clone-cheap provider registry that extensions upsert
/// into. The builtin [`codesmith_providers::default_registry`] is an
/// immutable `&'static`; hosts that want extension-registered providers
/// construct one of these (seeded from the builtin set), hand clones to
/// the `ExtensionRunner` (flush target) and to their client-resolution
/// path (read side) — the runner never depends on the provider
/// implementation crate.
///
/// Locking: the internal [`RwLock`] is held only for the map operation;
/// factory [`build`](ProviderRegistry::build) always runs outside it.
///
/// Known limitation: an already-built client is not hot-swapped — a
/// registration takes effect at the next client resolution.
#[derive(Clone, Default)]
pub struct SharedProviderRegistry {
    inner: Arc<RwLock<ProviderRegistry>>,
}

impl SharedProviderRegistry {
    /// Create an empty shared registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ProviderRegistry::new())),
        }
    }

    /// Create a shared registry pre-seeded with `registry`'s factories.
    #[must_use]
    pub fn from_registry(registry: ProviderRegistry) -> Self {
        Self {
            inner: Arc::new(RwLock::new(registry)),
        }
    }

    /// Merge every factory of `registry` into this one (upsert). Hosts call
    /// this once at startup to seed the builtin set.
    pub fn seed_from(&self, registry: &ProviderRegistry) {
        let mut w = self.inner.write().unwrap_or_else(|e| e.into_inner());
        for factory in registry.snapshot() {
            w.register(factory);
        }
    }

    /// Register (or replace) a factory, keyed by its id. Returns the
    /// registration guard: dropping it removes the factory (see
    /// [`ProviderRegistration`]). This is the house "registration is
    /// reversible" mechanism — a generation unloads by dropping its guards.
    #[must_use]
    pub fn register(&self, factory: Arc<dyn ProviderFactory>) -> ProviderRegistration {
        let id = factory.id();
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .register(factory.clone());
        ProviderRegistration {
            shared: self.clone(),
            id,
            factory,
        }
    }

    /// Remove `factory` from `id` only if it is still the registered one
    /// (`Arc::ptr_eq` identity), so a stale guard from an old generation
    /// cannot un-register a newer registration.
    fn remove_if_current(&self, id: &ProviderId, factory: &Arc<dyn ProviderFactory>) {
        let mut w = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = w.resolve(id)
            && Arc::ptr_eq(&current, factory)
        {
            w.remove(id);
        }
    }

    /// Look up the factory registered for `id`, if any.
    #[must_use]
    pub fn resolve(&self, id: &ProviderId) -> Option<Arc<dyn ProviderFactory>> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .resolve(id)
    }

    /// Resolve the factory for `cfg.provider` and build a client. Same
    /// error contract as [`ProviderRegistry::build`] (names the registered
    /// ids on a miss). The lock is released before `build` runs.
    pub fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        let factory = self.resolve(&cfg.provider).ok_or_else(|| {
            let registered = self
                .ids()
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!(
                "no provider factory registered for '{}'; registered: [{}]",
                cfg.provider.as_str(),
                registered
            )
        })?;
        factory.build(cfg)
    }

    /// All registered provider ids (unordered).
    #[must_use]
    pub fn ids(&self) -> Vec<ProviderId> {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).ids()
    }
}

/// The registration guard returned by [`SharedProviderRegistry::register`].
/// Dropping it removes the factory it registered — unless a newer
/// registration for the same id has since replaced it (identity-checked
/// via `Arc::ptr_eq`). Reload clears a generation by dropping its guards;
/// any new contribution type built on this pattern gets symmetric unload
/// for free (plan discipline 4, first exercised by route A).
pub struct ProviderRegistration {
    shared: SharedProviderRegistry,
    id: ProviderId,
    factory: Arc<dyn ProviderFactory>,
}

impl Drop for ProviderRegistration {
    fn drop(&mut self) {
        self.shared.remove_if_current(&self.id, &self.factory);
    }
}

// === ProviderAlias / AliasingFactory (route A, script-mod shape) ==========

/// Declarative provider-alias spec: re-expose a **builtin** provider under
/// a new custom id with optional config overrides. This is the
/// script-mod-facing form of provider registration — Rhai mods cannot
/// implement `LlmClient` (no async/net by design), so their provider
/// contribution is an alias onto a compatible builtin (e.g. an
/// OpenAI-compatible gateway) rather than a client implementation.
#[derive(Debug, Clone)]
pub struct ProviderAlias {
    /// New custom provider id. Must not shadow a builtin kind.
    pub id: String,
    /// Builtin provider the alias delegates to.
    pub target: ProviderId,
    /// Override for `ProviderConfig::base_url`.
    pub base_url: Option<String>,
    /// Override for `ProviderConfig::default_model`.
    pub default_model: Option<String>,
    /// Override for `ProviderConfig::http_headers` (replaces, not merges).
    pub http_headers: Option<HashMap<String, String>>,
}

impl ProviderAlias {
    /// Validate + build the aliasing factory. Errors as strings (for the
    /// caller to wrap in its own error type) when `id` shadows a builtin
    /// kind or `target` is not builtin — misconfiguration fails loud at
    /// registration, not at first client build.
    pub fn into_factory(self, shared: SharedProviderRegistry) -> Result<AliasingFactory, String> {
        let id = ProviderId::from(self.id.as_str());
        if let ProviderId::Builtin(kind) = &id {
            return Err(format!(
                "provider alias id {:?} shadows builtin kind '{}'",
                self.id,
                kind.as_str()
            ));
        }
        if !matches!(self.target, ProviderId::Builtin(_)) {
            return Err(format!(
                "provider alias target '{}' must be a builtin provider",
                self.target.as_str()
            ));
        }
        Ok(AliasingFactory {
            id,
            target: self.target,
            shared,
            base_url: self.base_url,
            default_model: self.default_model,
            http_headers: self.http_headers,
        })
    }
}

/// A [`ProviderFactory`] that re-exposes an existing factory under a new
/// [`ProviderId::Custom`] id with optional config overrides. Built from a
/// [`ProviderAlias`]; the target factory resolves lazily through the
/// shared registry at each build, so alias construction never races the
/// host's seeding.
pub struct AliasingFactory {
    id: ProviderId,
    target: ProviderId,
    shared: SharedProviderRegistry,
    base_url: Option<String>,
    default_model: Option<String>,
    http_headers: Option<HashMap<String, String>>,
}

impl ProviderFactory for AliasingFactory {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<LlmClientHandle> {
        let Some(target) = self.shared.resolve(&self.target) else {
            bail!(
                "provider alias '{}': target factory '{}' is not registered",
                self.id.as_str(),
                self.target.as_str()
            );
        };
        let mut cfg = cfg.clone();
        cfg.provider = self.target.clone();
        if let Some(u) = &self.base_url {
            cfg.base_url = u.clone();
        }
        if let Some(m) = &self.default_model {
            cfg.default_model = m.clone();
        }
        if let Some(h) = &self.http_headers {
            cfg.http_headers = h.clone();
        }
        target.build(&cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{LlmClient, LlmClientHandle, RetryConfig, StreamEventBox};
    use crate::models::{MessageRequest, MessageResponse};
    use codesmith_config::ProviderKind;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    /// Minimal test-only `LlmClient` so factory tests can return a real handle.
    struct EchoClient {
        model: String,
    }

    impl LlmClient for EchoClient {
        fn provider_name(&self) -> &'static str {
            "echo"
        }
        fn model(&self) -> &str {
            &self.model
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>> {
            Box::pin(async { Err(anyhow::anyhow!("echo mock")) })
        }
        fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<StreamEventBox>> + Send + '_>> {
            Box::pin(async { Err(anyhow::anyhow!("echo mock")) })
        }
    }

    struct EchoFactory {
        id: ProviderId,
    }

    impl ProviderFactory for EchoFactory {
        fn id(&self) -> ProviderId {
            self.id.clone()
        }
        fn build(&self, cfg: &ProviderConfig) -> anyhow::Result<LlmClientHandle> {
            Ok(Arc::new(EchoClient {
                model: cfg.default_model.clone(),
            }))
        }
    }

    fn cfg_for(id: ProviderId) -> ProviderConfig {
        ProviderConfig {
            provider: id,
            api_key: String::from("k"),
            base_url: String::from("https://example.test/v1"),
            default_model: String::from("m"),
            retry: RetryConfig::disabled(),
            http_headers: std::collections::HashMap::new(),
            on_retry: None,
        }
    }

    #[test]
    fn provider_id_from_known_string_is_builtin() {
        assert_eq!(
            ProviderId::from("deepseek"),
            ProviderId::Builtin(ProviderKind::Deepseek)
        );
        assert_eq!(
            ProviderId::from("anthropic"),
            ProviderId::Builtin(ProviderKind::Anthropic)
        );
    }

    #[test]
    fn provider_id_from_unknown_string_is_custom() {
        assert_eq!(
            ProviderId::from("acme-llm"),
            ProviderId::Custom("acme-llm".to_string())
        );
    }

    #[test]
    fn provider_id_as_str_round_trips() {
        assert_eq!(
            ProviderId::Builtin(ProviderKind::Openrouter).as_str(),
            "openrouter"
        );
        assert_eq!(ProviderId::Custom("acme".to_string()).as_str(), "acme");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let registry = ProviderRegistry::new();
        assert!(registry.resolve(&ProviderId::from("deepseek")).is_none());
    }

    #[test]
    fn register_and_resolve_builtin() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(EchoFactory {
            id: ProviderId::from("deepseek"),
        }));
        assert!(
            registry
                .resolve(&ProviderId::Builtin(ProviderKind::Deepseek))
                .is_some()
        );
    }

    #[test]
    fn register_and_resolve_custom() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(EchoFactory {
            id: ProviderId::from("acme-llm"),
        }));
        assert!(
            registry
                .resolve(&ProviderId::Custom("acme-llm".to_string()))
                .is_some()
        );
    }

    #[test]
    fn build_unknown_returns_err() {
        let registry = ProviderRegistry::new();
        let err = registry
            .build(&cfg_for(ProviderId::from("nope")))
            .err()
            .expect("expected an error for an unregistered provider");
        assert!(err.to_string().contains("no provider factory registered"));
    }

    #[test]
    fn build_registered_returns_client() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(EchoFactory {
            id: ProviderId::from("deepseek"),
        }));
        let handle = registry
            .build(&cfg_for(ProviderId::Builtin(ProviderKind::Deepseek)))
            .unwrap();
        assert_eq!(handle.provider_name(), "echo");
        assert_eq!(handle.model(), "m");
    }

    #[test]
    fn register_replaces_existing_factory_for_same_id() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(EchoFactory {
            id: ProviderId::from("deepseek"),
        }));
        // Re-registering the same id upserts (last wins), matching pi-ai's
        // `setProvider`.
        registry.register(Arc::new(EchoFactory {
            id: ProviderId::from("deepseek"),
        }));
        assert_eq!(registry.ids().len(), 1);
    }

    // === SharedProviderRegistry (route A) =================================

    #[test]
    fn shared_register_resolves_and_drop_guard_removes() {
        let shared = SharedProviderRegistry::new();
        let factory = Arc::new(EchoFactory {
            id: ProviderId::from("acme-llm"),
        });
        let guard = shared.register(factory);
        assert!(
            shared
                .resolve(&ProviderId::Custom("acme-llm".to_string()))
                .is_some()
        );
        drop(guard);
        assert!(
            shared
                .resolve(&ProviderId::Custom("acme-llm".to_string()))
                .is_none(),
            "dropping the registration guard un-registers the factory"
        );
    }

    #[test]
    fn stale_guard_does_not_remove_newer_registration() {
        let shared = SharedProviderRegistry::new();
        let old = shared.register(Arc::new(EchoFactory {
            id: ProviderId::from("acme-llm"),
        }));
        // A newer registration for the same id replaces the old factory…
        let _new = shared.register(Arc::new(EchoFactory {
            id: ProviderId::from("acme-llm"),
        }));
        // …so dropping the OLD guard must not un-register the new one.
        drop(old);
        assert!(
            shared
                .resolve(&ProviderId::Custom("acme-llm".to_string()))
                .is_some()
        );
    }

    #[test]
    fn shared_seed_from_merges_builtins() {
        let mut builtins = ProviderRegistry::new();
        builtins.register(Arc::new(EchoFactory {
            id: ProviderId::from("deepseek"),
        }));
        let shared = SharedProviderRegistry::new();
        shared.seed_from(&builtins);
        assert!(
            shared
                .resolve(&ProviderId::Builtin(ProviderKind::Deepseek))
                .is_some()
        );
    }

    #[test]
    fn shared_build_unknown_names_registered_ids() {
        let shared = SharedProviderRegistry::new();
        let err = shared
            .build(&cfg_for(ProviderId::from("nope")))
            .err()
            .expect("expected an error for an unregistered provider");
        assert!(err.to_string().contains("no provider factory registered"));
    }

    #[test]
    fn shared_build_registered_returns_client() {
        let shared = SharedProviderRegistry::new();
        // Hold the guard: dropping it un-registers (proven above).
        let _guard = shared.register(Arc::new(EchoFactory {
            id: ProviderId::from("acme-llm"),
        }));
        let handle = shared
            .build(&cfg_for(ProviderId::from("acme-llm")))
            .unwrap();
        assert_eq!(handle.provider_name(), "echo");
    }

    // === ProviderAlias / AliasingFactory (route A, script shape) ==========

    fn alias_for(id: &str, target: &str) -> ProviderAlias {
        ProviderAlias {
            id: id.to_string(),
            target: ProviderId::from(target),
            base_url: Some("https://gw.example.test/v1".to_string()),
            default_model: Some("gw-model".to_string()),
            http_headers: Some(HashMap::from([(
                "X-Gateway".to_string(),
                "acme".to_string(),
            )])),
        }
    }

    #[test]
    fn alias_rejects_builtin_shadowing_id() {
        let err = alias_for("deepseek", "openai")
            .into_factory(SharedProviderRegistry::new())
            .err()
            .expect("shadowing id must fail");
        assert!(err.contains("shadows builtin kind"));
    }

    #[test]
    fn alias_rejects_non_builtin_target() {
        let err = alias_for("my-gw", "not-a-builtin")
            .into_factory(SharedProviderRegistry::new())
            .err()
            .expect("non-builtin target must fail");
        assert!(err.contains("must be a builtin provider"));
    }

    #[test]
    fn aliasing_factory_overrides_and_delegates() {
        let shared = SharedProviderRegistry::new();
        let _target = shared.register(Arc::new(EchoFactory {
            id: ProviderId::from("openai"),
        }));
        let factory = alias_for("my-gw", "openai")
            .into_factory(shared.clone())
            .expect("valid alias");
        assert_eq!(factory.id(), ProviderId::Custom("my-gw".to_string()));
        let _alias = shared.register(Arc::new(factory));
        let handle = shared
            .build(&cfg_for(ProviderId::from("my-gw")))
            .expect("alias builds through the target factory");
        assert_eq!(handle.model(), "gw-model");
    }

    #[test]
    fn aliasing_factory_missing_target_fails_loud() {
        // Empty shared registry: the alias registers fine (lazy target
        // resolution) but building names both ids in the error.
        let shared = SharedProviderRegistry::new();
        let factory = alias_for("my-gw", "openai")
            .into_factory(shared.clone())
            .expect("valid alias");
        let _alias = shared.register(Arc::new(factory));
        let err = shared
            .build(&cfg_for(ProviderId::from("my-gw")))
            .err()
            .expect("build must fail without the target factory");
        let msg = err.to_string();
        assert!(msg.contains("my-gw"), "names the alias id: {msg}");
        assert!(msg.contains("openai"), "names the target id: {msg}");
    }
}
