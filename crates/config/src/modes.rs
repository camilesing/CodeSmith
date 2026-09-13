//! Named runtime modes — shareable bundles of agent dials.
//!
//! A *mode* is a single TOML file that overrides a curated subset of the
//! runtime configuration: which tools are offered, how deeply the model
//! thinks, how much memory persists across sessions, how aggressive the
//! approval/sandbox posture is, and which model answers. Every field is
//! optional — a mode is a *delta* over the user's existing config, not a
//! replacement for it. Switching modes (`/mode <name>` or `--mode <name>`)
//! applies the delta to the live session; anything the mode leaves unset
//! keeps its current value.
//!
//! Resolution order for mode *definitions* (later layers win on name
//! collisions):
//!
//! 1. built-in modes compiled into the binary (`minimal`, `balanced`,
//!    `maximal`, `plan`)
//! 2. user modes at `~/.codesmith/modes/*.toml`
//! 3. project modes at `<workspace>/.codesmith/modes/*.toml`
//!
//! The *active* mode is chosen by `--mode <name>` on the CLI, else the
//! `mode` key in `config.toml`, else the last mode selected in the TUI
//! (persisted in `settings.toml`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Built-in mode definitions, compiled in so the first-run experience has
/// working presets before the user writes any file.
pub const BUILTIN_MINIMAL_TOML: &str = include_str!("modes/minimal.toml");
pub const BUILTIN_BALANCED_TOML: &str = include_str!("modes/balanced.toml");
pub const BUILTIN_MAXIMAL_TOML: &str = include_str!("modes/maximal.toml");
pub const BUILTIN_PLAN_TOML: &str = include_str!("modes/plan.toml");

/// Directory names scanned for user/project mode files (relative to the
/// codesmith home and the workspace root respectively).
pub const MODES_DIR_NAME: &str = "modes";

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

/// Tool surface dials for a mode.
///
/// `include` is an allowlist — when set, only those tools are offered to
/// the model. `exclude` is a denylist applied after `include`, so a mode
/// can say "the default surface minus web tools" without enumerating the
/// whole catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeToolsToml {
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
}

/// One mode definition. All dials optional; absent fields inherit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModeDefinitionToml {
    /// Display name. Defaults to the file stem for file-loaded modes;
    /// required (implicitly) for built-ins.
    #[serde(default)]
    pub name: Option<String>,
    /// One-line description shown in `/mode` listings.
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
    pub tools: Option<ModeToolsToml>,
    /// Feature-flag overrides keyed like `[features]` in config.toml
    /// (e.g. `subagents = false`).
    #[serde(default)]
    pub features: Option<BTreeMap<String, bool>>,
}

impl ModeDefinitionToml {
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

    /// Parse + validate a mode file body. `fallback_name` (typically the
    /// file stem) is used when the file has no `name` field.
    pub fn parse_toml(source: &str, fallback_name: &str) -> Result<Self> {
        let mut definition: Self = toml::from_str(source)
            .map_err(|err| anyhow::anyhow!("mode '{fallback_name}': {err}"))?;
        definition.validate()?;
        if definition.name.as_deref().is_none_or(str::is_empty) {
            definition.name = Some(fallback_name.to_string());
        }
        Ok(definition)
    }
}

/// Which layer a mode definition was loaded from. Project modes override
/// user modes, which override built-ins (by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSource {
    BuiltIn,
    User,
    Project,
}

impl ModeSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// A mode definition plus its resolved name and origin.
#[derive(Debug, Clone)]
pub struct LoadedMode {
    pub name: String,
    pub definition: ModeDefinitionToml,
    pub source: ModeSource,
}

/// All modes visible to a workspace: built-ins + `~/.codesmith/modes/` +
/// `<workspace>/.codesmith/modes/`, deduplicated by name with later layers
/// winning. Invalid files are skipped and reported in `warnings` so one
/// broken community file cannot brick startup.
#[derive(Debug, Clone, Default)]
pub struct ModeCatalog {
    modes: BTreeMap<String, LoadedMode>,
    /// Non-fatal load problems (e.g. a malformed user mode file).
    pub warnings: Vec<String>,
}

impl ModeCatalog {
    /// Load the catalog for a workspace using the default locations.
    pub fn load(workspace: Option<&Path>) -> Self {
        let user_dir = crate::codesmith_home()
            .map(|home| home.join(MODES_DIR_NAME))
            .ok();
        let project_dir = workspace.map(|ws| ws.join(".codesmith").join(MODES_DIR_NAME));
        Self::load_from(user_dir.as_deref(), project_dir.as_deref())
    }

    /// Testable core: built-ins, then `user_dir`, then `project_dir`.
    pub fn load_from(user_dir: Option<&Path>, project_dir: Option<&Path>) -> Self {
        let mut catalog = Self::default();
        catalog.insert_builtin(BUILTIN_MINIMAL_TOML, "minimal");
        catalog.insert_builtin(BUILTIN_BALANCED_TOML, "balanced");
        catalog.insert_builtin(BUILTIN_MAXIMAL_TOML, "maximal");
        catalog.insert_builtin(BUILTIN_PLAN_TOML, "plan");

        if let Some(dir) = user_dir {
            catalog.insert_dir(dir, ModeSource::User);
        }
        if let Some(dir) = project_dir {
            catalog.insert_dir(dir, ModeSource::Project);
        }
        catalog
    }

    fn insert_builtin(&mut self, source: &str, fallback_name: &str) {
        match ModeDefinitionToml::parse_toml(source, fallback_name) {
            Ok(definition) => {
                let name = definition
                    .name
                    .clone()
                    .unwrap_or_else(|| fallback_name.to_string());
                self.modes.insert(
                    name.clone(),
                    LoadedMode {
                        name,
                        definition,
                        source: ModeSource::BuiltIn,
                    },
                );
            }
            Err(err) => self.warnings.push(format!("built-in mode: {err}")),
        }
    }

    fn insert_dir(&mut self, dir: &Path, source: ModeSource) {
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
        for path in files {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "mode".to_string());
            let body = match std::fs::read_to_string(&path) {
                Ok(body) => body,
                Err(err) => {
                    self.warnings
                        .push(format!("{}: unreadable ({err})", path.display()));
                    continue;
                }
            };
            match ModeDefinitionToml::parse_toml(&body, &stem) {
                Ok(definition) => {
                    let name = definition.name.clone().unwrap_or(stem);
                    self.modes.insert(
                        name.clone(),
                        LoadedMode {
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
    pub fn get(&self, name: &str) -> Option<&LoadedMode> {
        self.modes.get(name)
    }

    /// Case-insensitive lookup so `/mode Minimal` works like `/mode minimal`.
    #[must_use]
    pub fn get_ci(&self, name: &str) -> Option<&LoadedMode> {
        if let Some(hit) = self.modes.get(name) {
            return Some(hit);
        }
        let lowered = name.trim().to_ascii_lowercase();
        self.modes
            .values()
            .find(|mode| mode.name.to_ascii_lowercase() == lowered)
    }

    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.modes.keys().map(String::as_str).collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LoadedMode> {
        self.modes.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_modes_parse_and_validate() {
        let catalog = ModeCatalog::load_from(None, None);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        for name in ["minimal", "balanced", "maximal", "plan"] {
            assert!(catalog.get(name).is_some(), "missing built-in {name}");
        }
    }

    #[test]
    fn builtin_dials_are_expected() {
        let catalog = ModeCatalog::load_from(None, None);
        let minimal = &catalog.get("minimal").unwrap().definition;
        assert_eq!(minimal.reasoning_effort.as_deref(), Some("off"));
        assert_eq!(minimal.memory_level.as_deref(), Some("goldfish"));
        assert_eq!(minimal.max_subagents, Some(0));
        assert!(minimal.tools.as_ref().unwrap().include.is_some());

        let balanced = &catalog.get("balanced").unwrap().definition;
        assert_eq!(balanced.app_mode, None);
        assert_eq!(balanced.tools, None);

        let maximal = &catalog.get("maximal").unwrap().definition;
        assert_eq!(maximal.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(maximal.memory_level.as_deref(), Some("elephant"));

        let plan = &catalog.get("plan").unwrap().definition;
        assert_eq!(plan.app_mode.as_deref(), Some("plan"));
    }

    #[test]
    fn minimal_tool_allowlist_only_names_real_tools() {
        let catalog = ModeCatalog::load_from(None, None);
        let include = catalog
            .get("minimal")
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
                "minimal mode includes unknown tool '{name}'"
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
        let bad = ModeDefinitionToml {
            app_mode: Some("ninja".into()),
            ..ModeDefinitionToml::default()
        };
        assert!(bad.validate().is_err());

        let bad = ModeDefinitionToml {
            reasoning_effort: Some("ultra".into()),
            ..ModeDefinitionToml::default()
        };
        assert!(bad.validate().is_err());

        let bad = ModeDefinitionToml {
            memory_level: Some("whale".into()),
            ..ModeDefinitionToml::default()
        };
        assert!(bad.validate().is_err());

        let bad = ModeDefinitionToml {
            approval_policy: Some("maybe".into()),
            ..ModeDefinitionToml::default()
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

[tools]
include = ["read_file", "grep_files", "list_dir"]
exclude = ["exec_shell"]

[features]
subagents = false
web_search = false
"#;
        let mode = ModeDefinitionToml::parse_toml(source, "fallback").unwrap();
        assert_eq!(mode.name.as_deref(), Some("review"));
        assert_eq!(mode.max_subagents, Some(2));
        let tools = mode.tools.unwrap();
        assert_eq!(
            tools.include.unwrap(),
            vec!["read_file", "grep_files", "list_dir"]
        );
        assert_eq!(tools.exclude.unwrap(), vec!["exec_shell"]);
        assert_eq!(mode.features.unwrap().get("subagents"), Some(&false));
    }

    #[test]
    fn parse_defaults_name_to_fallback() {
        let mode =
            ModeDefinitionToml::parse_toml("reasoning_effort = \"off\"\n", "my-mode").unwrap();
        assert_eq!(mode.name.as_deref(), Some("my-mode"));
    }

    #[test]
    fn project_overrides_user_over_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user-modes");
        let project = tmp.path().join("project-modes");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&project).unwrap();

        // Same name at every layer; project must win.
        std::fs::write(user.join("minimal.toml"), "description = \"user flavor\"\n").unwrap();
        std::fs::write(
            project.join("minimal.toml"),
            "description = \"project flavor\"\n",
        )
        .unwrap();
        // A user-only mode survives.
        std::fs::write(
            user.join("focus.toml"),
            "description = \"focus\"\nreasoning_effort = \"high\"\n",
        )
        .unwrap();
        // A broken file is skipped with a warning, not fatal.
        std::fs::write(user.join("broken.toml"), "reasoning_effort = \"???\"\n").unwrap();

        let catalog = ModeCatalog::load_from(Some(&user), Some(&project));
        assert_eq!(
            catalog.get("minimal").unwrap().definition.description,
            Some("project flavor".to_string())
        );
        assert_eq!(catalog.get("minimal").unwrap().source, ModeSource::Project);
        assert_eq!(
            catalog.get("focus").unwrap().source,
            ModeSource::User,
            "user-only mode should survive"
        );
        assert!(
            catalog.warnings.iter().any(|w| w.contains("broken")),
            "broken file should warn: {:?}",
            catalog.warnings
        );
    }

    #[test]
    fn case_insensitive_lookup() {
        let catalog = ModeCatalog::load_from(None, None);
        assert!(catalog.get_ci("Minimal").is_some());
        assert!(catalog.get_ci("  MAXIMAL ").is_some());
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
