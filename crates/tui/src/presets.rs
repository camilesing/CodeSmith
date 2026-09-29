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

use anyhow::{Context, Result, bail};
use codesmith_agent_runtime::mode::{AppMode, ApprovalMode, ReasoningEffort};
use codesmith_config::presets::{
    MemoryLevel, PresetCatalog, PresetDefinitionToml, PresetSource, PresetToolsToml,
    canonicalize_preset_name,
};

use crate::config::Config;
use crate::tui::app::App;

/// Load every preset visible to the current workspace (built-ins + user +
/// project). Invalid files are skipped; surface the catalog warnings so
/// `/preset` can tell the user their file has a problem.
pub fn catalog_for(app: &App) -> PresetCatalog {
    PresetCatalog::load(Some(&app.workspace))
}

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
/// value; config-bound switches are summarized as restart-required.
pub fn apply(app: &mut App, name: &str) -> Result<PresetApplySummary> {
    let catalog = catalog_for(app);
    // Map deprecated mode names (minimal/balanced/maximal) and reject
    // `diy` with an actionable message.
    let (canonical, deprecation) = canonicalize_preset_name(name)?;
    let loaded = catalog
        .get_ci(&canonical)
        .with_context(|| {
            let available = catalog.names().join(", ");
            format!("unknown preset '{name}'. Available: {available}")
        })?
        .clone();

    let definition = loaded.definition;
    let mut summary = PresetApplySummary {
        preset_name: Some(loaded.name.clone()),
        warnings: catalog.warnings.clone(),
        ..PresetApplySummary::default()
    };
    if let Some(warning) = deprecation {
        summary.warnings.push(warning);
    }

    if let Some(app_mode) = &definition.app_mode {
        let parsed = AppMode::from_setting(app_mode);
        let changed = app.set_mode(parsed);
        summary.line(format!(
            "app mode: {}{}",
            parsed.label().to_ascii_lowercase(),
            if changed { "" } else { " (already active)" }
        ));
    }

    if let Some(effort) = &definition.reasoning_effort {
        let parsed = ReasoningEffort::from_setting(effort);
        app.reasoning_effort = parsed;
        app.last_effective_reasoning_effort = None;
        app.needs_redraw = true;
        summary.line(format!("thinking: {}", parsed.as_setting()));
    }

    if let Some(policy) = &definition.approval_policy
        && let Some(parsed) = ApprovalMode::from_config_value(policy)
    {
        app.approval_mode = parsed;
        summary.line(format!("approval: {}", parsed.label().to_ascii_lowercase()));
    }

    if let Some(cap) = definition.max_subagents {
        app.max_subagents = cap;
        if cap == 0 {
            summary.line("sub-agents: off".to_string());
        } else {
            summary.line(format!("sub-agents: capped at {cap}"));
        }
    }

    if let Some(model) = &definition.model {
        app.set_model_selection(model.clone());
        summary.line(format!("model: {model}"));
    }

    if let Some(level) = &definition.memory_level
        && let Some(parsed) = MemoryLevel::from_setting(level)
    {
        match parsed {
            MemoryLevel::Goldfish => {
                app.use_memory = false;
                app.kod_enabled = false;
            }
            MemoryLevel::Notebook => {
                app.use_memory = true;
                app.kod_enabled = false;
            }
            MemoryLevel::Elephant => {
                app.use_memory = true;
                app.kod_enabled = true;
            }
        }
        summary.line(format!(
            "memory: {} ({})",
            parsed.as_setting(),
            parsed.description()
        ));
        summary.restart("memory injection into the system prompt");
    }

    if let Some(tools) = &definition.tools {
        if let Some(include) = &tools.include
            && !include.is_empty()
        {
            app.active_allowed_tools = Some(include.clone());
            summary.line(format!("tools: allowlist of {}", include.len()));
        } else {
            app.active_allowed_tools = None;
        }
        match &tools.exclude {
            Some(exclude) if !exclude.is_empty() => {
                app.active_blocked_tools = Some(exclude.clone());
                summary.line(format!("tools: {} blocked", exclude.len()));
            }
            _ => {
                app.active_blocked_tools = None;
            }
        }
    } else {
        app.active_allowed_tools = None;
        app.active_blocked_tools = None;
    }

    if definition.provider.is_some() {
        summary.restart(format!(
            "provider: {} (the LLM client is resolved at startup)",
            definition.provider.as_deref().unwrap_or_default()
        ));
    }
    let governed: Vec<_> = definition
        .bool_dials()
        .into_iter()
        .filter(|(_, value)| value.is_some())
        .map(|(key, _)| key)
        .collect();
    if !governed.is_empty() {
        summary.restart(format!("resource switches: {}", governed.join(", ")));
    }
    if let Some(features) = &definition.features
        && !features.is_empty()
    {
        let keys = features.keys().cloned().collect::<Vec<_>>().join(", ");
        summary.restart(format!("features: {keys}"));
    }

    app.active_preset = Some(loaded.name.clone());
    app.needs_redraw = true;
    persist_active_preset(&loaded.name);
    Ok(summary)
}

/// Deactivate the preset layer: dials keep their current values, but the
/// preset's tool filters are cleared and the footer chip disappears.
pub fn clear(app: &mut App) -> PresetApplySummary {
    let name = app.active_preset.take();
    app.active_allowed_tools = None;
    app.active_blocked_tools = None;
    app.needs_redraw = true;
    persist_active_preset("");
    let mut summary = PresetApplySummary::default();
    summary.line(match name {
        Some(name) => format!("preset layer '{name}' cleared — dials keep their current values"),
        None => "no preset was active".to_string(),
    });
    summary
}

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
/// by an explicit `/preset` command.
pub fn apply_at_startup(app: &mut App, name: &str) {
    let catalog = catalog_for(app);
    let Some(loaded) = catalog.get_ci(name) else {
        return; // unknown selections already warned in apply_config_preset
    };
    let definition = &loaded.definition;
    if let Some(app_mode) = &definition.app_mode {
        app.set_mode(AppMode::from_setting(app_mode));
    }
    match &definition.tools {
        Some(tools) => {
            if let Some(include) = &tools.include
                && !include.is_empty()
            {
                app.active_allowed_tools = Some(include.clone());
            } else {
                app.active_allowed_tools = None;
            }
            match &tools.exclude {
                Some(exclude) if !exclude.is_empty() => {
                    app.active_blocked_tools = Some(exclude.clone());
                }
                _ => {
                    app.active_blocked_tools = None;
                }
            }
        }
        None => {
            app.active_allowed_tools = None;
            app.active_blocked_tools = None;
        }
    }
    app.active_preset = Some(loaded.name.clone());
    app.needs_redraw = true;
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

/// Export the app's current dials as a preset file under
/// `<workspace>/.codesmith/presets/<name>.toml`. Refuses to overwrite a
/// built-in name unless `force` is set.
pub fn export(app: &App, name: Option<&str>, force: bool) -> Result<String> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    let Some(name) = name else {
        bail!("Usage: /preset export <name> — pick a (non-built-in) name for the new preset");
    };
    if !force {
        let catalog = catalog_for(app);
        if let Some(existing) = catalog.get_ci(name)
            && existing.source == PresetSource::BuiltIn
        {
            bail!(
                "'{name}' is a built-in preset; choose another name or use /preset export! {name} to override it locally"
            );
        }
    }

    let approval = match app.approval_mode {
        ApprovalMode::Auto => "auto",
        ApprovalMode::Suggest => "suggest",
        ApprovalMode::Never => "never",
    };
    let memory_level = if !app.use_memory {
        MemoryLevel::Goldfish
    } else if app.kod_enabled {
        MemoryLevel::Elephant
    } else {
        MemoryLevel::Notebook
    };
    let definition = PresetDefinitionToml {
        name: Some(name.to_string()),
        description: Some("Exported from the current session via /preset export.".to_string()),
        app_mode: Some(app.mode.as_setting().to_string()),
        reasoning_effort: Some(app.reasoning_effort.as_setting().to_string()),
        approval_policy: Some(approval.to_string()),
        sandbox_mode: None,
        memory_level: Some(memory_level.as_setting().to_string()),
        max_subagents: Some(app.max_subagents),
        model: Some(app.model.clone()),
        provider: None,
        tools: match (&app.active_allowed_tools, &app.active_blocked_tools) {
            (None, None) => None,
            (allowed, blocked) => Some(PresetToolsToml {
                include: allowed.clone(),
                exclude: blocked.clone(),
            }),
        },
        features: None,
        ..PresetDefinitionToml::default()
    };

    let dir = app.workspace.join(".codesmith").join("presets");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{name}.toml"));
    let body =
        toml::to_string_pretty(&definition).with_context(|| "serializing exported preset")?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(format!("Exported preset '{name}' to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, TuiOptions};
    use std::path::PathBuf;

    /// Redirects config/settings persistence into a temp home so `apply()`
    /// (which persists the active preset to settings.toml) never touches the
    /// real user directory. Holds the process-wide test-env mutex.
    struct TestEnv {
        _lock: std::sync::MutexGuard<'static, ()>,
        _guard: crate::test_support::EnvVarGuard,
        _dir: tempfile::TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let lock = crate::test_support::lock_test_env();
            let dir = tempfile::tempdir().unwrap();
            let guard = crate::test_support::EnvVarGuard::set(
                "CODESMITH_CONFIG_PATH",
                dir.path().join("config.toml"),
            );
            Self {
                _lock: lock,
                _guard: guard,
                _dir: dir,
            }
        }
    }

    fn test_app() -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: PathBuf::from("."),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: PathBuf::from("."),
            memory_path: PathBuf::from("memory.md"),
            notes_path: PathBuf::from("notes.txt"),
            mcp_config_path: PathBuf::from("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &crate::config::Config::default())
    }

    #[test]
    fn apply_unknown_preset_reports_available() {
        let _env = TestEnv::new();
        let mut app = test_app();
        let err = apply(&mut app, "does-not-exist").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown preset"), "got: {msg}");
        assert!(msg.contains("simple"), "should list built-ins: {msg}");
        assert!(app.active_preset.is_none());
    }

    #[test]
    fn apply_simple_sets_live_dials() {
        let _env = TestEnv::new();
        let mut app = test_app();
        app.reasoning_effort = ReasoningEffort::Max;
        app.max_subagents = 8;

        let summary = apply(&mut app, "simple").unwrap();
        assert_eq!(app.active_preset.as_deref(), Some("simple"));
        assert_eq!(app.reasoning_effort, ReasoningEffort::Medium);
        assert_eq!(app.max_subagents, 0);
        assert!(!app.use_memory);
        let allowed = app.active_allowed_tools.clone().unwrap();
        assert!(allowed.contains(&"read_file".to_string()));
        assert!(!allowed.contains(&"web_search".to_string()));
        assert!(summary.preset_name.as_deref() == Some("simple"));
        // Resource switches cannot hot-switch; they are restart notes.
        assert!(
            summary
                .restart_notes
                .iter()
                .any(|note| note.contains("resource switches"))
        );
    }

    #[test]
    fn apply_all_turns_memory_and_subagents_on() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "all").unwrap();
        assert!(app.use_memory);
        // Notebook level: memory on, Knowledge On Demand only with
        // experiment's elephant level.
        assert!(!app.kod_enabled);
        assert_eq!(app.max_subagents, 20);
        // No tool restriction in all.
        assert!(app.active_allowed_tools.is_none());
    }

    #[test]
    fn apply_experiment_turns_knowledge_on_demand_on() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "experiment").unwrap();
        assert!(app.use_memory);
        assert!(app.kod_enabled);
        assert_eq!(app.max_subagents, 20);
    }

    #[test]
    fn apply_plan_switches_app_mode() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "plan").unwrap();
        assert_eq!(app.mode, AppMode::Plan);
        assert!(app.use_memory, "notebook keeps explicit notes");
        assert!(!app.kod_enabled);
    }

    #[test]
    fn clear_removes_layer_and_filters() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "simple").unwrap();
        let summary = clear(&mut app);
        assert!(app.active_preset.is_none());
        assert!(app.active_allowed_tools.is_none());
        assert!(app.active_blocked_tools.is_none());
        assert!(summary.render().contains("cleared"));
    }

    #[test]
    fn apply_is_case_insensitive() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "Simple").unwrap();
        assert_eq!(app.active_preset.as_deref(), Some("simple"));
    }

    #[test]
    fn apply_maps_deprecated_mode_names() {
        let _env = TestEnv::new();
        let mut app = test_app();
        // `minimal` resolves through the catalog to the simple tier's
        // dials (the alias mapping itself is exercised at startup by
        // `apply_config_preset`).
        apply(&mut app, "minimal").unwrap();
        assert_eq!(app.reasoning_effort, ReasoningEffort::Medium);
        assert_eq!(app.max_subagents, 0);
    }

    #[test]
    fn describe_marks_active_and_lists_sources() {
        let catalog = PresetCatalog::load_from(None, None);
        let text = describe(&catalog, Some("simple"));
        assert!(text.contains("← active"));
        assert!(text.contains("built-in"));
        assert!(text.contains("/preset export"));
    }

    #[test]
    fn export_writes_project_preset_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.workspace = tmp.path().to_path_buf();
        app.reasoning_effort = ReasoningEffort::High;
        app.max_subagents = 4;

        let msg = export(&app, Some("my-preset"), false).unwrap();
        let path = tmp.path().join(".codesmith/presets/my-preset.toml");
        assert!(path.exists(), "{msg}");

        let body = std::fs::read_to_string(&path).unwrap();
        let parsed = PresetDefinitionToml::parse_toml(&body, "x").unwrap();
        assert_eq!(parsed.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(parsed.max_subagents, Some(4));

        // The exported file is loadable as a project preset.
        let catalog = PresetCatalog::load_from(None, Some(&tmp.path().join(".codesmith/presets")));
        assert!(catalog.get("my-preset").is_some(), "{:?}", catalog.names());
    }

    #[test]
    fn export_refuses_builtin_names_without_force() {
        // Temp workspace: the forced export below must not write into the
        // source tree.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.workspace = tmp.path().to_path_buf();
        let err = export(&app, Some("simple"), false).unwrap_err();
        assert!(err.to_string().contains("built-in"));
        assert!(export(&app, Some("simple"), true).is_ok());
        assert!(tmp.path().join(".codesmith/presets/simple.toml").exists());
    }

    #[test]
    fn every_builtin_tier_is_internally_consistent() {
        // Applying a builtin tier to an empty config must fill every
        // governed key without flagging a deviation: a tier that
        // contradicts itself (e.g. memory_level implying Knowledge On
        // Demand while memory_kod_enabled says off) would surface as diy
        // on every fresh install.
        let catalog = PresetCatalog::load_from(None, None);
        for name in codesmith_config::presets::TIER_NAMES {
            let definition = catalog.get(name).unwrap().definition.clone();
            let mut config = Config::default();
            assert!(
                !apply_to_config(&mut config, &definition),
                "tier {name} deviates from an empty config"
            );
        }
    }

    #[test]
    fn apply_to_config_fills_unset_keys_and_keeps_explicit_ones() {
        let definition = PresetCatalog::load_from(None, None)
            .get("simple")
            .unwrap()
            .definition
            .clone();

        // Fresh config: everything unset → tier baselines are filled.
        let mut config = Config::default();
        let deviated = apply_to_config(&mut config, &definition);
        assert!(!deviated);
        assert_eq!(config.index.as_ref().unwrap().enabled, Some(false));
        assert_eq!(config.lsp.as_ref().unwrap().enabled, Some(false));
        assert_eq!(config.reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(config.max_subagents, Some(0));
        assert!(
            !config
                .features()
                .enabled(codesmith_agent_runtime::features::Feature::WebSearch)
        );

        // Explicit values win and flag a deviation.
        let mut config: Config = toml::from_str(
            r#"
            [lsp]
            enabled = true
            "#,
        )
        .unwrap();
        let deviated = apply_to_config(&mut config, &definition);
        assert!(deviated);
        assert_eq!(
            config.lsp.as_ref().unwrap().enabled,
            Some(true),
            "explicit LSP survives"
        );
        assert_eq!(
            config.snapshots.as_ref().unwrap().enabled,
            Some(false),
            "unset key is filled from the tier"
        );
    }

    #[test]
    fn apply_to_config_matching_explicit_values_do_not_deviate() {
        let definition = PresetCatalog::load_from(None, None)
            .get("middle")
            .unwrap()
            .definition
            .clone();
        // middle says index on, lsp on, cost_saving off — a user config
        // spelling the same values stays "middle", not diy.
        let mut config: Config = toml::from_str(
            r#"
            [index]
            enabled = true
            [auto]
            cost_saving = false
            "#,
        )
        .unwrap();
        let deviated = apply_to_config(&mut config, &definition);
        assert!(!deviated);
    }

    #[test]
    fn apply_config_preset_defaults_to_middle_and_canonicalizes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        let applied = apply_config_preset(&mut config, tmp.path()).unwrap();
        assert_eq!(applied.name, "middle");
        assert_eq!(config.preset.as_deref(), Some("middle"));
        // Quality-first baseline: the strong brain routes; cost_saving is
        // the opt-in cheap router.
        assert_eq!(config.auto.as_ref().unwrap().cost_saving, Some(false));
        assert_eq!(
            config.index.as_ref().unwrap().enabled,
            Some(true),
            "middle keeps the index on"
        );

        // The deprecated `mode` key is canonicalized into `preset`.
        let mut config: Config = toml::from_str("mode = \"minimal\"\n").unwrap();
        let applied = apply_config_preset(&mut config, tmp.path()).unwrap();
        assert_eq!(applied.name, "simple");
        assert_eq!(config.preset.as_deref(), Some("simple"));
        assert!(config.mode.is_none(), "deprecated alias consumed");
        assert_eq!(config.index.as_ref().unwrap().enabled, Some(false));
    }

    #[test]
    fn apply_config_preset_marks_deviating_config_as_diy() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config: Config = toml::from_str(
            r#"
            preset = "simple"
            [lsp]
            enabled = true
            "#,
        )
        .unwrap();
        let applied = apply_config_preset(&mut config, tmp.path()).unwrap();
        assert_eq!(applied.name, "simple");
        assert!(applied.deviated);
        assert!(config.preset_deviated);
        assert_eq!(config.effective_preset(), "diy");
        assert_eq!(
            config.lsp.as_ref().unwrap().enabled,
            Some(true),
            "the explicit value still wins"
        );
    }

    #[test]
    fn apply_config_preset_unknown_selection_falls_back_to_middle() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config: Config = toml::from_str("preset = \"no-such-tier\"\n").unwrap();
        let applied = apply_config_preset(&mut config, tmp.path()).unwrap();
        assert_eq!(applied.name, "middle");
    }

    #[test]
    fn apply_at_startup_sets_tool_surface_and_name_without_persist() {
        let _env = TestEnv::new();
        let mut app = test_app();
        app.max_subagents = 7;
        apply_at_startup(&mut app, "simple");
        assert_eq!(app.active_preset.as_deref(), Some("simple"));
        assert!(app.active_allowed_tools.is_some());
        // Config-backed live dials are NOT overridden here: the App was
        // already constructed from the filled config.
        assert_eq!(app.max_subagents, 7);
        // Nothing was persisted: settings stays clean.
        assert!(persisted_selection().is_none());
    }
}
