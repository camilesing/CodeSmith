//! `[lsp]` config schema extracted from `crates/tui/src/lsp/mod.rs` so
//! `EngineConfig` can hold an `Option<LspConfig>` without a tui dependency.
//!
//! The resolve logic (`LspConfig::resolve_command`) lives here too because
//! inherent impls must reside in the defining crate; it delegates to
//! [`crate::lsp_registry`] for the default server table.

use std::collections::HashMap;

use serde::Deserialize;

use crate::lsp_registry::{Language, server_for};

/// `[lsp]` config schema. Mirrors the TOML keys documented in
/// `config.example.toml`. Unknown keys are ignored.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LspConfig {
    /// Master switch. When `false`, the manager skips every operation and
    /// returns an empty diagnostics list.
    pub enabled: bool,
    /// Maximum time in milliseconds to wait for the LSP server to publish
    /// diagnostics after a `didOpen`/`didChange`. Default 5000 ms.
    pub poll_after_edit_ms: u64,
    /// Maximum diagnostics to keep per file. Excess items are dropped after
    /// sorting by severity. Default 20.
    pub max_diagnostics_per_file: usize,
    /// When `true`, warnings (severity 2) are kept in the output. When
    /// `false` (default), only errors (severity 1) are surfaced.
    pub include_warnings: bool,
    /// Optional override for the `Language -> (cmd, args)` table. Keys use
    /// [`Language::as_key`] (e.g. `"rust"`).
    pub servers: HashMap<String, Vec<String>>,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_after_edit_ms: 5_000,
            max_diagnostics_per_file: 20,
            include_warnings: false,
            servers: HashMap::new(),
        }
    }
}

impl LspConfig {
    /// Resolve `(command, args)` for `lang`. User-supplied overrides take
    /// precedence over the built-in registry.
    pub fn resolve_command(&self, lang: Language) -> Option<(String, Vec<String>)> {
        if let Some(parts) = self.servers.get(lang.as_key())
            && let Some((first, rest)) = parts.split_first()
        {
            return Some((first.clone(), rest.to_vec()));
        }
        let (cmd, args) = server_for(lang)?;
        Some((
            cmd.to_string(),
            args.iter().map(|a| (*a).to_string()).collect(),
        ))
    }
}
