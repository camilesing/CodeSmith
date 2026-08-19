//! Instance-based index backend registry — the direct mirror of
//! `ProviderRegistry` (`crates/agent/src/provider/mod.rs`):
//! `HashMap<String, Arc<dyn IndexBackendFactory>>` with upsert semantics;
//! [`build`](IndexBackendRegistry::build) resolves by id and fails with a
//! message naming the registered ids.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::backend::{IndexBackend, IndexBackendConfig, IndexBackendFactory, IndexCapability};

/// Registry of index backend factories. `Clone` is a shallow Arc-map copy,
/// so a host that wants to extend the built-in registry clones it and
/// registers additional factories on its own copy.
#[derive(Clone, Default)]
pub struct IndexBackendRegistry {
    factories: HashMap<String, Arc<dyn IndexBackendFactory>>,
}

impl IndexBackendRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register (or replace) a backend factory. Last-registered factory
    /// for an id wins, matching `ProviderRegistry::register`.
    pub fn register(&mut self, factory: Arc<dyn IndexBackendFactory>) {
        self.factories.insert(factory.id().to_string(), factory);
    }

    /// Look up the factory registered for `id`, if any.
    #[must_use]
    pub fn resolve(&self, id: &str) -> Option<Arc<dyn IndexBackendFactory>> {
        self.factories.get(id).cloned()
    }

    /// Resolve the factory for `id` and build a backend. Fails when the id
    /// is unknown (listing registered ids) or the factory rejects the
    /// config.
    pub fn build(&self, id: &str, cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>> {
        match self.factories.get(id) {
            Some(factory) => factory.build(cfg),
            None => {
                let registered = self.ids().join(", ");
                bail!("no index backend factory registered for '{id}'; registered: [{registered}]");
            }
        }
    }

    /// Whether a registered factory declares `capability` for `id`.
    #[must_use]
    pub fn has_capability(&self, id: &str, capability: IndexCapability) -> bool {
        self.factories
            .get(id)
            .is_some_and(|f| f.capabilities().contains(&capability))
    }

    /// All registered backend ids, sorted for stable diagnostics.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.factories.keys().cloned().collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Extraction;
    use crate::types::Language;

    struct StubFactory {
        id: &'static str,
        caps: &'static [IndexCapability],
    }

    impl IndexBackendFactory for StubFactory {
        fn id(&self) -> &str {
            self.id
        }
        fn capabilities(&self) -> &'static [IndexCapability] {
            self.caps
        }
        fn build(&self, cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>> {
            Ok(Arc::new(StubBackend { cfg: cfg.clone() }))
        }
    }

    struct StubBackend {
        cfg: IndexBackendConfig,
    }

    impl IndexBackend for StubBackend {
        fn id(&self) -> &str {
            "stub"
        }
        fn supported_languages(&self) -> &[Language] {
            &self.cfg.languages
        }
        fn extract(
            &self,
            _file: &std::path::Path,
            _source: &str,
            _lang: Language,
        ) -> Result<Extraction> {
            Ok(Extraction::default())
        }
    }

    fn symbols_factory() -> Arc<StubFactory> {
        Arc::new(StubFactory {
            id: "stub",
            caps: &[IndexCapability::Symbols],
        })
    }

    #[test]
    fn register_upserts_last_factory_wins() {
        let mut registry = IndexBackendRegistry::new();
        assert!(registry.ids().is_empty());
        registry.register(symbols_factory());
        assert_eq!(registry.ids(), vec!["stub".to_string()]);
        assert!(registry.resolve("stub").is_some());
        assert!(registry.resolve("missing").is_none());
    }

    #[test]
    fn build_unknown_id_lists_registered_backends() {
        let mut registry = IndexBackendRegistry::new();
        registry.register(symbols_factory());
        let err = registry
            .build(
                "ctags",
                &IndexBackendConfig {
                    workspace_root: std::path::PathBuf::from("/tmp"),
                    languages: vec![],
                },
            )
            .map(|_| ())
            .expect_err("unknown id must fail");
        let msg = err.to_string();
        assert!(msg.contains("'ctags'"), "{msg}");
        assert!(
            msg.contains("stub"),
            "error must list registered ids: {msg}"
        );
    }

    #[test]
    fn build_resolves_registered_factory() {
        let mut registry = IndexBackendRegistry::new();
        registry.register(symbols_factory());
        let backend = registry
            .build(
                "stub",
                &IndexBackendConfig {
                    workspace_root: std::path::PathBuf::from("/tmp"),
                    languages: vec![Language::Rust],
                },
            )
            .expect("registered id must build");
        assert_eq!(backend.id(), "stub");
        assert_eq!(backend.supported_languages(), &[Language::Rust]);
    }

    #[test]
    fn has_capability_reflects_factory_declaration() {
        let mut registry = IndexBackendRegistry::new();
        registry.register(symbols_factory());
        assert!(registry.has_capability("stub", IndexCapability::Symbols));
        assert!(!registry.has_capability("stub", IndexCapability::Semantic));
        assert!(!registry.has_capability("missing", IndexCapability::Symbols));
    }
}
