//! Host-services trait contracts for the engine.
//!
//! The engine core (once moved to `codesmith-agent-runtime`) must not depend
//! on terminal-coupled service types — the TUI's `LspManager` (which drives
//! LSP server processes), `SharedShellManager`, `SharedSubAgentManager`,
//! `SharedBackgroundTaskRegistry`, `SeamManager`, etc. These stay in the TUI
//! (or a future app-server host) and are injected behind the traits in this
//! module.
//!
//! Today the TUI's concrete `EngineHost` struct implements [`HostServices`];
//! the engine body calls these trait methods on `self.host`. When the
//! `Engine` struct moves to `codesmith-agent-runtime`, the `host` field
//! becomes `Arc<dyn HostServices>` and the body code is unchanged.
//!
//! This mirrors the existing trait-erasure bridges (`ToolDispatcher`,
//! `RuntimeUi`, `HookHost`) and is the natural continuation of the
//! "shed heavy fields + host-inject" decision: the services are host-provided
//! rather than stored on the portable `EngineConfig`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::background_task::{
    BackgroundTaskPollResult, BackgroundTaskPollSnapshot, BackgroundTaskStatus,
    BackgroundTaskSummary,
};
use crate::lsp_config::LspConfig;
use crate::lsp_diagnostics::DiagnosticBlock;
use crate::models::Message;

/// Terminal-agnostic LSP manager surface.
///
/// The engine core queries post-edit diagnostics through this trait so it
/// need not depend on the TUI's concrete `LspManager`. The two methods mirror
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
}

/// Terminal-agnostic background-task registry surface.
///
/// The engine core drives background shell/agent/dream lifecycle through
/// this trait so it need not depend on the TUI's concrete
/// `BackgroundTaskRegistry` (which bridges `ShellManager` /
/// `SubAgentManager` / `TaskManager`). Each method acquires the registry
/// lock internally and returns plain data types, so callers never hold a
/// guard across `Event`-channel awaits.
#[async_trait::async_trait]
pub trait BgRegistryApi: Send + Sync {
    /// Register a background shell task; returns the summary used to emit
    /// `BackgroundTaskStarted`.
    async fn register_shell_task(
        &self,
        shell_id: String,
        command: String,
        cwd: PathBuf,
    ) -> BackgroundTaskSummary;
    /// Cancel a background task by id.
    async fn cancel_task(&self, id: &str) -> anyhow::Result<()>;
    /// Snapshot of all tracked tasks (for `/jobs` / `BackgroundTaskList`).
    async fn list_tasks(&self) -> Vec<BackgroundTaskSummary>;
    /// Bytes of output produced since the last read for `id`, if any.
    async fn read_output_delta(&self, id: &str) -> Option<String>;
    /// Request backgrounding for every live shell task; returns the tasks
    /// backgrounded.
    async fn background_all(&self) -> Vec<BackgroundTaskSummary>;
    /// Register a dream/memory-consolidation task; returns its summary.
    async fn register_dream_task(&self, memory_path: PathBuf) -> BackgroundTaskSummary;
    /// Force a status transition; returns a poll result if the state moved.
    async fn update_task_status(
        &self,
        id: &str,
        new_status: BackgroundTaskStatus,
        error: Option<String>,
    ) -> Option<BackgroundTaskPollResult>;
    /// Atomically poll all tasks, drain pending notifications, and evict
    /// notified terminal tasks. Returns the poll results and notifications
    /// produced during this pass so the host poller can emit them as events
    /// without holding the registry lock.
    async fn poll_once(&self) -> BackgroundTaskPollSnapshot;
}

/// Terminal-agnostic seam (layered-context) manager surface.
///
/// The engine core queries the Flash seam manager through this trait so it
/// need not depend on the TUI's concrete `SeamManager` (which drives the
/// compaction path). `enabled` replaces the `config().enabled` the engine
/// used to read, so the `SeamConfig` struct can stay TUI-local.
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

/// Host services injected into the engine.
///
/// Each accessor returns a trait-erased view of a service that the engine
/// body needs but whose concrete type lives in the host (TUI today). The
/// trait is extended incrementally as more services are decoupled from the
/// `Engine` struct (LSP first; background-task registry next; subagent
/// manager, seam manager, shell, workshop to follow).
pub trait HostServices: Send + Sync {
    /// Post-edit LSP diagnostics service.
    fn lsp(&self) -> &dyn LspManagerApi;

    /// Background-task registry. Returned as an owned, cloneable handle so
    /// the engine's background poller can capture it across a `spawn`.
    fn bg_registry(&self) -> Arc<dyn BgRegistryApi>;

    /// Layered-context (seam) manager, when configured. `None` when the
    /// feature is disabled — callers early-return, matching the previous
    /// `if let Some(seam_mgr) = self.seam_manager` guards.
    fn seam(&self) -> Option<&dyn SeamManagerApi>;
}
