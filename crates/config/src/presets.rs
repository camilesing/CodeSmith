//! Layered configuration presets — shareable bundles of agent dials.
//!
//! A *preset* is a single TOML file that provides baseline values for a
//! curated subset of the runtime configuration: which tools are offered,
//! how deeply the model thinks, how much memory persists across sessions,
//! how many sub-agents may run, and a set of resource-bearing switches
//! (code index, LSP diagnostics, snapshots, memory injection, background
//! checks). Every field is optional — a preset is a *baseline* under the
//! user's existing config, not a replacement for it: values are only
//! filled in for keys the user left unset, so explicit configuration
//! always wins (`preset = "simple"` plus `[lsp] enabled = true` keeps LSP
//! on).
//!
//! The four progressive tiers — `simple`, `middle` (the factory default),
//! `all`, `experiment` — are built into the binary. When the user's
//! explicit config deviates from the selected tier on any governed key,
//! the *effective* preset is reported as `diy` (a derived state; it
//! cannot be selected). `plan` remains a built-in workflow preset.
//!
//! Resolution order for preset *definitions* (later layers win on name
//! collisions):
//!
//! 1. built-in presets compiled into the binary (`simple`, `middle`,
//!    `all`, `experiment`, `plan`)
//! 2. user presets at `~/.codesmith/presets/*.toml`
//!    (legacy `~/.codesmith/modes/*.toml` still scanned, with a warning)
//! 3. project presets at `<workspace>/.codesmith/presets/*.toml`
//!    (legacy `modes/` likewise)
//!
//! The *active* preset is chosen by `--preset <name>` on the CLI, else the
//! `preset` key in `config.toml` (the legacy `mode` key still works as a
//! deprecated alias), else `middle`, else the last preset selected in the
//! TUI (persisted in `settings.toml`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Built-in preset definitions, compiled in so the first-run experience
/// has working tiers before the user writes any file.
pub const BUILTIN_SIMPLE_TOML: &str = include_str!("presets/simple.toml");
pub const BUILTIN_MIDDLE_TOML: &str = include_str!("presets/middle.toml");
pub const BUILTIN_ALL_TOML: &str = include_str!("presets/all.toml");
pub const BUILTIN_EXPERIMENT_TOML: &str = include_str!("presets/experiment.toml");
pub const BUILTIN_PLAN_TOML: &str = include_str!("presets/plan.toml");

/// Directory scanned for user/project preset files (relative to the
/// codesmith home and the workspace root respectively).
pub const PRESETS_DIR_NAME: &str = "presets";
/// Pre-rename directory still scanned for compatibility, with a warning.
pub const LEGACY_MODES_DIR_NAME: &str = "modes";

/// The four progressive tiers, in order. Used by the matrix renderer and
/// the monotonicity tests; `plan` is intentionally excluded (it is a
/// workflow preset, not a tier).
pub const TIER_NAMES: &[&str] = &["simple", "middle", "all", "experiment"];

/// Old mode names accepted as deprecated aliases of the tier names.
pub const DEPRECATED_PRESET_ALIASES: &[(&str, &str)] = &[
    ("minimal", "simple"),
    ("balanced", "middle"),
    ("maximal", "all"),
];

/// Canonicalize a user-supplied preset name: trims, maps deprecated mode
/// aliases onto the tier names (returning a deprecation warning), and
/// rejects `diy` — it is a derived state, not a selectable preset.
/// Unknown names pass through unchanged: they may name a user/project
/// preset file, which the catalog resolves (and warns about) later.
pub fn canonicalize_preset_name(raw: &str) -> Result<(String, Option<String>)> {
    let name = raw.trim();
    let lowered = name.to_ascii_lowercase();
    if lowered == "diy" {
        bail!(
            "'diy' is a derived state, not a preset: it is shown when your \
             explicit config deviates from the selected tier. Pick simple | \
             middle | all | experiment (or a custom preset name)."
        );
    }
    if let Some((from, to)) = DEPRECATED_PRESET_ALIASES
        .iter()
        .find(|(from, _)| *from == lowered)
    {
        return Ok((
            to.to_string(),
            Some(format!(
                "preset name '{from}' is deprecated and mapped to '{to}'; \
                 rename it in your config to silence this warning"
            )),
        ));
    }
    Ok((name.to_string(), None))
}

/// Canonical memory dials (M2). These map onto the existing multi-layer
/// memory system rather than introducing a new one:
///
/// - `goldfish` — no cross-session memory is loaded or written.
/// - `notebook` — only what the user explicitly asks to remember
///   (`# note` quick-adds, the `remember` tool); nothing is learned
///   automatically.
/// - `elephant` — full auto-memory: user memory file plus Knowledge On
///   Demand with budget/decay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryLevel {
    Goldfish,
    Notebook,
    #[default]
    Elephant,
}

impl MemoryLevel {
    #[must_use]
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "goldfish" | "off" | "none" | "disabled" => Some(Self::Goldfish),
            "notebook" | "manual" => Some(Self::Notebook),
            "elephant" | "auto" | "full" => Some(Self::Elephant),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Goldfish => "goldfish",
            Self::Notebook => "notebook",
            Self::Elephant => "elephant",
        }
    }

    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Goldfish => "no cross-session memory",
            Self::Notebook => "only explicitly saved notes",
            Self::Elephant => "auto memory with budget and decay",
        }
    }
}

/// Tool surface dials for a preset.
///
/// `include` is an allowlist — when set, only those tools are offered to
/// the model. `exclude` is a denylist applied after `include`, so a preset
/// can say "the default surface minus web tools" without enumerating the
/// whole catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetToolsToml {
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
}

/// One preset definition. All dials optional; absent fields inherit.
///
/// The `*_enabled`/`*_check`/`*_audit` bools are the *governed switches*:
/// resource-bearing config keys a tier may baseline. They are applied
/// fill-if-unset (explicit user config wins) and participating in the
/// `diy` deviation check.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresetDefinitionToml {
    /// Display name. Defaults to the file stem for file-loaded presets;
    /// required (implicitly) for built-ins.
    #[serde(default)]
    pub name: Option<String>,
    /// One-line description shown in `/preset` listings.
    #[serde(default)]
    pub description: Option<String>,
    /// Runtime app mode: `"agent" | "yolo" | "plan" | "coordinator"`.
    #[serde(default)]
    pub app_mode: Option<String>,
    /// Thinking tier: `"off" | "low" | "medium" | "high" | "max" | "auto"`.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Approval posture: `"suggest" | "auto" | "never"`.
    #[serde(default)]
    pub approval_policy: Option<String>,
    /// Sandbox policy, same vocabulary as `sandbox_mode` in config.toml:
    /// `"read-only" | "workspace-write" | "danger-full-access"`.
    #[serde(default)]
    pub sandbox_mode: Option<String>,
    /// Memory dial: `"goldfish" | "notebook" | "elephant"`.
    #[serde(default)]
    pub memory_level: Option<String>,
    /// Cap on concurrent sub-agents (`0` disables sub-agents).
    #[serde(default)]
    pub max_subagents: Option<usize>,
    /// Model override (id as accepted by the active provider).
    #[serde(default)]
    pub model: Option<String>,
    /// Provider override. Applies at startup; switching providers mid-
    /// session requires a restart because the LLM client is resolved once.
    #[serde(default)]
    pub provider: Option<String>,
    /// Tool surface dials.
    #[serde(default)]
    pub tools: Option<PresetToolsToml>,
    /// Feature-flag baselines keyed like `[features]` in config.toml
    /// (e.g. `subagents = false`).
    #[serde(default)]
    pub features: Option<BTreeMap<String, bool>>,
    /// `[index] enabled` — persistent code index (local SQLite compute).
    #[serde(default)]
    pub index_enabled: Option<bool>,
    /// `[lsp] enabled` — post-edit LSP diagnostics injection.
    #[serde(default)]
    pub lsp_enabled: Option<bool>,
    /// `[lsp] include_warnings` — surface warnings in addition to errors.
    #[serde(default)]
    pub lsp_include_warnings: Option<bool>,
    /// `[snapshots] enabled` — workspace side-git snapshots (disk/IO).
    #[serde(default)]
    pub snapshots_enabled: Option<bool>,
    /// `[memory] enabled` — user-memory prompt injection.
    #[serde(default)]
    pub memory_enabled: Option<bool>,
    /// `[memory] kod_enabled` — directory-based Knowledge On Demand.
    #[serde(default)]
    pub memory_kod_enabled: Option<bool>,
    /// `[context] enabled` — layered context manager with Flash seams.
    #[serde(default)]
    pub context_enabled: Option<bool>,
    /// `[context] project_pack` — deterministic project context pack in
    /// the stable prompt prefix.
    #[serde(default)]
    pub context_project_pack: Option<bool>,
    /// `[capacity] enabled` — runtime capacity controller (can rewrite
    /// the live transcript).
    #[serde(default)]
    pub capacity_enabled: Option<bool>,
    /// `[auto] cost_saving` — `--model auto` router prefers the cheap
    /// model for ambiguous requests.
    #[serde(default)]
    pub auto_cost_saving: Option<bool>,
    /// `[update] check_for_updates` — startup background update check.
    #[serde(default)]
    pub update_check: Option<bool>,
    /// `[network] audit` — one audit-log line per outbound network call.
    #[serde(default)]
    pub network_audit: Option<bool>,
    /// `strict_tool_mode` — DeepSeek beta strict tool schemas.
    #[serde(default)]
    pub strict_tool_mode: Option<bool>,
}

impl PresetDefinitionToml {
    /// The governed bool switches in stable display order, with the
    /// config.toml key each one baselines. Used by the matrix renderer,
    /// the deviation check, and the monotonicity tests.
    #[must_use]
    pub fn bool_dials(&self) -> Vec<(&'static str, Option<bool>)> {
        vec![
            ("[index].enabled", self.index_enabled),
            ("[lsp].enabled", self.lsp_enabled),
            ("[lsp].include_warnings", self.lsp_include_warnings),
            ("[snapshots].enabled", self.snapshots_enabled),
            ("[memory].enabled", self.memory_enabled),
            ("[memory].kod_enabled", self.memory_kod_enabled),
            ("[context].project_pack", self.context_project_pack),
            ("[context].enabled", self.context_enabled),
            ("[capacity].enabled", self.capacity_enabled),
            ("[auto].cost_saving", self.auto_cost_saving),
            ("[update].check_for_updates", self.update_check),
            ("[network].audit", self.network_audit),
            ("strict_tool_mode", self.strict_tool_mode),
        ]
    }

    /// Validate enum-ish fields so typos surface at load time with an
    /// actionable message instead of silently falling back later.
    pub fn validate(&self) -> Result<()> {
        if let Some(app_mode) = &self.app_mode
            && !matches!(
                app_mode.trim().to_ascii_lowercase().as_str(),
                "agent" | "yolo" | "plan" | "coordinator"
            )
        {
            bail!("invalid app_mode '{app_mode}' (expected agent | yolo | plan | coordinator)");
        }
        if let Some(effort) = &self.reasoning_effort
            && !matches!(
                effort.trim().to_ascii_lowercase().as_str(),
                "off" | "low" | "medium" | "high" | "max" | "auto"
            )
        {
            bail!(
                "invalid reasoning_effort '{effort}' (expected off | low | medium | high | max | auto)"
            );
        }
        if let Some(policy) = &self.approval_policy
            && !matches!(
                policy.trim().to_ascii_lowercase().as_str(),
                "suggest" | "suggested" | "on-request" | "untrusted" | "auto" | "never"
            )
        {
            bail!("invalid approval_policy '{policy}' (expected suggest | auto | never)");
        }
        if let Some(level) = &self.memory_level
            && MemoryLevel::from_setting(level).is_none()
        {
            bail!("invalid memory_level '{level}' (expected goldfish | notebook | elephant)");
        }
        if let Some(tools) = &self.tools {
            for list in [&tools.include, &tools.exclude].into_iter().flatten() {
                if list.iter().any(|name| name.trim().is_empty()) {
                    bail!("tools lists must not contain empty names");
                }
            }
        }
        Ok(())
    }

    /// Parse + validate a preset file body. `fallback_name` (typically
    /// the file stem) is used when the file has no `name` field.
    pub fn parse_toml(source: &str, fallback_name: &str) -> Result<Self> {
        let mut definition: Self = toml::from_str(source)
            .map_err(|err| anyhow::anyhow!("preset '{fallback_name}': {err}"))?;
        definition.validate()?;
        if definition.name.as_deref().is_none_or(str::is_empty) {
            definition.name = Some(fallback_name.to_string());
        }
        Ok(definition)
    }
}

/// Which layer a preset definition was loaded from. Project presets
/// override user presets, which override built-ins (by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetSource {
    BuiltIn,
    User,
    Project,
}

impl PresetSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// A preset definition plus its resolved name and origin.
#[derive(Debug, Clone)]
pub struct LoadedPreset {
    pub name: String,
    pub definition: PresetDefinitionToml,
    pub source: PresetSource,
}

/// All presets visible to a workspace: built-ins, then
/// `~/.codesmith/presets/`, then `<workspace>/.codesmith/presets/`
/// (legacy `modes/` directories are still scanned, with a migration
/// warning), deduplicated by name with later layers winning. Invalid
/// files are skipped and reported in `warnings` so one broken community
/// file cannot brick startup.
#[derive(Debug, Clone, Default)]
pub struct PresetCatalog {
    presets: BTreeMap<String, LoadedPreset>,
    /// Non-fatal load problems (e.g. a malformed user preset file).
    pub warnings: Vec<String>,
}

impl PresetCatalog {
    /// Load the catalog for a workspace using the default locations.
    pub fn load(workspace: Option<&Path>) -> Self {
        let Ok(home) = crate::codesmith_home() else {
            return Self::load_from(None, None);
        };
        let user_dir = home.join(PRESETS_DIR_NAME);
        let legacy_user_dir = home.join(LEGACY_MODES_DIR_NAME);
        let (project_dir, legacy_project_dir) = match workspace {
            Some(ws) => (
                Some(ws.join(".codesmith").join(PRESETS_DIR_NAME)),
                Some(ws.join(".codesmith").join(LEGACY_MODES_DIR_NAME)),
            ),
            None => (None, None),
        };
        Self::load_from_full(
            Some(&legacy_user_dir),
            Some(&user_dir),
            legacy_project_dir.as_deref(),
            project_dir.as_deref(),
        )
    }

    /// Testable core: built-ins, then `user_dir`, then `project_dir`.
    pub fn load_from(user_dir: Option<&Path>, project_dir: Option<&Path>) -> Self {
        Self::load_from_full(None, user_dir, None, project_dir)
    }

    /// Full loader with legacy `modes/` directories ranked *below* the
    /// new `presets/` directory of the same layer.
    pub fn load_from_full(
        legacy_user_dir: Option<&Path>,
        user_dir: Option<&Path>,
        legacy_project_dir: Option<&Path>,
        project_dir: Option<&Path>,
    ) -> Self {
        let mut catalog = Self::default();
        catalog.insert_builtin(BUILTIN_SIMPLE_TOML, "simple");
        catalog.insert_builtin(BUILTIN_MIDDLE_TOML, "middle");
        catalog.insert_builtin(BUILTIN_ALL_TOML, "all");
        catalog.insert_builtin(BUILTIN_EXPERIMENT_TOML, "experiment");
        catalog.insert_builtin(BUILTIN_PLAN_TOML, "plan");

        if let Some(dir) = legacy_user_dir {
            catalog.insert_dir(dir, PresetSource::User, Some("user"));
        }
        if let Some(dir) = user_dir {
            catalog.insert_dir(dir, PresetSource::User, None);
        }
        if let Some(dir) = legacy_project_dir {
            catalog.insert_dir(dir, PresetSource::Project, Some("project"));
        }
        if let Some(dir) = project_dir {
            catalog.insert_dir(dir, PresetSource::Project, None);
        }
        catalog
    }

    fn insert_builtin(&mut self, source: &str, fallback_name: &str) {
        match PresetDefinitionToml::parse_toml(source, fallback_name) {
            Ok(definition) => {
                let name = definition
                    .name
                    .clone()
                    .unwrap_or_else(|| fallback_name.to_string());
                self.presets.insert(
                    name.clone(),
                    LoadedPreset {
                        name,
                        definition,
                        source: PresetSource::BuiltIn,
                    },
                );
            }
            Err(err) => self.warnings.push(format!("built-in preset: {err}")),
        }
    }

    fn insert_dir(&mut self, dir: &Path, source: PresetSource, legacy: Option<&str>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return, // missing directory is the common case
        };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
            })
            .collect();
        files.sort(); // deterministic ordering for warnings and overrides
        if files.is_empty() {
            return;
        }
        if let Some(layer) = legacy {
            self.warnings.push(format!(
                "legacy {} modes/ directory found with {} preset file(s); \
                 move them to {}/ to silence this warning",
                layer,
                files.len(),
                PRESETS_DIR_NAME
            ));
        }
        for path in files {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "preset".to_string());
            let body = match std::fs::read_to_string(&path) {
                Ok(body) => body,
                Err(err) => {
                    self.warnings
                        .push(format!("{}: unreadable ({err})", path.display()));
                    continue;
                }
            };
            match PresetDefinitionToml::parse_toml(&body, &stem) {
                Ok(definition) => {
                    let name = definition.name.clone().unwrap_or(stem);
                    self.presets.insert(
                        name.clone(),
                        LoadedPreset {
                            name,
                            definition,
                            source,
                        },
                    );
                }
                Err(err) => self.warnings.push(format!("{}: {err}", path.display())),
            }
        }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&LoadedPreset> {
        self.presets.get(name)
    }

    /// Case-insensitive lookup so `/preset Simple` works like
    /// `/preset simple`.
    #[must_use]
    pub fn get_ci(&self, name: &str) -> Option<&LoadedPreset> {
        if let Some(hit) = self.presets.get(name) {
            return Some(hit);
        }
        let lowered = name.trim().to_ascii_lowercase();
        self.presets
            .values()
            .find(|preset| preset.name.to_ascii_lowercase() == lowered)
    }

    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.presets.keys().map(String::as_str).collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LoadedPreset> {
        self.presets.values()
    }
}

/// The governed-switch matrix across the four tiers: one row per
/// `bool_dials()` key, columns ordered as [`TIER_NAMES`]. `None` means
/// the tier has no opinion (the key is not governed). Used by
/// `preset show` and the monotonicity tests.
#[must_use]
pub fn builtin_tier_matrix() -> Vec<(&'static str, [Option<bool>; 4])> {
    let catalog = PresetCatalog::load_from(None, None);
    let tiers: Vec<Vec<Option<bool>>> = TIER_NAMES
        .iter()
        .map(|name| {
            catalog
                .get(name)
                .map(|loaded| {
                    loaded
                        .definition
                        .bool_dials()
                        .into_iter()
                        .map(|(_, value)| value)
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    catalog
        .get(TIER_NAMES[0])
        .map(|loaded| loaded.definition.clone())
        .unwrap_or_default()
        .bool_dials()
        .into_iter()
        .enumerate()
        .map(|(i, (key, _))| (key, [tiers[0][i], tiers[1][i], tiers[2][i], tiers[3][i]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_parse_and_validate() {
        let catalog = PresetCatalog::load_from(None, None);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        for name in ["simple", "middle", "all", "experiment", "plan"] {
            assert!(catalog.get(name).is_some(), "missing built-in {name}");
        }
    }

    #[test]
    fn tier_dials_are_expected() {
        let catalog = PresetCatalog::load_from(None, None);
        let simple = &catalog.get("simple").unwrap().definition;
        assert_eq!(simple.reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(simple.memory_level.as_deref(), Some("goldfish"));
        assert_eq!(simple.max_subagents, Some(0));
        assert!(simple.tools.as_ref().unwrap().include.is_some());
        assert_eq!(simple.index_enabled, Some(false));
        assert_eq!(simple.update_check, Some(false));

        let middle = &catalog.get("middle").unwrap().definition;
        assert_eq!(middle.app_mode, None);
        assert_eq!(middle.tools, None);
        assert_eq!(middle.index_enabled, Some(true));
        // Quality-first default: the strong brain classifies; cost_saving is
        // the opt-in cheap router.
        assert_eq!(middle.auto_cost_saving, Some(false));
        assert_eq!(middle.lsp_include_warnings, Some(false));
        // The router posture is catalog-wide, not middle-specific: every
        // tier that carries the dial pins it off; only `plan` omits it.
        for id in ["simple", "all", "experiment"] {
            assert_eq!(
                catalog.get(id).unwrap().definition.auto_cost_saving,
                Some(false),
                "preset {id} must keep the quality-first router default"
            );
        }
        assert_eq!(
            catalog.get("plan").unwrap().definition.auto_cost_saving,
            None
        );

        let all = &catalog.get("all").unwrap().definition;
        // Notebook (not elephant): KOD arrives only with experiment.
        assert_eq!(all.memory_level.as_deref(), Some("notebook"));
        assert_eq!(all.max_subagents, Some(20));
        assert_eq!(all.lsp_include_warnings, Some(true));
        assert_eq!(all.capacity_enabled, Some(false));

        let experiment = &catalog.get("experiment").unwrap().definition;
        assert_eq!(experiment.capacity_enabled, Some(true));
        assert_eq!(experiment.strict_tool_mode, Some(true));
        assert_eq!(
            experiment.features.as_ref().unwrap().get("vision_model"),
            Some(&true)
        );

        let plan = &catalog.get("plan").unwrap().definition;
        assert_eq!(plan.app_mode.as_deref(), Some("plan"));
    }

    #[test]
    fn governed_switch_matrix_is_monotonic() {
        // Every governed key must be non-decreasing along
        // simple → middle → all → experiment: once a tier turns a switch
        // on, deeper tiers may not turn it off.
        for (key, row) in builtin_tier_matrix() {
            let mut seen_on = false;
            for value in row {
                let value = value.unwrap_or(true);
                if seen_on {
                    assert!(value, "tier matrix regresses on {key}: {row:?}");
                }
                seen_on |= value;
            }
        }
    }

    #[test]
    fn feature_flags_are_monotonic_across_tiers() {
        let catalog = PresetCatalog::load_from(None, None);
        let tiers: Vec<_> = TIER_NAMES
            .iter()
            .map(|name| catalog.get(name).unwrap().definition.clone())
            .collect();
        let mut keys: Vec<&String> = tiers[0].features.as_ref().unwrap().keys().collect();
        keys.sort();
        for key in keys {
            let mut seen_on = false;
            for tier in &tiers {
                let value = tier.features.as_ref().unwrap().get(key).copied();
                // Tiers either spell the key out (Some) or defer; all four
                // built-ins spell every feature key out.
                let Some(value) = value else {
                    panic!(
                        "tier {} does not govern feature {key}",
                        tier.name.clone().unwrap_or_default()
                    );
                };
                if seen_on {
                    assert!(value, "feature {key} regresses across tiers");
                }
                seen_on |= value;
            }
        }
    }

    #[test]
    fn every_tier_spells_out_every_governed_switch() {
        // Totality: a tier that omits a governed key silently widens the
        // diy surface; keep the matrix explicit.
        let catalog = PresetCatalog::load_from(None, None);
        for name in TIER_NAMES {
            let definition = &catalog.get(name).unwrap().definition;
            for (key, value) in definition.bool_dials() {
                assert!(value.is_some(), "tier {name} omits governed key {key}");
            }
        }
    }

    #[test]
    fn simple_tool_allowlist_only_names_real_tools() {
        let catalog = PresetCatalog::load_from(None, None);
        let include = catalog
            .get("simple")
            .unwrap()
            .definition
            .tools
            .as_ref()
            .unwrap()
            .include
            .clone()
            .unwrap();
        // Cross-check against the default active native tool names so the
        // built-in stays valid when the registry evolves.
        let known = codesmith_agent_runtime_tool_names();
        for name in &include {
            assert!(
                known.contains(&name.as_str()),
                "simple preset includes unknown tool '{name}'"
            );
        }
    }

    /// Mirror of the engine's default-active tool list for the cross-check
    /// above. Kept local to the test so the config crate does not depend on
    /// agent-runtime.
    fn codesmith_agent_runtime_tool_names() -> Vec<&'static str> {
        vec![
            "agent_open",
            "apply_patch",
            "checklist_write",
            "edit_file",
            "exec_interact",
            "exec_shell",
            "exec_shell_interact",
            "exec_shell_wait",
            "exec_wait",
            "fetch_url",
            "file_search",
            "find_references",
            "git_diff",
            "git_status",
            "grep_files",
            "list_dir",
            "read_file",
            "run_tests",
            "symbol_search",
            "task_create",
            "task_list",
            "task_read",
            "task_shell_start",
            "task_shell_wait",
            "update_plan",
            "web_search",
            "write_file",
        ]
    }

    #[test]
    fn parse_rejects_invalid_enums() {
        let bad = PresetDefinitionToml {
            app_mode: Some("ninja".into()),
            ..PresetDefinitionToml::default()
        };
        assert!(bad.validate().is_err());

        let bad = PresetDefinitionToml {
            reasoning_effort: Some("ultra".into()),
            ..PresetDefinitionToml::default()
        };
        assert!(bad.validate().is_err());

        let bad = PresetDefinitionToml {
            memory_level: Some("whale".into()),
            ..PresetDefinitionToml::default()
        };
        assert!(bad.validate().is_err());

        let bad = PresetDefinitionToml {
            approval_policy: Some("maybe".into()),
            ..PresetDefinitionToml::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn parse_accepts_full_shape() {
        let source = r#"
name = "review"
description = "Read-only code review posture"
app_mode = "agent"
reasoning_effort = "high"
approval_policy = "never"
sandbox_mode = "read-only"
memory_level = "notebook"
max_subagents = 2
model = "deepseek-v4-pro"
index_enabled = false
lsp_enabled = false

[tools]
include = ["read_file", "grep_files", "list_dir"]
exclude = ["exec_shell"]

[features]
subagents = false
web_search = false
"#;
        let preset = PresetDefinitionToml::parse_toml(source, "fallback").unwrap();
        assert_eq!(preset.name.as_deref(), Some("review"));
        assert_eq!(preset.max_subagents, Some(2));
        assert_eq!(preset.index_enabled, Some(false));
        assert_eq!(preset.lsp_enabled, Some(false));
        let tools = preset.tools.unwrap();
        assert_eq!(
            tools.include.unwrap(),
            vec!["read_file", "grep_files", "list_dir"]
        );
        assert_eq!(tools.exclude.unwrap(), vec!["exec_shell"]);
        assert_eq!(preset.features.unwrap().get("subagents"), Some(&false));
    }

    #[test]
    fn parse_defaults_name_to_fallback() {
        let preset =
            PresetDefinitionToml::parse_toml("reasoning_effort = \"off\"\n", "my-preset").unwrap();
        assert_eq!(preset.name.as_deref(), Some("my-preset"));
    }

    #[test]
    fn deprecated_aliases_canonicalize_with_warning() {
        let (name, warning) = canonicalize_preset_name("minimal").unwrap();
        assert_eq!(name, "simple");
        assert!(warning.unwrap().contains("deprecated"));

        let (name, warning) = canonicalize_preset_name("Balanced").unwrap();
        assert_eq!(name, "middle");
        assert!(warning.is_some());

        let (name, _) = canonicalize_preset_name("maximal").unwrap();
        assert_eq!(name, "all");

        let (name, warning) = canonicalize_preset_name("simple").unwrap();
        assert_eq!(name, "simple");
        assert!(warning.is_none());

        // Unknown names pass through: they may be user preset files.
        let (name, _) = canonicalize_preset_name("  focus  ").unwrap();
        assert_eq!(name, "focus");
    }

    #[test]
    fn diy_is_rejected_as_a_selection() {
        assert!(canonicalize_preset_name("diy").is_err());
        assert!(canonicalize_preset_name("DIY").is_err());
    }

    #[test]
    fn project_overrides_user_over_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user-presets");
        let project = tmp.path().join("project-presets");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&project).unwrap();

        // Same name at every layer; project must win.
        std::fs::write(user.join("simple.toml"), "description = \"user flavor\"\n").unwrap();
        std::fs::write(
            project.join("simple.toml"),
            "description = \"project flavor\"\n",
        )
        .unwrap();
        // A user-only preset survives.
        std::fs::write(
            user.join("focus.toml"),
            "description = \"focus\"\nreasoning_effort = \"high\"\n",
        )
        .unwrap();
        // A broken file is skipped with a warning, not fatal.
        std::fs::write(user.join("broken.toml"), "reasoning_effort = \"???\"\n").unwrap();

        let catalog = PresetCatalog::load_from(Some(&user), Some(&project));
        assert_eq!(
            catalog.get("simple").unwrap().definition.description,
            Some("project flavor".to_string())
        );
        assert_eq!(catalog.get("simple").unwrap().source, PresetSource::Project);
        assert_eq!(
            catalog.get("focus").unwrap().source,
            PresetSource::User,
            "user-only preset should survive"
        );
        assert!(
            catalog.warnings.iter().any(|w| w.contains("broken")),
            "broken file should warn: {:?}",
            catalog.warnings
        );
    }

    #[test]
    fn legacy_modes_directory_is_scanned_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy-modes");
        let fresh = tmp.path().join("fresh-presets");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();

        std::fs::write(
            legacy.join("oldie.toml"),
            "description = \"from the modes era\"\n",
        )
        .unwrap();
        // Same name in both directories: the new one must win.
        std::fs::write(legacy.join("simple.toml"), "description = \"legacy\"\n").unwrap();
        std::fs::write(fresh.join("simple.toml"), "description = \"fresh\"\n").unwrap();

        let catalog = PresetCatalog::load_from_full(Some(&legacy), Some(&fresh), None, None);
        assert_eq!(
            catalog.get("oldie").unwrap().definition.description,
            Some("from the modes era".to_string())
        );
        assert_eq!(
            catalog.get("simple").unwrap().definition.description,
            Some("fresh".to_string())
        );
        assert!(
            catalog
                .warnings
                .iter()
                .any(|w| w.contains("legacy") && w.contains("modes")),
            "legacy dir should warn: {:?}",
            catalog.warnings
        );
    }

    #[test]
    fn case_insensitive_lookup() {
        let catalog = PresetCatalog::load_from(None, None);
        assert!(catalog.get_ci("Simple").is_some());
        assert!(catalog.get_ci("  EXPERIMENT ").is_some());
        assert!(catalog.get_ci("nope").is_none());
    }

    #[test]
    fn memory_level_parsing() {
        assert_eq!(
            MemoryLevel::from_setting("goldfish"),
            Some(MemoryLevel::Goldfish)
        );
        assert_eq!(
            MemoryLevel::from_setting("NOTEBOOK"),
            Some(MemoryLevel::Notebook)
        );
        assert_eq!(
            MemoryLevel::from_setting("elephant"),
            Some(MemoryLevel::Elephant)
        );
        assert_eq!(MemoryLevel::from_setting("whale"), None);
        assert_eq!(MemoryLevel::Elephant.as_setting(), "elephant");
    }
}
