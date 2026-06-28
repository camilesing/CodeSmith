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
}
