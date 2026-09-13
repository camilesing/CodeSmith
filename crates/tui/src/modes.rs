//! Applying named runtime modes to live TUI state.
//!
//! A mode (see `codesmith_config::modes`) is a delta bundle of agent
//! dials. This module owns the *application* of a mode to the running
//! [`App`]: live dials (app mode, thinking tier, approval, sub-agent cap,
//! tool surface, model, memory flags) switch immediately and take effect
//! on the next turn; config-bound dials (provider, feature flags,
//! memory injection) are flagged as restart-required so the summary is
//! honest about what changed.

use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use codesmith_agent_runtime::mode::{AppMode, ApprovalMode, ReasoningEffort};
use codesmith_config::modes::{
    MemoryLevel, ModeCatalog, ModeDefinitionToml, ModeSource, ModeToolsToml,
};

use crate::tui::app::App;

/// Load every mode visible to the current workspace (built-ins + user +
/// project). Invalid files are skipped; surface the catalog warnings so
/// `/mode` can tell the user their file has a problem.
pub fn catalog_for(app: &App) -> ModeCatalog {
    ModeCatalog::load(Some(&app.workspace))
}

/// Outcome of applying (or clearing) a mode, rendered into the chat.
#[derive(Debug, Default)]
pub struct ModeApplySummary {
    pub mode_name: Option<String>,
    pub applied: Vec<String>,
    pub restart_notes: Vec<String>,
    pub warnings: Vec<String>,
}

impl ModeApplySummary {
    fn line(&mut self, text: impl Into<String>) {
        self.applied.push(text.into());
    }

    fn restart(&mut self, text: impl Into<String>) {
        self.restart_notes.push(text.into());
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(name) = &self.mode_name {
            let _ = writeln!(out, "Mode: {name}");
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

/// Apply a named mode to the live app. Dials the mode leaves unset keep
/// their current value (delta semantics).
pub fn apply(app: &mut App, name: &str) -> Result<ModeApplySummary> {
    let catalog = catalog_for(app);
    let loaded = catalog
        .get_ci(name)
        .with_context(|| {
            let available = catalog.names().join(", ");
            format!("unknown mode '{name}'. Available: {available}")
        })?
        .clone();

    let definition = loaded.definition;
    let mut summary = ModeApplySummary {
        mode_name: Some(loaded.name.clone()),
        warnings: catalog.warnings.clone(),
        ..ModeApplySummary::default()
    };

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
    if let Some(features) = &definition.features
        && !features.is_empty()
    {
        let keys = features.keys().cloned().collect::<Vec<_>>().join(", ");
        summary.restart(format!("features: {keys}"));
    }

    app.active_mode = Some(loaded.name.clone());
    app.needs_redraw = true;
    persist_active_mode(&loaded.name);
    Ok(summary)
}

/// Deactivate the mode layer: dials keep their current values, but the
/// mode's tool filters are cleared and the footer chip disappears.
pub fn clear(app: &mut App) -> ModeApplySummary {
    let name = app.active_mode.take();
    app.active_allowed_tools = None;
    app.active_blocked_tools = None;
    app.needs_redraw = true;
    persist_active_mode("");
    let mut summary = ModeApplySummary::default();
    summary.line(match name {
        Some(name) => format!("mode layer '{name}' cleared — dials keep their current values"),
        None => "no mode was active".to_string(),
    });
    summary
}

/// Persist the active mode choice so the next launch restores it. Empty
/// string clears the stored value; failures are non-fatal (the session
/// still runs, it just won't restore).
fn persist_active_mode(name: &str) {
    let mut settings = crate::settings::Settings::load().unwrap_or_default();
    let _ = settings.set("active_mode", name);
    if let Err(err) = settings.save() {
        tracing::warn!(error = %err, mode = name, "failed to persist active mode");
    }
}

/// Apply config-bound dials (provider, sandbox, approval, memory, feature
/// flags) to a loaded [`Config`] at startup, *before* the engine is
/// constructed. Live dials (app mode, thinking, tools, model, sub-agent
/// cap) are handled by [`apply`] on the `App`; this covers the half that
/// only the config can express.
pub fn apply_to_config(config: &mut crate::config::Config, definition: &ModeDefinitionToml) {
    if let Some(provider) = &definition.provider {
        config.provider = Some(provider.clone());
    }
    if let Some(policy) = &definition.approval_policy {
        config.approval_policy = Some(policy.clone());
    }
    if let Some(sandbox) = &definition.sandbox_mode {
        config.sandbox_mode = Some(sandbox.clone());
    }
    if let Some(cap) = definition.max_subagents {
        config.max_subagents = Some(cap);
    }
    if let Some(level) = &definition.memory_level
        && let Some(parsed) = MemoryLevel::from_setting(level)
    {
        let memory = config.memory.get_or_insert_with(Default::default);
        match parsed {
            MemoryLevel::Goldfish => {
                memory.enabled = Some(false);
                memory.kod_enabled = Some(false);
            }
            MemoryLevel::Notebook => {
                memory.enabled = Some(true);
                memory.kod_enabled = Some(false);
            }
            MemoryLevel::Elephant => {
                memory.enabled = Some(true);
                memory.kod_enabled = Some(true);
            }
        }
    }
    if let Some(features) = &definition.features
        && !features.is_empty()
    {
        config
            .features
            .get_or_insert_with(Default::default)
            .entries
            .extend(features.iter().map(|(k, v)| (k.clone(), *v)));
    }
}

/// Resolve the mode named by `config.mode` (if any) against the workspace
/// catalog and fold its config-bound dials in. Returns the resolved
/// definition so callers can also apply startup-only values (model,
/// sub-agent cap) that live outside `Config`. Unknown names log a warning
/// and disable the mode layer rather than failing the launch.
pub fn apply_config_mode(
    config: &mut crate::config::Config,
    workspace: &std::path::Path,
) -> Option<ModeDefinitionToml> {
    let name = config.mode.clone()?;
    let catalog = ModeCatalog::load(Some(workspace));
    let Some(loaded) = catalog.get_ci(&name) else {
        tracing::warn!(
            mode = %name,
            available = ?catalog.names(),
            "mode from config not found; ignoring"
        );
        config.mode = None;
        return None;
    };
    let definition = loaded.definition.clone();
    apply_to_config(config, &definition);
    Some(definition)
}

/// Restore the mode layer at startup, if one should be active.
///
/// Precedence: explicit `name` (from `--mode` or config.toml `mode = "..."`)
/// beats the persisted settings choice. A persisted name that no longer
/// resolves (e.g. a deleted project mode file) is dropped with a status
/// note rather than failing the launch.
pub fn restore_at_startup(app: &mut App, explicit: Option<&str>) {
    let chosen = explicit.map(str::to_string).or_else(|| {
        crate::settings::Settings::load()
            .ok()
            .and_then(|s| s.active_mode)
    });
    let Some(name) = chosen else {
        return;
    };
    if name.is_empty() {
        return;
    }
    if let Err(err) = apply(app, &name) {
        app.status_message = Some(format!("Mode '{name}' not restored: {err}"));
    }
}

/// Render the `/mode` listing: every mode with its source, description,
/// and active marker.
pub fn describe(catalog: &ModeCatalog, active: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Modes (switch with /mode <name>):");
    for mode in catalog.iter() {
        let marker = if Some(mode.name.as_str()) == active {
            "← active"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {:<12} [{:<8}] {} {}",
            mode.name,
            mode.source.label(),
            mode.definition.description.as_deref().unwrap_or(""),
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
        "Layers: built-in < ~/.codesmith/modes/ < <workspace>/.codesmith/modes/ (later wins)."
    );
    let _ = writeln!(
        out,
        "Share a mode by committing its .toml file; /mode export writes one."
    );
    out
}

/// Export the app's current dials as a mode file under
/// `<workspace>/.codesmith/modes/<name>.toml`. Refuses to overwrite a
/// built-in name unless `force` is set.
pub fn export(app: &App, name: Option<&str>, force: bool) -> Result<String> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    let Some(name) = name else {
        return Err(anyhow::anyhow!(
            "Usage: /mode export <name> — pick a (non-built-in) name for the new mode"
        ));
    };
    if !force {
        let catalog = catalog_for(app);
        if let Some(existing) = catalog.get_ci(name)
            && existing.source == ModeSource::BuiltIn
        {
            bail!(
                "'{name}' is a built-in mode; choose another name or use /mode export! {name} to override it locally"
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
    let definition = ModeDefinitionToml {
        name: Some(name.to_string()),
        description: Some("Exported from the current session via /mode export.".to_string()),
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
            (allowed, blocked) => Some(ModeToolsToml {
                include: allowed.clone(),
                exclude: blocked.clone(),
            }),
        },
        features: None,
    };

    let dir = app.workspace.join(".codesmith").join("modes");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{name}.toml"));
    let body = toml::to_string_pretty(&definition).with_context(|| "serializing exported mode")?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(format!("Exported mode '{name}' to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, TuiOptions};
    use std::path::PathBuf;

    /// Redirects config/settings persistence into a temp home so `apply()`
    /// (which persists the active mode to settings.toml) never touches the
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
    fn apply_unknown_mode_reports_available() {
        let _env = TestEnv::new();
        let mut app = test_app();
        let err = apply(&mut app, "does-not-exist").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown mode"), "got: {msg}");
        assert!(msg.contains("minimal"), "should list built-ins: {msg}");
        assert!(app.active_mode.is_none());
    }

    #[test]
    fn apply_minimal_sets_live_dials() {
        let _env = TestEnv::new();
        let mut app = test_app();
        app.reasoning_effort = ReasoningEffort::Max;
        app.max_subagents = 8;

        let summary = apply(&mut app, "minimal").unwrap();
        assert_eq!(app.active_mode.as_deref(), Some("minimal"));
        assert_eq!(app.reasoning_effort, ReasoningEffort::Off);
        assert_eq!(app.max_subagents, 0);
        assert!(!app.use_memory);
        let allowed = app.active_allowed_tools.clone().unwrap();
        assert!(allowed.contains(&"read_file".to_string()));
        assert!(!allowed.contains(&"web_search".to_string()));
        assert!(summary.mode_name.as_deref() == Some("minimal"));
    }

    #[test]
    fn apply_maximal_turns_memory_and_subagents_on() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "maximal").unwrap();
        assert!(app.use_memory);
        assert!(app.kod_enabled);
        assert_eq!(app.max_subagents, 20);
        assert_eq!(app.reasoning_effort, ReasoningEffort::Max);
        // No tool restriction in maximal.
        assert!(app.active_allowed_tools.is_none());
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
    fn apply_balanced_is_pure_identity() {
        let _env = TestEnv::new();
        let mut app = test_app();
        app.max_subagents = 3;
        apply(&mut app, "balanced").unwrap();
        assert_eq!(app.max_subagents, 3);
        assert!(app.active_allowed_tools.is_none());
        assert_eq!(app.active_mode.as_deref(), Some("balanced"));
    }

    #[test]
    fn clear_removes_layer_and_filters() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "minimal").unwrap();
        let summary = clear(&mut app);
        assert!(app.active_mode.is_none());
        assert!(app.active_allowed_tools.is_none());
        assert!(app.active_blocked_tools.is_none());
        assert!(summary.render().contains("cleared"));
    }

    #[test]
    fn apply_is_case_insensitive() {
        let _env = TestEnv::new();
        let mut app = test_app();
        apply(&mut app, "Minimal").unwrap();
        assert_eq!(app.active_mode.as_deref(), Some("minimal"));
    }

    #[test]
    fn describe_marks_active_and_lists_sources() {
        let catalog = ModeCatalog::load_from(None, None);
        let text = describe(&catalog, Some("minimal"));
        assert!(text.contains("← active"));
        assert!(text.contains("built-in"));
        assert!(text.contains("/mode export"));
    }

    #[test]
    fn export_writes_project_mode_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.workspace = tmp.path().to_path_buf();
        app.reasoning_effort = ReasoningEffort::High;
        app.max_subagents = 4;

        let msg = export(&app, Some("my-mode"), false).unwrap();
        let path = tmp.path().join(".codesmith/modes/my-mode.toml");
        assert!(path.exists(), "{msg}");

        let body = std::fs::read_to_string(&path).unwrap();
        let parsed = ModeDefinitionToml::parse_toml(&body, "x").unwrap();
        assert_eq!(parsed.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(parsed.max_subagents, Some(4));

        // The exported file is loadable as a project mode.
        let catalog = ModeCatalog::load_from(None, Some(&tmp.path().join(".codesmith/modes")));
        assert!(catalog.get("my-mode").is_some(), "{:?}", catalog.names());
    }

    #[test]
    fn export_refuses_builtin_names_without_force() {
        // Temp workspace: the forced export below must not write into the
        // source tree.
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.workspace = tmp.path().to_path_buf();
        let err = export(&app, Some("minimal"), false).unwrap_err();
        assert!(err.to_string().contains("built-in"));
        assert!(export(&app, Some("minimal"), true).is_ok());
        assert!(tmp.path().join(".codesmith/modes/minimal.toml").exists());
    }
}
