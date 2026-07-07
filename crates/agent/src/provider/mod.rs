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
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};

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
#[derive(Default)]
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
        assert!(registry
            .resolve(&ProviderId::Builtin(ProviderKind::Deepseek))
            .is_some());
    }

    #[test]
    fn register_and_resolve_custom() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(EchoFactory {
            id: ProviderId::from("acme-llm"),
        }));
        assert!(registry
            .resolve(&ProviderId::Custom("acme-llm".to_string()))
            .is_some());
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
}
