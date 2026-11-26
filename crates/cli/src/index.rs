//! Code-index host assembly: build the per-workspace index service from
//! `[index]` config and the default backend registry.
//!
//! The service is built once per REPL start (held on
//! `ReplState::index_service`) and threaded into
//! `RuntimeToolServices::index_service`, so every per-turn ToolContext — and
//! with it `symbol_search` / `find_references` — shares one index. Two
//! outcomes are kept apart:
//!
//! - a setting the registry rejects (an unregistered backend id) is
//!   misconfiguration and fails loud: the error reaches the user instead of
//!   a session whose tools answer "disabled via [index] config";
//! - a service that fails to build (the store cannot be opened) degrades to
//!   `None` with a line on the verbose log: the index is an accelerator,
//!   not a dependency.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use codesmith_index::{IndexServiceApi, build_service, default_registry};

use crate::config::Config;

/// Build the workspace index service: `None` when `[index]` is disabled, an
/// error when it names a backend the registry does not have.
pub fn build_index_service(
    workspace: &Path,
    config: &Config,
) -> Result<Option<Arc<dyn IndexServiceApi>>> {
    let index_config = config.index_config();
    if !index_config.is_enabled() || !index_config.symbols.is_enabled() {
        return Ok(None);
    }
    let registry = default_registry();
    // `build_service` validates too, but there the error reads like a store
    // that could not be opened — and is degraded the same way.
    index_config.validate(&registry)?;
    match build_service(workspace, &index_config, &registry) {
        Ok(service) => {
            tracing::info!(
                workspace = %workspace.display(),
                backend = index_config.symbols.backend_id(),
                "code index service attached"
            );
            Ok(Some(service))
        }
        Err(err) => {
            tracing::warn!(
                %err,
                workspace = %workspace.display(),
                "code index unavailable; symbol_search/find_references fail closed this session"
            );
            // The binary installs no `tracing` subscriber; the verbose log
            // is the channel a user can actually read.
            crate::logging::warn(format!(
                "code index unavailable for {}: {err} — symbol_search/find_references fail closed this session",
                workspace.display()
            ));
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unregistered backend id is a typo the user can fix: it must stop
    /// startup with the registry's message, not degrade to a session whose
    /// tools claim the index is disabled.
    #[test]
    fn an_unregistered_symbols_backend_is_an_error_not_a_disabled_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config: Config = toml::from_str(
            r#"[index.symbols]
backend = "ctags"
"#,
        )
        .expect("config");
        let message = build_index_service(tmp.path(), &config)
            .err()
            .expect("an unregistered backend must fail loud")
            .to_string();
        assert!(message.contains("ctags"), "{message}");
        assert!(
            message.contains("tree-sitter"),
            "the registered ids are named: {message}"
        );
    }

    /// The default backend is registered: validation passes and the service
    /// builds, so the fail-loud path does not swallow a valid `[index]`.
    #[test]
    fn the_default_backend_builds_a_service() {
        let _env = codesmith_agent_runtime::test_support::HermeticEnv::new();
        let tmp = tempfile::tempdir().expect("tempdir");
        let service = build_index_service(tmp.path(), &Config::default())
            .expect("the default [index] is valid");
        assert!(service.is_some(), "[index] is on by default");
    }
}
