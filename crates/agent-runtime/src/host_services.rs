//! Host-services trait contracts for the engine.
//!
//! The engine core (once moved to `codesmith-agent-runtime`) must not depend
//! on terminal-coupled service types — the CLI's `LspManager` (which drives
//! LSP server processes), `SharedShellManager`, `SharedSubAgentManager`,
//! `SharedBackgroundTaskRegistry`, `SeamManager`, etc. These stay in the CLI
//! (or a future app-server host) and are injected behind the traits in this
//! module.
//!
//! Today the CLI's concrete `EngineHost` struct implements [`HostServices`];
//! the engine body calls these trait methods on `self.host`. When the
//! `Engine` struct moves to `codesmith-agent-runtime`, the `host` field
//! becomes `Arc<dyn HostServices>` and the body code is unchanged.
//!
//! This mirrors the existing trait-erasure bridges (`ToolDispatcher`,
//! `RuntimeUi`, `HookHost`) and is the natural continuation of the
//! "shed heavy fields + host-inject" decision: the services are host-provided
//! rather than stored on the portable `EngineConfig`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::background_task::{
    BackgroundTaskNotification, BackgroundTaskStatus, BackgroundTaskSummary,
};
use crate::engine_config::EngineConfig;
use crate::events::Event;
use crate::hooks::HookHost;
use crate::llm_client::LlmClientHandle;
use crate::lsp_config::LspConfig;
use crate::lsp_diagnostics::DiagnosticBlock;
use crate::mcp::McpPool;
use crate::mode::AppMode;
use crate::models::{Message, Tool};
use crate::ops::Op;
use crate::runtime_ui::RuntimeUi;
use crate::sandbox::{SandboxPolicy, SandboxRuntimeConfig};
use crate::session::Session;
use crate::subagent::{SubAgentCompletion, SubAgentResult, SubAgentType};
use crate::tool_dispatch::ToolDispatcher;
use crate::tool_state::plan::SharedPlanState;
use crate::tool_state::todo::SharedTodoList;
use crate::tools::automation_types::{
    AutomationRecord, AutomationRunRecord, CreateAutomationRequest, UpdateAutomationRequest,
};
use crate::tools::shell_types::{ShellDeltaResult, ShellJobDetail, ShellJobSnapshot, ShellResult};
use crate::tools::task_types::{NewTaskRequest, TaskRecord, TaskSummary};
use crate::working_set::WorkingSet;

/// Terminal-agnostic LSP manager surface.
///
/// The engine core queries post-edit diagnostics through this trait so it
/// need not depend on the CLI's concrete `LspManager`. The two methods mirror
/// the inherent API used by the engine's `lsp_hooks` (`config().enabled` and
/// `diagnostics_for`).
#[async_trait::async_trait]
pub trait LspManagerApi: Send + Sync {
    /// Resolved LSP config (carries the `enabled` flag and server settings).
    fn config(&self) -> &LspConfig;

    /// Fetch diagnostics for `file` after edit `edit_seq`. Returns `None` when
    /// the LSP server is unavailable or reports nothing — failure is silent
    /// by design so a crashing LSP never blocks the agent.
    async fn diagnostics_for(&self, file: &Path, edit_seq: u64) -> Option<DiagnosticBlock>;

    /// Issue one read-only navigation request (`textDocument/definition`,
    /// `textDocument/references`, `textDocument/hover`) against the LSP
    /// server for `file`'s language, syncing `file`'s document
    /// (didOpen/didChange) first (round-4 TODO-45). `params` carries the
    /// request position in LSP wire form (0-based); the implementation fills
    /// in `textDocument.uri`. Returns `None` when navigation is unavailable —
    /// LSP disabled, no server for the language, the file unreadable, or the
    /// request failed/timed out — and `Some(value)` for any server answer
    /// (`null` / an empty array are valid answers, not unavailability).
    async fn navigation_request(
        &self,
        file: &Path,
        method: &str,
        params: serde_json::Value,
    ) -> Option<serde_json::Value>;
}

/// Terminal-agnostic background-task registry surface.
///
/// The engine core drives background agent/dream lifecycle through this
/// trait so it need not depend on the CLI's concrete
/// `BackgroundTaskRegistry` (which bridges `SubAgentManager` and holds the
/// foreground-shell detach flag for the shell manager). Each method acquires
/// the registry lock internally and returns plain data types, so callers
/// never hold a guard across `Event`-channel awaits.
#[async_trait::async_trait]
pub trait BgRegistryApi: Send + Sync {
    /// Register a background sub-agent task; returns the summary used to emit
    /// `BackgroundTaskStarted`. Mirrors `BackgroundTaskRegistry::register_agent_task`.
    async fn register_agent_task(
        &self,
        agent_id: String,
        agent_type: SubAgentType,
        model: String,
        prompt: String,
    ) -> BackgroundTaskSummary;
    /// Cancel a background task by id.
    async fn cancel_task(&self, id: &str) -> anyhow::Result<()>;
    /// Snapshot of all tracked tasks.
    async fn list_tasks(&self) -> Vec<BackgroundTaskSummary>;
    /// Look up a tracked task by exact id; `None` when no task matches.
    /// Mirrors `BackgroundTaskRegistry::get_task` (returns the portable
    /// summary).
    async fn get_task(&self, id: &str) -> Option<BackgroundTaskSummary>;
    /// Ask the shell manager to detach its foreground shell(s) on their next
    /// poll (Ctrl+B). Detached jobs are not registry tasks.
    async fn background_all(&self);
    /// Register a dream/memory-consolidation task; returns its summary.
    async fn register_dream_task(&self, memory_path: PathBuf) -> BackgroundTaskSummary;
    /// Force a status transition; returns the new status if the state moved.
    async fn update_task_status(
        &self,
        id: &str,
        new_status: BackgroundTaskStatus,
        error: Option<String>,
    ) -> Option<BackgroundTaskStatus>;
    /// Atomically poll all tasks, drain pending notifications, and evict
    /// notified terminal tasks. Returns the notifications produced during
    /// this pass so the host poller can emit them as events without holding
    /// the registry lock.
    async fn poll_once(&self) -> Vec<BackgroundTaskNotification>;
}

/// Terminal-agnostic seam (layered-context) manager surface.
///
/// The engine core queries the Flash seam manager through this trait so it
/// need not depend on the CLI's concrete `SeamManager` (which drives the
/// compaction path). `enabled` replaces the `config().enabled` the engine
/// used to read, so the `SeamConfig` struct can stay CLI-local.
#[async_trait::async_trait]
pub trait SeamManagerApi: Send + Sync {
    /// Whether the layered-context manager is enabled.
    fn enabled(&self) -> bool;
    /// Pick a seam level for the current input size, or `None` if no seam
    /// applies.
    fn seam_level_for(
        &self,
        active_input_tokens: usize,
        highest_existing_level: Option<u8>,
    ) -> Option<u8>;
    /// Start index of the verbatim (never-summarized) window.
    fn verbatim_window_start(&self, message_count: usize) -> usize;
    /// Number of active seams.
    async fn seam_count(&self) -> usize;
    /// Highest seam level currently recorded, if any.
    async fn highest_level(&self) -> Option<u8>;
    /// Extract `<archived_context>` blocks from the message history.
    async fn collect_seam_texts(&self, messages: &[Message]) -> Vec<String>;
    /// Produce a soft seam (`<archived_context>` block) for the given message
    /// range and level. Returns the XML block as a string, ready to append as
    /// an assistant message; empty when there is nothing to summarize.
    async fn produce_soft_seam(
        &self,
        messages: &[Message],
        level: u8,
        start_idx: usize,
        end_idx: usize,
        workspace: Option<&Path>,
        pinned_indices: &[usize],
    ) -> anyhow::Result<String>;
    /// Re-compact existing seams into a denser, higher-level block, fusing
    /// prior `<archived_context>` content with newer messages.
    async fn recompact(
        &self,
        existing_seams: &[String],
        new_messages: &[&Message],
        level: u8,
        start_idx: usize,
        end_idx: usize,
    ) -> anyhow::Result<String>;
    /// Produce a cycle briefing (`<carry_forward>` block) from existing seams
    /// and optional structured-state text. Uses the Flash side-channel.
    async fn produce_flash_briefing(
        &self,
        existing_seams: &[String],
        structured_state: Option<&str>,
    ) -> anyhow::Result<String>;
    /// Clear seam tracking (hard cycle reset).
    async fn reset(&self);
}

/// Terminal-agnostic sub-agent manager surface.
///
/// The engine core counts/lists/evicts sub-agents through this trait so it
/// need not depend on the CLI's concrete `SubAgentManager` (which drives
/// sub-agent lifecycle and persistence). Each method acquires the inner
/// `RwLock` itself and returns plain data, so callers never hold a guard
/// across an `Event`-channel await. Spawning — which assembles the
/// terminal-coupled `SubAgentRuntime` — goes through the
/// [`HostServices::spawn_subagent`] factory instead.
#[async_trait::async_trait]
pub trait SubAgentApi: Send + Sync {
    /// Number of sub-agents currently running in this process.
    async fn running_count(&self) -> usize;
    /// Snapshot of every tracked sub-agent — the 🤖 count's source, read
    /// directly by the UI that renders it (the engine's op channel is only
    /// drained between turns).
    async fn list(&self) -> Vec<SubAgentResult>;
    /// Evict completed agents older than `max_age`.
    async fn cleanup(&self, max_age: Duration);
    /// Snapshot of sub-agents currently live-running (status `Running` with
    /// an active task handle), excluding completed/evicted agents. Used by
    /// the compaction reinject path to resume active sub-agents into the
    /// cycle briefing.
    async fn live_running_snapshots(&self) -> Vec<SubAgentResult>;
}

/// Terminal-agnostic shell-manager surface for tool implementations.
///
/// The `exec_shell` family of tools drives background/streaming shell jobs
/// through this trait so `spec.rs` (and, downstream, `codesmith-tool-impls`)
/// need not depend on the concrete `ShellManager` (pty / process /
/// `SandboxManager` plumbing). Each method is synchronous: the host locks
/// its `std::sync::Mutex` internally and returns before any await, matching
/// the pre-trait `.lock().method()` call sites.
pub trait ShellManagerApi: Send + Sync {
    /// Clear any pending foreground→background detach request before a new
    /// foreground exec starts.
    fn clear_foreground_background_request(&self);
    /// Install the session's resolved sandbox runtime config prior to an exec.
    fn set_sandbox_runtime(&self, runtime: SandboxRuntimeConfig);
    /// Execute a shell command (background or foreground) with stdin/TTY
    /// options, a sandbox-policy override, and an extra env-var map merged
    /// into the spawned process environment. Mirrors
    /// `ShellManager::execute_with_options_env`.
    #[allow(clippy::too_many_arguments)]
    fn execute_with_options_env(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<SandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> anyhow::Result<ShellResult>;
    /// Interactive variant that accepts extra env vars (#456 shell_env hook).
    /// Mirrors `ShellManager::execute_interactive_with_policy_env`.
    fn execute_interactive_with_policy_env(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        policy_override: Option<SandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> anyhow::Result<ShellResult>;
    /// Write data to a background process's stdin.
    fn write_stdin(&self, task_id: &str, input: &str, close: bool) -> anyhow::Result<()>;
    /// Kill a running background process.
    fn kill(&self, task_id: &str) -> anyhow::Result<ShellResult>;
    /// Kill every currently running background shell process.
    fn kill_running(&self) -> anyhow::Result<Vec<ShellResult>>;
    /// Consume and return any pending foreground→background detach request.
    fn take_foreground_background_request(&self) -> bool;
    /// Get (optionally blocking) output from a background process.
    fn get_output(
        &self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> anyhow::Result<ShellResult>;
    /// Get incremental output from a background process, consuming any new
    /// output. Mirrors `ShellManager::get_output_delta`.
    fn get_output_delta(
        &self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult>;
    /// Attach durable task context to a live shell job.
    fn tag_linked_task(&self, task_id: &str, linked_task_id: Option<String>) -> anyhow::Result<()>;
    /// List all live and known-stale background shell jobs (for the host's
    /// command center). Mirrors `ShellManager::list_jobs`.
    fn list_jobs(&self) -> Vec<ShellJobSnapshot>;
    /// Inspect full output for a live or stale job. Mirrors
    /// `ShellManager::inspect_job`.
    fn inspect_job(&self, task_id: &str) -> anyhow::Result<ShellJobDetail>;
    /// Poll a background process and return incremental output. Mirrors
    /// `ShellManager::poll_delta` (a thin alias for `get_output_delta`).
    fn poll_delta(
        &self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> anyhow::Result<ShellDeltaResult>;
    /// Request that the currently-running foreground shell detach to the
    /// background at the next poll. Mirrors
    /// `ShellManager::request_foreground_background`.
    fn request_foreground_background(&self);
}

/// Terminal-agnostic durable task-manager surface.
///
/// The `task_*` family of tools drives durable background tasks through this
/// trait so `spec.rs` (and, downstream, `codesmith-tool-impls`) need not
/// depend on the CLI's concrete `TaskManager`. `SharedTaskManager` is
/// `Arc<TaskManager>` with interior `tokio::Mutex` state, so the trait is
/// implemented directly for `TaskManager` (no bridge, no extra locking): async
/// methods forward to the inherent async methods (which lock internally), and
/// the lock-free sync helpers (`artifact_absolute_path`, `write_task_artifact`)
/// forward verbatim.
#[async_trait::async_trait]
pub trait TaskManagerHost: Send + Sync {
    /// Enqueue a new durable task; returns the persisted record.
    async fn add_task(&self, req: NewTaskRequest) -> anyhow::Result<TaskRecord>;
    /// Recent durable tasks, newest first.
    async fn list_tasks(&self, limit: Option<usize>) -> Vec<TaskSummary>;
    /// Fetch a task by id or unambiguous prefix.
    async fn get_task(&self, id_or_prefix: &str) -> anyhow::Result<TaskRecord>;
    /// Cancel a queued/running task by id or prefix.
    async fn cancel_task(&self, id_or_prefix: &str) -> anyhow::Result<TaskRecord>;
    /// Apply model-visible tool metadata to a task and persist it.
    async fn record_tool_metadata(
        &self,
        id_or_prefix: &str,
        metadata: &serde_json::Value,
    ) -> anyhow::Result<TaskRecord>;
    /// Resolve a task artifact reference to an absolute path, confined to
    /// the task data root: references come from model-supplied task
    /// metadata, so absolute inputs and `..`-escapes are rejected (mirrors
    /// `artifacts::session_artifact_absolute_path`).
    fn artifact_absolute_path(&self, path: &Path) -> anyhow::Result<PathBuf>;
    /// Write a durable task artifact and return the persisted path reference.
    fn write_task_artifact(
        &self,
        task_id: &str,
        label: &str,
        content: &str,
    ) -> anyhow::Result<PathBuf>;
}

/// Terminal-agnostic automation-manager surface.
///
/// The `automation_*` family of tools drives durable scheduled jobs through
/// this trait so `spec.rs` (and, downstream, `codesmith-tool-impls`) need not
/// depend on the CLI's concrete `AutomationManager`. `SharedAutomationManager`
/// is `Arc<Mutex<AutomationManager>>` (tokio `Mutex`), so a small bridge
/// struct wraps the concrete handle and locks internally per call — matching
/// the pre-trait `.lock().await.method()` call sites. `run_now` takes a
/// trait-erased [`TaskManagerHost`] (portable) so the cross-manager enqueue
/// path stays within the runtime crate.
#[async_trait::async_trait]
pub trait AutomationManagerHost: Send + Sync {
    async fn create_automation(
        &self,
        req: CreateAutomationRequest,
    ) -> anyhow::Result<AutomationRecord>;
    async fn list_automations(&self) -> anyhow::Result<Vec<AutomationRecord>>;
    async fn get_automation(&self, id: &str) -> anyhow::Result<AutomationRecord>;
    async fn list_runs(
        &self,
        id: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<AutomationRunRecord>>;
    async fn update_automation(
        &self,
        id: &str,
        req: UpdateAutomationRequest,
    ) -> anyhow::Result<AutomationRecord>;
    async fn pause_automation(&self, id: &str) -> anyhow::Result<AutomationRecord>;
    async fn resume_automation(&self, id: &str) -> anyhow::Result<AutomationRecord>;
    async fn delete_automation(&self, id: &str) -> anyhow::Result<AutomationRecord>;
    /// Run an automation now, enqueuing a durable task via `task_manager`.
    async fn run_now(
        &self,
        automation_id: &str,
        task_manager: &Arc<dyn TaskManagerHost>,
    ) -> anyhow::Result<AutomationRunRecord>;
}

/// Terminal-agnostic desktop-notification surface.
///
/// The `notify` tool fires a single desktop notification through this trait
/// so `spec.rs` (and, downstream, `codesmith-tool-impls`) need not depend on
/// the host's terminal-coupled notification module (OSC 9 / BEL / macOS
/// Notification Center / tmux DCS passthrough). The host resolves the
/// delivery protocol from the terminal environment and handles tmux detection
/// internally; the tool just hands it a message string. Fire-and-forget and
/// synchronous: the host writes a short escape sequence to stdout (or spawns
/// an `osascript` on a background thread on macOS) and returns before any
/// await. Silent no-op when the host has notifications disabled.
pub trait NotifierHost: Send + Sync {
    /// Fire a desktop notification with `msg`. Best-effort and silent on
    /// failure; the host picks the terminal protocol and wraps for tmux.
    fn notify_done(&self, msg: &str);
}

/// Host services injected into the engine.
///
/// Each accessor returns a trait-erased view of a service that the engine
/// body needs but whose concrete type lives in the host (CLI today). The
/// trait is extended incrementally as more services are decoupled from the
/// `Engine` struct (LSP first; background-task registry next; subagent
/// manager, seam manager, shell, workshop to follow).
#[async_trait::async_trait]
pub trait HostServices: Send + Sync {
    /// Post-edit LSP diagnostics service. Returned as an owned, cloneable
    /// handle so the executor's [`LspProbe`] can capture it for the duration
    /// of a run (matching `bg_registry` / `subagents` / `shell`); a borrow
    /// cannot escape the `&self Engine` accessor into the executor's owned
    /// probe field.
    fn lsp(&self) -> Arc<dyn LspManagerApi>;

    /// Background-task registry. Returned as an owned, cloneable handle so
    /// the engine's background poller can capture it across a `spawn`.
    fn bg_registry(&self) -> Arc<dyn BgRegistryApi>;

    /// Layered-context (seam) manager, when configured. `None` when the
    /// feature is disabled — callers early-return, matching the previous
    /// `if let Some(seam_mgr) = self.seam_manager` guards.
    fn seam(&self) -> Option<&dyn SeamManagerApi>;

    /// Sub-agent manager. Returned as an owned, cloneable handle so the
    /// engine's turn loop can count running sub-agents without naming the
    /// concrete `SubAgentManager`.
    fn subagents(&self) -> Arc<dyn SubAgentApi>;

    /// Durable on-disk data directory for tasks that persist state between
    /// turns (e.g. dream/memory-consolidation task output). `None` when no
    /// task data dir is configured — callers fall back to the workspace.
    fn task_data_dir(&self) -> Option<PathBuf>;

    /// Hook execution surface, when configured. `None` skips `PreCompact`
    /// (and future compaction-related) hooks. Returned as a trait-erased
    /// handle so the engine body stays free of the concrete `HookExecutor`.
    fn hooks(&self) -> Option<Arc<dyn HookHost>>;

    /// Assemble the per-turn tool dispatcher and model-visible tool catalog.
    ///
    /// This is the host-side factory that combines portable engine state
    /// (carried in [`TurnDispatchRequest`]) with the host's own
    /// terminal-coupled managers (`ShellManager`, `SubAgentManager`,
    /// `SandboxBackend`, …) to build the `ToolContext` /
    /// `ToolRegistryBuilder` / `SubAgentRuntime` that stay host-side, then
    /// returns the trait-erased registry (`Arc<dyn ToolDispatcher>`) and the
    /// catalog the streaming turn loop consumes. Keeping the assembly host-side
    /// is what lets the `Engine` body move to `codesmith-agent-runtime`
    /// without dragging those concrete types across the crate boundary.
    async fn build_turn_dispatcher(&self, req: TurnDispatchRequest<'_>) -> TurnDispatchPlan;

    /// Spawn a background sub-agent of type `General` from `prompt`.
    ///
    /// Assembles the terminal-coupled `SubAgentRuntime` (which the engine
    /// body cannot name) host-side, resolves the assignment route, and
    /// registers the agent with the host's `SubAgentManager`. Returns the new
    /// agent id on success.
    async fn spawn_subagent(
        &self,
        req: SpawnSubAgentRequest<'_>,
    ) -> anyhow::Result<SubAgentSpawnResult>;

    /// Capture deterministic cross-cycle state (todos / plan / working-set /
    /// sub-agents) and render it as a system block.
    ///
    /// `StructuredState` itself is terminal-coupled (rendered host-side using
    /// the host's own `SubAgentManager`); the engine body only needs the
    /// rendered `Option<String>` to feed the cycle briefing / seed messages.
    async fn capture_structured_state(&self, req: StructuredStateRequest<'_>) -> Option<String>;

    /// Extract the file paths an `apply_patch` tool input intends to touch.
    ///
    /// Routed through the host so the engine core stays decoupled from the
    /// concrete `apply_patch` tool implementation (which lives in
    /// `codesmith-tool-impls`). Used by the post-edit LSP diagnostics hook to
    /// know which files to pull diagnostics for. Best-effort: returns an empty
    /// vec when the input cannot be parsed — a missing LSP server must never
    /// block the agent.
    fn preflight_apply_patch_paths(&self, input: &serde_json::Value) -> Vec<PathBuf>;

    /// Spawn the AgentTeams leader inbox poller for `team_name`.
    ///
    /// The poller watches the leader's team inbox and fans dispatches back
    /// into the engine op channel (`tx_op`). It is terminal-coupled (depends
    /// on the host's team protocol handlers / teammate mailbox infrastructure
    /// which live in `codesmith-cli`), so the engine body spawns it through
    /// the host rather than naming the concrete `run_leader_inbox_poller`.
    /// The host owns the supervised spawn; `cancel_token` is a child of the
    /// engine's turn token so engine shutdown cancels the poller.
    ///
    /// A spawn failure must not be silent: without this poller an AgentTeams
    /// leader stops receiving dispatches entirely, so hosts surface spawn
    /// failures in the returned `Result` and the engine logs them.
    fn spawn_leader_inbox_poller(
        &self,
        team_name: String,
        tx_op: mpsc::Sender<Op>,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<()>;

    /// Downcast the host to its concrete type. The CLI's `EngineConstruct`
    /// uses this to recover the concrete `EngineHost` (and its
    /// `subagent_manager` / `mod_reload` / `lsp_manager` …) from the
    /// `Arc<dyn HostServices>` the engine holds, so the host MUST be the
    /// concrete `EngineHost` or the downcast `expect` fires. Any capability
    /// needed by non-CLI hosts should be added as a real trait method
    /// instead of going through this hatch.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Inputs the engine body supplies to [`HostServices::build_turn_dispatcher`].
///
/// Every field is a portable (runtime-crate) type so the request can cross the
/// `Arc<dyn HostServices>` boundary once the `Engine` moves into
/// `codesmith-agent-runtime`. The host combines these with its own
/// terminal-coupled managers to build the `ToolContext` /
/// `ToolRegistryBuilder` / `SubAgentRuntime` that stay host-side.
pub struct TurnDispatchRequest<'a> {
    /// Active application mode (drives toolset + sandbox policy).
    pub mode: AppMode,
    /// Whether tool calls auto-approve this turn.
    pub auto_approve: bool,
    /// Live session (workspace, messages, model, working set, …).
    pub session: &'a Session,
    /// Resolved engine config (features, todos/plan state, sandbox, …).
    pub config: &'a EngineConfig,
    /// Cloned LLM client handle (used by review/rlm/fim tools + subagent runtime).
    pub llm_client: Option<LlmClientHandle>,
    /// Per-turn cancellation token (wired into `ToolContext` + mailbox).
    pub cancel_token: CancellationToken,
    /// Engine event channel (subagent mailbox drainer + runtime events).
    pub tx_event: mpsc::Sender<Event>,
    /// Channel fan-out for direct child sub-agent completion (#756).
    pub tx_subagent_completion: mpsc::UnboundedSender<SubAgentCompletion>,
    /// Resolved MCP pool (already ensured by the engine body), when enabled.
    pub mcp_pool: Option<Arc<McpPool>>,
    /// MCP tool definitions (already connected by the engine body).
    pub mcp_tools: Vec<Tool>,
    /// Terminal-agnostic UI bridge (clipboard / notifications).
    pub runtime_ui: &'a Arc<dyn RuntimeUi>,
}

/// Output of [`HostServices::build_turn_dispatcher`].
///
/// Carries the trait-erased tool registry and the model-visible catalog built
/// for this turn; the engine body feeds both into the streaming turn loop and
/// the `TurnComplete` event.
///
/// `framework_tool_set` is the framework-core [`ToolSet`] (§E) derived from the
/// concrete `ToolRegistry` *before* it is erased into `Arc<dyn ToolDispatcher>`
/// — the trait object cannot be recovered, so the `ToolSet` must be built while
/// the concrete type is still in hand. Unused until `HostAgentExecutor` is
/// wired into `handle_send_message`; populated here so the wire-in has the
/// `Arc<ToolSet>` it needs for
/// [`HostAgentExecutor::new`](crate::engine::HostAgentExecutor::new).
///
/// The registry / catalog / framework set are populated together or not at
/// all — modeled as an enum so an inconsistent "some fields `Some`, some
/// `None`" plan is unrepresentable (the three independent `Option` fields
/// this replaces enforced that invariant only in doc comments).
pub enum TurnDispatchPlan {
    /// No tools available for this mode.
    NoTools,
    /// Trait-erased registry, model-visible catalog (built-ins + MCP, with
    /// deferral applied), and the framework-core `ToolSet` (§E) for
    /// [`HostAgentExecutor`], always populated together.
    Tools {
        tool_registry: Arc<dyn ToolDispatcher>,
        tools: Vec<Tool>,
        framework_tool_set: Arc<codesmith_agent::tools::ToolSet>,
    },
}

impl TurnDispatchPlan {
    /// Trait-erased registry, when tools are available for this mode.
    pub fn tool_registry(&self) -> Option<&Arc<dyn ToolDispatcher>> {
        match self {
            Self::NoTools => None,
            Self::Tools { tool_registry, .. } => Some(tool_registry),
        }
    }

    /// Model-visible tool catalog; empty slice when no tools are available.
    pub fn tools(&self) -> &[Tool] {
        match self {
            Self::NoTools => &[],
            Self::Tools { tools, .. } => tools,
        }
    }

    /// Framework-core `ToolSet` (§E), when tools are available for this mode.
    pub fn framework_tool_set(&self) -> Option<&Arc<codesmith_agent::tools::ToolSet>> {
        match self {
            Self::NoTools => None,
            Self::Tools {
                framework_tool_set, ..
            } => Some(framework_tool_set),
        }
    }
}

/// Inputs the engine body supplies to [`HostServices::spawn_subagent`].
///
/// Every field is a portable (runtime-crate) type so the request crosses the
/// `Arc<dyn HostServices>` boundary once the `Engine` moves into
/// `codesmith-agent-runtime`. The host combines these with its own
/// terminal-coupled `SubAgentRuntime` assembly (which the engine body cannot
/// name) to spawn the agent. The body resolves the MCP pool itself (via
/// `ensure_mcp_pool`) and checks the LLM client for `None` before calling.
pub struct SpawnSubAgentRequest<'a> {
    /// Prompt for the spawned sub-agent.
    pub prompt: &'a str,
    /// Cloned LLM client handle (body guarantees `Some`).
    pub llm_client: LlmClientHandle,
    /// Live session (model, working set, allow_shell, auto_model, …).
    pub session: &'a Session,
    /// Resolved engine config (features, model overrides, timeouts, …).
    pub config: &'a EngineConfig,
    /// Per-turn cancellation token (wired into the `ToolContext`).
    pub cancel_token: CancellationToken,
    /// Engine event channel.
    pub tx_event: mpsc::Sender<Event>,
    /// Channel fan-out for direct child sub-agent completion (#756).
    pub tx_subagent_completion: mpsc::UnboundedSender<SubAgentCompletion>,
    /// Resolved MCP pool (already ensured by the engine body), when enabled.
    pub mcp_pool: Option<Arc<McpPool>>,
    /// Terminal-agnostic UI bridge (clipboard / notifications).
    pub runtime_ui: &'a Arc<dyn RuntimeUi>,
}

/// Output of [`HostServices::spawn_subagent`].
pub struct SubAgentSpawnResult {
    /// Id of the newly-spawned sub-agent.
    pub agent_id: String,
}

/// Inputs the engine body supplies to
/// [`HostServices::capture_structured_state`].
///
/// Every field is a portable (runtime-crate) type. The host renders the
/// snapshot using its own `SubAgentManager` (the one CLI-local input) and
/// returns the rendered system block.
pub struct StructuredStateRequest<'a> {
    /// Active mode label (e.g. `"agent"`).
    pub mode_label: &'a str,
    /// Session workspace root.
    pub workspace: PathBuf,
    /// Effective cwd, when known.
    pub cwd: Option<PathBuf>,
    /// Live working set.
    pub working_set: &'a WorkingSet,
    /// Shared todo list.
    pub todos: &'a SharedTodoList,
    /// Shared plan state.
    pub plan_state: &'a SharedPlanState,
}
