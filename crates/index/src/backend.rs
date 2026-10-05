//! Backend traits: the pluggable seams of the index subsystem.
//!
//! Three traits, each mirroring an established house pattern:
//!
//! - [`IndexBackendFactory`] / [`IndexBackend`] — the provider-seam analog
//!   of `ProviderFactory`: a registry-resolvable factory builds an
//!   IO-free, single-file extractor.
//! - [`IndexServiceApi`] — the `LspManagerApi`-style query/management
//!   surface injected into `ToolContext`. Its definition lives in the
//!   framework layer (`codesmith_tools::index_api`) so the kernel does not
//!   depend on this crate; re-exported here for path compatibility.
//! - [`SemanticIndexApi`] — reserved seam for a future embedding backend;
//!   defined, configurable, but unimplemented this cycle. Also defined in
//!   the framework layer and re-exported here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::types::{Language, Occurrence, Symbol};
pub use codesmith_tools::index_api::{
    IndexServiceApi, RefreshOutcome, SemanticHit, SemanticIndexApi,
};

/// What a backend can produce. Drives validation: selecting a backend for a
/// capability it does not declare fails fast with a clear error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexCapability {
    /// Symbol definitions + lexical occurrences (tree-sitter today).
    Symbols,
    /// Embedding-based semantic search (reserved).
    Semantic,
}

impl IndexCapability {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Symbols => "symbols",
            Self::Semantic => "semantic",
        }
    }
}

/// Neutral construction input for any index backend, resolved by the host
/// from `[index]` config. The analog of `ProviderConfig`.
#[derive(Debug, Clone)]
pub struct IndexBackendConfig {
    /// Canonical workspace root the index is scoped to.
    pub workspace_root: PathBuf,
    /// Languages enabled for this backend (already intersected with the
    /// backend's supported set by the caller).
    pub languages: Vec<Language>,
}

/// What one extraction produced. Occurrences include the definition sites
/// too (role `Definition`) so reference queries hit a single table.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<Occurrence>,
}

/// A single-file extractor. Implementations must be IO-free: the
/// orchestration layer reads the source and owns the store, so backends
/// stay trivially testable and cannot race the store.
pub trait IndexBackend: Send + Sync {
    /// Registry id of the factory that built this backend.
    fn id(&self) -> &str;

    /// Languages this backend can extract. The orchestrator never calls
    /// [`extract`](Self::extract) for anything else.
    fn supported_languages(&self) -> &[Language];

    /// Parse `source` (the contents of `file`) into symbols + occurrences.
    fn extract(&self, file: &Path, source: &str, lang: Language) -> Result<Extraction>;
}

/// Builds [`IndexBackend`]s for a registry id. The plugin seam — implement
/// this in `codesmith-index` (built-ins) or a downstream crate and register
/// an `Arc<dyn IndexBackendFactory>` into an [`IndexBackendRegistry`]
/// (see crate docs).
pub trait IndexBackendFactory: Send + Sync {
    /// Registry key selected via `[index.symbols] backend = "…"` (or the
    /// semantic table, per [`capabilities`](Self::capabilities)).
    fn id(&self) -> &str;

    /// Capabilities this factory can build for.
    fn capabilities(&self) -> &'static [IndexCapability];

    /// Build a backend from the neutral [`IndexBackendConfig`].
    fn build(&self, cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>>;
}
