//! Construction bridge for the engine.
//!
//! The `Engine` struct, its `impl` block, and the runtime submodules
//! (turn loop, dispatch, streaming, …) live in `codesmith-agent-runtime`.
//! This module re-exports them via a glob and supplies the terminal-coupled
//! construction layer that stays in `codesmith-tui`:
//!
//! - [`EngineHost`] — concrete host services (`ShellManager`, `LspManager`,
//!   `SubAgentManager`, …) the engine reaches through the `HostServices`
//!   trait.
//! - [`EngineHandle`] — UI-side mailbox for sending ops / approvals and
//!   receiving events.
//! - [`build_engine`] — assembles channels, the LLM client, the system
//!   prompt, and the wired host, then calls `Engine::new_runtime`.
//! - [`EngineConstruct`] — extension trait that restores the
//!   `Engine::new` / `new_with_client` / `new_with_host` constructor API
//!   the TUI (and its tests) call.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use codesmith_agent_runtime::host_services::{
    HostServices, ShellExecStatus, SpawnSubAgentRequest, StructuredStateRequest,
    TurnDispatchRequest,
};

// Explicit re-export of the engine items the TUI actually depends on. This
// replaces an earlier `pub use ...::engine::*` glob: the lists below are the
// auditable contract of what crosses the AR→TUI boundary. `use super::*` in
// the TUI submodules below — and `use crate::core::engine::*` elsewhere in
// the TUI — see exactly this surface (plus the local items defined further
// down in this file). Grouped by the AR engine submodule each item is
// re-exported from; keep it in sync with `crates/agent-runtime/src/engine/mod.rs`.

// Production surface — referenced by non-test TUI code (this bridge, `handle`,
// `runtime_traits`, `ui`, …). These items MUST stay `pub` in AR's engine
// module (see C7-2).
pub use codesmith_agent_runtime::engine::{
    CancelReason, Engine, EngineConfig, ApprovalDecision, UserInputDecision,
    build_model_tool_catalog, compact_tool_result_for_context, goal_objective_for_prompt,
    system_prompt_hash,
};

// Test-only surface — referenced exclusively from `#[cfg(test)]` modules
// (`engine/tests.rs`, `prompts.rs` tests, …). Gated so the production build
// does not flag them as unused imports.
#[cfg(test)]
pub use codesmith_agent_runtime::engine::{
    // top-level engine fn
    default_active_native_tool_names,
    // context
    COMPACTION_SUMMARY_MARKER, TURN_MAX_OUTPUT_TOKENS, context_input_budget,
    context_input_budget_for_provider, effective_max_output_tokens,
    effective_max_output_tokens_for_provider, extract_compaction_summary_prompt,
    is_context_length_error_message,
    // dispatch
    ToolExecOutcome, ToolExecutionBatch, ToolExecutionPlan, caller_allowed_for_tool,
    final_tool_input, format_tool_error, plan_tool_execution_batches,
    should_force_update_plan_first, should_parallelize_tool_batch, should_stop_after_plan_tool,
    // lsp_hooks
    edited_paths_for_tool,
    // streaming
    FAKE_WRAPPER_NOTICE, MAX_STREAM_ERRORS_BEFORE_FAIL, MAX_TRANSPARENT_STREAM_RETRIES,
    TOOL_CALL_START_MARKERS, ToolUseState, contains_fake_tool_wrapper,
    filter_tool_call_delta, should_transparently_retry_stream,
    // tool_catalog
    CODE_EXECUTION_TOOL_NAME, TOOL_SEARCH_BM25_NAME, TOOL_SEARCH_REGEX_NAME,
    active_tools_for_step, ensure_advanced_tooling, execute_code_execution_tool,
    execute_tool_search, initial_active_tools, maybe_activate_requested_deferred_tool,
    maybe_hydrate_requested_deferred_tool, missing_tool_error_message,
    preflight_requested_deferred_tool, should_default_defer_tool,
};

use crate::config::{ApiProvider, Config};
use crate::features::Feature;
use crate::llm_client::LlmClientHandle;
use codesmith_agent::provider::{ProviderConfig, ProviderId};
use crate::prompts;
use crate::seam_manager::{SeamConfig, SeamManager};
use crate::tools::plan::SharedPlanState;
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager, wrap_shell_manager};
use crate::tools::spec::RuntimeToolServices;
use crate::tools::subagent::{
    SharedSubAgentManager, SubAgentCompletion, new_shared_subagent_manager,
};
use crate::tools::todo::SharedTodoList;
use crate::tools::{ToolContext, ToolRegistryBuilder, ToolRegistryPluginExt};
use crate::tui::app::AppMode;
use crate::utils::spawn_supervised;

use super::capacity::CapacityController;
use super::events::{Event, TurnOutcomeStatus};
use super::ops::Op;
use super::session::Session;

// === EngineHandle ===

/// Handle to communicate with the engine.
///
/// The mailbox API (`send_op`, `cancel`, …) lives in `engine/handle.rs`.
#[derive(Clone)]
pub struct EngineHandle {
    /// Send operations to the engine
    pub tx_op: mpsc::Sender<Op>,
    /// Receive events from the engine
    pub rx_event: Arc<RwLock<mpsc::Receiver<Event>>>,
    /// Shared pointer to the cancellation token for the current request.
    pub cancel_token: Arc<StdMutex<CancellationToken>>,
    /// Latched reason for the most recent cancellation. Read by the
    /// approval / user-input handlers to enrich their error strings.
    /// Cleared by the engine when a fresh turn starts.
    pub cancel_reason: Arc<StdMutex<Option<CancelReason>>>,
    /// Send approval decisions to the engine
    pub tx_approval: mpsc::Sender<ApprovalDecision>,
    /// Send user input responses to the engine
    pub tx_user_input: mpsc::Sender<UserInputDecision>,
    /// Send steer input for an in-flight turn.
    pub tx_steer: mpsc::Sender<String>,
    /// §F1 — bound extension runner, surfaced from `build_extension_runtime`
    /// so `/extension status` / `/extension reload` (in `extension_commands`)
    /// can read + invalidate it without an engine round-trip. `None` when no
    /// extensions were built (embed path / pre-engine). Cloning the `Arc` is
    /// cheap; the runner itself is shared with the per-turn `HostAgentExecutor`.
    pub extension_runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
}

// `impl EngineHandle { ... }` lives in `engine/handle.rs`.

// === EngineHost ===

/// Host-injected runtime services the engine needs but whose concrete types
/// (`ShellManager`, `TaskManager`, `AutomationManager`, `HookExecutor`, …)
/// stay terminal-side. Kept out of `EngineConfig` so `EngineConfig` can live
/// in `codesmith-agent-runtime` without dragging ~10k lines of OS-bridging
/// managers across the crate boundary.
///
/// The engine holds this behind an `Arc<dyn HostServices>` trait object;
/// `impl HostServices for EngineHost` lives in `engine/runtime_traits.rs`.
#[derive(Debug, Clone)]
pub struct EngineHost {
    /// Durable runtime services exposed to model-visible tools.
    pub runtime_services: RuntimeToolServices,
    /// Hook executor for `pre_compact` (and future compaction-related) hooks.
    pub hooks: Option<crate::hooks::HookExecutor>,
    /// Post-edit LSP diagnostics manager. Defaults to a disabled manager;
    /// `build_engine` replaces it with the config-resolved one.
    pub lsp_manager: std::sync::Arc<crate::lsp::LspManager>,
    /// Flash seam (layered-context) manager, when configured. `None` when the
    /// feature is disabled.
    pub seam_manager: Option<SeamManager>,
    /// Background shell process manager. `Some` when the caller shares its
    /// shell handle (TUI app); `None` for headless paths, which get a fresh
    /// manager bound to the configured workspace.
    pub shell_manager: Option<SharedShellManager>,
    /// Sub-agent process manager.
    pub subagent_manager: SharedSubAgentManager,
    /// Session-scoped workshop variable store (#548). `None` when no
    /// `[workshop]` config is present.
    pub workshop_vars: Option<
        std::sync::Arc<tokio::sync::Mutex<crate::tools::large_output_router::WorkshopVariables>>,
    >,
    /// External sandbox backend (#516). `None` when no backend is configured.
    pub sandbox_backend: Option<std::sync::Arc<dyn crate::sandbox::backend::SandboxBackend>>,
}

impl Default for EngineHost {
    fn default() -> Self {
        Self {
            runtime_services: RuntimeToolServices::default(),
            hooks: None,
            lsp_manager: std::sync::Arc::new(crate::lsp::LspManager::disabled()),
            seam_manager: None,
            // `shell_manager` stays `None` here: callers that share their
            // shell (TUI app) set `Some` explicitly; headless paths leave it
            // `None` so `build_engine` creates a fresh manager bound to the
            // configured workspace.
            shell_manager: None,
            subagent_manager: new_shared_subagent_manager(
                std::path::PathBuf::new(),
                crate::config::MAX_SUBAGENTS,
            ),
            workshop_vars: None,
            sandbox_backend: None,
        }
    }
}

// === Submodules ===

mod handle;
mod runtime_traits;
pub(crate) mod tool_setup;

#[cfg(test)]
mod tests;

// === Plugin tool discovery ===

fn default_plugin_tools_dir() -> PathBuf {
    codesmith_config::codesmith_home()
        .unwrap_or_else(|_| {
            dirs::home_dir().map_or_else(|| PathBuf::from(".codesmith"), |h| h.join(".codesmith"))
        })
        .join("tools")
}

fn plugin_tools_dir(tools_config: Option<&crate::config::ToolsConfig>) -> PathBuf {
    if let Some(tools_config) = tools_config
        && let Some(custom_dir) = tools_config.plugin_dir.as_deref()
    {
        return PathBuf::from(shellexpand::tilde(custom_dir).as_ref());
    }
    default_plugin_tools_dir()
}

fn configure_plugin_tools(
    tool_registry: &mut crate::tools::ToolRegistry,
    tools_config: Option<&crate::config::ToolsConfig>,
) -> std::collections::HashSet<String> {
    let names_before: std::collections::HashSet<String> = tool_registry
        .names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    let plugin_dir = plugin_tools_dir(tools_config);
    tool_registry.load_plugins(&plugin_dir);

    if let Some(tools_config) = tools_config
        && let Some(ref overrides) = tools_config.overrides
    {
        tool_registry.apply_overrides(overrides, &plugin_dir);
    }

    let names_after: std::collections::HashSet<String> = tool_registry
        .names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    &names_after - &names_before
}

// === Construction ===

/// Recovery hint appended to auth errors when the rejected key came from an
/// environment variable and no saved config key is present. Construction-side
/// helper (takes the TUI `Config`); kept here because it names `ApiProvider`
/// variants that are TUI-coupled.
fn env_only_api_key_recovery_hint(api_config: &Config) -> Option<String> {
    if !crate::config::active_provider_uses_env_only_api_key(api_config) {
        return None;
    }

    let provider = api_config.api_provider();
    let env_var = match provider {
        ApiProvider::Deepseek => "DEEPSEEK_API_KEY",
        ApiProvider::NvidiaNim => "NVIDIA_API_KEY/NVIDIA_NIM_API_KEY",
        ApiProvider::Openai => "OPENAI_API_KEY",
        ApiProvider::Atlascloud => "ATLASCLOUD_API_KEY",
        ApiProvider::WanjieArk => "WANJIE_ARK_API_KEY/WANJIE_API_KEY/WANJIE_MAAS_API_KEY",
        ApiProvider::Volcengine => "VOLCENGINE_API_KEY/VOLCENGINE_ARK_API_KEY/ARK_API_KEY",
        ApiProvider::Openrouter => "OPENROUTER_API_KEY",
        ApiProvider::XiaomiMimo => "XIAOMI_MIMO_API_KEY/XIAOMI_API_KEY/MIMO_API_KEY",
        ApiProvider::Novita => "NOVITA_API_KEY",
        ApiProvider::Fireworks => "FIREWORKS_API_KEY",
        ApiProvider::Siliconflow => "SILICONFLOW_API_KEY",
        ApiProvider::Moonshot => "MOONSHOT_API_KEY/KIMI_API_KEY",
        ApiProvider::Sglang => "SGLANG_API_KEY",
        ApiProvider::Vllm => "VLLM_API_KEY",
        ApiProvider::Ollama => "OLLAMA_API_KEY",
        ApiProvider::Anthropic => "ANTHROPIC_API_KEY/CLAUDE_API_KEY",
    };

    Some(format!(
        "The rejected key came from {env_var}; no saved config key is present.\n\
         Run `codesmith auth status` to inspect credential sources, then \
         `codesmith auth set --provider {provider}` to save a valid key in ~/.codesmith/config.toml, \
         or remove the stale export and open a fresh shell.",
        provider = provider.as_str()
    ))
}

// === Provider registry wiring ===

/// Resolve the LLM client for `api_config` through a [`ProviderRegistry`]
/// seeded with the compiled-in rig-backed providers.
///
/// Builds the neutral [`ProviderConfig`] from the TUI `Config`'s six
/// construction fields, then delegates to `registry.build`. The engine never
/// names a concrete client type — that is the pluggability seam this slice
/// opens up.
///
/// Every provider — including the DeepSeek family — resolves to a rig-backed
/// factory from [`codesmith_providers::default_registry`]. Notably this
/// activates the **native Anthropic `/v1/messages` path**: previously every
/// provider — including `provider = "anthropic"` — routed through the
/// OpenAI-shaped hand-written client, which sent Anthropic config to
/// `/chat/completions` with bearer auth (the wrong endpoint). The rig
/// `AnthropicFactory` + `AnthropicShaper` now carries the native messages API
/// with per-block `cache_control` (verified against rig-core's serialization).
///
/// DeepSeek's thinking-mode `reasoning_content` replay (the last holdout for
/// the tui-local client) is handled by the rig adapter's `shape_messages`
/// (strip / `(reasoning omitted)` placeholder injection — #1542 / #1739 /
/// #1694) plus rig's faithful `reasoning_content` serialization for the OpenAI
/// / DeepSeek providers. See ROADMAP §A1 / §D1.
pub(crate) fn resolve_llm_client(api_config: &Config) -> anyhow::Result<LlmClientHandle> {
    // §D2 — when `custom_provider` is set, route to `ProviderId::Custom(id)`
    // so a host-registered factory (e.g. `mock`, or a user crate's factory)
    // is selected by id. The neutral `ProviderConfig` fields (api_key /
    // base_url / default_model / http_headers) are resolved by the `Config`
    // accessors, which already read from the matching `[[providers.custom]]`
    // entry for the custom path; only the `provider` id differs here.
    let provider = match api_config.custom_provider() {
        Some(id) => ProviderId::Custom(id.to_string()),
        None => ProviderId::from(api_config.api_provider().as_str()),
    };
    let cfg = ProviderConfig {
        provider,
        api_key: api_config.deepseek_api_key()?,
        base_url: api_config.deepseek_base_url(),
        default_model: api_config.default_model(),
        retry: codesmith_agent::llm_client::RetryConfig::from(api_config.retry_policy()),
        http_headers: api_config.http_headers(),
        on_retry: None,
    };
    codesmith_providers::default_registry().build(&cfg)
}

// === §F1 Extension runtime wiring ===

/// Discover compiled-in extensions, reconcile with the on-disk
/// [`ExtensionStateStore`](crate::extension_state::ExtensionStateStore)
/// (skip disabled), load + configure each against a stub
/// [`ExtensionApi`](codesmith_extensions::ExtensionApi), then `bind_core`
/// the host context. Returns the bound runner — cloned into each fresh
/// per-turn `HostAgentExecutor` (via `with_extension_runner`) AND surfaced
/// on [`EngineHandle::extension_runner`] for the `/extension` commands.
///
/// Mirrors the spec §6.1 reload sequence (steps 2-5): re-discover →
/// reconcile → re-load → re-configure → bind_core. Slice 1 does NOT
/// re-discover on `/extension reload` (§F2 wires live reload); this fn
/// runs once at engine build.
///
/// The async `Extension::configure` calls are driven on a fresh
/// single-thread runtime spawned on a plain OS thread (see the inline
/// rationale at step 3) rather than `tokio::task::block_in_place`: the
/// latter is only valid inside a multi-thread runtime (the TUI's
/// `#[tokio::main]` is multi-thread so it works in prod, but
/// `#[tokio::test]` defaults to `current_thread` and would panic), and
/// creating + dropping a nested runtime from a runtime worker thread
/// panics on shutdown. The OS-thread approach works in both. §F2 may
/// harden to share the host runtime.
fn build_extension_runtime(
    workspace: &std::path::Path,
    cancel_token: tokio_util::sync::CancellationToken,
) -> Arc<codesmith_extensions::ExtensionRunner> {
    let runner = Arc::new(codesmith_extensions::ExtensionRunner::new());
    let state = crate::extension_state::ExtensionStateStore::load_default()
        .unwrap_or_default();

    // 1. Discover compiled-in extensions (inventory).
    let discovered = codesmith_extensions::discover_static();

    // 2. Reconcile with state: skip disabled.
    let enabled: Vec<_> = discovered
        .into_iter()
        .filter(|reg| state.is_enabled(reg.metadata.id))
        .collect();

    // 3. Load + configure each against the stub api (best-effort; §F2 logs).
    //    The async `Extension::configure` is driven on a fresh single-thread
    //    runtime spawned on a plain OS thread: creating + dropping a tokio
    //    runtime from within another runtime's worker thread panics on
    //    shutdown (tokio blocking/shutdown.rs); and `tokio::task::block_in_place`
    //    is only valid inside a multi-thread runtime (the TUI's
    //    `#[tokio::main]` is multi-thread so it works in prod, but
    //    `#[tokio::test]` defaults to `current_thread` and would panic). The
    //    spawned thread owns the runtime's lifetime cleanly; `std::thread::scope`
    //    blocks until it completes. Skipped entirely when nothing's enabled
    //    (slice 1 pre-T10: no compiled-in extensions → tests stay fast + panic-free).
    if !enabled.is_empty() {
        let runner_for_thread = runner.clone();
        std::thread::scope(|s| {
            s.spawn(move || {
                let load_rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("extension load runtime");
                for reg in enabled {
                    let ext = (reg.factory)();
                    let _ = load_rt.block_on(runner_for_thread.load(&*ext));
                }
            });
        });
    }

    // 4. Build the host context + bind_core. The `idle` flag + the engine's
    //    `cancel_token` are shared so handlers observe host state + cancel.
    let idle = Arc::new(std::sync::Mutex::new(true));
    let ctx = Arc::new(codesmith_extensions::HostExtensionContext::new(
        workspace.to_path_buf(),
        codesmith_agent::extension::ExtensionMode::Tui,
        idle,
        cancel_token,
        runner.generation_arc(),
    ));
    runner.bind_core(ctx);

    runner
}

/// Assemble an [`Engine`] from TUI-coupled construction state.
///
/// Creates the op / event / approval / user-input / steer / subagent
/// channels, resolves the LLM client, builds the system prompt, wires the
/// concrete host managers (shell, subagent, seam, LSP, workshop, sandbox,
/// background-task registry), then delegates struct assembly to
/// [`Engine::new_runtime`] in `codesmith-agent-runtime`.
#[allow(clippy::too_many_arguments)]
pub fn build_engine(
    mut config: EngineConfig,
    api_config: &Config,
    injected_client: Option<LlmClientHandle>,
    mut host: EngineHost,
) -> (Engine, EngineHandle) {
    let (tx_op, rx_op) = mpsc::channel(32);
    let (tx_event, rx_event) = mpsc::channel(256);
    let (tx_approval, rx_approval) = mpsc::channel(64);
    let (tx_user_input, rx_user_input) = mpsc::channel(32);
    let (tx_steer, rx_steer) = mpsc::channel(64);
    let (tx_subagent_completion, rx_subagent_completion) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let shared_cancel_token = Arc::new(StdMutex::new(cancel_token.clone()));
    let cancel_reason: Arc<StdMutex<Option<CancelReason>>> = Arc::new(StdMutex::new(None));
    let tool_exec_lock = Arc::new(RwLock::new(()));

    // §F1 — build the extension runtime + bind to the host executor. Shares
    // the engine's `cancel_token` so handlers observe user-initiated ESC.
    let extension_runner = build_extension_runtime(&config.workspace, cancel_token.clone());

    if config.features.enabled(Feature::AgentTeams) {
        let team_context = config
            .team_context
            .clone()
            .or_else(|| host.runtime_services.team_context.clone())
            .unwrap_or_else(crate::tools::team::new_shared_team_context);
        config.team_context = Some(team_context.clone());
        host.runtime_services.team_context = Some(team_context);
        if host.runtime_services.permission_request_registry.is_none() {
            host.runtime_services.permission_request_registry =
                Some(crate::tools::team::new_shared_permission_registry());
        }
    }

    // Create the LLM client via the provider registry (abstraction/impl seam).
    // `injected_client` (tests) short-circuits; otherwise resolve through a
    // `ProviderRegistry` so the engine no longer names a concrete client type.
    let (llm_client, llm_client_error) = match injected_client {
        Some(client) => (Some(client), None),
        None => match resolve_llm_client(api_config) {
            Ok(client) => (Some(client), None),
            Err(err) => (None, Some(err.to_string())),
        },
    };
    let api_key_env_only_recovery = env_only_api_key_recovery_hint(api_config);

    let mut session = Session::new(
        config.model.clone(),
        config.workspace.clone(),
        config.allow_shell,
        config.trust_mode,
        config.notes_path.clone(),
        config.mcp_config_path.clone(),
    );
    // Set up stable system prompt with project context (default to agent mode).
    let (user_memory_block, knowledge_prompt_block) = if config.kod_enabled {
        let kod_block = crate::memory::compose_kod_block(&config.memory_dir);
        match kod_block {
            Some(block) => (None, Some(block)),
            None => (
                crate::memory::compose_block(config.memory_enabled, &config.memory_path),
                None,
            ),
        }
    } else {
        (
            crate::memory::compose_block(config.memory_enabled, &config.memory_path),
            None,
        )
    };
    let prompt_goal_objective =
        goal_objective_for_prompt(config.goal_objective.as_deref(), &config.goal_state);
    let runtime_context = prompts::PromptSessionContext {
        user_memory_block: user_memory_block.as_deref(),
        knowledge_prompt_block: knowledge_prompt_block.as_deref(),
        goal_objective: prompt_goal_objective.as_deref(),
        project_context_pack_enabled: config.project_context_pack_enabled,
        locale_tag: &config.locale_tag,
        translation_enabled: config.translation_enabled,
        model_id: &config.model,
        show_thinking: config.show_thinking,
        skills_block: crate::skills::render_available_skills_context_for_workspace(
            &config.workspace,
        )
        .or_else(|| {
            Some(config.skills_dir.as_path())
                .and_then(crate::skills::render_available_skills_context)
        }),
    }
    .runtime();
    let system_prompt =
        prompts::effective_prompt_bundle_for_mode_with_runtime_context_and_approval(
            AppMode::Agent,
            &config.workspace,
            None,
            Some(&config.skills_dir),
            Some(&config.instructions),
            prompts::PromptRuntimeContext {
                override_system_prompt: config.override_system_prompt.as_deref(),
                custom_system_prompt: config.custom_system_prompt.as_deref(),
                coordinator_system_prompt: config.coordinator_system_prompt.as_deref(),
                agent_system_prompt: config.agent_system_prompt.as_deref(),
                append_system_prompts: &config.append_system_prompts,
                cache_breaker: config.cache_breaker.as_deref(),
                ..runtime_context
            },
            session.approval_mode,
        )
        .render_system_prompt();
    let stable_prompt = Some(system_prompt);
    session.last_system_prompt_hash = Some(system_prompt_hash(stable_prompt.as_ref()));
    session.system_prompt = stable_prompt;

    // Initialize prefix-cache stability monitor (lazy-pin).
    let _ = session
        .prefix_stability
        .get_or_insert_with(|| crate::prefix_cache::PrefixStabilityManager::new_unpinned());

    let subagent_manager =
        new_shared_subagent_manager(config.workspace.clone(), config.max_subagents);
    let shell_manager = host
        .shell_manager
        .clone()
        .unwrap_or_else(|| new_shared_shell_manager(config.workspace.clone()));
    if let Ok(mut manager) = shell_manager.lock() {
        manager.set_prefer_bwrap(config.sandbox_runtime.prefer_bwrap || config.prefer_bwrap);
        manager.set_sandbox_runtime(config.sandbox_runtime.clone());
    }
    if host.runtime_services.shell_manager.is_none() {
        host.runtime_services.shell_manager = Some(wrap_shell_manager(shell_manager.clone()));
    }
    let capacity_controller =
        Arc::new(StdMutex::new(CapacityController::new(config.capacity.clone())));

    // Create Flash seam manager for layered context (#159).
    let seam_manager = llm_client.as_ref().map(|main_client| {
        let seam_config = SeamConfig {
            enabled: api_config.context.enabled.unwrap_or(false),
            verbatim_window_turns: api_config
                .context
                .verbatim_window_turns
                .unwrap_or(crate::seam_manager::VERBATIM_WINDOW_TURNS),
            l1_threshold: api_config
                .context
                .l1_threshold
                .unwrap_or(crate::seam_manager::DEFAULT_L1_THRESHOLD),
            l2_threshold: api_config
                .context
                .l2_threshold
                .unwrap_or(crate::seam_manager::DEFAULT_L2_THRESHOLD),
            l3_threshold: api_config
                .context
                .l3_threshold
                .unwrap_or(crate::seam_manager::DEFAULT_L3_THRESHOLD),
            cycle_threshold: api_config
                .context
                .cycle_threshold
                .unwrap_or(crate::seam_manager::DEFAULT_CYCLE_THRESHOLD),
            seam_model: api_config
                .context
                .seam_model
                .clone()
                .unwrap_or_else(|| crate::seam_manager::DEFAULT_SEAM_MODEL.to_string()),
        };
        SeamManager::new(main_client.clone(), seam_config)
    });
    host.seam_manager = seam_manager;

    host.lsp_manager = Arc::new(match config.lsp_config.clone() {
        Some(cfg) => crate::lsp::LspManager::new(cfg, config.workspace.clone()),
        None => crate::lsp::LspManager::disabled(),
    });

    // Workshop variable store (#548).
    let workshop_vars: Option<
        std::sync::Arc<tokio::sync::Mutex<crate::tools::large_output_router::WorkshopVariables>>,
    > = if config.workshop.is_some() {
        Some(std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::tools::large_output_router::WorkshopVariables::default(),
        )))
    } else {
        None
    };

    // External sandbox backend (#516).
    let sandbox_backend = crate::sandbox::backend::create_backend(api_config)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to create sandbox backend: {e}");
            None
        })
        .map(std::sync::Arc::from);

    let bg_registry_shell = shell_manager.clone();
    let bg_registry_agent = subagent_manager.clone();
    let bg_data_dir =
        dirs::home_dir().map_or_else(|| PathBuf::from(".codesmith"), |h| h.join(".codesmith"));
    let bg_registry = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::background_task::BackgroundTaskRegistry::new(
            bg_registry_shell,
            bg_registry_agent,
            None,
            bg_data_dir,
        ),
    ));
    host.runtime_services.background_task_registry = Some(std::sync::Arc::new(
        runtime_traits::BgRegistryHost(bg_registry),
    ));

    host.shell_manager = Some(shell_manager);
    host.subagent_manager = subagent_manager;
    host.workshop_vars = workshop_vars;
    host.sandbox_backend = sandbox_backend;

    let api_provider = api_config.api_provider();
    // Wrap the wired host behind the `HostServices` trait object.
    let host_concrete: Arc<EngineHost> = Arc::new(host);
    let host: Arc<dyn HostServices> = host_concrete.clone();

    let engine = Engine::new_runtime(
        config,
        host,
        llm_client,
        llm_client_error,
        api_key_env_only_recovery,
        session,
        api_provider,
        rx_op,
        rx_approval,
        rx_user_input,
        rx_steer,
        tx_event,
        tx_subagent_completion,
        rx_subagent_completion,
        cancel_token,
        shared_cancel_token.clone(),
        cancel_reason.clone(),
        tool_exec_lock,
        capacity_controller,
        tx_op.clone(),
        Arc::new(runtime_traits::TuiRuntimeUi),
        Some(extension_runner.clone()),
    );

    let handle = EngineHandle {
        tx_op,
        rx_event: Arc::new(RwLock::new(rx_event)),
        cancel_token: shared_cancel_token,
        cancel_reason,
        tx_approval,
        tx_user_input,
        tx_steer,
        extension_runner: Some(extension_runner),
    };

    (engine, handle)
}

// === Constructor extension trait ===

/// Extension trait that restores the `Engine::new` / `new_with_client` /
/// `new_with_host` constructor API on the runtime-crate `Engine`.
///
/// Once `Engine` moved to `codesmith-agent-runtime`, inherent `impl Engine`
/// blocks can no longer live in `codesmith-tui` (orphan rule). The
/// construction logic — which names TUI types (`Config`, `EngineHost`) — stays
/// here as a local trait impl, which the orphan rule permits because the
/// trait is local to this crate.
pub trait EngineConstruct {
    /// Create a new engine with a default [`EngineHost`].
    fn new(config: EngineConfig, api_config: &Config) -> (Engine, EngineHandle);

    /// Create a new engine with an injected LLM client (for integration tests).
    fn new_with_client(
        config: EngineConfig,
        api_config: &Config,
        client: LlmClientHandle,
    ) -> (Engine, EngineHandle);

    /// Create a new engine with host-injected runtime services.
    fn new_with_host(
        config: EngineConfig,
        api_config: &Config,
        host: EngineHost,
    ) -> (Engine, EngineHandle);

    /// Build the per-turn [`ToolContext`] for this engine (test helper).
    fn build_tool_context(&self, mode: AppMode, auto_approve: bool) -> ToolContext;

    /// Build the per-turn [`ToolRegistryBuilder`] for this engine (test helper).
    fn build_turn_tool_registry_builder(
        &self,
        mode: AppMode,
        todo_list: SharedTodoList,
        plan_state: SharedPlanState,
    ) -> ToolRegistryBuilder;

    /// Downcast the injected host to the concrete [`EngineHost`] (test helper).
    ///
    /// Replaces the removed `host_concrete: Arc<EngineHost>` field now that
    /// `Engine` lives in `codesmith-agent-runtime` and can no longer name the
    /// concrete TUI host type. Production code reaches the host through
    /// [`HostServices`]; only tests need the concrete view.
    fn host_concrete(&self) -> &EngineHost;
}

impl EngineConstruct for Engine {
    fn new(config: EngineConfig, api_config: &Config) -> (Engine, EngineHandle) {
        build_engine(config, api_config, None, EngineHost::default())
    }

    fn new_with_client(
        config: EngineConfig,
        api_config: &Config,
        client: LlmClientHandle,
    ) -> (Engine, EngineHandle) {
        build_engine(config, api_config, Some(client), EngineHost::default())
    }

    fn new_with_host(
        config: EngineConfig,
        api_config: &Config,
        host: EngineHost,
    ) -> (Engine, EngineHandle) {
        build_engine(config, api_config, None, host)
    }

    fn build_tool_context(&self, mode: AppMode, auto_approve: bool) -> ToolContext {
        let host = self.host_concrete();
        tool_setup::build_tool_context_for(
            host,
            &self.session,
            &self.config,
            mode,
            auto_approve,
            self.cancel_token.clone(),
            &self.runtime_ui,
        )
    }

    fn build_turn_tool_registry_builder(
        &self,
        mode: AppMode,
        todo_list: SharedTodoList,
        plan_state: SharedPlanState,
    ) -> ToolRegistryBuilder {
        tool_setup::build_turn_tool_registry_builder_for(
            &self.session,
            &self.config,
            &self.llm_client,
            mode,
            todo_list,
            plan_state,
        )
    }

    fn host_concrete(&self) -> &EngineHost {
        self.host
            .as_any()
            .downcast_ref::<EngineHost>()
            .expect("host_concrete requires a concrete EngineHost")
    }
}

// === Spawn ===

/// Spawn the engine in a background task.
///
/// `host` carries the terminal-side runtime services (`RuntimeToolServices`)
/// and hook executor that `EngineConfig` no longer holds directly.
pub fn spawn_engine(config: EngineConfig, api_config: &Config, host: EngineHost) -> EngineHandle {
    let (engine, handle) = Engine::new_with_host(config, api_config, host);

    spawn_supervised(
        "engine-event-loop",
        std::panic::Location::caller(),
        async move {
            engine.run().await;
        },
    );

    handle
}

// === Test helpers ===

#[cfg(test)]
pub(crate) struct MockEngineHandle {
    pub handle: EngineHandle,
    pub rx_op: mpsc::Receiver<Op>,
    rx_approval: mpsc::Receiver<ApprovalDecision>,
    pub rx_steer: mpsc::Receiver<String>,
    pub tx_event: mpsc::Sender<Event>,
    pub cancel_token: CancellationToken,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MockApprovalEvent {
    Approved {
        id: String,
    },
    Denied {
        id: String,
    },
    RetryWithPolicy {
        id: String,
        policy: crate::sandbox::SandboxPolicy,
    },
}

#[cfg(test)]
impl MockEngineHandle {
    pub(crate) async fn recv_approval_event(&mut self) -> Option<MockApprovalEvent> {
        match self.rx_approval.recv().await? {
            ApprovalDecision::Approved { id } => Some(MockApprovalEvent::Approved { id }),
            ApprovalDecision::Denied { id } => Some(MockApprovalEvent::Denied { id }),
            ApprovalDecision::RetryWithPolicy { id, policy } => {
                Some(MockApprovalEvent::RetryWithPolicy { id, policy })
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn mock_engine_handle() -> MockEngineHandle {
    let (tx_op, rx_op) = mpsc::channel(32);
    let (tx_event, rx_event) = mpsc::channel(256);
    let (tx_approval, rx_approval) = mpsc::channel(64);
    let (tx_user_input, _rx_user_input) = mpsc::channel(32);
    let (tx_steer, rx_steer) = mpsc::channel(64);
    let cancel_token = CancellationToken::new();
    let shared_cancel_token = Arc::new(StdMutex::new(cancel_token.clone()));
    let cancel_reason: Arc<StdMutex<Option<CancelReason>>> = Arc::new(StdMutex::new(None));
    let handle = EngineHandle {
        tx_op,
        rx_event: Arc::new(RwLock::new(rx_event)),
        cancel_token: shared_cancel_token,
        cancel_reason,
        tx_approval,
        tx_user_input,
        tx_steer,
        extension_runner: None,
    };

    MockEngineHandle {
        handle,
        rx_op,
        rx_approval,
        rx_steer,
        tx_event,
        cancel_token,
    }
}
