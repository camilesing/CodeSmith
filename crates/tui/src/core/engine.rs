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
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use codesmith_agent_runtime::host_services::HostServices;

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
    ApprovalDecision, CancelReason, Engine, EngineConfig, UserInputDecision, apply_tool_selection,
    build_model_tool_catalog, compact_tool_result_for_context, goal_objective_for_prompt,
    system_prompt_hash,
};

// Test-only surface — referenced exclusively from `#[cfg(test)]` modules
// (`engine/tests.rs`, `prompts.rs` tests, …). Gated so the production build
// does not flag them as unused imports.
#[cfg(test)]
pub use codesmith_agent_runtime::engine::{
    // tool_catalog
    CODE_EXECUTION_TOOL_NAME,
    // context
    COMPACTION_SUMMARY_MARKER,
    // streaming
    FAKE_WRAPPER_NOTICE,
    MAX_STREAM_ERRORS_BEFORE_FAIL,
    MAX_TRANSPARENT_STREAM_RETRIES,
    TOOL_CALL_START_MARKERS,
    TOOL_SEARCH_BM25_NAME,
    TOOL_SEARCH_REGEX_NAME,
    TURN_MAX_OUTPUT_TOKENS,
    // dispatch
    ToolExecOutcome,
    ToolExecutionBatch,
    ToolExecutionPlan,
    ToolUseState,
    active_tools_for_step,
    caller_allowed_for_tool,
    contains_fake_tool_wrapper,
    context_input_budget,
    context_input_budget_for_provider,
    // top-level engine fn
    default_active_native_tool_names,
    // lsp_hooks
    edited_paths_for_tool,
    effective_max_output_tokens,
    effective_max_output_tokens_for_provider,
    ensure_advanced_tooling,
    execute_code_execution_tool,
    execute_tool_search,
    extract_compaction_summary_prompt,
    filter_tool_call_delta,
    final_tool_input,
    format_tool_error,
    initial_active_tools,
    is_context_length_error_message,
    maybe_activate_requested_deferred_tool,
    maybe_hydrate_requested_deferred_tool,
    missing_tool_error_message,
    plan_tool_execution_batches,
    preflight_requested_deferred_tool,
    should_default_defer_tool,
    should_force_update_plan_first,
    should_parallelize_tool_batch,
    should_stop_after_plan_tool,
    should_transparently_retry_stream,
};

use crate::config::{ApiProvider, Config};
use crate::features::Feature;
use crate::llm_client::LlmClientHandle;
use crate::prompts;
use crate::seam_manager::{SeamConfig, SeamManager};
use crate::tools::large_output_router::UtilityLlm;
use crate::tools::plan::SharedPlanState;
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager, wrap_shell_manager};
use crate::tools::spec::RuntimeToolServices;
use crate::tools::subagent::{SharedSubAgentManager, new_shared_subagent_manager};
use crate::tools::todo::SharedTodoList;
use crate::tools::{ToolContext, ToolRegistryBuilder, ToolRegistryPluginExt};
use crate::tui::app::AppMode;
use crate::utils::spawn_supervised;
use codesmith_agent::provider::{ProviderConfig, ProviderId, SharedProviderRegistry};

use super::capacity::CapacityController;
use super::events::Event;
// Re-imported for the test module below, which picks it up via `use super::*`.
#[cfg(test)]
use super::events::TurnOutcomeStatus;
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
    /// §F script mod layer — mods discovered at engine build that await
    /// first-activation consent (passive TUI notice; `/mods list` is the
    /// durable query). A snapshot: watcher/tool reloads surface their own
    /// reports via logs + `/mods status`.
    pub mods_pending: Vec<crate::mod_ops::PendingModInfo>,
    /// Discipline 5 — the startup composition audit (loaded / failed-with-
    /// original-error / pending / disabled / trust-gated, one entry per
    /// discovered extension). `failed_audit_lines` is the display format.
    pub mods_audit: Vec<StartupAuditEntry>,
    /// Event-sourcing slice 4 — the engine session's fact ledger (std
    /// mutex), surfaced so the host can snapshot it into `SavedSession`
    /// and restore it on session load without an engine rebuild (the
    /// `extension_runner` precedent). Same `Arc` the engine's compaction
    /// feeds.
    pub fact_ledger: Arc<StdMutex<codesmith_agent_runtime::compaction::fact_ledger::FactLedger>>,
    /// Event-sourcing slice 6 — the session's recent-read-files working set
    /// (same `Arc` the executor's record site feeds), surfaced so session
    /// load can rebuild it from the transcript without an engine rebuild.
    pub recent_read_files:
        Arc<StdMutex<std::collections::VecDeque<codesmith_agent_runtime::session::RecentReadFile>>>,
    /// Capability composition point — the main-turn tool-catalog baseline
    /// (the `tools-change` diff source; the `/tools` readout), shared from
    /// `EngineHost` at build time so the TUI reads it without an engine
    /// round-trip.
    pub tool_catalog: crate::core::tool_catalog::SharedToolCatalog,
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
    /// Resolved `[utility_model]` handle for background assists (workshop
    /// synthesis, auto-route classification, seams). `None` when unconfigured
    /// or when a dedicated client could not be built — assists then use the
    /// main model.
    pub utility_llm: Option<crate::tools::large_output_router::UtilityLlm>,
    /// External sandbox backend (#516). `None` when no backend is configured.
    pub sandbox_backend: Option<std::sync::Arc<dyn crate::sandbox::backend::SandboxBackend>>,
    /// §F2c — bound extension runner, set by `build_engine` (alongside the
    /// `Engine`/`EngineHandle` field) so `HostServices` (`build_turn_dispatcher`
    /// / `spawn_subagent`) can emit `ProjectTrust` without going through the
    /// `Engine`. `None` for embeds/tests that skip the extension runtime.
    pub extension_runner: Option<std::sync::Arc<codesmith_extensions::ExtensionRunner>>,
    /// §F script mod layer — reload context handed to the model-visible
    /// `manage_mods` tool (runner + workspace + shared cancel token), set by
    /// `build_engine` alongside `extension_runner`. `None` for embeds/tests.
    pub mod_reload: Option<crate::mod_ops::ModReloadCtx>,
    /// Capability composition point — the last main-turn model-visible tool
    /// catalog baseline (names + origins). `build_turn_dispatcher` diffs each
    /// fresh catalog against it to emit `tools-change`; `/tools` renders it.
    /// Shared with `EngineHandle` (the `extension_runner` precedent).
    pub tool_catalog: crate::core::tool_catalog::SharedToolCatalog,
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
            utility_llm: None,
            sandbox_backend: None,
            extension_runner: None,
            mod_reload: None,
            tool_catalog: Default::default(),
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
/// Wrap `client` in a [`RecordingClient`](codesmith_agent::llm_client::record_replay::RecordingClient)
/// when `CODESMITH_RECORD_LLM` is set. Misconfiguration fails loud here:
/// the recording was explicitly requested, so an unopenable path aborts
/// engine construction instead of silently running unrecorded.
fn wrap_with_recording(client: LlmClientHandle) -> LlmClientHandle {
    let path = std::env::var("CODESMITH_RECORD_LLM").unwrap_or_default();
    if path.is_empty() {
        return client;
    }
    match codesmith_agent::llm_client::record_replay::RecordingClient::new(client, &path) {
        Ok(recorder) => Arc::new(recorder) as LlmClientHandle,
        Err(e) => panic!("CODESMITH_RECORD_LLM={path}: cannot open recording file: {e}"),
    }
}

/// Route A — the process-shared provider registry. Seeded once from the
/// builtin `default_registry`; the extension runner upserts
/// extension-registered factories into it (via `attach_shared_providers`),
/// and `resolve_llm_client` / `resolve_utility_llm` build through it, so
/// extension providers are selectable by id at the next client resolution.
///
/// Known limitation: an already-built client is not hot-swapped — a
/// provider registered mid-session takes effect when the client is next
/// resolved (new session, provider switch, doctor, ACP/MCP handshake).
pub(crate) fn shared_providers() -> &'static SharedProviderRegistry {
    static SHARED: OnceLock<SharedProviderRegistry> = OnceLock::new();
    SHARED.get_or_init(|| {
        SharedProviderRegistry::from_registry(codesmith_providers::default_registry().clone())
    })
}

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
        api_key: api_config.provider_api_key()?,
        base_url: api_config.provider_base_url(),
        default_model: api_config.default_model(),
        retry: codesmith_agent::llm_client::RetryConfig::from(api_config.retry_policy()),
        http_headers: api_config.http_headers(),
        on_retry: None,
    };
    // Route A — build through the shared registry so extension-registered
    // providers (registered since process start) are selectable by id.
    shared_providers().build(&cfg)
}

/// Resolve the optional `[utility_model]` into a ready-to-use handle.
///
/// Three outcomes:
/// - table absent → `None`; every assist falls back to the main model
/// - table present, same provider, no dedicated base_url/api_key → the main
///   client is reused with a per-request model override (the rig adapter
///   honours `MessageRequest.model` over the client default)
/// - table present with a different provider (or dedicated endpoint) → a
///   second client is built through the provider registry
///
/// Building a dedicated client never fails the session: on error the utility
/// model is dropped with a warning and assists use the main model. Custom
/// gateway (`custom_provider`) setups inherit the main client via the
/// same-provider branch — an explicit `provider` cannot name a custom id.
pub(crate) fn resolve_utility_llm(
    api_config: &Config,
    main_client: Option<&LlmClientHandle>,
) -> Option<UtilityLlm> {
    // Raw table (not `utility_model_config()`): the api_key inheritance done
    // by the accessor would turn an unset key into `Some(main_key)` and break
    // the same-provider reuse check below. Inheritance happens explicitly in
    // the dedicated-client branch.
    let utility = api_config.utility_model.clone()?;
    let main_provider = api_config.api_provider();
    let provider = utility.provider.unwrap_or(main_provider);
    let inherits_main =
        provider == main_provider && utility.base_url.is_none() && utility.api_key.is_none();
    if inherits_main {
        let client = main_client?.clone();
        return Some(UtilityLlm {
            client,
            model: utility.model,
        });
    }

    // Key resolution: an explicit utility key wins; otherwise the main key is
    // only valid for the same provider (never leak one vendor's key to
    // another). A cross-provider table without a key builds an empty key and
    // lets the factory surface the error, which falls back to the main model.
    let api_key = if let Some(key) = utility.api_key.clone() {
        key
    } else if provider == main_provider {
        api_config.provider_api_key().unwrap_or_default()
    } else {
        String::new()
    };
    let cfg = ProviderConfig {
        provider: ProviderId::from(provider.as_str()),
        api_key,
        base_url: utility
            .base_url
            .clone()
            .unwrap_or_else(|| api_config.provider_base_url()),
        default_model: utility.model.clone(),
        retry: codesmith_agent::llm_client::RetryConfig::from(api_config.retry_policy()),
        http_headers: api_config.http_headers(),
        on_retry: None,
    };
    match shared_providers().build(&cfg) {
        Ok(client) => Some(UtilityLlm {
            client,
            model: utility.model,
        }),
        Err(err) => {
            tracing::warn!(
                "utility model client unavailable; assists fall back to the main model: {err:#}"
            );
            None
        }
    }
}

/// Resolve the seam client + model (#159) honouring, in order:
/// 1. an explicit `[context] seam_model` — keeps the main client, the id
///    belongs to the main provider
/// 2. a configured utility model — same-provider setups reuse the main client
///    with a per-request model override; cross-provider setups use the
///    utility client
/// 3. the active provider's light tier (cheap-and-fast summarizer), which
///    falls back to the effective main model on pass-through providers —
///    never a hardcoded DeepSeek ID that the provider may not serve
pub(crate) fn resolve_seam_model_and_client(
    api_config: &Config,
    main_client: &LlmClientHandle,
    utility_llm: &Option<UtilityLlm>,
) -> (LlmClientHandle, String) {
    if let Some(explicit) = api_config.context.seam_model.clone() {
        return (main_client.clone(), explicit);
    }
    match utility_llm.as_ref() {
        Some(utility) => {
            let client = if utility.client.provider_name() != main_client.provider_name() {
                utility.client.clone()
            } else {
                main_client.clone()
            };
            (client, utility.model.clone())
        }
        None => (
            main_client.clone(),
            api_config.resolve_model_tier(crate::config::ModelTier::Light),
        ),
    }
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
    mods_enabled: bool,
    shared_cancel_token: Arc<StdMutex<tokio_util::sync::CancellationToken>>,
) -> (Arc<codesmith_extensions::ExtensionRunner>, PopulateReport) {
    let runner = Arc::new(codesmith_extensions::ExtensionRunner::new());
    // Route A — the runner's provider flush target is the same shared
    // registry `resolve_llm_client` builds through, so extension-registered
    // providers are visible to every resolution path.
    runner.attach_shared_providers(shared_providers().clone());
    let state = crate::extension_state::ExtensionStateStore::load_default().unwrap_or_default();
    let mut mod_state = crate::mod_state::ModStateStore::load_default().unwrap_or_default();
    let report = populate_extension_runtime(
        &runner,
        workspace,
        &state,
        &mut mod_state,
        mods_enabled,
        shared_cancel_token,
    );
    (runner, report)
}

/// Result of a populate pass (§F script mod layer Phase B6): how many
/// script mods loaded + which discovered mods await first-activation
/// consent. The TUI surfaces `pending_mods` as a passive notice.
#[derive(Debug, Clone, Default)]
pub struct PopulateReport {
    pub loaded_mods: usize,
    pub pending_mods: Vec<crate::mod_ops::PendingModInfo>,
    /// Discipline 5 — startup composition audit: one entry per discovered
    /// extension/mod with its terminal status. Failures keep the original
    /// error string; `failed_audit_lines` is the shared diagnostic format.
    pub audit: Vec<StartupAuditEntry>,
}

/// One extension-layer startup entry (discipline 5). `populate_extension_runtime`
/// collects one per discovered extension/mod.
///
/// Known limitations: a mod whose `mod.toml` fails discovery gets no entry
/// (it has no loadable identity) — discovery-stage skips stay in the
/// `tracing` warn log; and CodeSmith has no "required" extension concept,
/// so there is no audit-fails-fatal branch — optional extensions stay
/// fail-soft with this structured record instead.
#[derive(Debug, Clone)]
pub struct StartupAuditEntry {
    pub id: String,
    pub source: AuditSource,
    pub status: AuditStatus,
}

impl StartupAuditEntry {
    fn new(id: String, source: AuditSource, status: AuditStatus) -> Self {
        Self { id, source, status }
    }
}

/// Where an audited entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditSource {
    CompiledIn,
    Dylib { global: bool },
    ScriptMod { global: bool },
}

impl AuditSource {
    fn label(self) -> String {
        let scope = |global: bool| if global { "global" } else { "project" };
        match self {
            Self::CompiledIn => "compiled-in".to_string(),
            Self::Dylib { global } => format!("dylib, {}", scope(global)),
            Self::ScriptMod { global } => format!("script mod, {}", scope(global)),
        }
    }
}

/// Terminal status of an audited entry. `Failed` preserves the original
/// error string so the structured diagnostic replaces half a
/// troubleshooting doc.
#[derive(Debug, Clone)]
pub enum AuditStatus {
    Loaded,
    Failed {
        error: String,
    },
    /// Discovered but awaits first-activation consent (script mods only).
    PendingConsent,
    /// Disabled via `/extension` state or mods_state.
    Disabled,
    /// Dropped by the project trust gate (project-local source in an
    /// untrusted workspace).
    TrustGated,
}

/// `id [source]: first-line-of-error` for every failed audit entry — the
/// shared diagnostic format for the startup notice (`ui.rs`) and the
/// reload message (`mod_ops::reload_mods`).
pub(crate) fn failed_audit_lines(audit: &[StartupAuditEntry]) -> Vec<String> {
    audit
        .iter()
        .filter_map(|e| match &e.status {
            AuditStatus::Failed { error } => {
                let first: String = error
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect();
                Some(format!("{} [{}]: {first}", e.id, e.source.label()))
            }
            _ => None,
        })
        .collect()
}

/// Shared discover → reconcile → load → `bind_core` for both the initial build
/// and live reload. Does NOT clear handlers or bump generation — a fresh runner
/// needs neither, and [`reload_extension_runtime`] does both before calling
/// this.
fn populate_extension_runtime(
    runner: &Arc<codesmith_extensions::ExtensionRunner>,
    workspace: &std::path::Path,
    state: &crate::extension_state::ExtensionStateStore,
    mod_state: &mut crate::mod_state::ModStateStore,
    mods_enabled: bool,
    shared_cancel_token: Arc<StdMutex<tokio_util::sync::CancellationToken>>,
) -> PopulateReport {
    // 1. Discover compiled-in extensions (inventory).
    let discovered = codesmith_extensions::discover_static();

    // 2. Reconcile with state: skip disabled (the audit records it —
    //    discipline 5: a disabled extension is visible, not silently gone).
    let mut audit: Vec<StartupAuditEntry> = Vec::new();
    let mut enabled = Vec::new();
    for reg in discovered {
        if state.is_enabled(&reg.metadata.id) {
            enabled.push(reg);
        } else {
            audit.push(StartupAuditEntry::new(
                reg.metadata.id.to_string(),
                AuditSource::CompiledIn,
                AuditStatus::Disabled,
            ));
        }
    }

    // §F5b — discover dylibs (global + project; configured paths → §F5c
    // when settings.extensions lands). Global dir = ~/.codesmith/extensions
    // (effective_home_dir re-exported via crate::config); project dir =
    // <workspace>/.codesmith/extensions. apply_trust_gate drops project-local
    // (!global) sources when the workspace is not trusted (Model A — consume
    // FirstLoad's persisted-trust flip via is_workspace_trusted). Discovery is
    // trust-agnostic; the gate is the host's concern.
    let global_dir =
        crate::config::effective_home_dir().map(|home| home.join(".codesmith").join("extensions"));
    let project_dir = workspace.join(".codesmith").join("extensions");
    let project_trusted = crate::config::is_workspace_trusted(workspace);
    let global_roots: Vec<std::path::PathBuf> = global_dir.into_iter().collect();
    let project_roots = vec![project_dir];
    let discovered_dylib = codesmith_extensions::discover_dylib(&global_roots, &project_roots);
    // Trust gate first (audit records gated entries), then the disabled
    // filter (audit records those too).
    let (gated, surviving): (Vec<_>, Vec<_>) = if !project_trusted {
        discovered_dylib.into_iter().partition(|d| !d.global)
    } else {
        (Vec::new(), discovered_dylib)
    };
    for d in gated {
        audit.push(StartupAuditEntry::new(
            d.id,
            AuditSource::Dylib { global: d.global },
            AuditStatus::TrustGated,
        ));
    }
    let mut enabled_dylib = Vec::new();
    for d in surviving {
        if state.is_enabled(&d.id) {
            enabled_dylib.push(d);
        } else {
            audit.push(StartupAuditEntry::new(
                d.id,
                AuditSource::Dylib { global: d.global },
                AuditStatus::Disabled,
            ));
        }
    }

    // §F script mod layer Phase B6 — discover script mods, gate on trust +
    // activation state. Disabled mods skip; discovered-but-not-activated
    // mods skip AND are collected for the passive pending notice (the
    // first-activation consent gate, plan §五). An activated mod whose
    // entry-file hash no longer matches the recorded activation hash also
    // returns to pending: activation consent is bound to the content the
    // user approved (review round 3 — a git-pulled or auto-edited .rhai
    // must not execute on standing consent). `&mut mod_state` lets a
    // hash-less legacy activation load once and backfill its hash.
    let mut pending_mods: Vec<crate::mod_ops::PendingModInfo> = Vec::new();
    let mut mods_to_load: Vec<codesmith_extensions::DiscoveredMod> = Vec::new();
    if mods_enabled {
        let discovered_mods = crate::mod_ops::discover_workspace_mods(workspace);
        for m in discovered_mods {
            if mod_state.is_disabled(&m.id) {
                audit.push(StartupAuditEntry::new(
                    m.id.clone(),
                    AuditSource::ScriptMod { global: m.global },
                    AuditStatus::Disabled,
                ));
                continue;
            }
            if !mod_state.is_activated(&m.id) {
                audit.push(StartupAuditEntry::new(
                    m.id.clone(),
                    AuditSource::ScriptMod { global: m.global },
                    AuditStatus::PendingConsent,
                ));
                pending_mods.push(crate::mod_ops::PendingModInfo::from_discovered(&m));
                continue;
            }
            let actual_hash = match crate::mod_ops::entry_file_hash(&m.entry_path) {
                Ok(h) => h,
                Err(e) => {
                    audit.push(StartupAuditEntry::new(
                        m.id.clone(),
                        AuditSource::ScriptMod { global: m.global },
                        AuditStatus::Failed { error: e },
                    ));
                    continue;
                }
            };
            match mod_state.activation_hash_state(&m.id, &actual_hash) {
                crate::mod_state::ActivationHashState::Matches => {}
                crate::mod_state::ActivationHashState::NoRecord => {
                    let _guard = crate::mod_ops::mod_state_lock();
                    if let Err(e) = mod_state.record_activation_hash(&m.id, &actual_hash) {
                        tracing::warn!(
                            target: "codesmith_mods",
                            "backfill activation hash for {}: {e}",
                            m.id
                        );
                    }
                }
                crate::mod_state::ActivationHashState::Changed => {
                    audit.push(StartupAuditEntry::new(
                        m.id.clone(),
                        AuditSource::ScriptMod { global: m.global },
                        AuditStatus::PendingConsent,
                    ));
                    pending_mods.push(crate::mod_ops::PendingModInfo::changed_content(&m));
                    continue;
                }
            }
            mods_to_load.push(m);
        }
    }

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
    let mods_kv_dir = mod_state.kv_dir();
    let loaded_mods_cell = &std::sync::atomic::AtomicUsize::new(0);
    // Discipline 5 — audit entries continue inside the load thread (Loaded /
    // Failed{original error}); pre-thread entries (Disabled / PendingConsent
    // / TrustGated) were pushed above.
    let audit_cell: &StdMutex<Vec<StartupAuditEntry>> = &StdMutex::new(audit);
    if !enabled.is_empty() || !enabled_dylib.is_empty() || !mods_to_load.is_empty() {
        let runner_for_thread = runner.clone();
        std::thread::scope(|s| {
            s.spawn(move || {
                let load_rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("extension load runtime");
                for reg in enabled {
                    let id = reg.metadata.id.to_string();
                    let ext = (reg.factory)();
                    match load_rt.block_on(runner_for_thread.load(&*ext)) {
                        Ok(()) => audit_cell.lock().expect("audit cell poisoned").push(
                            StartupAuditEntry::new(
                                id,
                                AuditSource::CompiledIn,
                                AuditStatus::Loaded,
                            ),
                        ),
                        Err(e) => audit_cell.lock().expect("audit cell poisoned").push(
                            StartupAuditEntry::new(
                                id,
                                AuditSource::CompiledIn,
                                AuditStatus::Failed {
                                    error: e.to_string(),
                                },
                            ),
                        ),
                    }
                }
                // §F5b — load each discovered dylib on the same load
                // runtime. Best-effort: a failing dylib is warned + skipped
                // (§8.3 isolation) and audited as Failed.
                for d in enabled_dylib {
                    let (id, global) = (d.id.clone(), d.global);
                    match load_rt.block_on(runner_for_thread.load_dylib(&d.dylib_path)) {
                        Ok(()) => audit_cell.lock().expect("audit cell poisoned").push(
                            StartupAuditEntry::new(
                                id,
                                AuditSource::Dylib { global },
                                AuditStatus::Loaded,
                            ),
                        ),
                        Err(e) => {
                            tracing::warn!(
                                target: "codesmith_extensions::loader",
                                "skip dylib {}: {e}",
                                d.dylib_path.display()
                            );
                            audit_cell.lock().expect("audit cell poisoned").push(
                                StartupAuditEntry::new(
                                    id,
                                    AuditSource::Dylib { global },
                                    AuditStatus::Failed {
                                        error: e.to_string(),
                                    },
                                ),
                            );
                        }
                    }
                }
                // §F script mod layer — load each activated script mod on
                // the same load runtime (best-effort isolation, same as
                // dylibs: a failing mod is warned + skipped and audited as
                // Failed).
                for m in mods_to_load {
                    let (id, global) = (m.id.clone(), m.global);
                    match crate::mod_ops::load_rhai_mod(
                        &m,
                        mods_kv_dir.as_deref(),
                        runner_for_thread.message_projection_hub(),
                    ) {
                        Ok(rhai_mod) => match load_rt.block_on(runner_for_thread.load(&rhai_mod)) {
                            Ok(()) => {
                                loaded_mods_cell.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                audit_cell.lock().expect("audit cell poisoned").push(
                                    StartupAuditEntry::new(
                                        id,
                                        AuditSource::ScriptMod { global },
                                        AuditStatus::Loaded,
                                    ),
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    target: "codesmith_mods",
                                    "configure mod {}: {e}",
                                    id
                                );
                                audit_cell.lock().expect("audit cell poisoned").push(
                                    StartupAuditEntry::new(
                                        id,
                                        AuditSource::ScriptMod { global },
                                        AuditStatus::Failed {
                                            error: e.to_string(),
                                        },
                                    ),
                                );
                            }
                        },
                        Err(e) => {
                            tracing::warn!(
                                target: "codesmith_mods",
                                "skip mod {} ({}): {e}",
                                id,
                                m.dir.display()
                            );
                            audit_cell.lock().expect("audit cell poisoned").push(
                                StartupAuditEntry::new(
                                    id,
                                    AuditSource::ScriptMod { global },
                                    AuditStatus::Failed {
                                        error: e.to_string(),
                                    },
                                ),
                            );
                        }
                    }
                }
            });
        });
    }

    // 4. Build the host context + bind_core. The `idle` flag + the engine's
    //    **shared** `cancel_token` `Arc` are handed to the context so handlers
    //    observe host state + cancel. §F2c Layer 2: the shared `Arc<Mutex<_>>`
    //    form (not a snapshot) so `ctx.signal()` reflects per-turn
    //    `reset_cancel_token` swaps.
    let idle = Arc::new(std::sync::Mutex::new(true));
    let ctx = Arc::new(codesmith_extensions::HostExtensionContext::new(
        workspace.to_path_buf(),
        codesmith_agent::extension::ExtensionMode::Tui,
        idle,
        shared_cancel_token,
        runner.generation_arc(),
    ));
    runner.bind_core(ctx);
    // Discipline 5 — collect the audit, keep a stable order, and leave one
    // structured summary line when anything failed (per-failure warns are
    // already logged above; this is the count + call to action).
    let mut audit = std::mem::take(&mut *audit_cell.lock().expect("audit cell poisoned"));
    audit.sort_by(|a, b| a.id.cmp(&b.id));
    let failed = audit
        .iter()
        .filter(|e| matches!(e.status, AuditStatus::Failed { .. }))
        .count();
    if failed > 0 {
        tracing::warn!(
            target: "codesmith_extensions",
            "startup audit: {failed} extension(s) failed to load — fix and /extension reload"
        );
    }
    PopulateReport {
        loaded_mods: loaded_mods_cell.load(std::sync::atomic::Ordering::Relaxed),
        pending_mods,
        audit,
    }
}

/// §F2b T7 — live reload: re-discover + re-load + re-bind on the **shared**
/// runner `Arc` (every holder — `App.extension_runner` + the Engine's per-turn
/// `HostAgentExecutor` clone — sees the update, since it's the same `Arc`).
/// Clears handlers first so `bind_core`'s append-drain doesn't duplicate, +
/// bumps generation so any previously-captured `ExtensionApi`/`ExtensionContext`
/// reads stale (spec §7.3). Called by `/extension reload`
/// (`extension_commands`). `shared_cancel_token` is the engine's shared
/// cancel-token `Arc` — §F2c Layer 2 passes the **live** engine token (not a
/// fresh one) so a handler's `ctx.signal()` reflects the engine's per-turn
/// `reset_cancel_token` (no handler reads it yet; this is forward-looking
/// infra).
pub fn reload_extension_runtime(
    runner: &Arc<codesmith_extensions::ExtensionRunner>,
    workspace: &std::path::Path,
    state: &crate::extension_state::ExtensionStateStore,
    mod_state: &mut crate::mod_state::ModStateStore,
    mods_enabled: bool,
    shared_cancel_token: Arc<StdMutex<tokio_util::sync::CancellationToken>>,
) -> PopulateReport {
    runner.clear_handlers();
    // §F5d T3 — also clear tools/commands so the re-populate doesn't leave
    // stale bindings (name-keyed maps; safe concurrent w/ in-flight turn).
    runner.clear_tools();
    runner.clear_commands();
    // Route A — drop the generation's provider registration guards
    // (un-register; safe concurrent w/ in-flight turn — an already-built
    // client keeps its `Arc`).
    runner.clear_providers();
    // Route B — prompt sections are generation-scoped like providers.
    runner.clear_prompt_sections();
    // Route B — registered skills are generation-scoped the same way; the
    // next catalogue render (prompt refresh) picks up the new generation's
    // set.
    runner.clear_skills();
    // Session log folds — same generation scoping; the hub turns dirty and
    // the engine refolds from the live transcript at the next turn start
    // (after the new generation re-registers).
    runner.clear_message_projections();
    // §F5d T4 — move the live dylib `Library`s into `pending_drop` (UI-thread
    // MOVE: `mem::take` under one lock, the `Library` stays alive). The engine
    // op-loop top then `drop_pending`s them at the one moment the main-thread
    // `HostAgentExecutor` (the only in-flight dylib `Arc` holder) is already
    // dropped between turns — see agent-runtime `engine/mod.rs` op-loop +
    // spec §4a/§4b. `populate_extension_runtime` loads fresh dylibs into
    // `libraries` below. Idempotent + safe concurrent with an in-flight turn.
    runner.drain_libraries_to_pending();
    runner.invalidate();
    populate_extension_runtime(
        runner,
        workspace,
        state,
        mod_state,
        mods_enabled,
        shared_cancel_token,
    )
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

    // §F1 — build the extension runtime + bind to the host executor. §F2c
    // Layer 2: hand the engine's **shared** `cancel_token` `Arc` (not a
    // snapshot clone) so `ctx.signal()` reflects per-turn resets.
    // §F script mod layer — the populate report's pending list rides on the
    // handle for the TUI's passive first-run notice.
    let mods_enabled = api_config.mods_enabled();
    let (extension_runner, mods_report) =
        build_extension_runtime(&config.workspace, mods_enabled, shared_cancel_token.clone());
    // §F2c — surface the runner on `EngineHost` too so `HostServices`
    // (`build_turn_dispatcher` / `spawn_subagent`) can emit `ProjectTrust`
    // without going through the `Engine`.
    host.extension_runner = Some(extension_runner.clone());
    // §F script mod layer — the `manage_mods` tool reload context.
    if mods_enabled {
        host.mod_reload = Some(crate::mod_ops::ModReloadCtx {
            runner: extension_runner.clone(),
            workspace: config.workspace.clone(),
            shared_cancel_token: shared_cancel_token.clone(),
            mods_enabled,
        });
    }

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
    // Record/replay (dev plan capability 1+2): wrap the resolved client in
    // a JSONL recorder when CODESMITH_RECORD_LLM is set, so main-turn model
    // calls (full request envelope + streamed response) are replayable
    // keyless. Known gap: a cross-provider `[utility_model]` client is
    // built separately in `resolve_utility_llm` and is not recorded.
    let llm_client = llm_client.map(wrap_with_recording);

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
        is_simple: config.is_simple,
        personality: config.personality,
        skills_block: crate::skills::render_available_skills_context_with_registered(
            &config.workspace,
            Some(config.skills_dir.as_path()),
            &extension_runner.registered_skills(),
        ),
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

    // Initialize prefix-cache stability monitor (lazy-pin). `Arc`-shared
    // with the per-turn `HostAgentExecutor` (P0-3 wire-in) so fingerprint
    // re-pins persist across turns.
    let _ = session.prefix_stability.get_or_insert_with(|| {
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::prefix_cache::PrefixStabilityManager::new_unpinned(),
        ))
    });

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
    let capacity_controller = Arc::new(StdMutex::new(CapacityController::new(
        config.capacity.clone(),
    )));

    // Resolve the optional [utility_model] once. Seam defaults below and the
    // workshop synthesis handle both consume it.
    let utility_llm = resolve_utility_llm(api_config, llm_client.as_ref());

    // Create Flash seam manager for layered context (#159). An explicit
    // `[context] seam_model` always wins (and keeps the main client — the id
    // belongs to the main provider); otherwise a configured utility model
    // supplies the seam model, and a cross-provider utility also brings its
    // own client for seam calls.
    let seam_manager = llm_client.as_ref().map(|main_client| {
        let (seam_client, seam_model) =
            resolve_seam_model_and_client(api_config, main_client, &utility_llm);
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
            seam_model,
        };
        SeamManager::new(seam_client, seam_config)
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
    host.utility_llm = utility_llm;
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

    // Event-sourcing slice 4 — surface the engine session's fact ledger
    // (the same `Arc` compaction feeds) so the host can snapshot + restore.
    let handle = EngineHandle {
        tx_op,
        rx_event: Arc::new(RwLock::new(rx_event)),
        cancel_token: shared_cancel_token,
        cancel_reason,
        tx_approval,
        tx_user_input,
        tx_steer,
        fact_ledger: engine.session.fact_ledger.clone(),
        recent_read_files: engine.session.recent_read_files.clone(),
        extension_runner: Some(extension_runner),
        mods_pending: mods_report.pending_mods,
        mods_audit: mods_report.audit,
        tool_catalog: host_concrete.tool_catalog.clone(),
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
    #[allow(clippy::new_ret_no_self)]
    #[allow(dead_code)]
    fn new(config: EngineConfig, api_config: &Config) -> (Engine, EngineHandle);

    /// Create a new engine with an injected LLM client (for integration tests).
    #[allow(dead_code)]
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
            self.host_concrete().mod_reload.clone(),
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
        mods_pending: Vec::new(),
        mods_audit: Vec::new(),
        fact_ledger: Arc::new(StdMutex::new(Default::default())),
        recent_read_files: Arc::new(StdMutex::new(Default::default())),
        tool_catalog: Default::default(),
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
