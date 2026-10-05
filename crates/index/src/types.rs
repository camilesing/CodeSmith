//! Value types now live in the framework layer (`codesmith_tools::index_api`).
//! Re-exported here so internal `crate::types::…` paths and public
//! `codesmith_index::types::…` paths keep resolving unchanged.

pub use codesmith_tools::index_api::*;
