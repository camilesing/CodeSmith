//! Value types now live in the framework layer (`codesmith_tools::index_api`).
//! Re-exported here so internal `crate::types::…` paths and public
//! `codesmith_index::types::…` paths keep resolving unchanged. The list is
//! explicit so additions to the framework module surface here instead of
//! silently leaking through a glob.

pub use codesmith_tools::index_api::{
    DEFAULT_FILE_LIMIT, DEFAULT_SYMBOL_LIMIT, FileEntry, FileQuery, GlobMatcher, IndexStats,
    Language, Location, Occurrence, OccurrenceRole, RefreshBudget, Symbol, SymbolKind, SymbolQuery,
    glob_match,
};
