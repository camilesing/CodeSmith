//! Built-in backend factories and the default registry.
//!
//! `none` is always registered (it declares no capabilities, so selecting
//! it for an enabled capability fails validation with guidance). When the
//! `tree-sitter` feature is compiled in, the tree-sitter factory registers
//! for `symbols`; otherwise an [`UncompiledBackendFactory`] placeholder
//! keeps the id resolvable and fails at build time with the missing-feature
//! note — the same pattern `codesmith-providers` uses for gated backends.

use std::sync::Arc;

use anyhow::{Result, bail};

use crate::backend::{IndexBackend, IndexBackendConfig, IndexBackendFactory, IndexCapability};
use crate::registry::IndexBackendRegistry;

/// The do-nothing backend id. Declares no capabilities: config validation
/// rejects `backend = "none"` for any enabled capability.
pub const NONE_BACKEND_ID: &str = "none";

/// Id of the built-in tree-sitter symbol backend.
pub const TREE_SITTER_BACKEND_ID: &str = "tree-sitter";

/// Factory for the `none` pseudo-backend.
pub struct NoneFactory;

impl IndexBackendFactory for NoneFactory {
    fn id(&self) -> &str {
        NONE_BACKEND_ID
    }

    fn capabilities(&self) -> &'static [IndexCapability] {
        &[]
    }

    fn build(&self, _cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>> {
        bail!(
            "index backend '{NONE_BACKEND_ID}' performs no extraction; disable the capability instead"
        )
    }
}

/// Placeholder registered for built-in backends whose feature was not
/// compiled in. Keeps config validation and diagnostics resolvable while
/// failing the actual build with an actionable message.
pub struct UncompiledBackendFactory {
    pub backend_id: &'static str,
    pub missing_feature: &'static str,
    pub declared: &'static [IndexCapability],
}

impl IndexBackendFactory for UncompiledBackendFactory {
    fn id(&self) -> &str {
        self.backend_id
    }

    fn capabilities(&self) -> &'static [IndexCapability] {
        self.declared
    }

    fn build(&self, _cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>> {
        bail!(
            "index backend '{}' was not compiled into this build; rebuild with the `{}` feature (codesmith-index/{})",
            self.backend_id,
            self.missing_feature,
            self.missing_feature
        )
    }
}

/// The default registry: `none` always, plus the tree-sitter symbol
/// backend when its feature is on (or its uncompiled placeholder).
#[must_use]
pub fn default_registry() -> IndexBackendRegistry {
    let mut registry = IndexBackendRegistry::new();
    registry.register(Arc::new(NoneFactory));
    #[cfg(feature = "tree-sitter")]
    registry.register(Arc::new(crate::tree_sitter::TreeSitterFactory));
    #[cfg(not(feature = "tree-sitter"))]
    registry.register(Arc::new(UncompiledBackendFactory {
        backend_id: TREE_SITTER_BACKEND_ID,
        missing_feature: "tree-sitter",
        declared: &[IndexCapability::Symbols],
    }));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_backend_declares_no_capabilities() {
        let registry = default_registry();
        assert!(registry.resolve(NONE_BACKEND_ID).is_some());
        assert!(!registry.has_capability(NONE_BACKEND_ID, IndexCapability::Symbols));
    }

    #[cfg(not(feature = "tree-sitter"))]
    #[test]
    fn uncompiled_placeholder_fails_build_with_feature_note() {
        let registry = default_registry();
        assert!(registry.has_capability(TREE_SITTER_BACKEND_ID, IndexCapability::Symbols));
        let err = registry
            .build(
                TREE_SITTER_BACKEND_ID,
                &IndexBackendConfig {
                    workspace_root: std::path::PathBuf::from("/tmp"),
                    languages: vec![],
                },
            )
            .map(|_| ())
            .expect_err("uncompiled backend must fail build");
        assert!(err.to_string().contains("tree-sitter"), "{}", err);
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn tree_sitter_backend_registered_with_symbols_capability() {
        let registry = default_registry();
        assert!(registry.has_capability(TREE_SITTER_BACKEND_ID, IndexCapability::Symbols));
    }
}
