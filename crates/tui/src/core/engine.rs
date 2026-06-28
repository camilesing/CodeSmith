//! Core engine for `DeepSeek` CLI.
//!
//! The engine handles all AI interactions in a background task,
//! communicating with the UI via channels. This enables:
//! - Non-blocking UI during API calls
//! - Real-time streaming updates
//! - Proper cancellation support
//! - Tool execution orchestration

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde_json::json;
use tokio::sync::{Mutex as AsyncMutex, RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use crate::background_task::BackgroundTaskStatus;
use crate::client::DeepSeekClient;
use crate::compaction::session_memory_compact::{
    session_memory_compact, should_use_session_memory_compact,
};
use crate::compaction::{
    CompactionEnhancements, SessionMemorySidecar, compact_messages_safe,
    merge_system_prompts, should_compact,
};
use crate::config::{ApiProvider, Config, DEFAULT_TEXT_MODEL};
use crate::cycle_manager::{
    CycleBriefing, StructuredState, archive_cycle, build_seed_messages,
    estimate_briefing_tokens, produce_briefing, should_advance_cycle,
};
use crate::error_taxonomy::{ErrorCategory, ErrorEnvelope, StreamError};
use crate::features::Feature;
use crate::hooks::HookContext;
use crate::llm_client::LlmClientHandle;
use crate::mcp::McpPool;
#[cfg(test)]
use crate::models::ToolCaller;
use crate::models::{
    ContentBlock, ContentBlockStart, Delta, LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS, Message,
    MessageRequest, StreamEvent, SystemPrompt, Tool, Usage,
};
use crate::prompts;
use crate::purge::{emit_purge_completed, emit_purge_failed, emit_purge_started, run_purge};
use crate::seam_manager::{SeamConfig, SeamManager};
use crate::tools::goal::SharedGoalState;
use crate::tools::plan::{SharedPlanState, new_shared_plan_state};
use crate::tools::shell::ShellStatus;
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager};
use crate::tools::spec::{ApprovalRequirement, ToolError, ToolResult};
use crate::tools::subagent::{
    Mailbox, SharedSubAgentManager, SubAgentCompletion, SubAgentForkContext, SubAgentRuntime,
    SubAgentType, new_shared_subagent_manager, resolve_subagent_assignment_route,
};
use crate::tools::todo::{SharedTodoList, new_shared_todo_list};
use crate::tools::user_input::{UserInputRequest, UserInputResponse};
use crate::tools::{ToolContext, ToolRegistryBuilder};
use crate::tui::app::AppMode;
use crate::utils::spawn_supervised;

use super::capacity::{
    CapacityController, CapacityControllerConfig, CapacityDecision, CapacityObservationInput,
    CapacitySnapshot, GuardrailAction, RiskBand,
};
use super::capacity_memory::{
    CanonicalState, CapacityMemoryRecord, ReplayInfo, append_capacity_record,
    load_last_k_capacity_records, new_record_id, now_rfc3339,
};
use super::coherence::{CoherenceSignal, CoherenceState, next_coherence_state};
use super::events::{Event, TurnOutcomeStatus};
use super::ops::Op;
use super::session::Session;
use super::tool_parser;
use super::turn::{TurnContext, TurnToolCall, post_turn_snapshot, pre_turn_snapshot};

// === Types ===

/// Re-export of the engine configuration type
/// (canonical home: `codesmith_agent_runtime::engine_config`).
pub use codesmith_agent_runtime::engine_config::EngineConfig;

/// Host-services trait in scope so the engine body can call
/// `self.host.lsp()` / `self.host.bg_registry()` etc. (impl lives in
/// `runtime_traits`).
use codesmith_agent_runtime::host_services::HostServices;

/// Reason the active turn was cancelled. The token from `tokio_util`
/// does not carry a cause, so the engine keeps a sibling latch for
/// approval and user-input waits that need to explain cancellation.
///
/// `External`, `Preempted`, and `Internal` are reserved for the
/// remaining direct cancellation paths tracked in #1541.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CancelReason {
    /// User-initiated cancel (Esc, `/cancel`, click cancel on modal).
    User,
    /// External / runtime-API cancel (HTTP `DELETE /v1/threads/...`,
    /// task manager stop, parent agent cancel).
    External,
    /// Cancel triggered when a new turn starts before the previous one
    /// finished — e.g. plain Enter while busy after the queueing path
    /// pre-empts the running turn.
    Preempted,
    /// Engine internals tore down the turn (drop, channel close,
    /// shutdown). Rare — surfaced as an internal error.
    Internal,
}

impl CancelReason {
    fn describe(self) -> &'static str {
        match self {
            Self::User => "user cancelled the request",
            Self::External => "request cancelled by external caller",
            Self::Preempted => "request was preempted by a new turn",
            Self::Internal => "engine torn down before approval resolved",
        }
    }
}

/// Handle to communicate with the engine
#[derive(Clone)]
pub struct EngineHandle {
    /// Send operations to the engine
    pub tx_op: mpsc::Sender<Op>,
    /// Receive events from the engine
    pub rx_event: Arc<RwLock<mpsc::Receiver<Event>>>,
    /// Shared pointer to the cancellation token for the current request.
    cancel_token: Arc<StdMutex<CancellationToken>>,
    /// Latched reason for the most recent cancellation. Read by the
    /// approval / user-input handlers to enrich their error strings.
    /// Cleared by the engine when a fresh turn starts.
    cancel_reason: Arc<StdMutex<Option<CancelReason>>>,
    /// Send approval decisions to the engine
    tx_approval: mpsc::Sender<ApprovalDecision>,
    /// Send user input responses to the engine
    tx_user_input: mpsc::Sender<UserInputDecision>,
    /// Send steer input for an in-flight turn.
    tx_steer: mpsc::Sender<String>,
}

// `impl EngineHandle { ... }` moved to `engine/handle.rs` so the
// mailbox API can be reviewed independently of the engine internals.

// === Engine ===

/// Host-injected runtime services the engine needs but whose concrete types
/// (`ShellManager`, `TaskManager`, `AutomationManager`, `HookExecutor`, …)
/// stay terminal-side. Kept out of `EngineConfig` so `EngineConfig` can live
/// in `codesmith-agent-runtime` without dragging ~10k lines of OS-bridging
/// managers across the crate boundary (per the boundary documented in
/// `agent_runtime::background_task`).
///
/// When the Engine itself moves to agent-runtime (5.2.7) this becomes an
/// `Arc<dyn HostServices>` trait object; for now it is a concrete tui struct.
#[derive(Debug, Clone)]
pub struct EngineHost {
    /// Durable runtime services exposed to model-visible tools.
    pub runtime_services: crate::tools::spec::RuntimeToolServices,
    /// Hook executor for `pre_compact` (and future compaction-related) hooks.
    pub hooks: Option<crate::hooks::HookExecutor>,
    /// Post-edit LSP diagnostics manager. Defaults to a disabled manager;
    /// `new_impl` replaces it with the config-resolved one. Held on the host
    /// (not `EngineConfig`) so the engine body reaches it through the
    /// [`HostServices::lsp`] trait and stays free of the concrete `LspManager`.
    pub lsp_manager: std::sync::Arc<crate::lsp::LspManager>,
    /// Flash seam (layered-context) manager, when configured. `None` when the
    /// feature is disabled. Held on the host (not `EngineConfig`) so the engine
    /// body reaches it through the [`HostServices::seam`] trait and stays free
    /// of the concrete `SeamManager`; `new_impl` replaces it with the
    /// config-resolved one.
    pub seam_manager: Option<crate::seam_manager::SeamManager>,
}

impl Default for EngineHost {
    fn default() -> Self {
        Self {
            runtime_services: crate::tools::spec::RuntimeToolServices::default(),
            hooks: None,
            lsp_manager: std::sync::Arc::new(crate::lsp::LspManager::disabled()),
            seam_manager: None,
        }
    }
}

/// The core engine that processes operations and emits events
pub struct Engine {
    config: EngineConfig,
    host: EngineHost,
    llm_client: Option<LlmClientHandle>,
    llm_client_error: Option<String>,
    api_key_env_only_recovery: Option<String>,
    session: Session,
    api_provider: ApiProvider,
    subagent_manager: SharedSubAgentManager,
    shell_manager: SharedShellManager,
    mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
    rx_op: mpsc::Receiver<Op>,
    rx_approval: mpsc::Receiver<ApprovalDecision>,
    rx_user_input: mpsc::Receiver<UserInputDecision>,
    rx_steer: mpsc::Receiver<String>,
    tx_event: mpsc::Sender<Event>,
    /// Wakeup channel for the parent turn loop when a direct child sub-agent
    /// terminates (issue #756). Cloned into `SubAgentRuntime` so the runtime
    /// can fan completion events back into the engine.
    tx_subagent_completion: mpsc::UnboundedSender<SubAgentCompletion>,
    /// Receiver paired with `tx_subagent_completion`. Drained at the
    /// turn-loop's empty-tool_uses branch to surface `<codesmith:subagent.done>`
    /// sentinels into the parent's transcript before deciding to end the turn.
    pub(super) rx_subagent_completion: mpsc::UnboundedReceiver<SubAgentCompletion>,
    cancel_token: CancellationToken,
    shared_cancel_token: Arc<StdMutex<CancellationToken>>,
    /// Latched reason for the current cancellation, mirrored to
    /// `EngineHandle::cancel_reason`. Read by `approval.rs` when
    /// surfacing the "Request cancelled while awaiting …" error so the
    /// user-facing message names a cause.
    pub(super) cancel_reason: Arc<StdMutex<Option<CancelReason>>>,
    tool_exec_lock: Arc<RwLock<()>>,
    capacity_controller: CapacityController,
    coherence_state: CoherenceState,
    turn_counter: u64,
    /// Session-scoped workshop variable store (#548). Shared across all tool
    /// calls so `last_tool_result` persists within the session and can be
    /// promoted to the parent context via `promote_to_context`.
    workshop_vars: Option<
        std::sync::Arc<tokio::sync::Mutex<crate::tools::large_output_router::WorkshopVariables>>,
    >,
    /// External sandbox backend (#516). When `Some`, exec_shell routes commands
    /// through this instead of spawning a local process.
    sandbox_backend: Option<std::sync::Arc<dyn crate::sandbox::backend::SandboxBackend>>,
    /// Diagnostics collected during the current step's tool calls. Drained
    /// and forwarded as a synthetic user message before the next API call.
    pending_lsp_blocks: Vec<crate::lsp::DiagnosticBlock>,
    /// Cached SlopLedger gate block keyed by the ledger file's modified time.
    /// This keeps prompt refreshes cheap while still noticing append/update
    /// writes from slop ledger tools during the same session.
    slop_ledger_gate_cache: Option<(Option<SystemTime>, Option<String>)>,
    /// Knowledge On Demand prefetch orchestrator. Tracks already-surfaced
    /// memory paths and session byte budget across turns.
    knowledge_prefetch: crate::knowledge::prefetch::KnowledgePrefetch,
    /// Sender half of the engine op channel. Cloned into long-lived background
    /// lifecycle tasks such as the team inbox poller watcher.
    tx_op: mpsc::Sender<Op>,
    /// Terminal-agnostic UI bridge (notifications + clipboard). Backed by
    /// [`runtime_traits::TuiRuntimeUi`] here; the trait object keeps the
    /// engine core decoupled from concrete terminal services so it can later
    /// move to `codesmith-agent-runtime`.
    runtime_ui: Arc<dyn codesmith_agent_runtime::runtime_ui::RuntimeUi>,
}

// === Internal tool helpers ===

impl Engine {
    fn reset_cancel_token(&mut self) {
        let token = CancellationToken::new();
        self.cancel_token = token.clone();
        match self.shared_cancel_token.lock() {
            Ok(mut shared) => {
                *shared = token;
            }
            Err(poisoned) => {
                *poisoned.into_inner() = token;
            }
        }
        match self.cancel_reason.lock() {
            Ok(mut slot) => *slot = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }

    // === Knowledge On Demand helpers ===

    /// Spawn a KoD prefetch task for the current turn.
    ///
    /// Extracts the user query from session messages, clones the DeepSeek
    /// client for the side-query, and spawns the full prefetch pipeline
    /// (scan → rank → read → truncate) as a tokio task. The JoinHandle
    /// is stored in `self.knowledge_prefetch` for collection after tool
    /// execution.
    fn kod_prefetch_spawn(&mut self) {
        if !self.config.kod_enabled {
            return;
        }

        let client = match self.llm_client.clone() {
            Some(c) => c,
            None => return, // No client → no prefetch
        };

        // Extract user query from the last real user message in session.
        let user_query = self
            .session
            .messages
            .iter()
            .rev()
            .find_map(|msg| {
                if msg.role == "user" {
                    msg.content.iter().find_map(|block| match block {
                        crate::models::ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                } else {
                    None
                }
            })
            .unwrap_or_default();

        // Skip prefetch for trivially short queries.
        if user_query.trim().len() < 5 {
            return;
        }

        let memory_dir = self.config.memory_dir.clone();
        let already_surfaced = self.knowledge_prefetch.already_surfaced_paths();
        let session_budget = self.knowledge_prefetch.session_budget();
        let cancel_token = self.cancel_token.clone();
        let model = self.config.model.clone();

        // Build the side_query_fn closure that wraps DeepSeek API calls.
        let side_query_fn = |system_prompt: String, user_message: String| {
            let request = crate::models::MessageRequest {
                model,
                messages: vec![crate::models::Message {
                    role: "user".to_string(),
                    content: vec![crate::models::ContentBlock::Text {
                        text: user_message,
                        cache_control: None,
                    }],
                }],
                max_tokens: 256,
                system: Some(crate::models::SystemPrompt::Text(system_prompt)),
                tools: None,
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: None,
                stream: Some(false),
                temperature: Some(0.0), // Deterministic ranking
                top_p: None,
            };

            Box::pin(async move {
                let result = client
                    .create_message(request)
                    .await
                    .map_err(|e| format!("side-query API error: {e}"))?;

                // Extract text from response content blocks.
                let text = result
                    .content
                    .iter()
                    .find_map(|block| match block {
                        crate::models::ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                Ok(text)
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
                >
        };

        let handle = tokio::spawn(async move {
            crate::knowledge::prefetch::run_prefetch(
                &user_query,
                &memory_dir,
                already_surfaced,
                session_budget,
                cancel_token,
                &[], // recent_tools — empty for now, can be wired later
                side_query_fn,
            )
            .await
            .unwrap_or_else(|_| crate::knowledge::prefetch::PrefetchResult {
                surfaced: vec![],
                scan_headers: vec![],
                duration_ms: 0,
            })
        });

        self.knowledge_prefetch.set_prefetch_handle(handle);
    }

    /// Collect KoD prefetch results and inject surfaced memories into context.
    ///
    /// Polls the prefetch JoinHandle with a 10-second timeout. If the
    /// prefetch completed, deduplicates against tool result file paths,
    /// enforces session byte budget, and injects surfaced memories as
    /// a `<system-reminder>` message. On timeout or error, silently skips.
    async fn kod_prefetch_collect(&mut self) {
        let handle = match self.knowledge_prefetch.take_prefetch_handle() {
            Some(h) => h,
            None => return, // No prefetch was spawned this turn
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

        match result {
            Ok(Ok(prefetch_result)) => {
                if prefetch_result.surfaced.is_empty() {
                    return; // No relevant memories found
                }

                // Mark surfaced memories in tracking state.
                self.knowledge_prefetch
                    .mark_surfaced(&prefetch_result.surfaced)
                    .await;

                // Format surfaced memories for injection.
                let content =
                    crate::knowledge::prefetch::format_surfaced_memories(&prefetch_result.surfaced);

                // Inject as <system-reminder> synthetic user message.
                self.add_session_message(crate::models::Message {
                    role: "user".to_string(),
                    content: vec![crate::models::ContentBlock::Text {
                        text: format!("<system-reminder>\n{content}\n</system-reminder>"),
                        cache_control: None,
                    }],
                })
                .await;
            }
            Ok(Err(_)) | Err(_) => {
                // Prefetch task failed or timed out — silently skip.
                // No prefetch is better than a blocked turn.
            }
        }
    }

    fn env_only_api_key_recovery_hint(api_config: &Config) -> Option<String> {
        if !crate::config::active_provider_uses_env_only_api_key(api_config) {
            return None;
        }

        let provider = api_config.api_provider();
        let env_var = match provider {
            ApiProvider::Deepseek | ApiProvider::DeepseekCN => "DEEPSEEK_API_KEY",
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

    pub(super) fn decorate_auth_error_message(&self, message: String) -> String {
        let Some(hint) = self.api_key_env_only_recovery.as_ref() else {
            return message;
        };
        if crate::error_taxonomy::classify_error_message(&message) != ErrorCategory::Authentication
            || message.contains("no saved config key is present")
        {
            return message;
        }
        format!("{message}\n\n{hint}")
    }

    /// Create a new engine with the given configuration.
    ///
    /// Uses a default [`EngineHost`] (empty `RuntimeToolServices`, no hooks).
    /// Production callers that wire real tool services / hooks should use
    /// [`Engine::new_with_host`] instead.
    pub fn new(config: EngineConfig, api_config: &Config) -> (Self, EngineHandle) {
        Self::new_impl(config, api_config, None, EngineHost::default())
    }

    /// Create a new engine with an injected LLM client (for integration tests).
    ///
    /// The provided `LlmClientHandle` replaces the client that `Engine::new`
    /// would construct from `api_config`, enabling `MockLlmClient` injection
    /// without network dependency. Uses a default [`EngineHost`].
    pub fn new_with_client(
        config: EngineConfig,
        api_config: &Config,
        client: LlmClientHandle,
    ) -> (Self, EngineHandle) {
        Self::new_impl(config, api_config, Some(client), EngineHost::default())
    }

    /// Create a new engine with host-injected runtime services and hooks.
    ///
    /// Production wiring (`spawn_engine`, teammate runtime, …) passes the
    /// concrete `ShellManager`/`TaskManager`/`HookExecutor` handles here
    /// rather than on `EngineConfig`, keeping `EngineConfig` portable to
    /// `codesmith-agent-runtime`.
    pub fn new_with_host(
        config: EngineConfig,
        api_config: &Config,
        host: EngineHost,
    ) -> (Self, EngineHandle) {
        Self::new_impl(config, api_config, None, host)
    }

    fn new_impl(
        mut config: EngineConfig,
        api_config: &Config,
        injected_client: Option<LlmClientHandle>,
        mut host: EngineHost,
    ) -> (Self, EngineHandle) {
        if let Some(objective) = normalized_goal_objective(config.goal_objective.as_deref()) {
            sync_goal_state_from_host(&config.goal_state, Some(&objective), None, false);
        }

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

        if config.features.enabled(Feature::AgentTeams) {
            let team_context = config
                .team_context
                .clone()
                .or_else(|| host.runtime_services.team_context.clone())
                .unwrap_or_else(crate::tools::team::new_shared_team_context);
            config.team_context = Some(team_context.clone());
            host.runtime_services.team_context = Some(team_context);
            if host
                .runtime_services
                .permission_request_registry
                .is_none()
            {
                host.runtime_services.permission_request_registry =
                    Some(crate::tools::team::new_shared_permission_registry());
            }
        }

        // Create clients for both providers
        let (llm_client, llm_client_error) = match injected_client {
            Some(client) => (Some(client), None),
            None => match DeepSeekClient::new(api_config) {
                Ok(client) => (Some(Arc::new(client) as LlmClientHandle), None),
                Err(err) => (None, Some(err.to_string())),
            },
        };
        let api_key_env_only_recovery = Self::env_only_api_key_recovery_hint(api_config);

        let mut session = Session::new(
            config.model.clone(),
            config.workspace.clone(),
            config.allow_shell,
            config.trust_mode,
            config.notes_path.clone(),
            config.mcp_config_path.clone(),
        );
        // Set up stable system prompt with project context (default to agent mode).
        // Per-turn working-set metadata is injected into the latest user
        // message at request time so file churn does not rewrite this prefix.
        let (user_memory_block, knowledge_prompt_block) = if config.kod_enabled {
            let kod_block = crate::memory::compose_kod_block(&config.memory_dir);
            // When KoD is enabled, use knowledge block; fallback to legacy
            // memory block if MEMORY.md is empty but memory file exists.
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
        // The system prompt is available now but the tool catalog isn't
        // fully built until the first turn, so we start unpinned. The
        // first `check_and_update` call in the turn loop will pin the
        // fingerprint automatically.
        let _ = session.prefix_stability.get_or_insert_with(|| {
            // Use the tool registry's spec names for fingerprinting.
            // At this point tool spec builders may not be registered yet,
            // so we start with None — fingerprint will pin on first request.
            crate::prefix_cache::PrefixStabilityManager::new_unpinned()
        });

        let subagent_manager =
            new_shared_subagent_manager(config.workspace.clone(), config.max_subagents);
        let shell_manager = host
            .runtime_services
            .shell_manager
            .clone()
            .unwrap_or_else(|| new_shared_shell_manager(config.workspace.clone()));
        if let Ok(mut manager) = shell_manager.lock() {
            manager.set_prefer_bwrap(config.sandbox_runtime.prefer_bwrap || config.prefer_bwrap);
            manager.set_sandbox_runtime(config.sandbox_runtime.clone());
        }
        let capacity_controller = CapacityController::new(config.capacity.clone());

        // Create Flash seam manager for layered context (#159). v0.7.5 keeps
        // this opt-in until the prefix-cache audit proves when seam production
        // is worth the extra request and transcript mutation.
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

        // The host owns the seam manager so the engine body reaches it through
        // the `HostServices::seam` trait rather than a concrete `Engine` field.
        host.seam_manager = seam_manager;

        // The host owns the LSP manager so the engine body reaches it through
        // the `HostServices::lsp` trait rather than a concrete `Engine` field.
        host.lsp_manager = Arc::new(match config.lsp_config.clone() {
            Some(cfg) => crate::lsp::LspManager::new(cfg, config.workspace.clone()),
            None => crate::lsp::LspManager::disabled(),
        });

        // Workshop variable store (#548). Created unconditionally so the Arc
        // can be handed to every ToolContext; routing is gated on the router
        // field being Some rather than on the vars Arc being present.
        let workshop_vars: Option<
            std::sync::Arc<
                tokio::sync::Mutex<crate::tools::large_output_router::WorkshopVariables>,
            >,
        > = if config.workshop.is_some() {
            Some(std::sync::Arc::new(tokio::sync::Mutex::new(
                crate::tools::large_output_router::WorkshopVariables::default(),
            )))
        } else {
            None
        };

        // External sandbox backend (#516). Logged but non-fatal: if the
        // backend fails to construct, the engine continues with local
        // execution as the fallback.
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
        host.runtime_services.background_task_registry = Some(bg_registry);

        let api_provider = api_config.api_provider();
        let mut engine = Engine {
            config,
            host,
            llm_client,
            llm_client_error,
            api_key_env_only_recovery,
            session,
            api_provider,
            subagent_manager,
            shell_manager,
            mcp_pool: None,
            rx_op,
            rx_approval,
            rx_user_input,
            rx_steer,
            tx_event,
            tx_subagent_completion,
            rx_subagent_completion,
            cancel_token: cancel_token.clone(),
            shared_cancel_token: shared_cancel_token.clone(),
            cancel_reason: cancel_reason.clone(),
            tool_exec_lock,
            capacity_controller,
            coherence_state: CoherenceState::default(),
            turn_counter: 0,
            pending_lsp_blocks: Vec::new(),
            slop_ledger_gate_cache: None,
            knowledge_prefetch: crate::knowledge::prefetch::KnowledgePrefetch::new(),
            workshop_vars,
            sandbox_backend,
            tx_op: tx_op.clone(),
            runtime_ui: Arc::new(runtime_traits::TuiRuntimeUi),
        };
        engine.rehydrate_latest_canonical_state();

        let handle = EngineHandle {
            tx_op,
            rx_event: Arc::new(RwLock::new(rx_event)),
            cancel_token: shared_cancel_token,
            cancel_reason,
            tx_approval,
            tx_user_input,
            tx_steer,
        };

        (engine, handle)
    }

    /// Run the engine event loop
    #[allow(clippy::too_many_lines)]
    pub async fn run(mut self) {
        // Spawn background task poller — polls registry every 2s for status
        // changes, stalls, and completion notifications.
        {
            let bg_registry = self.host.bg_registry();
            let tx_event = self.tx_event.clone();
            let cancel_token = self.cancel_token.clone();
            spawn_supervised(
                "bg-task-poller",
                std::panic::Location::caller(),
                async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
                    loop {
                        interval.tick().await;
                        if cancel_token.is_cancelled() {
                            break;
                        }
                        // `poll_once` does poll + drain + evict under one
                        // registry lock and hands back the results so we can
                        // emit events without holding the lock across the
                        // `Event`-channel awaits.
                        let snapshot = bg_registry.poll_once().await;
                        for result in snapshot.results {
                            if result.stall_detected {
                                let _ = tx_event
                                    .send(Event::BackgroundTaskProgress {
                                        id: result.task_id.clone(),
                                        output_delta: result
                                            .output_delta
                                            .clone()
                                            .unwrap_or_default(),
                                        stall_detected: true,
                                    })
                                    .await;
                            }
                            if result.old_status != result.new_status
                                && result.new_status.is_terminal()
                            {
                                // Terminal transition — notification will be drained below
                            }
                        }
                        // Drain notifications and emit as events
                        for notification in snapshot.notifications {
                            let _ = tx_event
                                .send(Event::BackgroundTaskNotification { notification })
                                .await;
                        }
                    }
                },
            );
        }

        // Spawn AgentTeams leader inbox lifecycle watcher. The watcher starts a
        // poller when TeamContext becomes active and cancels it when the team is
        // deleted or the engine shuts down.
        if self.config.features.enabled(Feature::AgentTeams)
            && let Some(team_context) = self.config.team_context.clone()
        {
            let tx_op = self.tx_op.clone();
            let cancel_token = self.cancel_token.clone();
            spawn_supervised(
                "team-lifecycle-watcher",
                std::panic::Location::caller(),
                async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                    let mut active_team: Option<String> = None;
                    let mut poller_token: Option<CancellationToken> = None;
                    loop {
                        interval.tick().await;
                        if cancel_token.is_cancelled() {
                            if let Some(token) = poller_token.take() {
                                token.cancel();
                            }
                            break;
                        }

                        let current_team = {
                            let ctx = team_context.lock().await;
                            ctx.as_ref().map(|ctx| ctx.team_name.clone())
                        };

                        if current_team != active_team {
                            if let Some(token) = poller_token.take() {
                                token.cancel();
                            }
                            active_team = current_team.clone();
                            if let Some(team_name) = current_team {
                                let child_token = cancel_token.child_token();
                                poller_token = Some(child_token.clone());
                                let tx_op = tx_op.clone();
                                spawn_supervised(
                                    "team-leader-inbox-poller",
                                    std::panic::Location::caller(),
                                    crate::tools::team::run_leader_inbox_poller(
                                        team_name,
                                        tx_op,
                                        child_token,
                                    ),
                                );
                            }
                        }
                    }
                },
            );
        }

        while let Some(op) = self.rx_op.recv().await {
            match op {
                Op::SendMessage {
                    content,
                    mode,
                    model,
                    goal_objective,
                    reasoning_effort,
                    reasoning_effort_auto,
                    auto_model,
                    allow_shell,
                    trust_mode,
                    auto_approve,
                    approval_mode,
                    translation_enabled,
                    show_thinking,
                    allowed_tools,
                } => {
                    self.handle_send_message(
                        content,
                        mode,
                        model,
                        goal_objective,
                        reasoning_effort,
                        reasoning_effort_auto,
                        auto_model,
                        allow_shell,
                        trust_mode,
                        auto_approve,
                        approval_mode,
                        translation_enabled,
                        show_thinking,
                        allowed_tools,
                    )
                    .await;
                }
                Op::CancelRequest => {
                    self.cancel_token.cancel();
                    self.reset_cancel_token();
                }
                Op::ApproveToolCall { id } => {
                    // Tool approval handling will be implemented in tools module
                    let _ = self
                        .tx_event
                        .send(Event::status(format!("Approved tool call: {id}")))
                        .await;
                }
                Op::DenyToolCall { id } => {
                    let _ = self
                        .tx_event
                        .send(Event::status(format!("Denied tool call: {id}")))
                        .await;
                }
                Op::SpawnSubAgent { prompt } => {
                    let Some(client) = self.llm_client.clone() else {
                        let message = self
                            .llm_client_error
                            .as_deref()
                            .map(|err| format!("Failed to spawn sub-agent: {err}"))
                            .unwrap_or_else(|| {
                                "Failed to spawn sub-agent: API client not configured".to_string()
                            });
                        let _ = self
                            .tx_event
                            .send(Event::error(ErrorEnvelope::fatal(message)))
                            .await;
                        continue;
                    };

                    let mcp_pool = if self.config.features.enabled(Feature::Mcp) {
                        self.ensure_mcp_pool().await.ok()
                    } else {
                        None
                    };

                    let mut runtime = SubAgentRuntime::new(
                        client,
                        self.session.model.clone(),
                        // Sub-agents don't inherit YOLO mode - use Agent mode defaults
                        self.build_tool_context(AppMode::Agent, self.session.auto_approve),
                        self.session.allow_shell,
                        Some(self.tx_event.clone()),
                        Arc::clone(&self.subagent_manager),
                    )
                    .with_role_models(self.config.subagent_model_overrides.clone())
                    .with_auto_model(self.session.auto_model)
                    .with_reasoning_effort(
                        self.session.reasoning_effort.clone(),
                        self.session.reasoning_effort_auto,
                    )
                    .with_max_spawn_depth(self.config.max_spawn_depth)
                    .with_step_api_timeout(self.config.subagent_api_timeout)
                    .with_mcp_pool(mcp_pool)
                    .background_runtime();
                    let route = resolve_subagent_assignment_route(
                        &runtime,
                        None,
                        &prompt,
                        &SubAgentType::General,
                    )
                    .await;
                    runtime.model = route.model;
                    runtime.reasoning_effort = route.reasoning_effort;
                    runtime.reasoning_effort_auto = false;

                    let result = {
                        let mut manager = self.subagent_manager.write().await;
                        manager.spawn_background(
                            Arc::clone(&self.subagent_manager),
                            runtime,
                            SubAgentType::General,
                            prompt.clone(),
                            None,
                        )
                    };

                    match result {
                        Ok(snapshot) => {
                            let _ = self
                                .tx_event
                                .send(Event::status(format!(
                                    "Spawned sub-agent {}",
                                    snapshot.agent_id
                                )))
                                .await;
                        }
                        Err(err) => {
                            let _ = self
                                .tx_event
                                .send(Event::error(ErrorEnvelope::fatal(format!(
                                    "Failed to spawn sub-agent: {err}"
                                ))))
                                .await;
                        }
                    }
                }
                Op::ListSubAgents => {
                    let agents = {
                        let mut manager = self.subagent_manager.write().await;
                        manager.cleanup(Duration::from_secs(60 * 60));
                        manager.list()
                    };
                    let _ = self.tx_event.send(Event::AgentList { agents }).await;
                }
                Op::ChangeMode { mode } => {
                    let _ = self
                        .tx_event
                        .send(Event::status(format!("Mode changed to: {mode:?}")))
                        .await;
                }
                Op::SetModel { model } => {
                    self.session.auto_model = model.trim().eq_ignore_ascii_case("auto");
                    self.session.model = model;
                    self.config.model.clone_from(&self.session.model);
                    let _ = self
                        .tx_event
                        .send(Event::status(format!(
                            "Model set to: {}",
                            self.session.model
                        )))
                        .await;
                }
                Op::SetCompaction { config } => {
                    let enabled = config.enabled;
                    self.config.compaction = config;
                    let _ = self
                        .tx_event
                        .send(Event::status(format!(
                            "Auto-compaction {}",
                            if enabled { "enabled" } else { "disabled" }
                        )))
                        .await;
                }
                Op::SyncSession {
                    session_id,
                    messages,
                    system_prompt,
                    system_prompt_override,
                    model,
                    workspace,
                } => {
                    if let Some(session_id) = session_id {
                        self.session.id = session_id;
                    } else if messages.is_empty() && system_prompt.is_none() {
                        self.session.id = uuid::Uuid::new_v4().to_string();
                    }
                    self.session.messages = messages;
                    self.session.compaction_summary_prompt =
                        extract_compaction_summary_prompt(system_prompt.clone());
                    self.session.system_prompt = system_prompt;
                    self.session.system_prompt_override =
                        system_prompt_override && self.session.system_prompt.is_some();
                    self.session.auto_model = model.trim().eq_ignore_ascii_case("auto");
                    self.session.model = model;
                    self.session.workspace = workspace.clone();
                    self.config.model.clone_from(&self.session.model);
                    self.config.workspace = workspace.clone();
                    let ctx = crate::project_context::load_project_context_with_parents(&workspace);
                    self.session.project_context = if ctx.has_instructions() {
                        Some(ctx)
                    } else {
                        None
                    };
                    self.session.rebuild_working_set();
                    self.rehydrate_latest_canonical_state();
                    self.emit_session_updated().await;
                    let _ = self
                        .tx_event
                        .send(Event::status("Session context synced".to_string()))
                        .await;
                }
                Op::CompactContext => {
                    self.handle_manual_compaction(crate::core::ops::CompactMode::Full)
                        .await;
                }
                Op::CompactContextWithMode { mode } => {
                    self.handle_manual_compaction(mode).await;
                }
                Op::PurgeContext => {
                    self.handle_purge().await;
                }
                Op::EditLastTurn { new_message } => {
                    // #383: /edit — remove the last user+assistant exchange
                    // from the session, then re-send with the new content.
                    // Pop messages from the tail until we've removed the
                    // most recent user message and everything after it.
                    // First, find the last user message index.
                    let mut cut = None;
                    for (idx, msg) in self.session.messages.iter().enumerate().rev() {
                        if msg.role == "user" {
                            cut = Some(idx);
                            break;
                        }
                    }
                    if let Some(idx) = cut {
                        self.session.messages.truncate(idx);
                    }
                    // Now dispatch the new message as a normal send,
                    // reusing the engine's stored mode/model config.
                    let mode = AppMode::Agent; // default fallback
                    self.handle_send_message(
                        new_message,
                        mode,
                        self.session.model.clone(),
                        self.config.goal_objective.clone(),
                        self.session.reasoning_effort.clone(),
                        self.session.reasoning_effort_auto,
                        self.session.auto_model,
                        self.session.allow_shell,
                        self.session.trust_mode,
                        self.session.auto_approve,
                        self.session.approval_mode,
                        self.config.translation_enabled,
                        self.config.show_thinking,
                        self.config.allowed_tools.clone(),
                    )
                    .await;
                }
                Op::Shutdown => {
                    break;
                }

                // Background task operations.
                #[allow(dead_code)]
                Op::StartBackgroundShell {
                    command,
                    cwd,
                    timeout_secs,
                } => {
                    let timeout_ms = timeout_secs.unwrap_or(600).saturating_mul(1_000);
                    let cwd_str = cwd.as_ref().map(|path| path.to_string_lossy().to_string());
                    let result = {
                        let mut shell =
                            self.shell_manager.lock().unwrap_or_else(|p| p.into_inner());
                        shell.execute(&command, cwd_str.as_deref(), timeout_ms, true)
                    };
                    match result {
                        Ok(shell_result) => {
                            if let Some(shell_id) = shell_result.task_id.clone() {
                                let cwd = cwd.unwrap_or_else(|| self.session.cwd.clone());
                                let task = self
                                    .host
                                    .bg_registry()
                                    .register_shell_task(shell_id, command, cwd)
                                    .await;
                                let _ = self
                                    .tx_event
                                    .send(Event::BackgroundTaskStarted {
                                        id: task.id,
                                        task_type: task.task_type,
                                        description: task.description,
                                    })
                                    .await;
                            } else {
                                let status = if shell_result.status == ShellStatus::Completed {
                                    "completed"
                                } else {
                                    "failed"
                                };
                                let _ = self
                                    .tx_event
                                    .send(Event::status(format!(
                                        "Background shell {status} without task id"
                                    )))
                                    .await;
                            }
                        }
                        Err(err) => {
                            let _ = self
                                .tx_event
                                .send(Event::error(ErrorEnvelope::fatal(format!(
                                    "Failed to start background shell: {err}"
                                ))))
                                .await;
                        }
                    }
                }
                #[allow(dead_code)]
                Op::CancelBackgroundTask { id } => {
                    let result = self.host.bg_registry().cancel_task(&id).await;
                    match result {
                        Ok(()) => {
                            let _ = self
                                .tx_event
                                .send(Event::status(format!("Cancelled background task {id}")))
                                .await;
                        }
                        Err(err) => {
                            let _ = self
                                .tx_event
                                .send(Event::error(ErrorEnvelope::transient(format!(
                                    "Failed to cancel background task {id}: {err}"
                                ))))
                                .await;
                        }
                    }
                }
                #[allow(dead_code)]
                Op::ListBackgroundTasks => {
                    let tasks = self.host.bg_registry().list_tasks().await;
                    let _ = self
                        .tx_event
                        .send(Event::BackgroundTaskList { tasks })
                        .await;
                }
                #[allow(dead_code)]
                Op::PollBackgroundTask { id } => {
                    let delta = self.host.bg_registry().read_output_delta(&id).await;
                    if let Some(output_delta) = delta.filter(|delta| !delta.is_empty()) {
                        let _ = self
                            .tx_event
                            .send(Event::BackgroundTaskProgress {
                                id,
                                output_delta,
                                stall_detected: false,
                            })
                            .await;
                    }
                }
                #[allow(dead_code)]
                Op::BackgroundCurrentShell => {
                    let tasks = self.host.bg_registry().background_all().await;
                    let _ = self
                        .tx_event
                        .send(Event::status(format!(
                            "Requested backgrounding for {} shell task(s)",
                            tasks.len()
                        )))
                        .await;
                }
                #[allow(dead_code)]
                Op::BackgroundAll => {
                    let tasks = self.host.bg_registry().background_all().await;
                    let _ = self
                        .tx_event
                        .send(Event::status(format!(
                            "Requested backgrounding for {} shell task(s)",
                            tasks.len()
                        )))
                        .await;
                }
                #[allow(dead_code)]
                Op::StartDreamTask { memory_path } => {
                    let path = memory_path
                        .or_else(|| self.host.runtime_services.task_data_dir.clone())
                        .unwrap_or_else(|| {
                            self.session.workspace.join(".codesmith").join("memory")
                        });
                    let task = self.host.bg_registry().register_dream_task(path).await;
                    let _ = self
                        .tx_event
                        .send(Event::BackgroundTaskStarted {
                            id: task.id.clone(),
                            task_type: task.task_type,
                            description: task.description.clone(),
                        })
                        .await;
                    let _ = self
                        .host
                        .bg_registry()
                        .update_task_status(&task.id, BackgroundTaskStatus::Completed, None)
                        .await;
                }
                Op::TeamInboxDispatch { dispatch } => {
                    self.handle_team_inbox_dispatch(dispatch).await;
                }
            }
        }

        // #420: graceful MCP shutdown — send SIGTERM and give stdio servers
        // a brief window to exit before drop fires SIGKILL via kill_on_drop.
        // Best-effort: pool may not exist (no MCP configured) and the lock
        // can fail under contention; either way the kill_on_drop fallback
        // still reaps the children.
        if let Some(pool) = self.mcp_pool.as_ref() {
            let mut guard = pool.lock().await;
            guard.shutdown_all().await;
        }
    }

    async fn emit_session_updated(&self) {
        let _ = self
            .tx_event
            .send(Event::SessionUpdated {
                session_id: self.session.id.clone(),
                messages: self.session.messages.clone(),
                system_prompt: self.session.system_prompt.clone(),
                model: self.session.model.clone(),
                workspace: self.session.workspace.clone(),
            })
            .await;
    }

    async fn add_session_message(&mut self, message: Message) {
        self.session.add_message(message);
        self.emit_session_updated().await;
    }

    fn turn_metadata_block(
        &self,
        routed_model: &str,
        auto_model: bool,
        reasoning_effort: Option<&str>,
        reasoning_effort_auto: bool,
    ) -> ContentBlock {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let working_set_summary = self
            .session
            .working_set
            .summary_block(&self.config.workspace)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let conditional_skills = self.conditional_skills_block();

        let mut lines = vec![format!("Current local date: {today}")];
        if auto_model {
            lines.push(format!("Auto model route: {routed_model}"));
        }
        if reasoning_effort_auto && let Some(reasoning_effort) = reasoning_effort {
            lines.push(format!("Auto reasoning effort: {reasoning_effort}"));
        }
        if let Some(working_set_summary) = working_set_summary {
            lines.push(working_set_summary);
        }
        if let Some(conditional_skills) = conditional_skills {
            lines.push(conditional_skills);
        }
        let summary = lines.join("\n");

        ContentBlock::Text {
            text: format!("<turn_meta>\n{summary}\n</turn_meta>"),
            cache_control: None,
        }
    }

    fn conditional_skills_block(&self) -> Option<String> {
        let paths = self.session.working_set.top_paths(16);
        if paths.is_empty() {
            return None;
        }
        let registry = crate::skills::discover_for_workspace_and_dir(
            &self.config.workspace,
            &self.config.skills_dir,
        );
        let matches = crate::skills::matching_conditional_skills(&registry, &paths);
        if matches.is_empty() {
            return None;
        }
        let mut lines = vec!["## Matched Conditional Skills".to_string()];
        for skill in matches.into_iter().take(6) {
            let reason = skill
                .when_to_use
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    let description = skill.description.trim();
                    (!description.is_empty()).then_some(description)
                })
                .unwrap_or("");
            if reason.is_empty() {
                lines.push(format!(
                    "- {} matched paths [{}]. Load with `load_skill` if relevant. Source: {}",
                    skill.name,
                    skill.paths.join(", "),
                    skill.path.display()
                ));
            } else {
                lines.push(format!(
                    "- {}: {} Matched paths [{}]. Load with `load_skill` if relevant. Source: {}",
                    skill.name,
                    reason,
                    skill.paths.join(", "),
                    skill.path.display()
                ));
            }
        }
        Some(lines.join("\n"))
    }

    fn user_text_message_with_turn_metadata(&self, text: String) -> Message {
        self.user_text_message_with_turn_metadata_for_route(
            text,
            &self.session.model,
            self.session.auto_model,
            self.session.reasoning_effort.as_deref(),
            self.session.reasoning_effort_auto,
        )
    }

    fn user_text_message_with_turn_metadata_for_route(
        &self,
        text: String,
        routed_model: &str,
        auto_model: bool,
        reasoning_effort: Option<&str>,
        reasoning_effort_auto: bool,
    ) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![
                self.turn_metadata_block(
                    routed_model,
                    auto_model,
                    reasoning_effort,
                    reasoning_effort_auto,
                ),
                ContentBlock::Text {
                    text,
                    cache_control: None,
                },
            ],
        }
    }

    /// Handle a send message operation
    #[allow(clippy::too_many_arguments)]
    async fn handle_send_message(
        &mut self,
        content: String,
        mode: AppMode,
        model: String,
        goal_objective: Option<String>,
        reasoning_effort: Option<String>,
        reasoning_effort_auto: bool,
        auto_model: bool,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: crate::tui::approval::ApprovalMode,
        translation_enabled: bool,
        show_thinking: bool,
        allowed_tools: Option<Vec<String>>,
    ) {
        // Reset cancel token for fresh turn (in case previous was cancelled)
        self.reset_cancel_token();

        // Drain stale steer messages from previous turns.
        while self.rx_steer.try_recv().is_ok() {}

        // Create turn context first so start event includes a stable turn id.
        let mut turn = TurnContext::new(self.config.max_steps);
        self.turn_counter = self.turn_counter.saturating_add(1);
        self.capacity_controller.mark_turn_start(self.turn_counter);

        // Emit turn started event IMMEDIATELY so the UI knows the turn is
        // active. The snapshot below can take 30+ seconds on slow filesystems
        // (e.g. WSL2 /mnt/c) and must not delay the TurnStarted event.
        let _ = self
            .tx_event
            .send(Event::TurnStarted {
                turn_id: turn.id.clone(),
            })
            .await;

        // Snapshot the workspace BEFORE we touch a single tool. Run the git
        // work on the blocking pool so the async runtime stays responsive;
        // failure is non-fatal (the helper logs at WARN).
        if self.config.snapshots_enabled {
            // Clone the user prompt now — `content` is moved into
            // `user_text_message_with_turn_metadata_for_route` below, so we need
            // a copy for both pre- and post-turn snapshot labels. The
            // label carries a truncated first line so `/restore`
            // listings are human-readable.
            let snapshot_prompt = content.clone();
            let pre_workspace = self.session.workspace.clone();
            let pre_seq = self.turn_counter;
            let pre_cap = self.config.snapshots_max_workspace_bytes;
            let _ = tokio::task::spawn_blocking(move || {
                pre_turn_snapshot(&pre_workspace, pre_seq, pre_cap, Some(&snapshot_prompt))
            })
            .await;
        }

        // A new turn means any leftover retry banner (success cleared
        // it, failure pinned it) is no longer relevant — reset to idle
        // so the footer doesn't display a stale failure row across
        // turns (#499).
        crate::retry_status::clear();

        // Clone user prompt for post-turn snapshot label before `content`
        // is moved into `user_text_message_with_turn_metadata_for_route` below.
        let snapshot_prompt_post = content.clone();

        // Check if we have the appropriate client
        if self.llm_client.is_none() {
            let message = self
                .llm_client_error
                .as_deref()
                .map(|err| format!("Failed to send message: {err}"))
                .unwrap_or_else(|| "Failed to send message: API client not configured".to_string());
            let _ = self
                .tx_event
                .send(Event::error(ErrorEnvelope::fatal_auth(message.clone())))
                .await;
            let _ = self
                .tx_event
                .send(Event::TurnComplete {
                    usage: turn.usage.clone(),
                    status: TurnOutcomeStatus::Failed,
                    error: Some(message),
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        }

        self.session
            .working_set
            .observe_user_message(&content, &self.session.workspace);
        let force_update_plan_first = should_force_update_plan_first(mode, &content);

        // Add user message to session
        let user_msg = self.user_text_message_with_turn_metadata_for_route(
            content,
            &model,
            auto_model,
            reasoning_effort.as_deref(),
            reasoning_effort_auto,
        );
        self.session.add_message(user_msg);

        let previous_goal_objective = self.config.goal_objective.clone();

        self.session.model = model;
        self.config.model.clone_from(&self.session.model);
        self.config.goal_objective = goal_objective.clone();
        if normalized_goal_objective(previous_goal_objective.as_deref())
            != normalized_goal_objective(goal_objective.as_deref())
        {
            sync_goal_state_from_host(
                &self.config.goal_state,
                normalized_goal_objective(goal_objective.as_deref()).as_deref(),
                None,
                false,
            );
        }
        self.config.allowed_tools = allowed_tools;
        self.session.reasoning_effort = reasoning_effort;
        self.session.reasoning_effort_auto = reasoning_effort_auto;
        self.session.auto_model = auto_model;
        self.session.allow_shell = allow_shell;
        self.config.allow_shell = allow_shell;
        self.session.trust_mode = trust_mode;
        self.config.trust_mode = trust_mode;
        self.config.translation_enabled = translation_enabled;
        self.config.show_thinking = show_thinking;
        self.session.auto_approve = auto_approve;
        self.session.approval_mode = if auto_approve {
            crate::tui::approval::ApprovalMode::Auto
        } else {
            approval_mode
        };

        // Update system prompt to match current mode and include persisted compaction context.
        self.refresh_system_prompt(mode);
        self.emit_session_updated().await;

        // Build tool registry and tool list for the current mode
        let todo_list = self.config.todos.clone();
        let plan_state = self.config.plan_state.clone();

        let tool_context = self.build_tool_context(mode, auto_approve);
        let mut builder = self.build_turn_tool_registry_builder(mode, todo_list, plan_state);

        let fork_context_for_runtime = if self.config.features.enabled(Feature::Subagents) {
            let state = StructuredState::capture(
                mode.label(),
                self.config.workspace.clone(),
                std::env::current_dir().ok(),
                &self.session.working_set,
                &self.config.todos,
                &self.config.plan_state,
                Some(&self.subagent_manager),
            )
            .await;
            Some(SubAgentForkContext {
                system: self.session.system_prompt.clone(),
                messages: self.messages_with_turn_metadata(),
                structured_state_block: state.to_system_block(),
                current_assistant_text: None,
                current_turn_tool_calls: None,
            })
        } else {
            None
        };

        // Mailbox for structured sub-agent envelopes (#128/#130). One per
        // turn: the receiver is drained by a short-lived task that converts
        // envelopes into `Event::SubAgentMailbox` so the UI can route them
        // to the matching in-transcript card. The drainer exits naturally
        // when every cloned sender is dropped at turn-end.
        let mailbox_for_runtime = if self.config.features.enabled(Feature::Subagents) {
            let cancel_token = self.cancel_token.child_token();
            let (mailbox, mut receiver) = Mailbox::new(cancel_token.clone());
            let tx_event_clone = self.tx_event.clone();
            spawn_supervised(
                "subagent-mailbox-drainer",
                std::panic::Location::caller(),
                async move {
                    while let Some(envelope) = receiver.recv().await {
                        if tx_event_clone
                            .send(Event::SubAgentMailbox {
                                seq: envelope.seq,
                                message: envelope.message,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                },
            );
            Some((mailbox, cancel_token))
        } else {
            None
        };

        let mcp_pool = if self.config.features.enabled(Feature::Mcp) {
            self.ensure_mcp_pool().await.ok()
        } else {
            None
        };

        let mut tool_registry = match mode {
            AppMode::Agent | AppMode::Yolo => {
                if self.config.features.enabled(Feature::Subagents) {
                    let runtime = if let Some(client) = self.llm_client.clone() {
                        let mut rt = SubAgentRuntime::new(
                            client,
                            self.session.model.clone(),
                            tool_context.clone(),
                            self.session.allow_shell,
                            Some(self.tx_event.clone()),
                            Arc::clone(&self.subagent_manager),
                        )
                        .with_role_models(self.config.subagent_model_overrides.clone())
                        .with_auto_model(self.session.auto_model)
                        .with_reasoning_effort(
                            self.session.reasoning_effort.clone(),
                            self.session.reasoning_effort_auto,
                        )
                        .with_max_spawn_depth(self.config.max_spawn_depth)
                        .with_step_api_timeout(self.config.subagent_api_timeout)
                        .with_mcp_pool(mcp_pool.clone())
                        .with_parent_completion_tx(self.tx_subagent_completion.clone());
                        if let Some(context) = fork_context_for_runtime.clone() {
                            rt = rt.with_fork_context(context);
                        }
                        if let Some((mailbox, cancel_token)) = mailbox_for_runtime.as_ref() {
                            rt = rt
                                .with_mailbox(mailbox.clone())
                                .with_cancel_token(cancel_token.clone());
                        }
                        Some(rt)
                    } else {
                        None
                    };
                    Some(
                        builder
                            .with_subagent_tools(
                                self.subagent_manager.clone(),
                                runtime.expect("sub-agent runtime should exist with active client"),
                            )
                            .build(tool_context),
                    )
                } else {
                    Some(builder.build(tool_context))
                }
            }
            AppMode::Coordinator => {
                // Coordinator mode requires subagents — it must be able to
                // spawn worker agents. Add subagent + send_message tools.
                if self.config.features.enabled(Feature::Subagents)
                    && let Some(client) = self.llm_client.clone()
                {
                    let mut rt = SubAgentRuntime::new(
                        client,
                        self.session.model.clone(),
                        tool_context.clone(),
                        true, // Coordinator workers need shell access
                        Some(self.tx_event.clone()),
                        Arc::clone(&self.subagent_manager),
                    )
                    .with_role_models(self.config.subagent_model_overrides.clone())
                    .with_auto_model(self.session.auto_model)
                    .with_reasoning_effort(
                        self.session.reasoning_effort.clone(),
                        self.session.reasoning_effort_auto,
                    )
                    .with_max_spawn_depth(self.config.max_spawn_depth)
                    .with_step_api_timeout(self.config.subagent_api_timeout)
                    .with_mcp_pool(mcp_pool.clone())
                    .with_parent_completion_tx(self.tx_subagent_completion.clone());
                    if let Some(context) = fork_context_for_runtime.clone() {
                        rt = rt.with_fork_context(context);
                    }
                    if let Some((mailbox, cancel_token)) = mailbox_for_runtime.as_ref() {
                        rt = rt
                            .with_mailbox(mailbox.clone())
                            .with_cancel_token(cancel_token.clone());
                    }
                    builder = builder.with_subagent_tools(self.subagent_manager.clone(), rt);
                }
                // send_message — coordinator needs messaging but NOT
                // team_create/team_delete.
                if self.config.features.enabled(Feature::AgentTeams)
                    && let Some(tc) = self.config.team_context.clone()
                {
                    builder = builder.with_send_message_tool(tc);
                }
                builder = builder.with_task_stop_tool();
                Some(builder.build(tool_context))
            }
            _ => Some(builder.build(tool_context)),
        };

        // Load plugin tools from the user's tools directory and apply any
        // config.toml overrides. Explicit overrides win over auto-discovered
        // scripts with the same tool name.
        let mut plugin_tool_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if let Some(ref mut tool_registry) = tool_registry {
            plugin_tool_names = configure_plugin_tools(tool_registry, self.config.tools.as_ref());
        }

        let mcp_tools = if self.config.features.enabled(Feature::Mcp) {
            self.mcp_tools().await
        } else {
            Vec::new()
        };
        let tools = tool_registry.as_ref().map(|registry| {
            let mut catalog = build_model_tool_catalog(
                registry.to_api_tools_with_cache(true),
                mcp_tools,
                mode,
                &self.config.tools_always_load,
            );
            for tool in &mut catalog {
                if plugin_tool_names.contains(&tool.name) {
                    tool.defer_loading = Some(false);
                }
            }
            catalog
        });
        let tool_catalog_for_event = tools.clone();
        let base_url_for_event = self
            .llm_client
            .as_ref()
            .map(|client| client.base_url().to_string());

        // Main turn loop
        let (status, error) = self
            .handle_deepseek_turn(
                &mut turn,
                tool_registry.as_ref(),
                tools,
                mode,
                force_update_plan_first,
            )
            .await;

        // Sync session.cwd from worktree state after each turn.
        {
            let wt_state = self.config.worktree_state.lock().unwrap();
            if wt_state.active && wt_state.worktree_path.is_some() {
                self.session.cwd = wt_state.worktree_path.clone().unwrap();
            } else {
                self.session.cwd = self.session.workspace.clone();
            }
        }

        // Checkpoint-restart cycle boundary (issue #124). Run BEFORE
        // TurnComplete so the engine loop doesn't block the terminal after
        // the turn signal (#234). The status chip ("↻ context refreshing...")
        // is visible during the wait, and once TurnComplete fires the
        // terminal is immediately responsive. No-op unless the estimated
        // input tokens have crossed the per-cycle threshold.
        if matches!(status, TurnOutcomeStatus::Completed) {
            self.maybe_advance_cycle(mode).await;
        }

        // Update session usage
        self.session.total_usage.add(&turn.usage);

        // Emit turn complete event — after all post-turn bookkeeping so
        // the terminal is immediately responsive when the UI receives it.
        let _ = self
            .tx_event
            .send(Event::TurnComplete {
                usage: turn.usage,
                status,
                error,
                tool_catalog: tool_catalog_for_event,
                base_url: base_url_for_event,
            })
            .await;

        // Post-turn snapshot. Fire-and-forget: TurnComplete is already
        // emitted, so the UI is unblocked and the user can type / select /
        // paste immediately (#234). The git work proceeds on the blocking
        // pool without forcing the engine loop to await it.
        if self.config.snapshots_enabled {
            // `snapshot_prompt_post` was cloned from `content` above,
            // before `content` was moved into the session messages.
            let post_workspace = self.session.workspace.clone();
            let post_seq = self.turn_counter;
            let post_cap = self.config.snapshots_max_workspace_bytes;
            crate::utils::spawn_blocking_supervised("post-turn-snapshot", move || {
                post_turn_snapshot(
                    &post_workspace,
                    post_seq,
                    post_cap,
                    Some(&snapshot_prompt_post),
                );
            });
        }
    }

    async fn handle_manual_compaction(&mut self, mode: crate::core::ops::CompactMode) {
        if matches!(mode, crate::core::ops::CompactMode::Memory) {
            self.handle_session_memory_compaction().await;
            return;
        }

        let id = format!("compact_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let zero_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            ..Usage::default()
        };
        let Some(client) = self.llm_client.clone() else {
            let message = "Manual compaction unavailable: API client not configured".to_string();
            self.emit_compaction_failed(id, false, message.clone())
                .await;
            let _ = self
                .tx_event
                .send(Event::error(ErrorEnvelope::fatal_auth(message.clone())))
                .await;
            let _ = self
                .tx_event
                .send(Event::TurnComplete {
                    usage: zero_usage,
                    status: TurnOutcomeStatus::Failed,
                    error: Some(message),
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        };

        let start_message = "Manual context compaction started".to_string();
        self.emit_compaction_started(id.clone(), false, start_message)
            .await;

        let compaction_pins = self
            .session
            .working_set
            .pinned_message_indices(&self.session.messages, &self.session.workspace);
        let compaction_paths = self.session.working_set.top_paths(24);
        let messages_before = self.session.messages.len();
        let mut turn_status = TurnOutcomeStatus::Completed;
        let mut turn_error = None;

        let enhancements = self.build_compaction_enhancements();
        match compact_messages_safe(
            &*client,
            &self.session.messages,
            &self.config.compaction,
            Some(&self.session.workspace),
            Some(&compaction_pins),
            Some(&compaction_paths),
            enhancements.as_ref(),
        )
        .await
        {
            Ok(result) => {
                if !result.messages.is_empty() || self.session.messages.is_empty() {
                    let messages_after = result.messages.len();
                    self.session.messages = result.messages;
                    self.merge_compaction_summary(result.summary_prompt);
                    self.reinject_compaction_attachments(context_input_budget_for_provider(
                        self.api_provider,
                        &self.session.model,
                    ))
                    .await;
                    self.emit_session_updated().await;
                    let removed = messages_before.saturating_sub(messages_after);
                    let message = if result.retries_used > 0 {
                        format!(
                            "Compaction complete: {messages_before} → {messages_after} messages ({removed} removed, {} retries)",
                            result.retries_used
                        )
                    } else {
                        format!(
                            "Compaction complete: {messages_before} → {messages_after} messages ({removed} removed)"
                        )
                    };
                    self.emit_compaction_completed(
                        id,
                        false,
                        message,
                        Some(messages_before),
                        Some(messages_after),
                    )
                    .await;
                } else {
                    let message = "Compaction skipped: produced empty result".to_string();
                    self.emit_compaction_failed(id, false, message.clone())
                        .await;
                    turn_status = TurnOutcomeStatus::Failed;
                    turn_error = Some(message);
                }
            }
            Err(err) => {
                let message = format!("Manual context compaction failed: {err}");
                self.emit_compaction_failed(id, false, message.clone())
                    .await;
                let _ = self.tx_event.send(Event::status(message.clone())).await;
                turn_status = TurnOutcomeStatus::Failed;
                turn_error = Some(message);
            }
        }

        let _ = self
            .tx_event
            .send(Event::TurnComplete {
                usage: zero_usage,
                status: turn_status,
                error: turn_error,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    }

    async fn handle_session_memory_compaction(&mut self) {
        let id = format!("compact_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let zero_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            ..Usage::default()
        };

        self.emit_compaction_started(
            id.clone(),
            false,
            "Session-memory context compaction started".to_string(),
        )
        .await;

        let (memory_source, memory_content) = match self.session_memory_compaction_content() {
            Ok(Some(content)) => content,
            Ok(None) => {
                let message = "Session-memory compaction skipped: no enabled MEMORY.md or user memory content found".to_string();
                self.emit_compaction_failed(id, false, message.clone())
                    .await;
                let _ = self.tx_event.send(Event::status(message.clone())).await;
                let _ = self
                    .tx_event
                    .send(Event::TurnComplete {
                        usage: zero_usage,
                        status: TurnOutcomeStatus::Failed,
                        error: Some(message),
                        tool_catalog: None,
                        base_url: None,
                    })
                    .await;
                return;
            }
            Err(err) => {
                let message = format!("Session-memory compaction failed to load memory: {err}");
                self.emit_compaction_failed(id, false, message.clone())
                    .await;
                let _ = self.tx_event.send(Event::status(message.clone())).await;
                let _ = self
                    .tx_event
                    .send(Event::TurnComplete {
                        usage: zero_usage,
                        status: TurnOutcomeStatus::Failed,
                        error: Some(message),
                        tool_catalog: None,
                        base_url: None,
                    })
                    .await;
                return;
            }
        };

        let config = &self.session.session_memory_compact_config;
        if !should_use_session_memory_compact(&memory_content, &self.session.messages, config) {
            let message = format!(
                "Session-memory compaction skipped: conversation is below the {} token threshold or memory is empty",
                config.min_retain_tokens
            );
            self.emit_compaction_failed(id, false, message.clone())
                .await;
            let _ = self.tx_event.send(Event::status(message.clone())).await;
            let _ = self
                .tx_event
                .send(Event::TurnComplete {
                    usage: zero_usage,
                    status: TurnOutcomeStatus::Failed,
                    error: Some(message),
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        }

        let messages_before = self.session.messages.len();
        let result = session_memory_compact(&self.session.messages, &memory_content, config);
        let messages_after = result.messages.len();

        if result.removed_count == 0 || messages_after == messages_before {
            let message =
                "Session-memory compaction skipped: no messages could be removed".to_string();
            self.emit_compaction_failed(id, false, message.clone())
                .await;
            let _ = self.tx_event.send(Event::status(message.clone())).await;
            let _ = self
                .tx_event
                .send(Event::TurnComplete {
                    usage: zero_usage,
                    status: TurnOutcomeStatus::Failed,
                    error: Some(message),
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        }

        self.session.messages = result.messages;
        self.merge_compaction_summary(result.summary_prompt);
        self.reinject_compaction_attachments(context_input_budget_for_provider(
            self.api_provider,
            &self.session.model,
        ))
        .await;
        self.emit_session_updated().await;
        let removed = messages_before.saturating_sub(messages_after);
        let message = format!(
            "Session-memory compaction complete using {memory_source}: {messages_before} → {messages_after} messages ({removed} removed)"
        );
        self.emit_compaction_completed(
            id,
            false,
            message,
            Some(messages_before),
            Some(messages_after),
        )
        .await;
        let _ = self
            .tx_event
            .send(Event::TurnComplete {
                usage: zero_usage,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    }

    fn session_memory_compaction_content(&self) -> std::io::Result<Option<(String, String)>> {
        if self.config.kod_enabled {
            let entrypoint =
                crate::knowledge::paths::resolve_memory_entrypoint(&self.config.memory_dir);
            match std::fs::read_to_string(&entrypoint) {
                Ok(content) if !content.trim().is_empty() => {
                    return Ok(Some((entrypoint.display().to_string(), content)));
                }
                Ok(_) => return Ok(None),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err),
            }
        }

        if self.config.memory_enabled {
            match std::fs::read_to_string(&self.config.memory_path) {
                Ok(content) if !content.trim().is_empty() => {
                    return Ok(Some((
                        self.config.memory_path.display().to_string(),
                        content,
                    )));
                }
                Ok(_) => return Ok(None),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err),
            }
        }

        Ok(None)
    }

    /// Build the [`HookContext`] used when firing `PreCompact` hooks from
    /// the compaction path. Kept minimal: workspace, model, and current
    /// input-token estimate — enough for a hook to decide what to preserve.
    fn build_compaction_hook_context(&self) -> HookContext {
        HookContext::new()
            .with_workspace(self.session.workspace.clone())
            .with_model(&self.session.model)
            .with_tokens(self.estimated_input_tokens().min(u32::MAX as usize) as u32)
    }

    /// Assemble the optional enhancements handed to [`compact_messages_safe`]:
    /// a cloned `HookExecutor` (so the caller may mutate session state after
    /// the call) plus whatever session-memory content is currently on disk.
    ///
    /// Returns `None` when neither hooks nor session-memory material is
    /// available, so the compaction primitive takes its untouched fast path.
    fn build_compaction_enhancements(&self) -> Option<CompactionEnhancements> {
        let hooks = self.host.hooks.clone().map(|executor| {
            let session_id = executor.session_id().to_string();
            let context = self
                .build_compaction_hook_context()
                .with_session_id(&session_id);
            (executor, context)
        });

        let session_memory = match self.session_memory_compaction_content() {
            Ok(Some((_source, content))) => Some(SessionMemorySidecar {
                memory_content: content,
                config: self.session.session_memory_compact_config.clone(),
            }),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(
                    target: "compaction",
                    error = %err,
                    "failed to load session memory for session-memory-first compaction; skipping",
                );
                None
            }
        };

        if hooks.is_none() && session_memory.is_none() {
            None
        } else {
            Some(CompactionEnhancements {
                hooks,
                session_memory,
            })
        }
    }

    async fn handle_purge(&mut self) {
        let zero_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            ..Usage::default()
        };
        let Some(client) = self.llm_client.clone() else {
            let message = "Purge unavailable: API client not configured".to_string();
            emit_purge_failed(&self.tx_event, message.clone()).await;
            let _ = self
                .tx_event
                .send(Event::error(ErrorEnvelope::fatal_auth(message.clone())))
                .await;
            let _ = self
                .tx_event
                .send(Event::TurnComplete {
                    usage: zero_usage,
                    status: TurnOutcomeStatus::Failed,
                    error: Some(message),
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        };

        emit_purge_started(
            &self.tx_event,
            "Agent context purge in progress\u{2026}".to_string(),
        )
        .await;
        let messages_before = self.session.messages.len();

        let (status, error) = match run_purge(
            client.as_ref(),
            &self.session.messages,
            &self.session.model,
            self.session.reasoning_effort.clone(),
            effective_max_output_tokens_for_provider(self.api_provider, &self.session.model),
        )
        .await
        {
            Ok(result) => {
                let messages_after = result.messages.len();
                self.session.messages = result.messages;
                self.emit_session_updated().await;

                let summary = format!(
                    "Purge complete: {messages_before} → {messages_after} messages \
                         ({} removed, {} condensed)",
                    result.removed_count, result.replaced_count,
                );
                emit_purge_completed(
                    &self.tx_event,
                    messages_before,
                    messages_after,
                    result.removed_count,
                    result.replaced_count,
                    summary,
                )
                .await;
                (TurnOutcomeStatus::Completed, None)
            }
            Err(e) => {
                emit_purge_failed(&self.tx_event, e.clone()).await;
                (TurnOutcomeStatus::Failed, Some(e))
            }
        };

        let _ = self
            .tx_event
            .send(Event::TurnComplete {
                usage: zero_usage,
                status,
                error,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    }

    fn estimated_input_tokens(&self) -> usize {
        estimate_input_tokens_conservative(
            &self.session.messages,
            self.session.system_prompt.as_ref(),
        )
    }

    fn trim_oldest_messages_to_budget(&mut self, target_input_budget: usize) -> usize {
        let mut removed = 0usize;
        while self.session.messages.len() > MIN_RECENT_MESSAGES_TO_KEEP
            && self.estimated_input_tokens() > target_input_budget
        {
            self.session.messages.remove(0);
            removed = removed.saturating_add(1);
        }
        removed
    }

    async fn recover_context_overflow(
        &mut self,
        client: &dyn crate::llm_client::LlmClient,
        reason: &str,
    ) -> bool {
        let Some(target_budget) =
            context_input_budget_for_provider(self.api_provider, &self.session.model)
        else {
            return false;
        };

        let id = format!("compact_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let start_message = format!("Emergency context compaction started ({reason})");
        self.emit_compaction_started(id.clone(), true, start_message)
            .await;

        let before_tokens = self.estimated_input_tokens();
        let before_count = self.session.messages.len();

        // Phase 1: Responsive compact cascade — try cheapest recovery first.
        use crate::compaction::responsive_compact::{
            ResponsiveCompactAction, next_recovery_action,
        };
        let mut responsive_state = self.session.responsive_compact_state.clone();
        let mut recovery_attempt = 0u32;

        loop {
            let action = next_recovery_action(&responsive_state, recovery_attempt);
            match action {
                ResponsiveCompactAction::MicroCompact => {
                    let mut messages = self.session.messages.clone();
                    let bytes = crate::compaction::micro_compact::micro_compact_messages(
                        &mut messages,
                        &mut self.session.micro_compact_state,
                    );
                    if bytes > 0 {
                        self.session.messages = messages;
                        if self.estimated_input_tokens() <= target_budget {
                            crate::compaction::post_compact_cleanup::post_compact_cleanup(
                                &mut self.session,
                            );
                            let _ = self
                                .tx_event
                                .send(Event::status(
                                    "Emergency recovery: micro-compaction cleared enough context"
                                        .to_string(),
                                ))
                                .await;
                            self.emit_compaction_completed(
                                id.clone(),
                                true,
                                "Micro-compaction recovery".to_string(),
                                Some(before_count),
                                Some(self.session.messages.len()),
                            )
                            .await;
                            return true;
                        }
                    }
                    responsive_state.record_overflow();
                }
                ResponsiveCompactAction::PartialFrom | ResponsiveCompactAction::PartialUpTo => {
                    let direction = if action == ResponsiveCompactAction::PartialFrom {
                        crate::compaction::partial_compact::PartialCompactDirection::From
                    } else {
                        crate::compaction::partial_compact::PartialCompactDirection::UpTo
                    };
                    let pivot = crate::compaction::partial_compact::find_pivot_for_budget(
                        &self.session.messages,
                        direction,
                        target_budget / 2,
                    );
                    let request = crate::compaction::partial_compact::PartialCompactRequest {
                        direction,
                        pivot_index: pivot,
                        model: self.config.compaction.model.clone(),
                        user_feedback: Some(reason.to_string()),
                    };
                    match crate::compaction::partial_compact::partial_compact(
                        client,
                        &self.session.messages,
                        &request,
                        self.config.compaction.cache_summary,
                    )
                    .await
                    {
                        Ok(result) => {
                            if !result.messages.is_empty() {
                                self.session.messages = result.messages;
                                self.merge_compaction_summary(result.summary_prompt);
                                self.reinject_compaction_attachments(Some(target_budget))
                                    .await;
                                if self.estimated_input_tokens() <= target_budget {
                                    crate::compaction::post_compact_cleanup::post_compact_cleanup(
                                        &mut self.session,
                                    );
                                    let _ = self.tx_event.send(Event::status(
                                        format!("Emergency recovery: partial compaction ({}) succeeded", if direction == crate::compaction::partial_compact::PartialCompactDirection::From { "From" } else { "UpTo" })
                                    )).await;
                                    self.emit_compaction_completed(
                                        id.clone(),
                                        true,
                                        "Partial compaction recovery".to_string(),
                                        Some(before_count),
                                        Some(self.session.messages.len()),
                                    )
                                    .await;
                                    return true;
                                }
                            }
                            responsive_state.record_overflow();
                        }
                        Err(_) => {
                            responsive_state.record_overflow();
                        }
                    }
                }
                ResponsiveCompactAction::FullCompact => {
                    break; // Fall through to existing full compact logic below.
                }
                ResponsiveCompactAction::Fail => {
                    break; // No more recovery attempts — fall through to trim.
                }
            }
            recovery_attempt += 1;
            if responsive_state.is_exhausted() {
                break;
            }
        }

        self.session.responsive_compact_state = responsive_state;

        // Phase 2: Full LLM compaction (existing logic).
        let mut retries_used = 0u32;
        let mut summary_prompt = None;
        let mut compacted_messages = self.session.messages.clone();

        let mut forced_config = self.config.compaction.clone();
        forced_config.enabled = true;
        forced_config.token_threshold = forced_config
            .token_threshold
            .min(target_budget.saturating_sub(1))
            .max(1);
        // v0.8.11: forced compaction (capacity guardrail) bypasses the floor
        // because we're at a hard ceiling and have to free budget regardless
        // of cache cost.
        forced_config.auto_floor_tokens = 0;

        let enhancements = self.build_compaction_enhancements();
        match compact_messages_safe(
            &*client,
            &self.session.messages,
            &forced_config,
            Some(&self.session.workspace),
            None,
            None,
            enhancements.as_ref(),
        )
        .await
        {
            Ok(result) => {
                retries_used = result.retries_used;
                compacted_messages = result.messages;
                summary_prompt = result.summary_prompt;
            }
            Err(err) => {
                let _ = self
                    .tx_event
                    .send(Event::status(format!(
                        "Emergency compaction API pass failed: {err}. Falling back to local trim."
                    )))
                    .await;
            }
        }

        if !compacted_messages.is_empty() || self.session.messages.is_empty() {
            self.session.messages = compacted_messages;
        }
        self.merge_compaction_summary(summary_prompt);
        self.reinject_compaction_attachments(Some(target_budget))
            .await;

        let trimmed = self.trim_oldest_messages_to_budget(target_budget);
        if trimmed > 0 {
            self.reinject_compaction_attachments(Some(target_budget))
                .await;
        }
        self.emit_session_updated().await;
        let after_tokens = self.estimated_input_tokens();
        let after_count = self.session.messages.len();
        let recovered = after_tokens <= target_budget
            && (after_tokens < before_tokens || after_count < before_count || trimmed > 0);

        if recovered {
            let removed = before_count.saturating_sub(after_count);
            let mut details = format!(
                "Emergency compaction complete: {before_count} → {after_count} messages ({removed} removed), ~{before_tokens} → ~{after_tokens} tokens"
            );
            if retries_used > 0 {
                details.push_str(&format!(" ({retries_used} retries)"));
            }
            if trimmed > 0 {
                details.push_str(&format!(", trimmed {trimmed} oldest"));
            }
            self.emit_compaction_completed(
                id,
                true,
                details.clone(),
                Some(before_count),
                Some(after_count),
            )
            .await;
            let _ = self.tx_event.send(Event::status(details)).await;
            return true;
        }

        let message = format!(
            "Emergency context compaction failed to reduce request below model limit \
             (estimate ~{after_tokens} tokens, budget ~{target_budget})."
        );
        self.emit_compaction_failed(id, true, message.clone()).await;
        let _ = self.tx_event.send(Event::status(message)).await;
        false
    }

    fn build_tool_context(&self, mode: AppMode, auto_approve: bool) -> ToolContext {
        // Load the per-workspace trusted-paths list (#29) on every tool-context
        // build. Cheap (a small JSON file) and always reflects the latest
        // `/trust add` / `/trust remove` mutations without an explicit cache
        // refresh hook.
        let trusted = crate::workspace_trust::WorkspaceTrust::load_for(&self.session.workspace);
        let mut trusted_external_paths = trusted.paths().to_vec();
        let clipboard_images_dir = self.runtime_ui.clipboard_images_dir(&self.session.workspace);
        if !trusted_external_paths
            .iter()
            .any(|path| path == &clipboard_images_dir)
        {
            trusted_external_paths.push(clipboard_images_dir);
        }
        let mut ctx = ToolContext::with_auto_approve(
            self.session.workspace.clone(),
            self.session.trust_mode,
            self.session.notes_path.clone(),
            self.session.mcp_config_path.clone(),
            mode == AppMode::Yolo || mode == AppMode::Coordinator || auto_approve,
        )
        .with_state_namespace(self.session.id.clone())
        .with_features(self.config.features.clone())
        .with_shell_manager(self.shell_manager.clone())
        .with_runtime_services(self.host.runtime_services.clone())
        .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
            self.session.id.clone(),
            self.session.model.clone(),
            self.session.workspace.clone(),
            self.session.system_prompt.clone(),
            self.session.messages.clone(),
        ))
        .with_cancel_token(self.cancel_token.clone())
        .with_trusted_external_paths(trusted_external_paths);

        // Set effective cwd: if a worktree session is active, shift cwd
        // to the worktree path so relative paths resolve inside it.
        {
            let wt_state = self.config.worktree_state.lock().unwrap();
            if wt_state.active && wt_state.worktree_path.is_some() {
                ctx = ctx.with_cwd(wt_state.worktree_path.clone().unwrap());
            } else {
                ctx = ctx.with_cwd(self.session.cwd.clone());
            }
        }

        // Hand the user-memory path to tools so the model-callable
        // `remember` tool can append entries (#489). `None` when the
        // feature is disabled — tools short-circuit on that.
        if self.config.memory_enabled {
            ctx.memory_path = Some(self.config.memory_path.clone());
        }
        if self.config.kod_enabled {
            ctx.memory_dir = Some(self.config.memory_dir.clone());
        }

        if let Some(decider) = self.config.network_policy.as_ref() {
            ctx = ctx.with_network_policy(decider.clone());
        }

        // Wire the large-output router (#548). Only attaches when the
        // [workshop] config table is present; sub-agents don't inherit the
        // router (their ToolContext is built separately) to prevent recursive
        // routing of the synthesis call itself.
        if let Some(workshop_cfg) = self.config.workshop.as_ref()
            && let Some(vars_arc) = self.workshop_vars.as_ref()
        {
            let router =
                crate::tools::large_output_router::LargeOutputRouter::new(workshop_cfg.clone());
            ctx = ctx.with_large_output_router(router, vars_arc.clone());
        }

        // Wire the external sandbox backend (#516). exec_shell checks this
        // field and routes commands through the backend instead of spawning
        // a local process when it's set.
        if let Some(backend) = self.sandbox_backend.as_ref() {
            ctx = ctx.with_sandbox_backend(std::sync::Arc::clone(backend));
        }

        // Wire search provider config.
        ctx.search_provider = self.config.search_provider;
        ctx.search_api_key = self.config.search_api_key.clone();

        let policy = sandbox_policy_for_mode(mode, &self.session.workspace);
        let mut ctx = ctx
            .with_elevated_sandbox_policy(policy)
            .with_sandbox_runtime(self.config.sandbox_runtime.clone());
        if matches!(mode, AppMode::Plan) {
            ctx = ctx.with_shell_network_denied_hint(
                "Shell command blocked: Plan mode runs shell commands in a read-only sandbox — no writes, no network. Use Agent mode (`/mode agent`) for any command that creates or modifies files, or that needs network access.",
            );
        }
        ctx
    }

    async fn ensure_mcp_pool(&mut self) -> Result<Arc<AsyncMutex<McpPool>>, ToolError> {
        if let Some(pool) = self.mcp_pool.as_ref() {
            return Ok(Arc::clone(pool));
        }
        let mut pool = McpPool::from_config_path(&self.session.mcp_config_path)
            .map_err(|e| ToolError::execution_failed(format!("Failed to load MCP config: {e}")))?;
        if let Some(decider) = self.config.network_policy.as_ref() {
            pool = pool.with_network_policy(decider.clone());
        }
        let pool = Arc::new(AsyncMutex::new(pool));
        self.mcp_pool = Some(Arc::clone(&pool));
        Ok(pool)
    }

    async fn mcp_tools(&mut self) -> Vec<Tool> {
        let pool = match self.ensure_mcp_pool().await {
            Ok(pool) => pool,
            Err(err) => {
                let _ = self.tx_event.send(Event::status(err.to_string())).await;
                return Vec::new();
            }
        };

        let mut pool = pool.lock().await;
        let errors = pool.connect_all().await;
        for (server, err) in errors {
            let _ = self
                .tx_event
                .send(Event::status(format!(
                    "Failed to connect MCP server '{server}': {err:#}"
                )))
                .await;
        }

        pool.to_api_tools()
    }

    /// Handle a turn using the DeepSeek API.
    #[allow(clippy::too_many_lines)]
    /// Run the pre-request layered-context checkpoint (#159). Checks whether
    /// the active input estimate has crossed a soft-seam threshold and, if so,
    /// produces an `<archived_context>` block via Flash and appends it as an
    /// assistant message. Called from `handle_deepseek_turn` before each API
    /// request so the model always has the latest navigation aids.
    async fn layered_context_checkpoint(&mut self) {
        let Some(seam_mgr) = self.host.seam() else {
            return;
        };
        if !seam_mgr.enabled() {
            return;
        }

        let highest = seam_mgr.highest_level().await;
        let Some(level) = seam_mgr.seam_level_for(self.estimated_input_tokens(), highest) else {
            return;
        };

        // Determine the message range to summarize: everything before the
        // verbatim window. The verbatim window (last ~16 turns) stays
        // untouched so the model always has ground-truth recent context.
        let msg_count = self.session.messages.len();
        let verbatim_start = seam_mgr.verbatim_window_start(msg_count);
        if verbatim_start == 0 {
            return; // Not enough messages to summarize.
        }

        let msg_range_end = verbatim_start;
        let pinned = self
            .session
            .working_set
            .pinned_message_indices(&self.session.messages, &self.session.workspace);

        let _ = self
            .tx_event
            .send(Event::status(format!(
                "⏻ producing L{level} context seam ({msg_range_end} messages)…"
            )))
            .await;

        // If we have existing seams, recompact; otherwise produce fresh.
        let existing_seams = seam_mgr.collect_seam_texts(&self.session.messages).await;
        let seam_text = if existing_seams.is_empty() {
            match seam_mgr
                .produce_soft_seam(
                    &self.session.messages,
                    level,
                    0,
                    msg_range_end,
                    Some(&self.session.workspace),
                    &pinned,
                )
                .await
            {
                Ok(text) => text,
                Err(err) => {
                    crate::logging::warn(format!("L{level} soft seam failed: {err}"));
                    return;
                }
            }
        } else {
            let recent: Vec<&Message> = (0..msg_range_end)
                .filter_map(|i| self.session.messages.get(i))
                .collect();
            match seam_mgr
                .recompact(&existing_seams, &recent, level, 0, msg_range_end)
                .await
            {
                Ok(text) => text,
                Err(err) => {
                    crate::logging::warn(format!("L{level} recompact failed: {err}"));
                    return;
                }
            }
        };

        if seam_text.is_empty() {
            return;
        }

        // Capture seam count before the mutable borrow below.
        let seam_count = seam_mgr.seam_count().await;

        // Append the seam as an assistant message. This is an append-only
        // operation — no messages are deleted. The prefix cache stays hot.
        self.add_session_message(Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: seam_text,
                cache_control: None,
            }],
        })
        .await;

        let _ = self
            .tx_event
            .send(Event::status(format!(
                "⏻ L{level} seam complete ({seam_count} total, {msg_range_end} messages covered)"
            )))
            .await;
    }
    /// its token threshold (issue #124). No-op in the common case.
    ///
    /// Caller must invoke this only at a clean turn boundary (no in-flight
    /// tool, no open stream, no pending approval modal). The phase guard
    /// inside `should_advance_cycle` is a defence-in-depth check; the
    /// engine's wider state machine is the primary enforcement layer.
    ///
    /// Sub-agents are intentionally NOT awaited: each sub-agent has its own
    /// context, the parent's reset doesn't invalidate them. Their handles
    /// are captured in the structured-state block so the next cycle can see
    /// they're still running.
    async fn maybe_advance_cycle(&mut self, mode: AppMode) {
        if !should_advance_cycle(
            self.estimated_input_tokens() as u64,
            turn_response_headroom_tokens(),
            &self.session.model,
            &self.config.cycle,
            false,
        ) {
            return;
        }

        let Some(client) = self.llm_client.clone() else {
            crate::logging::warn(
                "Cycle boundary skipped: API client not configured for briefing turn",
            );
            return;
        };

        let from = self.session.cycle_count;
        let to = from.saturating_add(1);
        let archive_started = self.session.current_cycle_started;
        let max_briefing_tokens = self.config.cycle.briefing_max_for(&self.session.model);

        let _ = self
            .tx_event
            .send(Event::status(format!(
                "↻ context refreshing (cycle {from} → {to}, generating briefing…)"
            )))
            .await;

        // 1. Generate the model-curated briefing. Prefer the Flash seam
        //    manager (#159) for cost and speed; fall back to the main model
        //    (legacy produce_briefing) when the seam manager isn't available.
        let briefing_text = if let Some(seam_mgr) = self.host.seam() {
            let seams = seam_mgr.collect_seam_texts(&self.session.messages).await;
            let state_text = {
                let s = StructuredState::capture(
                    mode.label(),
                    self.config.workspace.clone(),
                    std::env::current_dir().ok(),
                    &self.session.working_set,
                    &self.config.todos,
                    &self.config.plan_state,
                    Some(&self.subagent_manager),
                )
                .await;
                s.to_system_block()
            };
            match seam_mgr
                .produce_flash_briefing(&seams, state_text.as_deref())
                .await
            {
                Ok(text) => text,
                Err(err) => {
                    crate::logging::warn(format!(
                        "Flash briefing failed, falling back to main model: {err}"
                    ));
                    match produce_briefing(
                        client.as_ref(),
                        &self.session.model,
                        &self.session.messages,
                        max_briefing_tokens,
                    )
                    .await
                    {
                        Ok(text) => text,
                        Err(err2) => {
                            crate::logging::warn(format!(
                                "Cycle briefing turn failed; skipping cycle advance: {err2}"
                            ));
                            let _ = self
                                .tx_event
                                .send(Event::status(format!(
                                    "↻ cycle handoff failed (continuing in cycle {from}): {err2}"
                                )))
                                .await;
                            return;
                        }
                    }
                }
            }
        } else {
            match produce_briefing(
                client.as_ref(),
                &self.session.model,
                &self.session.messages,
                max_briefing_tokens,
            )
            .await
            {
                Ok(text) => text,
                Err(err) => {
                    crate::logging::warn(format!(
                        "Cycle briefing turn failed; skipping cycle advance: {err}"
                    ));
                    let _ = self
                        .tx_event
                        .send(Event::status(format!(
                            "↻ cycle handoff failed (continuing in cycle {from}): {err}"
                        )))
                        .await;
                    return;
                }
            }
        };

        let briefing_tokens = estimate_briefing_tokens(&briefing_text);
        let now = chrono::Utc::now();
        let briefing = CycleBriefing {
            cycle: to,
            timestamp: now,
            briefing_text: briefing_text.clone(),
            token_estimate: briefing_tokens,
        };

        // 2. Archive the cycle to disk. If the archive write fails we still
        //    proceed with the swap — the briefing alone preserves enough
        //    state to continue, and the user can recover the lost archive
        //    from their session log if needed.
        match archive_cycle(
            &self.session.id,
            to,
            &self.session.messages,
            &self.session.model,
            archive_started,
        ) {
            Ok(path) => {
                crate::logging::info(format!("Cycle {to} archived to {}", path.display()));
            }
            Err(err) => {
                crate::logging::warn(format!(
                    "Failed to archive cycle {to}; continuing with swap: {err}"
                ));
            }
        }

        // 3. Capture structured state. Locks are held only for the snapshot.
        let state = StructuredState::capture(
            mode.label(),
            self.config.workspace.clone(),
            std::env::current_dir().ok(),
            &self.session.working_set,
            &self.config.todos,
            &self.config.plan_state,
            Some(&self.subagent_manager),
        )
        .await;
        let state_block = state.to_system_block();

        // 4. Build the seed messages. The next cycle starts with the
        //    base system prompt (refreshed below) and these seeds.
        let seed_messages = build_seed_messages(
            state_block.as_deref(),
            Some(&briefing),
            None, // pending_user_message — pulled from steer/queue elsewhere
        );

        // 5. Atomic swap.
        self.session.messages = seed_messages;
        self.session.cycle_count = to;
        self.session.current_cycle_started = now;
        self.session.cycle_briefings.push(briefing.clone());
        // Reset seam tracking for the new cycle.
        if let Some(seam_mgr) = self.host.seam() {
            seam_mgr.reset().await;
        }
        // Drop any compaction summary — that path is incompatible with the
        // fresh-context model and would Frankenstein-merge with the briefing.
        self.session.compaction_summary_prompt = None;
        self.refresh_system_prompt(mode);
        self.emit_session_updated().await;

        let _ = self
            .tx_event
            .send(Event::CycleAdvanced {
                from,
                to,
                briefing: briefing.clone(),
            })
            .await;
        let _ = self
            .tx_event
            .send(Event::status(format!(
                "↻ context refreshed (cycle {from} → {to}, briefing: {briefing_tokens} tokens carried)"
            )))
            .await;
    }

    /// Refresh the system prompt based on current mode and context.
    fn refresh_system_prompt(&mut self, mode: AppMode) {
        let (user_memory_block, knowledge_prompt_block) = if self.config.kod_enabled {
            let kod_block = crate::memory::compose_kod_block(&self.config.memory_dir);
            match kod_block {
                Some(block) => (None, Some(block)),
                None => (
                    crate::memory::compose_block(
                        self.config.memory_enabled,
                        &self.config.memory_path,
                    ),
                    None,
                ),
            }
        } else {
            (
                crate::memory::compose_block(self.config.memory_enabled, &self.config.memory_path),
                None,
            )
        };
        let prompt_goal_objective = goal_objective_for_prompt(
            self.config.goal_objective.as_deref(),
            &self.config.goal_state,
        );
        let runtime_context = prompts::PromptSessionContext {
            user_memory_block: user_memory_block.as_deref(),
            knowledge_prompt_block: knowledge_prompt_block.as_deref(),
            goal_objective: prompt_goal_objective.as_deref(),
            project_context_pack_enabled: self.config.project_context_pack_enabled,
            locale_tag: &self.config.locale_tag,
            translation_enabled: self.config.translation_enabled,
            model_id: &self.config.model,
            show_thinking: self.config.show_thinking,
        }
        .runtime();
        let base = prompts::effective_prompt_bundle_for_mode_with_runtime_context_and_approval(
            mode,
            &self.config.workspace,
            None,
            Some(&self.config.skills_dir),
            Some(&self.config.instructions),
            prompts::PromptRuntimeContext {
                override_system_prompt: self.config.override_system_prompt.as_deref(),
                custom_system_prompt: self.config.custom_system_prompt.as_deref(),
                coordinator_system_prompt: self.config.coordinator_system_prompt.as_deref(),
                agent_system_prompt: self.config.agent_system_prompt.as_deref(),
                append_system_prompts: &self.config.append_system_prompts,
                cache_breaker: self.config.cache_breaker.as_deref(),
                ..runtime_context
            },
            self.session.approval_mode,
        )
        .render_system_prompt();
        let mut stable_prompt =
            merge_system_prompts(Some(&base), self.session.compaction_summary_prompt.clone());

        // SlopLedger completion-gate: inject unresolved slop entries into the
        // system prompt so the agent can autonomously review them before
        // claiming the task is done (#2127).
        let gate_block = self.slop_ledger_gate_block();
        if let Some(ref block) = gate_block
            && let Some(SystemPrompt::Text(prompt_text)) = &mut stable_prompt
        {
            prompt_text.push_str("\n\n");
            prompt_text.push_str(block);
        }

        let stable_hash = system_prompt_hash(stable_prompt.as_ref());
        if self.session.system_prompt_override {
            self.session.last_system_prompt_hash = Some(stable_hash);
            return;
        }
        if self.session.last_system_prompt_hash != Some(stable_hash) {
            self.session.system_prompt = stable_prompt;
            self.session.last_system_prompt_hash = Some(stable_hash);
        }
    }

    fn slop_ledger_gate_block(&mut self) -> Option<String> {
        let modified = crate::slop_ledger::SlopLedger::default_path()
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok());

        if let Some((cached_modified, cached_block)) = &self.slop_ledger_gate_cache
            && *cached_modified == modified
        {
            return cached_block.clone();
        }

        let loaded = crate::slop_ledger::SlopLedger::load()
            .ok()
            .and_then(|ledger| {
                if ledger.has_open_entries() {
                    ledger.completion_gate_summary()
                } else {
                    None
                }
            });
        self.slop_ledger_gate_cache = Some((modified, loaded.clone()));
        loaded
    }

    /// Merge a compaction summary into the system prompt.
    ///
    /// **Zone affiliation (#2264)**: this mutates the system prompt, which is
    /// part of the `PinnedPrefix` zone in the three-zone contract. Compaction
    /// is the one intentional mid-session prefix mutation — the engine
    /// intentionally accepts the cache-invalidation cost because the
    /// context-reduction benefit outweighs it.
    fn merge_compaction_summary(&mut self, summary_prompt: Option<SystemPrompt>) {
        if summary_prompt.is_none() {
            return;
        }
        self.session.compaction_summary_prompt = merge_system_prompts(
            self.session.compaction_summary_prompt.as_ref(),
            summary_prompt.clone(),
        );
        let merged = merge_system_prompts(self.session.system_prompt.as_ref(), summary_prompt);
        self.session.last_system_prompt_hash = Some(system_prompt_hash(merged.as_ref()));
        self.session.system_prompt = merged;
    }

    async fn reinject_compaction_attachments(
        &mut self,
        target_input_budget: Option<usize>,
    ) -> usize {
        let plan_snapshot = { self.config.plan_state.lock().await.snapshot() };
        let plan_summary = format_plan_reinject_summary(&plan_snapshot);
        let todo_snapshot = { self.config.todos.lock().await.snapshot() };
        let todo_summary = format_todo_reinject_summary(&todo_snapshot);
        let mut candidates = Vec::new();

        if let Some(message) = crate::compaction::attachment_reinject::reinject_plan_attachment(
            plan_summary.as_deref().unwrap_or(""),
        ) {
            candidates.push(message);
        }
        if let Some(todo_summary) = todo_summary {
            candidates.push(compaction_reinject_message(format!(
                "Active todos resumed after context compaction:\n\n{todo_summary}"
            )));
        }
        let subagent_summaries = self.compaction_subagent_summaries().await;
        if let Some(message) = crate::compaction::attachment_reinject::reinject_subagent_attachments(
            &subagent_summaries,
        ) {
            candidates.push(message);
        }
        let recent_read_files = self
            .session
            .recent_read_files
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(message) =
            crate::compaction::attachment_reinject::reinject_read_file_attachments(
                &recent_read_files,
            )
        {
            candidates.push(message);
        }

        let mut injected = 0usize;
        for candidate in candidates {
            if self
                .session
                .messages
                .iter()
                .any(|message| message == &candidate)
            {
                continue;
            }
            if let Some(target_budget) = target_input_budget {
                let mut trial = self.session.messages.clone();
                trial.push(candidate.clone());
                if estimate_input_tokens_conservative(&trial, self.session.system_prompt.as_ref())
                    > target_budget
                {
                    continue;
                }
            }
            self.session.messages.push(candidate);
            injected = injected.saturating_add(1);
        }
        injected
    }

    async fn compaction_subagent_summaries(
        &self,
    ) -> Vec<crate::compaction::attachment_reinject::AgentSummary> {
        self.subagent_manager
            .read()
            .await
            .live_running_snapshots()
            .into_iter()
            .map(|snapshot| {
                let name = if snapshot.name.trim().is_empty() {
                    snapshot.agent_id.clone()
                } else {
                    snapshot.name.clone()
                };
                let role = snapshot.assignment.role.as_deref().unwrap_or("unspecified");
                let description = format!(
                    "id={}, role={}, objective={}, model={}, steps={}",
                    snapshot.agent_id,
                    role,
                    snapshot.assignment.objective,
                    snapshot.model,
                    snapshot.steps_taken
                );
                crate::compaction::attachment_reinject::AgentSummary {
                    name,
                    status: "running".to_string(),
                    description,
                }
            })
            .collect()
    }
}

fn compaction_reinject_message(content: String) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: format!("<system-reminder>\n{content}\n</system-reminder>"),
            cache_control: None,
        }],
    }
}

fn format_plan_reinject_summary(snapshot: &crate::tools::plan::PlanSnapshot) -> Option<String> {
    if snapshot.items.is_empty()
        && snapshot
            .explanation
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(explanation) = snapshot
        .explanation
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        lines.push(explanation.trim().to_string());
        lines.push(String::new());
    }
    for item in &snapshot.items {
        lines.push(format!("- {:?}: {}", item.status, item.step));
    }
    Some(lines.join("\n"))
}

fn format_todo_reinject_summary(snapshot: &crate::tools::todo::TodoListSnapshot) -> Option<String> {
    if snapshot.items.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for item in &snapshot.items {
        lines.push(format!(
            "- #{} {:?}: {}",
            item.id, item.status, item.content
        ));
    }
    Some(lines.join("\n"))
}

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

fn system_prompt_hash(prompt: Option<&SystemPrompt>) -> u64 {
    let mut hasher = DefaultHasher::new();
    match prompt {
        Some(SystemPrompt::Text(text)) => {
            0u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        Some(SystemPrompt::Blocks(blocks)) => {
            1u8.hash(&mut hasher);
            for block in blocks {
                block.block_type.hash(&mut hasher);
                block.text.hash(&mut hasher);
                if let Some(cache_control) = &block.cache_control {
                    cache_control.cache_type.hash(&mut hasher);
                }
            }
        }
        None => {
            2u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn normalized_goal_objective(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn sync_goal_state_from_host(
    goal_state: &SharedGoalState,
    objective: Option<&str>,
    token_budget: Option<u32>,
    completed: bool,
) {
    match goal_state.lock() {
        Ok(mut state) => state.sync_from_host(objective, token_budget, completed),
        Err(err) => tracing::warn!("goal state lock poisoned while syncing host goal: {err}"),
    }
}

fn goal_objective_for_prompt(
    configured_goal: Option<&str>,
    goal_state: &SharedGoalState,
) -> Option<String> {
    match goal_state.lock() {
        Ok(state) => {
            if state.objective().is_some() {
                return state.is_active().then(|| {
                    state
                        .objective()
                        .expect("checked goal objective")
                        .to_string()
                });
            }
        }
        Err(err) => tracing::warn!("goal state lock poisoned while building prompt: {err}"),
    }
    normalized_goal_objective(configured_goal)
}

/// Spawn the engine in a background task.
///
/// `host` carries the terminal-side runtime services (`RuntimeToolServices`)
/// and hook executor that `EngineConfig` no longer holds directly.
pub fn spawn_engine(
    config: EngineConfig,
    api_config: &Config,
    host: EngineHost,
) -> EngineHandle {
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

mod approval;
mod capacity_flow;
mod context;
mod handle;
pub(crate) use context::compact_tool_result_for_context;
use context::{
    COMPACTION_SUMMARY_MARKER, MAX_CONTEXT_RECOVERY_ATTEMPTS, MIN_RECENT_MESSAGES_TO_KEEP,
    context_input_budget_for_provider, effective_max_output_tokens_for_provider,
    estimate_input_tokens_conservative, extract_compaction_summary_prompt,
    is_context_length_error_message, summarize_text, turn_response_headroom_tokens,
};
mod dispatch;
mod loop_guard;
mod lsp_hooks;
mod runtime_traits;
mod streaming;
mod team_inbox;
mod tool_catalog;
mod tool_execution;
pub(crate) mod tool_setup;
mod turn_loop;

pub(crate) fn default_active_native_tool_names() -> &'static [&'static str] {
    tool_catalog::DEFAULT_ACTIVE_NATIVE_TOOLS
}

use self::approval::{ApprovalDecision, ApprovalResult, UserInputDecision};
#[cfg(test)]
use self::dispatch::should_parallelize_tool_batch;
use self::dispatch::{
    ParallelToolResult, ParallelToolResultEntry, ToolExecGuard, ToolExecOutcome,
    ToolExecutionBatch, ToolExecutionPlan, caller_allowed_for_tool, caller_type_for_tool_use,
    final_tool_input, format_tool_error, mcp_tool_approval_description, mcp_tool_is_parallel_safe,
    mcp_tool_is_read_only, parse_parallel_tool_calls, parse_tool_input,
    plan_tool_execution_batches, should_force_update_plan_first, should_stop_after_plan_tool,
};
use self::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
#[cfg(test)]
use self::lsp_hooks::edited_paths_for_tool;
#[cfg(test)]
use self::streaming::TOOL_CALL_START_MARKERS;
use self::streaming::{
    ContentBlockKind, FAKE_WRAPPER_NOTICE, MAX_STREAM_ERRORS_BEFORE_FAIL,
    MAX_TRANSPARENT_STREAM_RETRIES, STREAM_MAX_CONTENT_BYTES, STREAM_MAX_DURATION_SECS,
    ToolUseState, contains_fake_tool_wrapper, filter_tool_call_delta,
    should_transparently_retry_stream, stream_chunk_timeout_secs,
};
use self::tool_catalog::{
    CODE_EXECUTION_TOOL_NAME, JS_EXECUTION_TOOL_NAME, MULTI_TOOL_PARALLEL_NAME,
    REQUEST_USER_INPUT_NAME, active_tools_for_step, build_model_tool_catalog,
    ensure_advanced_tooling, execute_code_execution_tool, execute_tool_search,
    initial_active_tools, is_tool_search_tool, maybe_hydrate_requested_deferred_tool,
    missing_tool_error_message,
};
#[cfg(test)]
use self::tool_catalog::{
    TOOL_SEARCH_BM25_NAME, TOOL_SEARCH_REGEX_NAME, maybe_activate_requested_deferred_tool,
    preflight_requested_deferred_tool, should_default_defer_tool,
};
use self::tool_execution::emit_tool_audit;
use self::tool_setup::sandbox_policy_for_mode;
use crate::tools::js_execution::execute_js_execution_tool;

#[cfg(test)]
mod tests;
