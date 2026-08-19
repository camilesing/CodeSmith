//! `[index]` configuration tables (config.toml).
//!
//! Follows the house new-feature pattern: every field is `Option` with a
//! resolver method documenting the default, so an absent table or section
//! keeps prior behavior. Per-capability switches live in their own
//! sub-tables; backend selection goes through the registry id string and is
//! validated against the registered factories fast-fail.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::backend::IndexCapability;
use crate::registry::IndexBackendRegistry;
use crate::types::Language;

/// Default `[index.symbols] backend` when unset.
pub const DEFAULT_SYMBOLS_BACKEND: &str = "tree-sitter";

/// Default `[index] refresh_budget_ms` when unset.
pub const DEFAULT_REFRESH_BUDGET_MS: u64 = 2_000;

/// `[index]` section in config.toml.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    /// Master switch. `None` means enabled (default-on when a backend is
    /// compiled in), so the index costs nothing to adopt.
    pub enabled: Option<bool>,
    /// Wall-clock budget for the lazy incremental refresh a single query
    /// may trigger. Default: [`DEFAULT_REFRESH_BUDGET_MS`].
    pub refresh_budget_ms: Option<u64>,
    /// File inventory cache sub-table.
    pub files: IndexFilesConfig,
    /// Symbol index sub-table.
    pub symbols: IndexSymbolsConfig,
    /// Semantic index sub-table (reserved seam).
    pub semantic: IndexSemanticConfig,
}

impl IndexConfig {
    /// Effective master switch.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Effective refresh budget in milliseconds, floored at 1 ms.
    #[must_use]
    pub fn effective_refresh_budget_ms(&self) -> u64 {
        self.refresh_budget_ms
            .unwrap_or(DEFAULT_REFRESH_BUDGET_MS)
            .max(1)
    }

    /// Validate backend selections against the registry: the chosen
    /// symbols backend must exist and declare [`IndexCapability::Symbols`];
    /// a semantic backend, if enabled, must exist and declare
    /// [`IndexCapability::Semantic`]. Fails fast listing registered ids.
    pub fn validate(&self, registry: &IndexBackendRegistry) -> Result<()> {
        if self.symbols.is_enabled()
            && !registry.has_capability(self.symbols.backend_id(), IndexCapability::Symbols)
        {
            let registered = registry.ids().join(", ");
            bail!(
                "[index.symbols] backend '{}' is not registered with symbol capability; registered: [{}]",
                self.symbols.backend_id(),
                registered
            );
        }
        if self.semantic.is_enabled()
            && !registry.has_capability(self.semantic.backend_id(), IndexCapability::Semantic)
        {
            let registered = registry.ids().join(", ");
            bail!(
                "[index.semantic] backend '{}' is not registered with semantic capability; registered: [{}] (semantic search has no built-in backend yet — leave it disabled)",
                self.semantic.backend_id(),
                registered
            );
        }
        Ok(())
    }
}

/// `[index.files]` — file inventory cache. Built-in walk (`ignore` crate),
/// no swappable backend, so the table only carries a switch.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IndexFilesConfig {
    /// `None` means enabled (default).
    pub enabled: Option<bool>,
}

impl IndexFilesConfig {
    /// Effective switch.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// `[index.symbols]` — symbol index capability.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IndexSymbolsConfig {
    /// `None` means enabled (default).
    pub enabled: Option<bool>,
    /// Registry id of the extraction backend. Default:
    /// [`DEFAULT_SYMBOLS_BACKEND`].
    pub backend: Option<String>,
    /// Per-language switches; an absent language key means enabled.
    pub languages: IndexLanguages,
}

impl IndexSymbolsConfig {
    /// Effective switch.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Effective backend registry id.
    #[must_use]
    pub fn backend_id(&self) -> &str {
        self.backend.as_deref().unwrap_or(DEFAULT_SYMBOLS_BACKEND)
    }

    /// Whether `lang` participates in symbol extraction. Absent keys
    /// default to enabled, so adopting a new grammar needs no config
    /// change.
    #[must_use]
    pub fn language_enabled(&self, lang: Language) -> bool {
        match lang {
            Language::Rust => self.languages.rust.unwrap_or(true),
            Language::Python => self.languages.python.unwrap_or(true),
            Language::JavaScript => self.languages.javascript.unwrap_or(true),
            Language::TypeScript => self.languages.typescript.unwrap_or(true),
            Language::Go => self.languages.go.unwrap_or(true),
        }
    }
}

/// `[index.symbols.languages]` — one boolean per language, all `Option`
/// with enabled defaults.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct IndexLanguages {
    pub rust: Option<bool>,
    pub python: Option<bool>,
    pub javascript: Option<bool>,
    pub typescript: Option<bool>,
    pub go: Option<bool>,
}

/// `[index.semantic]` — reserved seam for embedding-based search. No
/// built-in backend this cycle; enabling it with no capable backend fails
/// [`IndexConfig::validate`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IndexSemanticConfig {
    /// `None` / `false` means disabled (default).
    pub enabled: Option<bool>,
    /// Registry id of the semantic backend. Default `"none"`.
    pub backend: Option<String>,
}

impl IndexSemanticConfig {
    /// Effective switch (default-off).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Effective backend registry id.
    #[must_use]
    pub fn backend_id(&self) -> &str {
        self.backend.as_deref().unwrap_or("none")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::IndexBackendConfig;
    use crate::registry::IndexBackendRegistry;
    use std::sync::Arc;

    struct SymbolsOnlyFactory;

    impl crate::backend::IndexBackendFactory for SymbolsOnlyFactory {
        fn id(&self) -> &str {
            "tree-sitter"
        }
        fn capabilities(&self) -> &'static [IndexCapability] {
            &[IndexCapability::Symbols]
        }
        fn build(
            &self,
            _cfg: &IndexBackendConfig,
        ) -> anyhow::Result<Arc<dyn crate::backend::IndexBackend>> {
            unreachable!("validate never builds")
        }
    }

    fn registry() -> IndexBackendRegistry {
        let mut registry = IndexBackendRegistry::new();
        registry.register(Arc::new(SymbolsOnlyFactory));
        registry
    }

    #[test]
    fn absent_config_defaults_to_enabled_with_tree_sitter() {
        let cfg: IndexConfig = toml::from_str("").expect("empty config parses");
        assert!(cfg.is_enabled());
        assert!(cfg.files.is_enabled());
        assert!(cfg.symbols.is_enabled());
        assert_eq!(cfg.symbols.backend_id(), "tree-sitter");
        assert!(!cfg.semantic.is_enabled());
        assert_eq!(cfg.effective_refresh_budget_ms(), DEFAULT_REFRESH_BUDGET_MS);
        for lang in Language::all() {
            assert!(cfg.symbols.language_enabled(*lang), "{lang:?}");
        }
        cfg.validate(&registry()).expect("defaults validate");
    }

    #[test]
    fn full_table_parses() {
        let raw = r#"
            enabled = true
            refresh_budget_ms = 500

            [files]
            enabled = true

            [symbols]
            enabled = true
            backend = "tree-sitter"
            [symbols.languages]
            rust = true
            python = false

            [semantic]
            enabled = false
            backend = "none"
        "#;
        let cfg: IndexConfig = toml::from_str(raw).expect("full table parses");
        assert_eq!(cfg.effective_refresh_budget_ms(), 500);
        assert_eq!(cfg.symbols.backend_id(), "tree-sitter");
        assert!(cfg.symbols.language_enabled(Language::Rust));
        assert!(!cfg.symbols.language_enabled(Language::Python));
        assert!(!cfg.semantic.is_enabled());
        cfg.validate(&registry()).expect("valid config passes");
    }

    #[test]
    fn unknown_symbols_backend_fails_validation_listing_registered() {
        let cfg: IndexConfig = toml::from_str(
            r#"
            [symbols]
            backend = "ctags"
        "#,
        )
        .expect("parses");
        let err = cfg
            .validate(&registry())
            .expect_err("unknown backend must fail");
        let msg = err.to_string();
        assert!(msg.contains("ctags"), "{msg}");
        assert!(msg.contains("tree-sitter"), "{msg}");
    }

    #[test]
    fn enabled_semantic_without_capable_backend_fails() {
        let cfg: IndexConfig = toml::from_str(
            r#"
            [semantic]
            enabled = true
        "#,
        )
        .expect("parses");
        let err = cfg
            .validate(&registry())
            .expect_err("semantic default 'none' must fail");
        assert!(err.to_string().contains("semantic"), "{}", err);
    }

    #[test]
    fn disabled_symbols_skips_backend_validation() {
        let cfg: IndexConfig = toml::from_str(
            r#"
            [symbols]
            enabled = false
            backend = "does-not-exist"
        "#,
        )
        .expect("parses");
        cfg.validate(&registry())
            .expect("disabled capability skips backend check");
    }
}
