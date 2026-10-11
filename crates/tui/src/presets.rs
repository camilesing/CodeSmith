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
    MemoryLevel, PresetCatalog, PresetDefinitionToml, canonicalize_preset_name,
};

use crate::config::Config;

/// Load every preset visible to the current workspace (built-ins + user +
/// project). Invalid files are skipped; surface the catalog warnings so
/// Outcome of applying (or clearing) a preset, rendered into the chat.
#[derive(Debug, Default)]
pub struct PresetApplySummary {
    pub preset_name: Option<String>,
    pub applied: Vec<String>,
    pub restart_notes: Vec<String>,
    pub warnings: Vec<String>,
}

impl PresetApplySummary {
    fn line(&mut self, text: impl Into<String>) {
        self.applied.push(text.into());
    }

    fn restart(&mut self, text: impl Into<String>) {
        self.restart_notes.push(text.into());
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(name) = &self.preset_name {
            let _ = writeln!(out, "Preset: {name}");
        }
        for line in &self.applied {
            let _ = writeln!(out, "  · {line}");
        }
        if !self.restart_notes.is_empty() {
            out.push('\n');
            let _ = writeln!(out, "Applies after restart:");
            for note in &self.restart_notes {
                let _ = writeln!(out, "  · {note}");
            }
        }
        if !self.warnings.is_empty() {
            out.push('\n');
            for warning in &self.warnings {
                let _ = writeln!(out, "⚠ {warning}");
            }
        }
        out
    }
}

/// Apply a named preset to the live app (override semantics — the user
/// just asked for it). Dials the preset leaves unset keep their current
/// Deactivate the preset layer: dials keep their current values, but the

/// Persist the active preset choice so the next launch restores it. Empty
/// string clears the stored value; failures are non-fatal (the session
/// still runs, it just won't restore).
fn persist_active_preset(name: &str) {
    let mut settings = crate::settings::Settings::load().unwrap_or_default();
    let _ = settings.set("active_preset", name);
    if let Err(err) = settings.save() {
        tracing::warn!(error = %err, preset = name, "failed to persist active preset");
    }
}

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

    if let Some(level) = &definition.memory_level
        && let Some(parsed) = MemoryLevel::from_setting(level)
    {
        let memory = config.memory.get_or_insert_with(Default::default);
        let (want_enabled, want_kod) = match parsed {
            MemoryLevel::Goldfish => (Some(false), Some(false)),
            MemoryLevel::Notebook => (Some(true), Some(false)),
            MemoryLevel::Elephant => (Some(true), Some(true)),
        };
        fill_bool(&mut memory.enabled, want_enabled, &mut deviated);
        fill_bool(&mut memory.kod_enabled, want_kod, &mut deviated);
    }

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

    let memory = config.memory.get_or_insert_with(Default::default);
    fill_bool(
        &mut memory.enabled,
        definition.memory_enabled,
        &mut deviated,
    );
    fill_bool(
        &mut memory.kod_enabled,
        definition.memory_kod_enabled,
        &mut deviated,
    );

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

/// Startup half of [`apply`]: config-backed dials (thinking, sub-agent
/// cap, memory, approval) were already resolved fill-if-unset into the
/// loaded config and flowed into the App, so only the dials with no
/// config representation (app mode, tool surface, the active-name chip)
/// are applied here. Never persists — the settings value is only written
/// The preset persisted by the last `/preset <name>` command (if any),
/// for startup selection when neither the CLI nor config.toml chose one.
pub fn persisted_selection() -> Option<String> {
    crate::settings::Settings::load()
        .ok()
        .and_then(|settings| settings.active_preset)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
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
