//! Engine configuration type.
//!
//! [`EngineConfig`] is the portable, terminal-agnostic configuration bundle
//! consumed by the engine. It holds only plain data and shared state types
//! that already live in `codesmith-agent-runtime`; the heavy terminal-coupled
//! runtime services (`RuntimeToolServices`) and `HookExecutor` are
//! deliberately *not* stored here — they are host-injected by the embedding
//! binary (the TUI today, via an `EngineHost`-style struct) so that this type
//! stays free of OS-bridging managers.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use crate::capacity::CapacityControllerConfig;
use crate::compaction::{CompactionConfig, DEFAULT_TEXT_MODEL};
use crate::config_types::{
    DEFAULT_MAX_SUBAGENTS, DEFAULT_SUBAGENT_API_TIMEOUT_SECS, SearchProvider, ToolsConfig,
    VisionModelConfig, WorkshopConfig,
};
use crate::cycle_manager::CycleConfig;
use crate::features::Features;
use crate::lsp_config::LspConfig;
use crate::network_policy::NetworkPolicyDecider;
use crate::prompt_sources::{InstructionSource, PromptAppendSource};
use crate::sandbox::SandboxRuntimeConfig;
use crate::skills;
use crate::snapshot::DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT;
use crate::subagent::DEFAULT_MAX_SPAWN_DEPTH;
use crate::tool_state::goal::{SharedGoalState, new_shared_goal_state};
use crate::tool_state::plan::{SharedPlanState, new_shared_plan_state};
use crate::tool_state::plan_mode::{SharedPlanModeState, new_shared_plan_mode_state};
use crate::tool_state::task_v2::SharedTaskV2Manager;
use crate::tool_state::team::SharedTeamContext;
use crate::tool_state::todo::{SharedTodoList, new_shared_todo_list};
use crate::tool_state::worktree::{SharedWorktreeSessionState, new_shared_worktree_session_state};

/// Configuration for the engine
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Model identifier to use for responses.
    pub model: String,
    /// Workspace root for tool execution and file operations.
    pub workspace: PathBuf,
    /// Allow shell tool execution when true.
    pub allow_shell: bool,
    /// Enable trust mode (skip approvals) when true.
    pub trust_mode: bool,
    /// Path to the notes file used by the notes tool.
    pub notes_path: PathBuf,
    /// Path to the MCP configuration file.
    pub mcp_config_path: PathBuf,
    /// Directory containing discoverable skills.
    pub skills_dir: PathBuf,
    /// Sources injected as `<instructions source="…">` blocks in the system
    /// prompt (#454). Each entry is either a disk path (read at render time)
    /// or an inline string. Loaded in declared order from the user's
    /// `instructions = [...]` config or constructed by embedders.
    ///
    /// Generalized from `Vec<PathBuf>` so embedders can inject inline content
    /// without staging a disk file. `From<PathBuf>` impl keeps existing callers
    /// working with `.into()` at the call site.
    pub instructions: Vec<InstructionSource>,
    /// Optional full system-prompt override from config/CLI. Replaces the
    /// assembled default prompt; append sections still render after it.
    pub override_system_prompt: Option<String>,
    /// Optional custom system prompt, lower priority than role/runtime override.
    pub custom_system_prompt: Option<String>,
    /// Optional coordinator-specific system prompt override.
    pub coordinator_system_prompt: Option<String>,
    /// Optional agent-specific system prompt override.
    pub agent_system_prompt: Option<String>,
    /// Additional prompt sections appended after the selected prompt base.
    pub append_system_prompts: Vec<PromptAppendSource>,
    /// Dynamic cache breaker appended for debugging provider prefix behavior.
    pub cache_breaker: Option<String>,
    pub project_context_pack_enabled: bool,
    /// When true, the model is instructed to respond in the current locale
    /// and a post-hoc translation layer replaces remaining English output.
    pub translation_enabled: bool,
    /// Whether user-visible transcript rendering shows thinking blocks.
    /// Prompt assembly uses this to avoid localizing hidden reasoning.
    pub show_thinking: bool,
    /// Maximum number of assistant steps before stopping.
    pub max_steps: u32,
    /// Maximum number of concurrently active subagents.
    pub max_subagents: usize,
    /// Feature flags controlling tool availability.
    pub features: Features,
    /// Auto-compaction settings for long conversations.
    ///
    /// High-level summarization compaction is enabled by default and uses
    /// provider-neutral context-window thresholds. The same config is also used
    /// for the per-tool-result truncation path (`compact_tool_result_for_context`)
    /// and by direct embedders that override compaction behavior.
    pub compaction: CompactionConfig,
    /// Checkpoint-restart cycle settings (issue #124).
    pub cycle: CycleConfig,
    /// Capacity-controller settings.
    pub capacity: CapacityControllerConfig,
    /// Shared Todo list state.
    pub todos: SharedTodoList,
    /// Shared Plan state.
    pub plan_state: SharedPlanState,
    /// Shared plan mode state for model-initiated plan transitions.
    pub plan_mode_state: SharedPlanModeState,
    /// Shared runtime goal state for model-visible goal tools.
    pub goal_state: SharedGoalState,
    /// Shared worktree isolation state for enter_worktree/exit_worktree tools.
    pub worktree_state: SharedWorktreeSessionState,
    /// Maximum sub-agent recursion depth (default 3). See
    /// `SubAgentRuntime::max_spawn_depth`. Override via
    /// `[runtime] max_spawn_depth = N` in `~/.deepseek/config.toml`.
    pub max_spawn_depth: u32,
    /// Per-domain network policy decider (#135). Shared across the session so
    /// session-scoped approvals (`/network allow <host>`) persist for the
    /// remainder of the run.
    pub network_policy: Option<NetworkPolicyDecider>,
    /// Whether to take side-git workspace snapshots before/after each turn.
    pub snapshots_enabled: bool,
    /// Maximum workspace size (in bytes) before snapshots self-disable on
    /// first init. `0` disables the cap. Resolved from
    /// `[snapshots] max_workspace_gb` × 1 GB at engine construction.
    pub snapshots_max_workspace_bytes: u64,
    /// Post-edit LSP diagnostics injection (#136). When `None`, the engine
    /// constructs a disabled manager so the field is always present.
    pub lsp_config: Option<LspConfig>,
    /// Task V2 manager for conversation-scoped task tracking.
    pub task_v2_manager: Option<SharedTaskV2Manager>,
    /// Per-role/type sub-agent model overrides already resolved from config.
    pub subagent_model_overrides: HashMap<String, String>,
    /// Whether the user-memory feature is enabled (#489). When `true` the
    /// engine reads `memory_path` on each prompt assembly and prepends a
    /// `<user_memory>` block to the system prompt.
    pub memory_enabled: bool,
    /// Path to the user memory file (#489). Always populated; only
    /// consulted when `memory_enabled` is `true`.
    pub memory_path: PathBuf,
    /// Whether Knowledge On Demand is enabled. When `true`, the engine
    /// uses a directory-based memory system with frontmatter-parsed files
    /// and async prefetch, replacing the legacy single-file `<user_memory>`.
    pub kod_enabled: bool,
    /// Path to the memory directory for KoD. Only consulted when
    /// `kod_enabled` is `true`.
    pub memory_dir: PathBuf,
    pub vision_config: Option<VisionModelConfig>,
    pub goal_objective: Option<String>,
    /// Tool restriction from custom slash command frontmatter.
    /// `None` means the current turn may use the normal tool set.
    pub allowed_tools: Option<Vec<String>>,
    /// Resolved BCP-47 locale tag (e.g. `"en"`, `"zh-Hans"`, `"ja"`)
    /// for the `## Environment` block in the system prompt. The
    /// caller resolves this from `Settings` once at engine
    /// construction; the engine never touches disk for it.
    pub locale_tag: String,
    /// When true, force `tool_choice: "required"` and opt compatible function
    /// schemas into DeepSeek beta strict mode.
    pub strict_tool_mode: bool,
    /// Workshop / large-tool-output routing (#548). `None` disables routing.
    pub workshop: Option<WorkshopConfig>,
    /// Which search backend `web_search` should use. Default: DuckDuckGo.
    pub search_provider: SearchProvider,
    /// API key for Tavily, Bocha, Metaso, or Baidu. `None` for Bing or DuckDuckGo.
    /// Metaso also falls back to `METASO_API_KEY` env var, then a built-in key.
    /// Baidu also falls back to `BAIDU_SEARCH_API_KEY`.
    pub search_api_key: Option<String>,
    /// Per-step DeepSeek API timeout for sub-agent `create_message` requests.
    /// Resolved from `[subagents] api_timeout_secs` (clamped to 1..=1800)
    /// once at engine construction, then threaded onto every
    /// `SubAgentRuntime` the engine builds (#1806, #1808).
    pub subagent_api_timeout: Duration,
    /// Native tools that should stay in the model-visible catalog even when
    /// they are outside the small default core surface (#2076).
    pub tools_always_load: HashSet<String>,
    /// When true and `/usr/bin/bwrap` is present on Linux, route exec_shell
    /// through bubblewrap instead of relying solely on Landlock (#2184).
    #[allow(dead_code)] // Wired through ShellManager in follow-up PR
    pub prefer_bwrap: bool,
    /// Effective runtime sandbox controls.
    pub sandbox_runtime: SandboxRuntimeConfig,
    /// Tool override and plugin configuration (`[tools]` table in config.toml).
    /// Applied to the per-turn tool registry after built-in tools are registered.
    /// When `None`, no overrides or plugin loading occurs.
    pub tools: Option<ToolsConfig>,
    /// Shared team context for multi-agent coordination.
    /// `None` disables AgentTeams runtime services. `Some(Mutex(None))` means
    /// AgentTeams is available but no team is active yet.
    pub team_context: Option<SharedTeamContext>,
    // Note: `runtime_services` (RuntimeToolServices) and `hooks`
    // (HookExecutor) are intentionally *not* stored on `EngineConfig` so it
    // stays portable to `codesmith-agent-runtime`. They are host-injected by
    // the embedding binary (TUI `EngineHost`, future `Arc<dyn HostServices>`)
    // — see `host.runtime_services` and `host.hooks` in the engine.
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_TEXT_MODEL.to_string(),
            workspace: PathBuf::from("."),
            allow_shell: true,
            trust_mode: false,
            notes_path: PathBuf::from("notes.txt"),
            mcp_config_path: PathBuf::from("mcp.json"),
            skills_dir: skills::default_skills_dir(),
            instructions: Vec::new(),
            override_system_prompt: None,
            custom_system_prompt: None,
            coordinator_system_prompt: None,
            agent_system_prompt: None,
            append_system_prompts: Vec::new(),
            cache_breaker: None,
            project_context_pack_enabled: true,
            translation_enabled: false,
            show_thinking: true,
            max_steps: 100,
            max_subagents: DEFAULT_MAX_SUBAGENTS,
            features: Features::with_defaults(),
            compaction: CompactionConfig::default(),
            cycle: CycleConfig::default(),
            capacity: CapacityControllerConfig::default(),
            todos: new_shared_todo_list(),
            plan_state: new_shared_plan_state(),
            plan_mode_state: new_shared_plan_mode_state(),
            goal_state: new_shared_goal_state(),
            worktree_state: new_shared_worktree_session_state(),
            max_spawn_depth: DEFAULT_MAX_SPAWN_DEPTH,
            network_policy: None,
            snapshots_enabled: true,
            snapshots_max_workspace_bytes: DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT,
            lsp_config: None,
            task_v2_manager: None,
            subagent_model_overrides: HashMap::new(),
            memory_enabled: false,
            memory_path: PathBuf::from("./memory.md"),
            kod_enabled: false,
            memory_dir: PathBuf::from("./memory"),
            vision_config: None,
            strict_tool_mode: false,
            goal_objective: None,
            allowed_tools: None,
            locale_tag: "en".to_string(),
            workshop: None,
            search_provider: SearchProvider::default(),
            search_api_key: None,
            subagent_api_timeout: Duration::from_secs(DEFAULT_SUBAGENT_API_TIMEOUT_SECS),
            tools_always_load: HashSet::new(),
            prefer_bwrap: false,
            sandbox_runtime: SandboxRuntimeConfig::default(),
            tools: None,
            team_context: None,
        }
    }
}
