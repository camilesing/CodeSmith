//! Capability manifest — the declarative capability-selection file
//! (`~/.codesmith/capabilities.toml`, the providers.toml paradigm).
//!
//! Composition-layer slice 2: one user-facing file selects what the
//! model-visible catalog may contain (**selection only** — implementation
//! replacement stays in config.toml `[tools].overrides` `script`/`command`).
//! The manifest is the session-level baseline; turn-scoped masks (preset
//! `tools.include`/`exclude`, slash-command frontmatter, the per-turn
//! `allowed_tools`/`blocked_tools` API) compose on top and cannot
//! resurrect a disabled tool — the same precedence the legacy
//! registry-level `Disabled` override had.
//!
//! Known limitations: the file is read once per process (restart picks up
//! edits); mods enable/consent state intentionally stays in
//! `ModStateStore` — that is lifecycle, not selection, and the separation
//! is final (settled 2026-10-06): merging would need format-preserving
//! AST edits plus reconcile-on-write, and neither the consent record
//! (activation that survives disable) nor the opposite failure semantics
//! (malformed mod state degrades to not-loaded; a malformed manifest
//! fails loud) has a natural single-file expression. Legacy config.toml
//! `[tools].overrides <name> = { type = "disabled" }` still parses
//! (external config contract) and is honored — unioned into the effective
//! set with a deprecation warning; its enforcement point moved to the
//! dispatch composition point (`EngineConfig.disabled_tools`), which both
//! the main turn (`build_turn_dispatcher`) and every sub-agent registry
//! (`SubAgentRuntime::capability_disabled_tools` →
//! `SubAgentToolRegistry::new`) apply — a disabled tool is neither visible
//! nor executable on either path, at any spawn depth.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Env override for the manifest path (tests/ops — the
/// `CODESMITH_PROVIDERS_MANIFEST` precedent). Unset →
/// `~/.codesmith/capabilities.toml`.
const CAPABILITIES_MANIFEST_ENV: &str = "CODESMITH_CAPABILITIES_MANIFEST";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapabilityManifest {
    /// Tool names removed from the model-visible catalog (session
    /// baseline; any origin — builtin, plugin, extension, mcp — by name).
    /// Entries are plain strings: tool names come and go, so a name that
    /// matches nothing is a no-op, not an error.
    pub tools_disabled: HashSet<String>,
}

impl CapabilityManifest {
    /// Parse with aggregated, field-path errors (discipline 6, the
    /// mod.toml pattern). Unknown sections/fields warn and are ignored;
    /// malformed known fields are collected into one error. A malformed
    /// manifest fails the engine build loudly (misconfiguration is
    /// self-contained here): degrading to an empty set would silently
    /// re-enable tools the user disabled for a reason.
    pub fn from_str(text: &str) -> Result<Self, String> {
        let table: toml::Table = text.parse().map_err(|e| format!("invalid TOML: {e}"))?;
        let mut errors: Vec<String> = Vec::new();
        let mut tools_disabled = HashSet::new();
        for (key, value) in &table {
            if key != "tools" {
                tracing::warn!(
                    target: "codesmith_capabilities",
                    "capabilities.toml: unknown section '{key}' ignored"
                );
                continue;
            }
            let Some(fields) = value.as_table() else {
                errors.push("tools: expected a table".to_string());
                continue;
            };
            for (field, v) in fields {
                if field != "disabled" {
                    tracing::warn!(
                        target: "codesmith_capabilities",
                        "capabilities.toml: unknown field 'tools.{field}' ignored"
                    );
                    continue;
                }
                let Some(entries) = v.as_array() else {
                    errors.push("tools.disabled: expected an array of tool names".to_string());
                    continue;
                };
                for entry in entries {
                    let Some(raw) = entry.as_str() else {
                        errors.push(format!(
                            "tools.disabled: expected a tool name, got `{entry}`"
                        ));
                        continue;
                    };
                    // Same lenience as `[tools].always_load`: trim, drop
                    // empty entries silently.
                    let name = raw.trim();
                    if !name.is_empty() {
                        tools_disabled.insert(name.to_string());
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(Self { tools_disabled })
        } else {
            Err(errors.join("; "))
        }
    }

    /// Load from `path`. A missing file is the normal first run → empty
    /// manifest; read errors and malformed content propagate (fail loud).
    pub fn load_from(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_str(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }
}

fn manifest_path() -> PathBuf {
    if let Ok(path) = std::env::var(CAPABILITIES_MANIFEST_ENV)
        && !path.trim().is_empty()
    {
        return PathBuf::from(path);
    }
    // Same state dir ModStateStore/ExtensionStateStore use (~/.codesmith).
    codesmith_config::ensure_state_dir(".")
        .map(|dir| dir.join("capabilities.toml"))
        // An unresolvable state dir surfaces as a read error at load.
        .unwrap_or_else(|_| PathBuf::from("capabilities.toml"))
}

fn manifest() -> Result<&'static CapabilityManifest, String> {
    static MANIFEST: OnceLock<Result<CapabilityManifest, String>> = OnceLock::new();
    MANIFEST
        .get_or_init(|| {
            let path = manifest_path();
            CapabilityManifest::load_from(&path).map_err(|e| {
                format!(
                    "capabilities manifest invalid ({e}) — fix {}",
                    path.display()
                )
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Union legacy config.toml `[tools].overrides` `disabled` entries into
/// `set` (deprecated but honored for the external config contract; one
/// enforcement point applies both — see `build_turn_dispatcher`).
fn union_legacy(set: &mut HashSet<String>, tools: Option<&crate::config::ToolsConfig>) {
    if let Some(overrides) = tools.and_then(|t| t.overrides.as_ref()) {
        for (name, override_cfg) in overrides {
            if matches!(override_cfg, crate::config::ToolOverride::Disabled)
                && set.insert(name.clone())
            {
                tracing::warn!(
                    target: "codesmith_capabilities",
                    "config.toml [tools].overrides.{name} = disabled is deprecated — \
                     list it in capabilities.toml [tools] disabled instead"
                );
            }
        }
    }
}

/// The session-level disabled set: the manifest plus the legacy entries.
/// A malformed manifest propagates as an error (fail loud, with the file
/// path) instead of a panic — the interactive TUI build path surfaces it
/// without tearing down the alternate screen.
pub fn effective_disabled(
    tools: Option<&crate::config::ToolsConfig>,
) -> Result<HashSet<String>, String> {
    let mut set = manifest()?.tools_disabled.clone();
    union_legacy(&mut set, tools);
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tools_disabled_and_ignores_unknown_with_warning() {
        let m = CapabilityManifest::from_str(
            r#"
[tools]
disabled = ["edit_file", " exec_shell ", "", "edit_file"]

[experimental]
foo = 1
"#,
        )
        .expect("valid manifest");
        assert_eq!(
            m.tools_disabled,
            ["edit_file", "exec_shell"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }

    #[test]
    fn malformed_known_field_aggregates_errors() {
        let err = CapabilityManifest::from_str("[tools]\ndisabled = \"edit_file\"\n")
            .expect_err("wrong type must fail");
        assert!(err.contains("tools.disabled"), "field path present: {err}");
        let err = CapabilityManifest::from_str("[tools]\ndisabled = [42]\n")
            .expect_err("non-string entry must fail");
        assert!(err.contains("tool name"), "entry error present: {err}");
    }

    #[test]
    fn invalid_toml_fails_with_parse_context() {
        let err = CapabilityManifest::from_str("[tools").expect_err("broken TOML");
        assert!(err.contains("invalid TOML"), "{err}");
    }

    #[test]
    fn missing_file_is_empty_read_error_propagates() {
        let m = CapabilityManifest::load_from(Path::new("/nonexistent/capabilities.toml"))
            .expect("missing file = first run");
        assert!(m.tools_disabled.is_empty());
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("capabilities.toml");
        std::fs::write(&path, "[tools]\ndisabled = 3\n").unwrap();
        assert!(CapabilityManifest::load_from(&path).is_err());
    }

    #[test]
    fn union_legacy_adds_only_disabled_entries() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(
            "edit_file".to_string(),
            crate::config::ToolOverride::Disabled,
        );
        overrides.insert(
            "read_file".to_string(),
            crate::config::ToolOverride::Command {
                command: "x".to_string(),
                args: None,
            },
        );
        let tools = crate::config::ToolsConfig {
            overrides: Some(overrides),
            ..Default::default()
        };
        let mut set: HashSet<String> = ["web_search".to_string()].into_iter().collect();
        union_legacy(&mut set, Some(&tools));
        assert_eq!(
            set,
            ["web_search", "edit_file"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // No ToolsConfig at all → untouched.
        let mut empty: HashSet<String> = HashSet::new();
        union_legacy(&mut empty, None);
        assert!(empty.is_empty());
    }

    #[test]
    fn tools_config_shape_is_untouched() {
        // Pins the fields `effective_disabled` reads, so a ToolsConfig
        // change consciously updates this module.
        let tools = crate::config::ToolsConfig::default();
        assert!(tools.overrides.is_none());
    }
}
