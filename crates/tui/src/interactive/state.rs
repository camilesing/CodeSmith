//! REPL session state — the non-rendering mirror of the retired TUI `App`.
//!
//! `ReplState` carries exactly the fields the engine construction, the
//! status line, and the slash-command surface need. UI-only concerns
//! (transcript cells, pickers, themes) are intentionally absent: the REPL
//! prints to stdout and reads lines from stdin.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use codesmith_agent_runtime::telemetry::TelemetrySink;

use crate::approval_grants::{ApprovalGrants, approvals_toml_path};
use crate::compaction::CompactionConfig;
use crate::config::{ApiProvider, Config};
use crate::core::engine::{EngineConfig, EngineHandle};
use crate::cycle_manager::CycleConfig;
use crate::hooks::HookExecutor;
use crate::localization::{Locale, resolve_locale};
use crate::mcp;
use crate::models::{
    Message, SystemPrompt, compaction_threshold_for_model,
    compaction_threshold_for_model_and_effort,
};
use crate::session_manager::SessionManager;
use crate::settings::Settings;
use crate::tools::plan::{SharedPlanState, new_shared_plan_state};
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager, wrap_shell_manager};
use crate::tools::spec::RuntimeToolServices;
use crate::tools::subagent::{DEFAULT_MAX_SPAWN_DEPTH, SubAgentResult};
use crate::tools::todo::{SharedTodoList, new_shared_todo_list};
use crate::tui::app::{AppMode, HuntState, ReasoningEffort, SessionState, TuiOptions};
use crate::tui::approval::ApprovalMode;
use codesmith_config::presets::PresetCatalog;

/// Known limitations of the line-based REPL, recorded beside the behaviour
/// it owns so the next reader does not assume a capability that was never
/// built (repo rule: write down what a design does not do):
///
/// - Single-line input: no multiline composer, no paste-burst editor.
/// - No mouse support, no viewport scrolling, no themes beyond ANSI colors.
/// - Reasoning/thinking deltas are not streamed to the terminal.
/// - Desktop notifications (OSC 9/BEL) are not emitted on turn completion.
pub struct ReplState {
    pub mode: AppMode,
    /// Session cost/token telemetry (mirrors the retired `App.session`).
    pub session: SessionState,
    /// Goal/hunt sub-state feeding `EngineConfig::goal_state`.
    pub hunt: HuntState,
    /// Active tool allowlist from the current preset / slash-command
    /// frontmatter; `None` means the normal tool set.
    pub active_allowed_tools: Option<Vec<String>>,
    /// Active tool denylist (preset `tools.exclude`); `None` excludes nothing.
    pub active_blocked_tools: Option<Vec<String>>,
    /// Name of the active preset tier (simple | middle | all | ... | diy).
    pub active_preset: Option<String>,
    /// Authoritative API conversation (mirrors engine `SessionUpdated`).
    pub api_messages: Vec<Message>,
    pub system_prompt: Option<SystemPrompt>,
    pub is_loading: bool,
    pub is_compacting: bool,
    /// A tool-approval prompt is blocking the turn.
    pub awaiting_approval: bool,
    pub model: String,
    pub auto_model: bool,
    pub last_effective_model: Option<String>,
    pub api_provider: ApiProvider,
    pub model_ids_passthrough: bool,
    pub reasoning_effort: ReasoningEffort,
    pub last_effective_reasoning_effort: Option<ReasoningEffort>,
    pub workspace: PathBuf,
    pub config_path: Option<PathBuf>,
    pub config_profile: Option<String>,
    pub mcp_config_path: PathBuf,
    pub skills_dir: PathBuf,
    pub memory_path: PathBuf,
    pub memory_dir: PathBuf,
    pub kod_enabled: bool,
    pub use_memory: bool,
    pub show_thinking: bool,
    pub is_simple: bool,
    pub translation_enabled: bool,
    pub ui_locale: Locale,
    pub allow_shell: bool,
    pub max_subagents: usize,
    pub trust_mode: bool,
    pub yolo: bool,
    pub approval_mode: ApprovalMode,
    pub auto_compact: bool,
    pub compact_threshold: usize,
    /// Live sub-agent tracking: agent id → latest status text.
    pub agent_progress: HashMap<String, String>,
    pub subagent_cache: Vec<SubAgentResult>,
    pub hooks: HookExecutor,
    pub shell_manager: SharedShellManager,
    pub runtime_services: RuntimeToolServices,
    pub index_service: Option<Arc<dyn codesmith_index::IndexServiceApi>>,
    pub todos: SharedTodoList,
    pub plan_state: SharedPlanState,
    pub telemetry_sink: Option<TelemetrySink>,
    pub current_session_id: Option<String>,
    pub approval_grants: ApprovalGrants,
    /// Tool names (or fingerprints) approved for this session.
    pub approval_session_approved: HashSet<String>,
    /// Tool names (or fingerprints) denied this session; re-requests
    /// auto-deny without re-prompting.
    pub approval_session_denied: HashSet<String>,
    pub cycle: CycleConfig,
    pub engine: Option<EngineHandle>,
    pub turn_started_at: Option<Instant>,
    pub cumulative_turn_duration: std::time::Duration,
    pub mcp_configured_count: usize,
    pub lsp_enabled: bool,
    /// Effective merged config (user + project + preset layer). Slash
    /// commands mutate this copy; engine rebuilds read it.
    pub config: Config,
    /// Bound extension runner surfaced from the engine spawn.
    pub extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    /// Tool-catalog baseline shared from the engine (the `/tools` readout).
    pub tool_catalog: Option<crate::core::tool_catalog::SharedToolCatalog>,
    /// The live session's fact ledger (engine-shared `Arc`).
    pub fact_ledger:
        Option<Arc<std::sync::Mutex<codesmith_agent_runtime::compaction::fact_ledger::FactLedger>>>,
    /// The session's recent-read-files working set (engine-shared `Arc`).
    pub recent_read_files: Option<
        Arc<
            std::sync::Mutex<
                std::collections::VecDeque<codesmith_agent_runtime::session::RecentReadFile>,
            >,
        >,
    >,
}

impl ReplState {
    /// Mirror of the retired `App::new`'s non-UI construction.
    pub fn new(options: TuiOptions, config: &Config) -> Self {
        let TuiOptions {
            model,
            workspace,
            config_path,
            config_profile,
            allow_shell,
            max_subagents,
            skills_dir: global_skills_dir,
            memory_path,
            mcp_config_path,
            use_memory,
            start_in_agent_mode,
            skip_onboarding: _,
            yolo,
            ..
        } = options;

        let settings = Settings::load().unwrap_or_else(|_| Settings::default());
        let memory_dir = config.memory_dir();
        let kod_enabled = config.kod_enabled();
        let mut provider = config.api_provider();
        // Settings override the config provider so runtime switches survive
        // restarts.
        if let Some(provider_str) = settings.default_provider.as_ref()
            && let Some(parsed) = ApiProvider::parse(provider_str)
        {
            provider = parsed;
        }
        let mut effective_auth_config = config.clone();
        effective_auth_config.provider = Some(provider.as_str().to_string());
        let model_ids_passthrough = effective_auth_config.model_ids_pass_through();

        let model = settings
            .provider_models
            .as_ref()
            .and_then(|m| m.get(provider.as_str()).cloned())
            .or_else(|| {
                if matches!(provider, ApiProvider::Deepseek) {
                    settings.default_model.clone()
                } else {
                    None
                }
            })
            .unwrap_or(model);
        let auto_model = model.trim().eq_ignore_ascii_case("auto");
        let configured_reasoning_effort = settings
            .reasoning_effort
            .as_deref()
            .or_else(|| config.reasoning_effort());
        let threshold_model = if auto_model {
            crate::config::DEFAULT_TEXT_MODEL
        } else {
            model.as_str()
        };
        let compact_threshold = auto_compact_threshold(
            threshold_model,
            configured_reasoning_effort,
            settings.auto_compact,
        );
        let reasoning_effort = if auto_model {
            ReasoningEffort::Auto
        } else {
            configured_reasoning_effort.map_or_else(ReasoningEffort::default, |s| {
                ReasoningEffort::from_setting(s)
            })
        };

        let initial_mode = if yolo {
            AppMode::Yolo
        } else if start_in_agent_mode {
            AppMode::Agent
        } else if std::env::var("CODESMITH_COORDINATOR_MODE").is_ok()
            && config
                .features()
                .enabled(crate::features::Feature::CoordinatorMode)
        {
            AppMode::Coordinator
        } else {
            AppMode::from_setting(&settings.default_mode)
        };

        let allow_shell = allow_shell || initial_mode == AppMode::Yolo;
        let shell_manager = new_shared_shell_manager(workspace.clone());
        let hooks = HookExecutor::new(config.hooks_config(), workspace.clone());
        let plan_state = new_shared_plan_state();
        let skills_dir = resolve_skills_dir(&workspace, &global_skills_dir, config);
        let approval_mode = if matches!(initial_mode, AppMode::Yolo) {
            ApprovalMode::Auto
        } else {
            config
                .approval_policy
                .as_deref()
                .and_then(ApprovalMode::from_config_value)
                .unwrap_or_default()
        };

        Self {
            mode: initial_mode,
            session: SessionState::default(),
            hunt: HuntState::default(),
            active_allowed_tools: None,
            active_blocked_tools: None,
            active_preset: None,
            api_messages: Vec::new(),
            system_prompt: None,
            is_loading: false,
            is_compacting: false,
            awaiting_approval: false,
            model,
            auto_model,
            last_effective_model: None,
            api_provider: provider,
            model_ids_passthrough,
            reasoning_effort,
            last_effective_reasoning_effort: None,
            workspace,
            config_path,
            config_profile,
            mcp_config_path: mcp_config_path.clone(),
            skills_dir,
            memory_path,
            memory_dir,
            kod_enabled,
            use_memory,
            show_thinking: settings.show_thinking,
            is_simple: settings.is_simple,
            translation_enabled: false,
            ui_locale: resolve_locale(&settings.locale),
            allow_shell,
            max_subagents,
            trust_mode: initial_mode == AppMode::Yolo,
            yolo: initial_mode == AppMode::Yolo,
            approval_mode,
            auto_compact: settings.auto_compact,
            compact_threshold,
            agent_progress: HashMap::new(),
            subagent_cache: Vec::new(),
            hooks,
            shell_manager: shell_manager.clone(),
            runtime_services: RuntimeToolServices {
                shell_manager: Some(wrap_shell_manager(shell_manager)),
                // The interactive session IS the team lead whenever a team
                // exists; protocol tools authorize lead-only actions by it.
                team_sender: Some(crate::tools::team::team_lead_name().to_string()),
                ..RuntimeToolServices::default()
            },
            index_service: None,
            todos: new_shared_todo_list(),
            plan_state,
            telemetry_sink: None,
            current_session_id: None,
            approval_grants: approvals_toml_path()
                .map(|path| ApprovalGrants::load(&path))
                .unwrap_or_default(),
            approval_session_approved: HashSet::new(),
            approval_session_denied: HashSet::new(),
            cycle: CycleConfig::default(),
            engine: None,
            turn_started_at: None,
            cumulative_turn_duration: std::time::Duration::ZERO,
            mcp_configured_count: mcp::load_config(&mcp_config_path)
                .map(|cfg| cfg.servers.len())
                .unwrap_or(0),
            lsp_enabled: config.lsp.as_ref().and_then(|l| l.enabled).unwrap_or(true),
            config: config.clone(),
            extension_runner: None,
            tool_catalog: None,
            fact_ledger: None,
            recent_read_files: None,
        }
    }

    pub fn engine(&self) -> anyhow::Result<&EngineHandle> {
        self.engine
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine not running"))
    }

    pub fn set_mode(&mut self, mode: AppMode) {
        if self.mode == mode {
            return;
        }
        let entering_yolo = mode == AppMode::Yolo && self.mode != AppMode::Yolo;
        let leaving_yolo = self.mode == AppMode::Yolo && mode != AppMode::Yolo;
        let entering_coordinator =
            mode == AppMode::Coordinator && self.mode != AppMode::Coordinator;
        let leaving_coordinator = self.mode == AppMode::Coordinator && mode != AppMode::Coordinator;
        self.mode = mode;

        if entering_coordinator {
            // Coordinator mode requires auto-approve so workers can execute
            // their tools without prompting for each one.
            self.approval_mode = ApprovalMode::Auto;
        } else if leaving_coordinator {
            self.approval_mode = ApprovalMode::Suggest;
        }

        if entering_yolo {
            self.allow_shell = true;
            self.trust_mode = true;
            self.approval_mode = ApprovalMode::Auto;
        } else if leaving_yolo {
            self.allow_shell = false;
            self.trust_mode = false;
            self.approval_mode = ApprovalMode::Suggest;
        }
    }

    pub fn set_model_selection(&mut self, model: String) {
        let auto_model = model.trim().eq_ignore_ascii_case("auto");
        self.model = if auto_model {
            "auto".to_string()
        } else {
            model
        };
        self.auto_model = auto_model;
        self.last_effective_model = None;
        self.last_effective_reasoning_effort = None;
        if auto_model {
            self.reasoning_effort = ReasoningEffort::Auto;
        }
        self.update_model_compaction_budget();
    }

    pub fn model_selection_for_persistence(&self) -> String {
        if self.auto_model || self.model.trim().eq_ignore_ascii_case("auto") {
            "auto".to_string()
        } else {
            self.model.clone()
        }
    }

    pub fn accepts_custom_model_ids(&self) -> bool {
        self.model_ids_passthrough
            || crate::config::provider_passes_model_through(self.api_provider)
    }

    pub fn effective_model_for_budget(&self) -> &str {
        if self.auto_model {
            self.last_effective_model
                .as_deref()
                .filter(|model| *model != "auto")
                .unwrap_or(crate::config::DEFAULT_TEXT_MODEL)
        } else {
            &self.model
        }
    }

    pub fn model_display_label(&self) -> String {
        if self.auto_model {
            if let Some(effective) = self.last_effective_model.as_deref()
                && effective != "auto"
            {
                return format!("auto: {effective}");
            }
            return "auto".to_string();
        }
        self.model.clone()
    }

    pub fn reasoning_effort_display_label(&self) -> String {
        if self.auto_model || self.reasoning_effort == ReasoningEffort::Auto {
            if let Some(effective) = self.last_effective_reasoning_effort {
                return format!("auto: {}", effective.short_label());
            }
            return "auto".to_string();
        }
        self.reasoning_effort.short_label().to_string()
    }

    pub fn update_model_compaction_budget(&mut self) {
        let model = self.effective_model_for_budget().to_string();
        self.compact_threshold =
            compaction_threshold_for_model_and_effort(&model, self.reasoning_effort.api_value());
    }

    pub fn compaction_config(&self) -> CompactionConfig {
        CompactionConfig {
            enabled: self.auto_compact,
            token_threshold: self.compact_threshold,
            model: self.effective_model_for_budget().to_string(),
            ..Default::default()
        }
    }

    pub fn running_agent_count(&self) -> usize {
        self.agent_progress.len()
    }

    /// Apply a preset tier's live dials (mode, tool allow/deny lists) —
    /// the `ReplState` mirror of `presets::apply_at_startup`.
    pub fn apply_preset_at_startup(&mut self, name: &str) {
        let catalog = PresetCatalog::load(Some(&self.workspace));
        let Some(loaded) = catalog.get_ci(name) else {
            return; // unknown selections already warned in apply_config_preset
        };
        let definition = &loaded.definition;
        if let Some(app_mode) = &definition.app_mode {
            self.set_mode(AppMode::from_setting(app_mode));
        }
        match &definition.tools {
            Some(tools) => {
                if let Some(include) = &tools.include
                    && !include.is_empty()
                {
                    self.active_allowed_tools = Some(include.clone());
                } else {
                    self.active_allowed_tools = None;
                }
                match &tools.exclude {
                    Some(exclude) if !exclude.is_empty() => {
                        self.active_blocked_tools = Some(exclude.clone());
                    }
                    _ => {
                        self.active_blocked_tools = None;
                    }
                }
            }
            None => {
                self.active_allowed_tools = None;
                self.active_blocked_tools = None;
            }
        }
        self.active_preset = Some(loaded.name.clone());
    }

    /// Load a saved session into local state. The engine receives the same
    /// snapshot via `Op::SyncSession` from the caller.
    pub fn adopt_saved_session(
        &mut self,
        session_id: &str,
        messages: Vec<Message>,
        system_prompt: Option<String>,
        model: String,
    ) {
        self.api_messages = messages;
        self.system_prompt = system_prompt.map(SystemPrompt::Text);
        self.set_model_selection(model);
        self.current_session_id = Some(session_id.to_string());
    }
}

/// Threshold for the auto-compaction dial, honouring the settings toggle.
fn auto_compact_threshold(
    model: &str,
    reasoning_effort: Option<&str>,
    auto_compact_enabled: bool,
) -> usize {
    if auto_compact_enabled {
        compaction_threshold_for_model_and_effort(model, reasoning_effort)
    } else {
        // Auto-compaction off: fall back to the model's hard window as the
        // (unused) threshold so compaction never fires early.
        compaction_threshold_for_model(model)
    }
}

/// Mirror of the retired `App`-scoped `resolve_skills_dir` (tui/app.rs).
pub(crate) fn resolve_skills_dir(
    workspace: &std::path::Path,
    global_skills_dir: &std::path::Path,
    config: &Config,
) -> PathBuf {
    let agents_skills_dir = workspace.join(".agents").join("skills");
    if agents_skills_dir.exists() {
        return agents_skills_dir;
    }
    let local_skills_dir = workspace.join("skills");
    if local_skills_dir.exists() {
        return local_skills_dir;
    }
    if config.skills_dir.is_none()
        && let Some(global_agents) = crate::skills::agents_global_skills_dir()
        && global_agents.exists()
    {
        return global_agents;
    }
    global_skills_dir.to_path_buf()
}

/// Port of the retired `build_engine_config` (tui/ui.rs) against
/// `ReplState`.
pub fn build_engine_config(state: &ReplState, config: &Config) -> anyhow::Result<EngineConfig> {
    Ok(EngineConfig {
        model: state.model.clone(),
        workspace: state.workspace.clone(),
        allow_shell: state.allow_shell,
        trust_mode: state.trust_mode,
        notes_path: config.notes_path(),
        mcp_config_path: config.mcp_config_path(),
        skills_dir: state.skills_dir.clone(),
        instructions: config
            .instructions_paths()
            .into_iter()
            .map(Into::into)
            .collect(),
        override_system_prompt: config.system_prompt_override_text(),
        custom_system_prompt: None,
        coordinator_system_prompt: None,
        agent_system_prompt: None,
        append_system_prompts: config
            .append_system_prompt_paths()
            .into_iter()
            .map(crate::prompts::PromptAppendSource::file)
            .collect(),
        cache_breaker: None,
        project_context_pack_enabled: config.project_context_pack_enabled(),
        translation_enabled: state.translation_enabled,
        show_thinking: state.show_thinking,
        is_simple: state.is_simple,
        personality: config.personality(),
        // Effectively unlimited: a turn ends when the model stops emitting
        // tool calls; users interrupt with Ctrl+C (same contract the TUI
        // shipped).
        max_steps: u32::MAX,
        max_subagents: state.max_subagents,
        features: config.features(),
        parse_gate: config.edit_config().parse_gate_enabled(),
        compaction: state.compaction_config(),
        cycle: state.cycle.clone(),
        capacity: crate::core::capacity::capacity_controller_config_from_app(config),
        todos: state.todos.clone(),
        plan_state: state.plan_state.clone(),
        plan_mode_state: crate::tools::plan_mode::new_shared_plan_mode_state(),
        task_v2_manager: None,
        goal_state: crate::tools::goal::new_shared_goal_state_from_host(
            state.hunt.quarry.clone(),
            state.hunt.token_budget,
            state.hunt.verdict == crate::tui::app::HuntVerdict::Hunted,
        ),
        worktree_state: crate::tools::worktree::new_shared_worktree_session_state(),
        max_spawn_depth: DEFAULT_MAX_SPAWN_DEPTH,
        allowed_tools: state.active_allowed_tools.clone(),
        blocked_tools: state.active_blocked_tools.clone().unwrap_or_default(),
        network_policy: Some(config.network_policy_decider()),
        snapshots_enabled: config.snapshots_config().is_enabled(),
        snapshots_max_workspace_bytes: config
            .snapshots_config()
            .max_workspace_gb
            .saturating_mul(1024 * 1024 * 1024),
        lsp_config: config
            .lsp
            .clone()
            .map(crate::config::LspConfigToml::into_runtime),
        subagent_model_overrides: config.subagent_model_overrides(),
        auto_route: config.auto_route_context(),
        subagent_api_timeout: std::time::Duration::from_secs(config.subagent_api_timeout_secs()),
        stream_idle_timeout: std::time::Duration::from_secs(config.stream_idle_timeout_secs()),
        stream_idle_retry_increment: std::time::Duration::from_secs(
            config.stream_idle_retry_increment_secs(),
        ),
        result_claim_verifier: config.result_claim_verifier(),
        subagent_inherit_full_registry: config.subagent_inherit_full_registry(),
        prefer_bwrap: config.prefer_bwrap.unwrap_or(false),
        sandbox_runtime: config.sandbox_runtime_config(),
        memory_enabled: config.memory_enabled(),
        memory_path: config.memory_path(),
        kod_enabled: config.kod_enabled(),
        memory_dir: config.memory_dir(),
        memory_excludes: config.memory_excludes(),
        vision_config: config.vision_model_config(),
        strict_tool_mode: config.strict_tool_mode.unwrap_or(false),
        goal_objective: state.hunt.quarry.clone(),
        locale_tag: state.ui_locale.tag().to_string(),
        workshop: config.workshop.clone(),
        search_provider: config.search_provider(),
        index_enabled: config.index_tools_enabled(),
        search_api_key: config.search.as_ref().and_then(|s| s.api_key.clone()),
        tools_always_load: config.tools_always_load(),
        disabled_tools: crate::capabilities::effective_disabled(config.tools.as_ref())
            .map_err(anyhow::Error::msg)?,
        tools: config.tools.clone(),
        team_context: None,
        file_freshness_tracker:
            codesmith_agent_runtime::tools::freshness::FileFreshnessTracker::new(),
        telemetry_sink: state.telemetry_sink.clone(),
    })
}

/// Resume a session into the engine and state, mirroring the exec resume
/// path (`run_exec_agent`) rather than the TUI's App-coupled loader.
pub async fn resume_session(
    state: &mut ReplState,
    resume_session_id: &str,
) -> anyhow::Result<bool> {
    use crate::core::ops::Op;

    let manager = match SessionManager::default_location() {
        Ok(manager) => manager,
        Err(err) => {
            eprintln!("could not open session manager: {err}");
            return Ok(false);
        }
    };
    let load_result = if resume_session_id == "latest" {
        match manager.get_latest_session_for_workspace(&state.workspace) {
            Ok(Some(meta)) => manager.load_session(&meta.id).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    } else {
        manager.load_session_by_prefix(resume_session_id).map(Some)
    };
    let saved = match load_result {
        Ok(Some(saved)) => saved,
        Ok(None) => {
            println!("no sessions found to resume");
            return Ok(false);
        }
        Err(err) => {
            eprintln!("failed to load session: {err}");
            return Ok(false);
        }
    };
    let saved_id = saved.metadata.id.clone();
    state.adopt_saved_session(
        &saved_id,
        saved.messages.clone(),
        saved.system_prompt.clone(),
        saved.metadata.model.clone(),
    );
    state.session.last_tool_catalog = saved.last_tool_catalog.clone();
    state.session.last_base_url = saved.last_base_url.clone();
    restore_engine_backed_session_state(state, &saved);
    let engine = state.engine()?;
    engine
        .send(Op::SyncSession {
            session_id: Some(saved_id.clone()),
            messages: saved.messages,
            system_prompt: saved.system_prompt.map(SystemPrompt::Text),
            system_prompt_override: false,
            model: state.model.clone(),
            workspace: state.workspace.clone(),
        })
        .await?;
    println!(
        "resumed session {}",
        crate::session_manager::truncate_id(&saved_id)
    );
    Ok(true)
}

/// Port of `restore_engine_backed_session_state` (tui/ui.rs): rebuild the
/// engine-side working set and fact ledger from the resumed messages.
fn restore_engine_backed_session_state(
    state: &mut ReplState,
    session: &crate::session_manager::SavedSession,
) {
    if let Some(files) = state
        .engine
        .as_ref()
        .map(|engine| engine.recent_read_files.clone())
    {
        let rebuilt = codesmith_agent_runtime::session::rebuild_recent_read_files_from_messages(
            &state.api_messages,
        );
        let mut guard = files.lock().unwrap_or_else(|e| e.into_inner());
        *guard = rebuilt.lock().unwrap_or_else(|e| e.into_inner()).clone();
    }
    if let Some(ledger) = state
        .engine
        .as_ref()
        .map(|engine| engine.fact_ledger.clone())
    {
        let mut guard = ledger.lock().unwrap_or_else(|e| e.into_inner());
        *guard = session.fact_ledger.clone().unwrap_or_default();
    }
}
