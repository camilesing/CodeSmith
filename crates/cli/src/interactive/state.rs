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

use crate::app_state::{AppMode, HuntState, InteractiveOptions, ReasoningEffort, SessionState};
use crate::approval_grants::{ApprovalGrants, approvals_toml_path};
use crate::compaction::CompactionConfig;
use crate::config::{ApiProvider, Config};
use crate::core::engine::{EngineConfig, EngineHandle};
use crate::cycle_manager::CycleConfig;
use crate::hooks::{HookContext, HookEvent, HookExecutor};
use crate::localization::{Locale, resolve_locale};
use crate::mcp;
use crate::models::{Message, SystemPrompt};
use crate::session_manager::SessionManager;
use crate::settings::Settings;
use crate::tools::plan::{SharedPlanState, new_shared_plan_state};
use crate::tools::plan_mode::SharedPlanModeState;
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager, wrap_shell_manager};
use crate::tools::spec::RuntimeToolServices;
use crate::tools::subagent::SubAgentResult;
use crate::tools::todo::{SharedTodoList, new_shared_todo_list};
use codesmith_agent_runtime::mode::ApprovalMode;
use codesmith_config::presets::PresetCatalog;

/// Known limitations of the line-based REPL, recorded beside the behaviour
/// it owns so the next reader does not assume a capability that was never
/// built (repo rule: write down what a design does not do):
///
/// - Multiline input is `\`-continuation and `Ctrl+J` everywhere; the
///   modifier forms (`Shift+Enter`, `Alt+Enter`) only fire in terminals
///   that report the modifier with CR. There is no paste-burst editor.
/// - Completion (Tab) covers registered command names (at the start of the
///   prompt and mid-prompt), user command names, the registry's closed
///   argument vocabularies, `@path` mentions from a cached workspace index,
///   and `!` shell lines (workspace paths for a word with a separator, the
///   project's own `!` history otherwise). Session ids, skills, presets, and
///   live model names are not completed (they need a network call or an
///   uncached walk), and there is no popup menu while typing — feedback is
///   the dim inline hint (see `completion.rs`).
/// - No mouse support, no viewport scrolling, no themes beyond ANSI colors.
/// - A key that acts (`Ctrl+O`, `Ctrl+T`, `Alt+P`, `Shift+Tab`, `?`) ends
///   the prompt so the REPL can print, then re-prompts with the draft: the
///   transcript cannot be rewritten, so the panel appears above a fresh
///   prompt line and the cursor returns to the end of the draft rather than
///   where it was. The `Ctrl+T` checklist therefore also prints above the
///   prompt instead of replacing a row.
/// - `jj` → Escape (`vimInsertModeRemaps`) is not implemented: rustyline 14
///   exposes no `Cmd` a handler could use to leave vi insert mode.
/// - Option/Alt keys (`Alt+P`, `Alt+T`) need the terminal to send Option as
///   Meta; see `/terminal-setup`.
/// - Reasoning/thinking deltas are not streamed to the terminal.
/// - Desktop notifications (OSC 9/BEL) are not emitted on turn completion.
/// - Tool-call detail is append-only replay (`/expand`, Ctrl+O): the
///   terminal transcript cannot be rewritten, so expanding reprints detail
///   instead of un-collapsing lines in place.
/// - While a turn is active the composer has no history (`Up`/`Down` reach
///   the queue, not past inputs) and no completion: Tab is `complete` only
///   at the prompt. A message typed mid-turn reaches the model when the tool
///   call it waited on finishes, or as the next turn if the turn ends first.
/// - The composer's cursor sits on one terminal row: a message long enough to
///   wrap is edited blind past the first row, and the mid-turn kill keys have
///   no kill ring (no Ctrl+Y mid-turn).
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
    /// Model name carried by the last turn's stream `MessageStart` frame.
    ///
    /// It is not a window onto a gateway's backend: every production provider
    /// synthesizes that frame locally from the request id
    /// (`crates/providers/src/rig_adapter/stream.rs`), so in practice this
    /// equals the configured id. It stays as the rung behind the user's
    /// `[providers.<id>] display_model` override; to label what an alias really
    /// serves, set that key.
    ///
    /// Known limitations: the startup banner has no response yet; after
    /// `/resume` it falls back to the configured id until the next turn
    /// completes; and the context-window budget
    /// (`effective_model_for_budget`) stays keyed on the requested id.
    pub reported_model: Option<String>,
    /// Failed-attempt bookkeeping accumulated from `Event::TurnComplete`
    /// (round-3 TODO-42): summary records of this session's failed model
    /// rounds. Persisted onto `SavedSession::failed_attempts` at the turn
    /// boundary and re-adopted on resume/`/load`. Log-only — never part of
    /// `api_messages` or any model request.
    pub failed_attempts: Vec<codesmith_agent_runtime::session_store::AttemptRecord>,
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
    /// Collapsed one-line tool-call rendering (default) vs the expanded
    /// two-line form; toggled in-turn with Ctrl+O.
    pub tool_detail: bool,
    /// Whether the `Ctrl+T` checklist panel prints above each prompt. The
    /// session's todo list itself lives in `todos`, shared with the engine.
    pub show_tasks: bool,
    /// Messages typed while a turn was running and not yet delivered
    /// (Claude Code's queue). The pump steers them into the running turn as
    /// tool calls finish; whatever is left when the turn ends is sent by the
    /// REPL loop as the next turn, in the order typed.
    pub queued_messages: Vec<String>,
    /// What was in the mid-turn composer when the turn ended. The REPL loop
    /// takes it as the next prompt's text, so a turn that ends first never
    /// swallows half-typed input.
    pub turn_end_draft: Option<String>,
    /// Text a slash command handed back for the next prompt: `/pr` puts the
    /// fetched pull request here. The REPL loop takes it before reading, so
    /// the user sees an editable draft rather than a message already sent.
    pub next_input: Option<String>,
    /// Skill names as first discovered this session, so `/reload-skills` can
    /// say what appeared or disappeared since startup.
    pub skill_baseline: Option<Vec<String>>,
    /// Output of `settings.statusline_command`, refreshed by the REPL loop
    /// (the renderer stays synchronous and just prints what is here).
    pub statusline_text: Option<String>,
    /// Rolling log of completed tool calls for `/expand` and the Ctrl+O
    /// replay, capped at [`crate::tool_summary::TOOL_LOG_CAP`].
    pub tool_log: Vec<crate::tool_summary::ToolCallRecord>,
    /// Turn index, bumped on each `TurnStarted` so `/expand` can address
    /// "the last turn" of the log.
    pub tool_log_turn: usize,
    pub subagent_cache: Vec<SubAgentResult>,
    pub hooks: HookExecutor,
    pub shell_manager: SharedShellManager,
    pub runtime_services: RuntimeToolServices,
    pub index_service: Option<Arc<dyn codesmith_index::IndexServiceApi>>,
    pub todos: SharedTodoList,
    pub plan_state: SharedPlanState,
    /// Shared with the engine (same handle it drives plan mode with), so the
    /// REPL can name the current plan file — the `exit_plan_mode` approval
    /// renders its body from this slug.
    pub plan_mode_state: SharedPlanModeState,
    /// Where a plan session returns to — this session's own record, written
    /// when it enters Plan mode (Shift+Tab, `/mode`, or following the model's
    /// `enter_plan_mode`) and read when an approved `exit_plan_mode` restores
    /// the mode. Deliberately not mirrored into `PlanModeState`: that flag
    /// means a *model-initiated* session, and a second copy of the mode would
    /// only stay right while every writer remembered to update it.
    pub plan_restore_mode: Option<AppMode>,
    pub telemetry_sink: Option<TelemetrySink>,
    pub current_session_id: Option<String>,
    pub approval_grants: ApprovalGrants,
    /// **Grouping** keys approved for this session (`shell:<prefix>`,
    /// `patch:<hash>`, `net:<host>`, `tool:<name>`), inserted by `[a]` — so
    /// "always allow" covers what the prompt's legend says it covers, and a
    /// later call with different arguments can match without re-prompting.
    pub approval_session_approved: HashSet<String>,
    /// **Exact** call keys denied this session, inserted by `[e]`; a retry of
    /// that same call auto-denies without re-prompting, while a different call
    /// to the same tool still asks.
    pub approval_session_denied: HashSet<String>,
    pub cycle: CycleConfig,
    pub engine: Option<EngineHandle>,
    /// Durable task manager (started by `run_repl`).
    pub task_manager: Option<crate::task_manager::SharedTaskManager>,
    /// Media paths queued by /attach, consumed by the next submit.
    pub pending_attachments: Vec<PathBuf>,
    pub turn_started_at: Option<Instant>,
    pub cumulative_turn_duration: std::time::Duration,
    pub mcp_configured_count: usize,
    pub lsp_enabled: bool,
    /// Effective merged config (user + project + preset layer). Slash
    /// commands mutate this copy; engine rebuilds read it.
    pub config: Config,
    /// Persisted UI settings, loaded once at startup. Kept so the editor
    /// configuration (history cap, edit mode, `@`-completion bounds) and
    /// `/config` read the same snapshot the session started with.
    pub settings: Settings,
    /// Bound extension runner surfaced from the engine spawn.
    pub extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    /// Canonical names of builtin commands disabled via `[commands]
    /// disabled` (validated + resolved once at REPL start; mod-sourced
    /// disables are read live off the runner).
    pub disabled_commands: Vec<String>,
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
    pub fn new(options: InteractiveOptions, config: &Config) -> Self {
        let InteractiveOptions {
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
        let compact_threshold = config.compaction_threshold(threshold_model);
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
        let plan_mode_state = crate::tools::plan_mode::new_shared_plan_mode_state();
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
            reported_model: None,
            failed_attempts: Vec::new(),
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
            tool_detail: false,
            show_tasks: false,
            queued_messages: Vec::new(),
            turn_end_draft: None,
            next_input: None,
            skill_baseline: None,
            statusline_text: None,
            tool_log: Vec::new(),
            tool_log_turn: 0,
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
            plan_mode_state,
            plan_restore_mode: None,
            telemetry_sink: None,
            current_session_id: None,
            approval_grants: approvals_toml_path()
                .map(|path| ApprovalGrants::load(&path))
                .unwrap_or_default(),
            approval_session_approved: HashSet::new(),
            approval_session_denied: HashSet::new(),
            cycle: CycleConfig::default(),
            engine: None,
            task_manager: None,
            pending_attachments: Vec::new(),
            turn_started_at: None,
            cumulative_turn_duration: std::time::Duration::ZERO,
            mcp_configured_count: mcp::load_config(&mcp_config_path)
                .map(|cfg| cfg.servers.len())
                .unwrap_or(0),
            lsp_enabled: config.lsp.as_ref().and_then(|l| l.enabled).unwrap_or(true),
            config: config.clone(),
            settings,
            extension_runner: None,
            disabled_commands: Vec::new(),
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

    /// Advance the Shift+Tab ring by one mode ([`AppMode::cycle_next`]).
    /// Coordinator joins the ring only while its experimental feature is
    /// enabled — the same gate the startup path above applies.
    pub fn cycle_mode(&mut self) {
        let include_coordinator = self
            .config
            .features()
            .enabled(crate::features::Feature::CoordinatorMode);
        let next = self.mode.cycle_next(include_coordinator);
        self.set_mode(next);
    }

    pub fn set_mode(&mut self, mode: AppMode) {
        if self.mode == mode {
            return;
        }
        let previous_mode = self.mode;
        let entering_yolo = mode == AppMode::Yolo && self.mode != AppMode::Yolo;
        let leaving_yolo = self.mode == AppMode::Yolo && mode != AppMode::Yolo;
        let entering_coordinator =
            mode == AppMode::Coordinator && self.mode != AppMode::Coordinator;
        let leaving_coordinator = self.mode == AppMode::Coordinator && mode != AppMode::Coordinator;
        self.mode = mode;

        // A plan session returns to the mode it came from. Both entry routes
        // funnel through here — the user's switch, and the REPL following the
        // model's `enter_plan_mode` result — so the stash and the mode cannot
        // disagree.
        if mode == AppMode::Plan && previous_mode != AppMode::Plan {
            self.plan_restore_mode = Some(previous_mode);
        } else if mode != AppMode::Plan {
            self.plan_restore_mode = None;
        }

        if entering_coordinator {
            // Coordinator mode requires auto-approve so workers can execute
            // their tools without prompting for each one — and its workers
            // need shell, so the gate is forced on here (mirroring the
            // Yolo branch) instead of being bypassed at the runtime layer:
            // a user who opted out of shell sees the opt-in change modes,
            // and `/status` stays truthful.
            self.approval_mode = ApprovalMode::Auto;
            self.allow_shell = true;
        } else if leaving_coordinator {
            self.approval_mode = ApprovalMode::Suggest;
            self.allow_shell = false;
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

        // `/status` prints this field, so it tracks the mode rather than the
        // startup flag it was seeded from.
        self.yolo = mode == AppMode::Yolo;

        // mode_change hooks (observer-only) on every effective transition —
        // `/mode`, `/yolo`, preset startup, and resume all funnel through
        // here. Detached so a hook process never blocks the mode switch.
        if self.hooks.has_hooks_for_event(HookEvent::ModeChange) {
            let context = self
                .hook_context()
                .with_previous_mode(previous_mode.as_setting());
            crate::hooks::emit_detached(
                Arc::new(self.hooks.clone()),
                HookEvent::ModeChange,
                context,
            );
        }
    }

    /// Base [`HookContext`] for REPL-emitted hook events — the mirror of the
    /// engine's turn-loop template (`CallbackBridge`): ephemeral hook session
    /// id, durable thread id, workspace, model, lowercase mode, and the
    /// session-cumulative token/cost totals.
    pub fn hook_context(&self) -> HookContext {
        let mut context = HookContext::new()
            .with_session_id(self.hooks.session_id())
            .with_workspace(self.workspace.clone())
            .with_model(&self.model)
            .with_mode(self.mode.as_setting())
            .with_tokens(self.session.total_tokens)
            .with_cost(self.session.session_cost);
        if let Some(thread_id) = self.current_session_id.as_deref() {
            context = context.with_thread_id(thread_id);
        }
        context
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
        // The previous model's server report no longer describes this
        // selection — drop it until the next response reports again.
        self.reported_model = None;
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

    /// The session's context-window denominator in tokens: the active
    /// provider's `context_window` override when configured, else the static
    /// table for the effective (routed) model. `None` renders as `🧭 —`.
    pub fn context_window(&self) -> Option<u32> {
        self.config
            .effective_context_window(self.effective_model_for_budget())
    }

    pub fn model_display_label(&self) -> String {
        // A configured display name wins verbatim: the user declared what the
        // endpoint serves, and presentation should not second-guess it.
        if let Some(configured) = self.config.model_display_name() {
            return configured.to_string();
        }
        // Otherwise the `MessageStart` name (in practice the requested id —
        // see `reported_model`) wins over the routed and configured ids.
        let reported = self
            .reported_model
            .as_deref()
            .filter(|model| !model.is_empty());
        if self.auto_model {
            if let Some(effective) = reported
                .or(self.last_effective_model.as_deref())
                .filter(|model| *model != "auto")
            {
                return format!("auto: {effective}");
            }
            return "auto".to_string();
        }
        reported.unwrap_or(&self.model).to_string()
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
        self.compact_threshold = self.config.compaction_threshold(&model);
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
        plan_mode_state: state.plan_mode_state.clone(),
        task_v2_manager: None,
        goal_state: crate::tools::goal::new_shared_goal_state_from_host(
            state.hunt.quarry.clone(),
            state.hunt.token_budget,
            state.hunt.verdict == crate::app_state::HuntVerdict::Hunted,
        ),
        worktree_state: crate::tools::worktree::new_shared_worktree_session_state(),
        max_spawn_depth: config.max_spawn_depth(),
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
        subagent_external_provider: crate::tools::subagent::headless::provider_from_config(config)?,
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
        web_service: Some(std::sync::Arc::new(
            codesmith_tool_impls::web_service::HttpWebService::new(
                config.search_provider(),
                config.search.as_ref().and_then(|s| s.api_key.clone()),
            ),
        )),
        index_enabled: config.index_tools_enabled(),
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
    // Round-3 TODO-42 — failed-attempt bookkeeping survives resume: re-adopt
    // the persisted records so the next turn-boundary save keeps them
    // (otherwise every resume+save cycle silently dropped the history).
    state.failed_attempts = saved.failed_attempts.clone();
    restore_engine_backed_session_state(state, &saved).await;
    // Round-7 TODO-48 — undelivered queued input rides the resume: the
    // journal was written the moment each message was queued and survives
    // crashes that lose the in-memory queue.
    let restored = crate::interactive::inbox_journal::adopt_for_resume(
        state,
        manager.sessions_dir(),
        &saved_id,
    );
    if restored > 0 {
        println!("⏸ restored {restored} queued message(s) from the inbox journal");
    }
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
/// REPL-side projections from the resumed messages.
///
/// Restore checklist for the resume path (this runs *before* `Op::SyncSession`):
/// - todo list / plan state — fold rebuild here (round-3 TODO-38; the Arcs
///   live on `ReplState` and are shared with `EngineConfig`, so the next
///   turn's tools see the restored state);
/// - recent read files — fold rebuild into the engine surface;
/// - fact ledger — snapshot restore (non-foldable once compaction drops the
///   source messages: foldable state becomes a projection, non-foldable a
///   snapshot);
/// - working set + canonical capacity state — engine-side, rebuilt by the
///   `Op::SyncSession` handler that follows this call.
///
/// Known limitation: every rebuild is an O(transcript) full fold; incremental
/// projection checkpoints are deliberately not built (round-3 plan §2.1) —
/// revisit only if long-session resume cost is observed as a problem.
async fn restore_engine_backed_session_state(
    state: &mut ReplState,
    session: &crate::session_manager::SavedSession,
) {
    // todo/plan: fold rebuilds from the resumed transcript. Not gated on the
    // engine — the shared Arcs outlive any single engine instance.
    *state.todos.lock().await =
        codesmith_agent_runtime::tool_state::todo::TodoList::rebuild_from_messages(
            &state.api_messages,
        );
    *state.plan_state.lock().await =
        codesmith_agent_runtime::tool_state::plan::PlanState::rebuild_from_messages(
            &state.api_messages,
        );
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

#[cfg(test)]
mod restore_tests {
    // The env lock spans the async body by the same convention as the
    // engine tests (core/engine/tests.rs header): the guard is deliberately
    // held across await points on the current-thread runtime.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    /// Regression (round-3 TODO-38): resuming a session must rebuild the
    /// todo/plan projections from the transcript. The original wiring lived
    /// in the retired tui crate's `apply_loaded_session`; the REPL port
    /// (`restore_engine_backed_session_state`) only carried over
    /// recent_read_files and fact_ledger, so resume silently started the
    /// todo list and plan from empty. The Arcs restored here are the same
    /// ones `ReplState` shares with `EngineConfig` (see `EngineConfig`
    /// construction), so the next turn's tools see the rebuilt state.
    /// Round-7 TODO-48 — resuming a session adopts its inbox journal: text
    /// queued mid-turn before a crash reappears in the queue. Baseline
    /// (pre-fix) the journal file existed but nothing read it, so the queue
    /// resumed empty.
    #[test]
    fn adopt_for_resume_seeds_the_queue_from_the_journal() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let journal =
            crate::interactive::inbox_journal::InboxJournal::for_session(tmp.path(), "sess-inbox")
                .expect("journal");
        journal.append("still undelivered").unwrap();
        journal.append("second in line").unwrap();

        let config = crate::config::Config::default();
        let options = crate::app_state::InteractiveOptions {
            model: "test-model".to_string(),
            workspace: std::env::temp_dir(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            max_subagents: 2,
            skills_dir: std::env::temp_dir(),
            memory_path: std::env::temp_dir().join("memory.md"),
            mcp_config_path: std::env::temp_dir().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        let mut state = ReplState::new(options, &config);
        let restored = crate::interactive::inbox_journal::adopt_for_resume(
            &mut state,
            tmp.path(),
            "sess-inbox",
        );
        assert_eq!(restored, 2, "both journal entries are adopted");
        assert_eq!(
            state.queued_messages,
            vec![
                "still undelivered".to_string(),
                "second in line".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn resume_restore_rebuilds_todo_and_plan_projections() {
        let _lock = crate::test_support::lock_test_env();
        fn options() -> crate::app_state::InteractiveOptions {
            crate::app_state::InteractiveOptions {
                model: "test-model".to_string(),
                workspace: std::env::temp_dir(),
                config_path: None,
                config_profile: None,
                allow_shell: false,
                max_subagents: 2,
                skills_dir: std::env::temp_dir(),
                memory_path: std::env::temp_dir().join("memory.md"),
                mcp_config_path: std::env::temp_dir().join("mcp.json"),
                use_memory: false,
                start_in_agent_mode: true,
                yolo: false,
                resume_session_id: None,
                initial_input: None,
            }
        }
        fn tool_result(id: &str) -> codesmith_agent::models::ContentBlock {
            codesmith_agent::models::ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: "ok".to_string(),
                is_error: Some(false),
                content_blocks: None,
            }
        }
        // Transcript with executed todo_write + update_plan calls: each
        // ToolUse is paired with a non-error ToolResult, which is what
        // `executed_tool_call_ids` (the fold's execution gate) requires.
        let messages = vec![
            codesmith_agent::models::Message {
                role: "assistant".to_string(),
                content: vec![codesmith_agent::models::ContentBlock::ToolUse {
                    id: "t1".to_string(),
                    name: "todo_write".to_string(),
                    input: serde_json::json!({
                        "todos": [
                            {"content": "wire the restore", "status": "in_progress"},
                            {"content": "verify the fold", "status": "pending"}
                        ]
                    }),
                    caller: None,
                }],
            },
            codesmith_agent::models::Message {
                role: "user".to_string(),
                content: vec![tool_result("t1")],
            },
            codesmith_agent::models::Message {
                role: "assistant".to_string(),
                content: vec![codesmith_agent::models::ContentBlock::ToolUse {
                    id: "p1".to_string(),
                    name: "update_plan".to_string(),
                    input: serde_json::json!({
                        "explanation": "resume keeps the plan",
                        "plan": [
                            {"step": "restore projections", "status": "completed"},
                            {"step": "run the suite", "status": "pending"}
                        ]
                    }),
                    caller: None,
                }],
            },
            codesmith_agent::models::Message {
                role: "user".to_string(),
                content: vec![tool_result("p1")],
            },
        ];

        let config = crate::config::Config::default();
        let mut state = ReplState::new(options(), &config);
        state.adopt_saved_session(
            "sess-restore",
            messages.clone(),
            None,
            "test-model".to_string(),
        );
        let saved = crate::session_manager::SavedSession {
            // v1 — matches the private CURRENT_SESSION_SCHEMA_VERSION.
            schema_version: 1,
            metadata: codesmith_agent_runtime::session_store::SessionMetadata {
                id: "sess-restore".to_string(),
                title: "restore regression".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                message_count: messages.len(),
                total_tokens: 0,
                model: "test-model".to_string(),
                reported_model: None,
                workspace: std::env::temp_dir(),
                mode: None,
                cost: Default::default(),
                parent_session_id: None,
                forked_from_message_count: None,
                cumulative_turn_secs: 0,
            },
            messages,
            system_prompt: None,
            last_tool_catalog: None,
            last_base_url: None,
            fact_ledger: None,
            context_references: Vec::new(),
            artifacts: Vec::new(),
            failed_attempts: Vec::new(),
        };
        restore_engine_backed_session_state(&mut state, &saved).await;

        let todos = state.todos.lock().await;
        assert_eq!(
            todos.snapshot().items.len(),
            2,
            "resume must rebuild the todo list from the transcript"
        );
        drop(todos);
        let plan = state.plan_state.lock().await;
        assert_eq!(
            plan.steps().len(),
            2,
            "resume must rebuild the plan state from the transcript"
        );
    }
}
