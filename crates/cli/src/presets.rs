//! Applying named configuration presets to the TUI.
//!
//! A preset (see `codesmith_config::presets`) is a bundle of agent dials
//! and resource-switch baselines. This module owns the *application* of a
//! preset:
//!
//! - **startup** ([`apply_config_preset`] + [`apply_at_startup`]): preset
//!   values are folded into the loaded [`Config`] with fill-if-unset
//!   semantics — explicit user configuration (file, profile, project
//!   overlay, env, CLI) always wins. Any explicit value that differs from
//!   the selected tier marks the effective preset as `diy`.
//! - **hot switch** ([`apply`], `/preset <name>`): the user's real-time
//!   command overrides the live session immediately; config-bound dials
//!   (index, LSP, snapshots, …) are flagged as restart-required.
//!
//! Selection precedence: `--preset` CLI > `preset` (or legacy `mode`) key
//! in config.toml > the preset persisted by the last `/preset` command >
//! the factory default `middle`.

use std::fmt::Write as _;

use codesmith_config::presets::{
    MemoryLevel, PresetCatalog, PresetDefinitionToml, PresetToolsToml, canonicalize_preset_name,
};

use crate::config::Config;

/// Fill `slot` from the preset's `value` when unset; flag a deviation
/// when the explicit value disagrees with the preset.
fn fill_bool(slot: &mut Option<bool>, value: Option<bool>, deviated: &mut bool) {
    match (&mut *slot, value) {
        (None, Some(value)) => *slot = Some(value),
        (Some(existing), Some(value)) if *existing != value => *deviated = true,
        _ => {}
    }
}

fn fill_string(slot: &mut Option<String>, value: Option<&str>, deviated: &mut bool) {
    match (&mut *slot, value) {
        (None, Some(value)) => *slot = Some(value.to_string()),
        (Some(existing), Some(value)) if existing != value => *deviated = true,
        _ => {}
    }
}

fn fill_usize(slot: &mut Option<usize>, value: Option<usize>, deviated: &mut bool) {
    match (&mut *slot, value) {
        (None, Some(value)) => *slot = Some(value),
        (Some(existing), Some(value)) if *existing != value => *deviated = true,
        _ => {}
    }
}

/// Fold a preset's config-bound dials into a loaded [`Config`] with
/// fill-if-unset semantics: only keys the user left unset are filled,
/// and any explicit value that differs from the preset flags a
/// deviation (which reports the effective preset as `diy`).
///
/// Returns whether a deviation was detected.
pub fn apply_to_config(config: &mut Config, definition: &PresetDefinitionToml) -> bool {
    let mut deviated = false;

    fill_string(
        &mut config.provider,
        definition.provider.as_deref(),
        &mut deviated,
    );
    fill_string(
        &mut config.approval_policy,
        definition.approval_policy.as_deref(),
        &mut deviated,
    );
    fill_string(
        &mut config.sandbox_mode,
        definition.sandbox_mode.as_deref(),
        &mut deviated,
    );
    fill_string(
        &mut config.reasoning_effort,
        definition.reasoning_effort.as_deref(),
        &mut deviated,
    );
    fill_usize(
        &mut config.max_subagents,
        definition.max_subagents,
        &mut deviated,
    );

    if let Some(features) = &definition.features
        && !features.is_empty()
    {
        let table = config.features.get_or_insert_with(Default::default);
        for (key, value) in features {
            match table.entries.get(key) {
                None => {
                    table.entries.insert(key.clone(), *value);
                }
                Some(&existing) if existing != *value => deviated = true,
                _ => {}
            }
        }
    }

    let index = config.index.get_or_insert_with(Default::default);
    fill_bool(&mut index.enabled, definition.index_enabled, &mut deviated);

    let lsp = config.lsp.get_or_insert_with(Default::default);
    fill_bool(&mut lsp.enabled, definition.lsp_enabled, &mut deviated);
    fill_bool(
        &mut lsp.include_warnings,
        definition.lsp_include_warnings,
        &mut deviated,
    );

    let snapshots = config.snapshots.get_or_insert_with(Default::default);
    fill_bool(
        &mut snapshots.enabled,
        definition.snapshots_enabled,
        &mut deviated,
    );

    // Resolve BOTH preset-side memory sources into one desired value
    // first (explicit keys override the derived level): feeding them
    // through fill_bool separately let a preset that sets both
    // `memory_level` and `memory_kod_enabled` compare its own two keys
    // against each other, spuriously flagging `deviated` (mislabeling the
    // tier as diy) and letting the level silently win.
    let derived = definition
        .memory_level
        .as_deref()
        .and_then(MemoryLevel::from_setting)
        .map(|parsed| match parsed {
            MemoryLevel::Goldfish => (Some(false), Some(false)),
            MemoryLevel::Notebook => (Some(true), Some(false)),
            MemoryLevel::Elephant => (Some(true), Some(true)),
        });
    let (want_enabled, want_kod) = match derived {
        Some((enabled, kod)) => (
            definition.memory_enabled.or(enabled),
            definition.memory_kod_enabled.or(kod),
        ),
        None => (definition.memory_enabled, definition.memory_kod_enabled),
    };
    if want_enabled.is_some() || want_kod.is_some() {
        let memory = config.memory.get_or_insert_with(Default::default);
        fill_bool(&mut memory.enabled, want_enabled, &mut deviated);
        fill_bool(&mut memory.kod_enabled, want_kod, &mut deviated);
    }

    fill_bool(
        &mut config.context.enabled,
        definition.context_enabled,
        &mut deviated,
    );
    fill_bool(
        &mut config.context.project_pack,
        definition.context_project_pack,
        &mut deviated,
    );

    let capacity = config.capacity.get_or_insert_with(Default::default);
    fill_bool(
        &mut capacity.enabled,
        definition.capacity_enabled,
        &mut deviated,
    );

    let auto = config.auto.get_or_insert_with(Default::default);
    fill_bool(
        &mut auto.cost_saving,
        definition.auto_cost_saving,
        &mut deviated,
    );

    let update = config.update.get_or_insert_with(Default::default);
    fill_bool(
        &mut update.check_for_updates,
        definition.update_check,
        &mut deviated,
    );

    let network = config.network.get_or_insert_with(Default::default);
    fill_bool(&mut network.audit, definition.network_audit, &mut deviated);

    fill_bool(
        &mut config.strict_tool_mode,
        definition.strict_tool_mode,
        &mut deviated,
    );

    deviated
}

/// A preset resolved and folded into the config at startup.
#[derive(Debug, Clone)]
pub struct AppliedPreset {
    /// Canonical preset name (aliases like `minimal` already mapped).
    pub name: String,
    /// The definition that was applied.
    pub definition: PresetDefinitionToml,
    /// Whether explicit config values deviated from the tier (`diy`).
    pub deviated: bool,
}

/// Resolve the selected preset (falling back to the factory default
/// `middle`), fold its config-bound dials into `config` with fill-if-unset
/// semantics, and record the effective state on the config. Unknown
/// selections log a warning and fall back to `middle` rather than failing
/// the launch.
pub fn apply_config_preset(
    config: &mut Config,
    workspace: &std::path::Path,
) -> Option<AppliedPreset> {
    let selection = match config.preset_selection().map(str::to_string) {
        Some(raw) => match canonicalize_preset_name(&raw) {
            Ok((name, Some(warning))) => {
                tracing::warn!("{warning}");
                name
            }
            Ok((name, None)) => name,
            Err(err) => {
                tracing::warn!(
                    selection = %raw,
                    error = %err,
                    "invalid preset selection; falling back to the default tier"
                );
                "middle".to_string()
            }
        },
        None => "middle".to_string(),
    };

    let catalog = PresetCatalog::load(Some(workspace));
    for warning in &catalog.warnings {
        tracing::warn!("{warning}");
    }
    let loaded = match catalog.get_ci(&selection).cloned() {
        Some(loaded) => loaded,
        None => {
            tracing::warn!(
                preset = %selection,
                available = ?catalog.names(),
                "preset not found; falling back to the default tier"
            );
            // `middle` is compiled in, so this only fails if the catalog
            // itself is broken; bail out with no preset layer in that case.
            catalog.get_ci("middle").cloned()?
        }
    };
    Some(fold(config, loaded))
}

fn fold(config: &mut Config, loaded: codesmith_config::presets::LoadedPreset) -> AppliedPreset {
    let definition = loaded.definition.clone();
    let deviated = apply_to_config(config, &definition);
    config.preset_deviated |= deviated;
    config.preset = Some(loaded.name.clone());
    // The deprecated alias is consumed: the canonical selection replaces it.
    config.mode = None;
    AppliedPreset {
        name: loaded.name,
        definition,
        deviated,
    }
}

/// The preset persisted by the last `/preset <name>` command (if any),
/// for startup selection when neither the CLI nor config.toml chose one.
pub fn persisted_selection() -> Option<String> {
    crate::settings::Settings::load()
        .ok()
        .and_then(|settings| settings.active_preset)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Build a preset-definition snapshot for `/preset export <name>`: start
/// from the active definition (keeping its config-bound dials), overlay
/// the live dials the REPL tracks (mode + tool lists), and rename. This is
/// a snapshot of preset-owned state, not a full config dump.
pub fn export_definition(
    base: Option<&PresetDefinitionToml>,
    app_mode: &str,
    allowed_tools: Option<&[String]>,
    blocked_tools: Option<&[String]>,
    name: &str,
) -> PresetDefinitionToml {
    let mut definition = base.cloned().unwrap_or_default();
    definition.name = Some(name.to_string());
    definition.app_mode = Some(app_mode.to_string());
    definition.tools = Some(PresetToolsToml {
        include: allowed_tools.map(<[String]>::to_vec),
        exclude: blocked_tools.map(<[String]>::to_vec),
    });
    definition
}

/// Render the `/preset` listing: every preset with its source,
/// description, and active marker.
pub fn describe(catalog: &PresetCatalog, active: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Presets (switch with /preset <name>):");
    for preset in catalog.iter() {
        let marker = if Some(preset.name.as_str()) == active {
            "← active"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {:<12} [{:<8}] {} {}",
            preset.name,
            preset.source.label(),
            preset.definition.description.as_deref().unwrap_or(""),
            marker
        );
    }
    if !catalog.warnings.is_empty() {
        out.push('\n');
        for warning in &catalog.warnings {
            let _ = writeln!(out, "⚠ {warning}");
        }
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "Tiers: simple < middle (default) < all < experiment; explicit config keys always win."
    );
    let _ = writeln!(
        out,
        "Layers: built-in < ~/.codesmith/presets/ < <workspace>/.codesmith/presets/ (later wins)."
    );
    let _ = writeln!(
        out,
        "Share a preset by committing its .toml file; /preset export writes one."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_definition_overlays_live_dials_on_the_active_base() {
        let mut base = PresetDefinitionToml {
            name: Some("all".to_string()),
            app_mode: Some("agent".to_string()),
            max_subagents: Some(4),
            ..PresetDefinitionToml::default()
        };
        base.memory_level = Some("elephant".to_string());
        let allowed = vec!["exec_shell".to_string(), "read_file".to_string()];

        let exported = export_definition(Some(&base), "yolo", Some(&allowed), None, "my-setup");

        // Live dials win.
        assert_eq!(exported.name.as_deref(), Some("my-setup"));
        assert_eq!(exported.app_mode.as_deref(), Some("yolo"));
        assert_eq!(
            exported.tools.as_ref().and_then(|t| t.include.clone()),
            Some(allowed)
        );
        assert_eq!(
            exported.tools.as_ref().and_then(|t| t.exclude.clone()),
            None
        );
        // Config-bound dials carry over from the active definition.
        assert_eq!(exported.max_subagents, Some(4));
        assert_eq!(exported.memory_level.as_deref(), Some("elephant"));
    }

    #[test]
    fn export_definition_without_base_snapshots_mode_and_tools() {
        let exported = export_definition(
            None,
            "plan",
            None,
            Some(&["web_search".to_string()]),
            "solo",
        );
        assert_eq!(exported.name.as_deref(), Some("solo"));
        assert_eq!(exported.app_mode.as_deref(), Some("plan"));
        assert_eq!(
            exported.tools.as_ref().and_then(|t| t.exclude.clone()),
            Some(vec!["web_search".to_string()])
        );
        assert!(exported.tools.as_ref().is_some_and(|t| t.include.is_none()));
        // Round-trips through the preset parser.
        let body = toml::to_string_pretty(&exported).expect("serialize");
        let parsed = PresetDefinitionToml::parse_toml(&body, "fallback").expect("parse back");
        assert_eq!(parsed.app_mode.as_deref(), Some("plan"));
    }
}
