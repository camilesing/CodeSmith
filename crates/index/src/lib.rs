//! Code index pluggability core: value types, backend traits, a backend
//! registry, TOML configuration, and the per-workspace SQLite store.
//!
//! This is the index subsystem's provider-seam analog: `codesmith-index`
//! holds the abstractions plus the built-in file-inventory walk and (behind
//! the `tree-sitter` feature) the tree-sitter symbol backend. Hosts select a
//! backend through `[index.symbols] backend = "..."` in config.toml; the
//! [`IndexBackendRegistry`] resolves the id to an
//! [`Arc<dyn IndexBackendFactory>`], mirroring how
//! `codesmith-providers` plugs into `ProviderRegistry`.
//!
//! # Adding a backend
//!
//! ```ignore
//! use codesmith_index::{
//!     Extraction, IndexBackend, IndexBackendConfig, IndexBackendFactory,
//!     IndexCapability, Language,
//! };
//! use std::path::Path;
//! use std::sync::Arc;
//!
//! struct CtagsFactory;
//! impl IndexBackendFactory for CtagsFactory {
//!     fn id(&self) -> &str { "ctags" }
//!     fn capabilities(&self) -> &'static [IndexCapability] { &[IndexCapability::Symbols] }
//!     fn build(&self, cfg: &IndexBackendConfig) -> anyhow::Result<Arc<dyn IndexBackend>> {
//!         Ok(Arc::new(CtagsBackend { languages: cfg.languages.clone() }))
//!     }
//! }
//!
//! struct CtagsBackend { languages: Vec<Language> }
//! impl IndexBackend for CtagsBackend {
//!     fn id(&self) -> &str { "ctags" }
//!     fn supported_languages(&self) -> &[Language] { &self.languages }
//!     fn extract(&self, _file: &Path, source: &str, lang: Language)
//!         -> anyhow::Result<Extraction> {
//!         // parse `source` into symbols + occurrences …
//! #       let _ = (source, lang);
//! #       Ok(Extraction::default())
//!     }
//! }
//! ```
//!
//! The host registers the factory into an [`IndexBackendRegistry`]; nothing
//! in the kernel names a concrete backend type, so implementations are
//! freely replaceable (including from a downstream crate compiled against
//! this one).

pub mod backend;
pub mod builtin;
pub mod config;
pub mod registry;
pub mod service;
pub mod store;
#[cfg(feature = "tree-sitter")]
pub mod tree_sitter;
pub mod types;
pub mod walk;

pub use backend::{
    Extraction, IndexBackend, IndexBackendConfig, IndexBackendFactory, IndexCapability,
    IndexServiceApi, RefreshOutcome, SemanticHit, SemanticIndexApi,
};
pub use builtin::{
    NONE_BACKEND_ID, NoneFactory, TREE_SITTER_BACKEND_ID, UncompiledBackendFactory,
    default_registry,
};
pub use config::{
    DEFAULT_REFRESH_BUDGET_MS, DEFAULT_SYMBOLS_BACKEND, IndexConfig, IndexFilesConfig,
    IndexSemanticConfig, IndexSymbolsConfig,
};
pub use registry::IndexBackendRegistry;
pub use service::{IndexService, build_service};
pub use store::{IndexStore, SCHEMA_VERSION};
pub use types::{
    FileEntry, FileQuery, IndexStats, Language, Location, Occurrence, OccurrenceRole,
    RefreshBudget, Symbol, SymbolKind, SymbolQuery,
};
pub use walk::{MAX_EXTRACT_BYTES, WalkEntry, walk_workspace};
