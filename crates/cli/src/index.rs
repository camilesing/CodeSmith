//! Code-index host assembly: build the per-workspace index service from
//! `[index]` config and the default backend registry.
//!
//! The service is built once per workspace (cached on `App::index_service`)
//! and threaded into `RuntimeToolServices::index_service`, so every
//! per-turn ToolContext — and with it `symbol_search` / `find_references` —
//! shares one index. Failures degrade to `None` (tools fail closed with a
//! clear error) instead of blocking session startup: the index is an
//! accelerator, not a dependency.

use std::path::Path;
use std::sync::Arc;

use codesmith_index::{IndexServiceApi, build_service, default_registry};

use crate::config::Config;

/// Build the workspace index service, or `None` when disabled/unavailable.
pub fn build_index_service(workspace: &Path, config: &Config) -> Option<Arc<dyn IndexServiceApi>> {
    let index_config = config.index_config();
    if !index_config.is_enabled() || !index_config.symbols.is_enabled() {
        return None;
    }
    let registry = default_registry();
    match build_service(workspace, &index_config, &registry) {
        Ok(service) => {
            tracing::info!(
                workspace = %workspace.display(),
                backend = index_config.symbols.backend_id(),
                "code index service attached"
            );
            Some(service)
        }
        Err(err) => {
            tracing::warn!(
                %err,
                workspace = %workspace.display(),
                "code index unavailable; symbol_search/find_references fail closed this session"
            );
            None
        }
    }
}
