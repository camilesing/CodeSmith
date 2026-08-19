//! Backend traits: the pluggable seams of the index subsystem.
//!
//! Three traits, each mirroring an established house pattern:
//!
//! - [`IndexBackendFactory`] / [`IndexBackend`] — the provider-seam analog
//!   of `ProviderFactory`: a registry-resolvable factory builds an
//!   IO-free, single-file extractor.
//! - [`IndexServiceApi`] — the `LspManagerApi`-style query/management
//!   surface injected into `ToolContext`.
//! - [`SemanticIndexApi`] — reserved seam for a future embedding backend;
//!   defined, configurable, but unimplemented this cycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::{
    FileEntry, FileQuery, IndexStats, Language, Occurrence, RefreshBudget, Symbol, SymbolQuery,
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

/// Outcome of a lazy incremental refresh: what the refresh did plus the
/// resulting stats (including any `stale_files` left over by the budget).
#[derive(Debug, Clone, Serialize)]
pub struct RefreshOutcome {
    pub stats: IndexStats,
    /// Files (re-)extracted during this refresh.
    pub refreshed_files: usize,
    /// Wall-clock duration of the refresh in milliseconds.
    pub duration_ms: u64,
}

/// Query / management surface for the per-workspace index. Injected into
/// `ToolContext` as `Option<Arc<dyn IndexServiceApi>>` (mirrors
/// `LspManagerApi`); `None` means the index is disabled or the context is a
/// test that does not need one.
#[async_trait]
pub trait IndexServiceApi: Send + Sync {
    /// Case-insensitive substring search over symbol definitions.
    async fn search_symbols(&self, query: SymbolQuery) -> Result<Vec<Symbol>>;

    /// Definitions whose name case-insensitively equals `name`.
    async fn find_definition(&self, name: &str) -> Result<Vec<Symbol>>;

    /// Lexical occurrences (definitions + references) of `name`.
    async fn find_references(&self, name: &str) -> Result<Vec<Occurrence>>;

    /// File inventory listing (path/metadata), filtered by glob/extension.
    async fn list_files(&self, query: FileQuery) -> Result<Vec<FileEntry>>;

    /// Lazy incremental freshness pass bounded by `budget`. Query methods
    /// call this internally; exposing it lets a host command force a
    /// stronger refresh.
    async fn refresh(&self, budget: RefreshBudget) -> Result<RefreshOutcome>;

    /// Cached counters; does not touch the filesystem.
    fn stats(&self) -> IndexStats;
}

/// One semantic (embedding) search hit. Reserved with the seam — not
/// produced by any built-in backend this cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticHit {
    /// Workspace-relative path of the hit.
    pub path: String,
    /// 1-based line of the best-matching chunk.
    pub line: u32,
    /// Similarity score in `[0, 1]` (higher is better).
    pub score: f32,
}

/// Reserved seam for embedding-based search. The trait, the
/// `[index.semantic]` config section, and an `embeddings` store placeholder
/// exist so a future backend lands without touching the orchestration
/// layer. No built-in implementation this cycle.
#[async_trait]
pub trait SemanticIndexApi: Send + Sync {
    /// (Re-)embed the given files.
    async fn upsert(&self, files: &[PathBuf]) -> Result<()>;

    /// Top-`k` chunks similar to the natural-language query.
    async fn search(&self, query: &str, k: usize) -> Result<Vec<SemanticHit>>;
}
