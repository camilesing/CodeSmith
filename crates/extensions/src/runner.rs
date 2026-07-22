//! `ExtensionRunner` — host runtime for extensions.
//!
//! Owns: the generation counter (`Arc<AtomicU64>`, spec §7.3 stale-context
//! guard), the loaded `Extension` set, the `pending_*` registration queues
//! (filled by the stub `ExtensionApi` during `configure`, flushed by
//! `bind_core`), the bound handler list, and the bound `ExtensionContext`
//! handed to handlers/commands at dispatch time.
//!
//! Slice 1: handlers are observers; `emit` fans out best-effort (per §8.3 —
//! slice 1 awaits each handler directly; §F2 hardens with proper
//! `catch_unwind` so one panicking handler cannot tear down the agent loop).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codesmith_agent::extension::*;

use crate::api::StubExtensionApi;

/// A tool queued by the stub `ExtensionApi` during `configure`, awaiting
/// `bind_core` flush into the host `ToolRegistry`.
pub(crate) struct PendingTool {
    pub tool: Box<dyn ToolDefinition>,
}

/// A command queued by the stub `ExtensionApi`.
pub(crate) struct PendingCommand {
    pub command: Box<dyn CommandDefinition>,
}

/// A handler subscribed during `configure`.
pub(crate) struct PendingHandler {
    pub handler: Arc<dyn Handler>,
}

/// Container for the pre-`bind_core` registration queues. Shared (via
/// `Arc<Mutex<Pending>>`) between the runner + the stub `ExtensionApi` so
/// the stub can push + the runner can drain.
#[derive(Default)]
pub(crate) struct Pending {
    pub tools: Vec<PendingTool>,
    pub commands: Vec<PendingCommand>,
    pub handlers: Vec<PendingHandler>,
}

/// The host runtime. Constructed by [`ExtensionRunner::new`] +
/// [`ExtensionRunner::load`](Self::load) (runs each extension's `configure`
/// against a **stub** api), then [`ExtensionRunner::bind_core`](Self::bind_core)
/// swaps the stub for the **real** api + flushes `pending_*` into the live
/// registries.
pub struct ExtensionRunner {
    generation: Arc<AtomicU64>,
    pending: Arc<Mutex<Pending>>,
    /// Bound at `bind_core` — the live context handed to handlers/commands.
    /// Held as `Arc<dyn ExtensionCommandContext>` (the sub-trait) so command
    /// handlers get the exact type they expect; event handlers receive
    /// `&dyn ExtensionContext` via stable trait-upcasting (Rust 1.86+).
    context: Mutex<Option<Arc<dyn ExtensionCommandContext>>>,
    /// Bound at `bind_core` — the flushed tools (name → def), for the host's
    /// `ExtensionToolSpecAdapter` to wrap.
    tools: Mutex<HashMap<String, Arc<dyn ToolDefinition>>>,
    commands: Mutex<HashMap<String, Arc<dyn CommandDefinition>>>,
    handlers: Mutex<Vec<Arc<dyn Handler>>>,
}

impl ExtensionRunner {
    /// Create an empty runner at generation 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            pending: Arc::new(Mutex::new(Pending::default())),
            context: Mutex::new(None),
            tools: Mutex::new(HashMap::new()),
            commands: Mutex::new(HashMap::new()),
            handlers: Mutex::new(Vec::new()),
        }
    }

    /// Current generation (for stale-context checks by captured `api`/`ctx`).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Expose the generation `Arc<AtomicU64>` so the host can construct a
    /// `HostExtensionContext` sharing the same counter (stale-context
    /// consistency between runner + context).
    #[must_use]
    pub fn generation_arc(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.generation)
    }

    /// Invalidate the runtime (spec §7.3): bumps generation so any
    /// previously-captured `ExtensionApi`/`ExtensionContext` reads stale.
    /// Called by `reload` / session-replace / fork / switch.
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Load + configure one extension against a **stub** api. Registrations
    /// queue into `pending_*`. Called by `build_extension_runtime` (Task 9)
    /// for each discovered extension, BEFORE `bind_core`.
    pub async fn load(&self, ext: &dyn Extension) -> Result<(), ExtensionError> {
        let stub = StubExtensionApi::new(self.generation.clone(), self.pending.clone());
        ext.configure(&stub).await
    }

    /// Bind the host context + flush `pending_*` into the live registries.
    /// After this, [`emit`](Self::emit) / [`try_dispatch_command`](Self::try_dispatch_command)
    /// are live. The stub→real swap (spec §4) happens here: any later
    /// `register_*` via a captured stub `Arc<dyn ExtensionApi>` would read
    /// a stale generation + return `StaleContext` (slice 1: stubs are
    /// short-lived — dropped after `load` — so this is a §F2 concern; the
    /// generation guard is the stable contract).
    pub fn bind_core(&self, context: Arc<dyn ExtensionCommandContext>) {
        *self.context.lock().unwrap() = Some(context);
        let mut pending = self.pending.lock().unwrap();
        let mut tools = self.tools.lock().unwrap();
        let mut commands = self.commands.lock().unwrap();
        let mut handlers = self.handlers.lock().unwrap();
        for pt in pending.tools.drain(..) {
            let name = pt.tool.name().to_string();
            let arc: Arc<dyn ToolDefinition> = Arc::from(pt.tool);
            tools.insert(name, arc);
        }
        for pc in pending.commands.drain(..) {
            let name = pc.command.name().to_string();
            let arc: Arc<dyn CommandDefinition> = Arc::from(pc.command);
            commands.insert(name, arc);
        }
        for ph in pending.handlers.drain(..) {
            handlers.push(ph.handler);
        }
    }

    /// Emit an event to every bound handler, best-effort. A handler error
    /// is discarded (§8.3 — one failing handler does not block others).
    /// No-op if `bind_core` has not run. Slice 1 awaits each handler
    /// directly; §F2 hardens with `catch_unwind`.
    pub async fn emit(&self, event: &ExtensionEvent) {
        let ctx = match self.context.lock().unwrap().clone() {
            Some(ctx) => ctx,
            None => return,
        };
        let handlers = self.handlers.lock().unwrap().clone();
        for h in handlers {
            // Slice 1: discard errors (best-effort); §F2 adds catch_unwind.
            let _ = h.handle(event, &*ctx).await;
        }
    }

    /// Look up a registered command by name (exact match; `:N` conflict
    /// suffixing is §F2 — slice 1 uses first-wins via HashMap insert). Used
    /// by the tui `extension_commands::try_dispatch` (Task 8).
    pub async fn try_dispatch_command(
        &self,
        name: &str,
        args: &str,
    ) -> Option<CommandOutput> {
        let cmd = self.commands.lock().unwrap().get(name).cloned()?;
        let ctx = self.context.lock().unwrap().clone()?;
        cmd.run(&*ctx, args).await.ok()
    }

    /// Snapshot of bound tools (name → def) for the host's
    /// `ExtensionToolSpecAdapter` to wrap + register into `ToolRegistry`
    /// (Task 5/9).
    pub fn bound_tools(&self) -> Vec<(String, Arc<dyn ToolDefinition>)> {
        self.tools
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Names of bound commands (for `/extension list`, Task 8).
    pub fn bound_command_names(&self) -> Vec<String> {
        self.commands.lock().unwrap().keys().cloned().collect()
    }
}

impl Default for ExtensionRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tokio_util::sync::CancellationToken;

    struct Ctx {
        generation: u64,
    }
    #[async_trait::async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &Path { Path::new(".") }
        fn mode(&self) -> ExtensionMode { ExtensionMode::Tui }
        fn is_idle(&self) -> bool { true }
        fn signal(&self) -> CancellationToken { CancellationToken::new() }
        fn generation(&self) -> u64 { self.generation }
    }

    // `bind_core` holds `Arc<dyn ExtensionCommandContext>`; the test Ctx must
    // impl the sub-trait (a marker in slice 1) for the coercion to fire.
    impl ExtensionCommandContext for Ctx {}

    struct RecExt {
        seen: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait::async_trait]
    impl Extension for RecExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("rec");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.on(Arc::new(RecHandler { seen: self.seen.clone() }))?;
            Ok(())
        }
    }

    struct RecHandler {
        seen: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait::async_trait]
    impl Handler for RecHandler {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<(), ExtensionError> {
            self.seen.lock().unwrap().push(match event {
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::SessionShutdown => "SessionShutdown",
                _ => "other",
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn stale_context_guard_invalidate_bumps_generation() {
        let runner = ExtensionRunner::new();
        assert_eq!(runner.generation(), 0);
        runner.invalidate();
        assert_eq!(runner.generation(), 1);
        runner.invalidate();
        assert_eq!(runner.generation(), 2);
    }

    #[tokio::test]
    async fn emit_fans_out_to_bound_handler() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner.load(&RecExt { seen: seen.clone() }).await.unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        runner
            .emit(&ExtensionEvent::TurnStart { turn_id: "t1".into() })
            .await;
        runner.emit(&ExtensionEvent::SessionShutdown).await;
        let s = seen.lock().unwrap();
        assert_eq!(*s, vec!["TurnStart", "SessionShutdown"]);
    }

    #[tokio::test]
    async fn emit_before_bind_core_is_noop() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner.load(&RecExt { seen: seen.clone() }).await.unwrap();
        // No bind_core — emit must not panic + must not dispatch.
        runner.emit(&ExtensionEvent::SessionShutdown).await;
        assert!(seen.lock().unwrap().is_empty());
    }
}
