//! `ExtensionRunner` — host runtime for extensions.
//!
//! Owns: the generation counter (`Arc<AtomicU64>`, spec §7.3 stale-context
//! guard), the loaded `Extension` set, the `pending_*` registration queues
//! (filled by the stub `ExtensionApi` during `configure`, flushed by
//! `bind_core`), the bound handler list, and the bound `ExtensionContext`
//! handed to handlers/commands at dispatch time.
//!
//! Slice 1: handlers are observers; §F2a upgrades `emit` to chain
//! `HandlerOutcome`s (transform visible to the next handler; `Cancel`/`Block`
//! short-circuit), filter per-variant via `kind_filter`, and isolate each
//! handler call behind `catch_unwind` (§8.3 — one panicking handler cannot
//! tear down the agent loop).

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codesmith_agent::extension::*;
use codesmith_agent::provider::{ProviderFactory, ProviderRegistration, SharedProviderRegistry};
use futures_util::FutureExt;
use libloading::Library;

use crate::api::StubExtensionApi;

/// A tool queued by the stub `ExtensionApi` during `configure`, awaiting
/// `bind_core` flush into the host `ToolRegistry`.
pub(crate) struct PendingTool {
    pub tool: Box<dyn ToolDefinition>,
}

/// Route B — prompt-section limit (fail loud beyond, at registration).
pub(crate) const MAX_PROMPT_SECTIONS: usize = 16;

/// Route B — shared validation (stub-time and live registration paths).
pub(crate) fn validate_prompt_section(id: &str, text: &str) -> Result<(), ExtensionError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ExtensionError::Config(format!(
            "prompt section id {id:?} must match [a-zA-Z0-9_-]"
        )));
    }
    if text.trim().is_empty() {
        return Err(ExtensionError::Config(format!(
            "prompt section {id:?} text must be non-empty"
        )));
    }
    Ok(())
}

/// A provider factory queued by the stub `ExtensionApi` during `configure`,
/// awaiting `bind_core` flush into the runner's [`SharedProviderRegistry`]
/// (route A).
pub(crate) struct PendingProvider {
    pub factory: Arc<dyn ProviderFactory>,
}

/// A command queued by the stub `ExtensionApi`.
pub(crate) struct PendingCommand {
    pub command: Box<dyn CommandDefinition>,
}

/// A handler subscribed during `configure`, with its variant filter
/// (`None` = subscribe-to-all via `on`; `Some(kind)` = per-variant via
/// `on_variant`). §F2a.
pub(crate) struct PendingHandler {
    pub handler: Arc<dyn Handler>,
    pub kind_filter: Option<ExtensionEventKind>,
}

/// A bound handler + its variant filter (None = subscribe-to-all). §F2a T7
/// makes `ExtensionRunner::handlers` a `Vec<RegisteredHandler>`; `bind_core`
/// drains `PendingHandler` into this (carrying `kind_filter` through).
#[derive(Clone)]
pub(crate) struct RegisteredHandler {
    pub handler: Arc<dyn Handler>,
    pub kind_filter: Option<ExtensionEventKind>,
}

/// The result of [`ExtensionRunner::emit`]: the final (possibly transformed)
/// event + the terminal chain outcome. The host inspects `outcome` at each
/// seam (proceed / cancel / block) and, at transform-capable seams, applies
/// `event`'s actionable field (§F2b wires the host to honor these; §F2a
/// returns them + proves the chain in isolation).
///
/// `#[must_use]` (§F2b) so the host can't silently drop an outcome at a
/// transform/block seam — observe-only sites bind `let _ =`, actionable seams
/// bind `let out =` and inspect `out.outcome` / `out.event`.
#[derive(Debug, Clone)]
#[must_use]
pub struct EmitOutcome {
    /// The event after all handlers (possibly transformed by `Transform`).
    pub event: ExtensionEvent,
    /// Terminal outcome: `Continue` if no handler short-circuited; `Cancel`
    /// or `Block` if one did. Never `Transform` (folds into `event`).
    pub outcome: HandlerOutcome,
}

/// Container for the pre-`bind_core` registration queues. Shared (via
/// `Arc<Mutex<Pending>>`) between the runner + the stub `ExtensionApi` so
/// the stub can push + the runner can drain.
#[derive(Default)]
pub(crate) struct Pending {
    pub tools: Vec<PendingTool>,
    pub commands: Vec<PendingCommand>,
    pub handlers: Vec<PendingHandler>,
    pub providers: Vec<PendingProvider>,
    pub prompt_sections: Vec<(String, String)>,
    pub message_projections: Vec<PendingMessageProjection>,
}

/// A message projection queued by the stub `ExtensionApi`.
pub(crate) struct PendingMessageProjection {
    pub owner: String,
    pub key: String,
    pub init: serde_json::Value,
    pub fold: codesmith_agent::extension::MessageFoldFn,
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
    /// Bound at `bind_core` — the flushed handlers, each carrying its variant
    /// filter (`None` = subscribe-to-all via `on`; `Some(kind)` = per-variant
    /// via `on_variant`). T8's `emit` filters on this before dispatch.
    handlers: Mutex<Vec<RegisteredHandler>>,
    /// §F5b — loaded dylib `Library` handles. Pushed by `load_dylib`. On
    /// reload, [`drain_libraries_to_pending`](Self::drain_libraries_to_pending)
    /// (§F5d T4) MOVES these to `pending_drop` (the `Library` stays alive —
    /// its code/vtables must outlive any registered contributions still in
    /// `tools`/`commands`/`handlers`: `clear_tools`/`clear_commands` drop the
    /// name-keyed bindings but a removed dylib's tool `Arc` may still
    /// reference its vtable until the per-turn `ToolRegistry` rebuilds);
    /// [`drop_pending`](Self::drop_pending) at the engine op-loop turn
    /// boundary then unloads them safely. Pre-§F5d this was a bounded leak
    /// (§F5b Q1); §F5d makes it a safe two-phase drop.
    libraries: Mutex<Vec<Library>>,
    /// §F5d T4 — staging area for `Library`s orphaned by a UI-thread
    /// `reload_extension_runtime` clear. Populated by
    /// [`drain_libraries_to_pending`](Self::drain_libraries_to_pending)
    /// (a safe `Arc`-free MOVE under one lock — the `Library` stays alive)
    /// and drained+dropped by [`drop_pending`](Self::drop_pending) at the
    /// engine op-loop top (the one moment the main-thread
    /// `HostAgentExecutor` — the only in-flight dylib `Arc` holder between
    /// turns — is already dropped). Never dropped on the UI thread: doing so
    /// while an in-flight turn holds a dylib `Arc` would be UAF (dangling
    /// vtable). See spec §4a/§4b.
    pending_drop: Mutex<Vec<Library>>,
    /// Route A — the shared provider registry extension factories flush
    /// into at `bind_core`. Defaults to a private instance; hosts that
    /// resolve clients through their own shared instance call
    /// [`attach_shared_providers`](Self::attach_shared_providers) before
    /// load/bind so both sides see the same map. `Mutex` (not a plain
    /// field) so the host can re-point it through `&self`.
    shared: Mutex<SharedProviderRegistry>,
    /// Route A — the live registration guards (drop = un-register, see
    /// [`ProviderRegistration`]). `clear_providers` / reload drop the Vec;
    /// that is the whole unload story for this contribution type.
    providers: Mutex<Vec<ProviderRegistration>>,
    /// Route B (systemPrompt sections) — `(id, text)` in registration
    /// order. Register at mod load; `clear_prompt_sections` / reload drop
    /// them (a dropped section shifts the prompt prefix once, the same
    /// cost class as a whole-prompt replacement).
    prompt_sections: Mutex<Vec<(String, String)>>,
    /// Session log folds — mod-registered message projections. The hub is
    /// runner-created but host-shared: the engine folds transcript
    /// mutations through the same `Arc` (`fold_message` / `refold_all`),
    /// `projection_state(key)` reads from inside mod scripts, and
    /// `clear_message_projections` / reload drop the generation's entries
    /// (the engine refolds from the transcript on the next turn).
    message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc,
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
            libraries: Mutex::new(Vec::new()),
            pending_drop: Mutex::new(Vec::new()),
            shared: Mutex::new(SharedProviderRegistry::new()),
            providers: Mutex::new(Vec::new()),
            prompt_sections: Mutex::new(Vec::new()),
            message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc::new(
                codesmith_agent::extension::MessageProjectionHub::new(),
            ),
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

    /// Clear all bound handlers (§F2b T7 live reload). Called by
    /// [`reload_extension_runtime`](crate::reload_extension_runtime) BEFORE
    /// re-discovery so re-binding doesn't duplicate handlers — `bind_core`'s
    /// drain appends to `handlers`, it doesn't replace. A runtime lifecycle
    /// method; does NOT change the §F2a contract (`HandlerOutcome`/`EmitOutcome`
    /// /`on_variant`/`catch_unwind` stay stable).
    pub fn clear_handlers(&self) {
        self.handlers
            .lock()
            .expect("handlers lock poisoned")
            .clear();
    }

    /// §F5d — clear all bound **tools** (name→`Arc<dyn ToolDefinition>` map).
    ///
    /// Safe to call while an in-flight engine turn holds clones: each tool is
    /// refcounted (`Arc`) + the per-turn `ToolRegistry` already captured its own
    /// `Arc`; clearing here only affects FUTURE turns' `register_extension_tools`
    /// rebuild. Called from `reload_extension_runtime` before re-populate.
    pub fn clear_tools(&self) {
        self.tools.lock().expect("tools lock poisoned").clear();
    }

    /// §F5d — clear all bound **commands** (name→`CommandDefinition` map). Same
    /// concurrency reasoning as `clear_tools`. Called from
    /// `reload_extension_runtime`.
    pub fn clear_commands(&self) {
        self.commands
            .lock()
            .expect("commands lock poisoned")
            .clear();
    }

    /// Route A — clear all bound **providers** by dropping the generation's
    /// registration guards (drop = un-register; see
    /// [`ProviderRegistration`]). Same concurrency reasoning as
    /// `clear_tools`: an already-built client keeps working (it holds its
    /// `Arc`); only future resolutions stop seeing the extension factory.
    /// Called from `reload_extension_runtime` before re-populate.
    pub fn clear_providers(&self) {
        // Move out under the lock, drop the guards outside it: guard drops
        // take the shared registry's write lock (and factory `Drop`s may
        // run), which must not happen under this lock.
        let drained = {
            let mut providers = self.providers.lock().expect("providers lock poisoned");
            std::mem::take(&mut *providers)
        };
        drop(drained);
    }

    /// Route A — clone of the shared provider registry extensions flush
    /// into. Hosts resolve clients through this instance to see
    /// extension-registered factories.
    #[must_use]
    pub fn shared_providers(&self) -> SharedProviderRegistry {
        self.shared
            .lock()
            .expect("shared provider registry lock poisoned")
            .clone()
    }

    /// Route B (systemPrompt sections) — append `text` as a named section.
    /// Errors on an empty id/text, an unknown id charset, or beyond
    /// [`MAX_PROMPT_SECTIONS`] sections (fail loud at load, not mid-session).
    pub fn register_prompt_section(&self, id: String, text: String) -> Result<(), ExtensionError> {
        validate_prompt_section(&id, &text)?;
        let mut sections = self
            .prompt_sections
            .lock()
            .expect("prompt sections poisoned");
        if sections.len() >= MAX_PROMPT_SECTIONS {
            return Err(ExtensionError::Config(format!(
                "too many prompt sections (>{MAX_PROMPT_SECTIONS})"
            )));
        }
        // Re-registering an id replaces its text in place (id is identity).
        if let Some(slot) = sections.iter_mut().find(|(existing, _)| *existing == id) {
            slot.1 = text;
        } else {
            sections.push((id, text));
        }
        Ok(())
    }

    /// Route B — merge the registered sections onto `base` (append-only,
    /// registration order). `None` when no sections are registered (the
    /// caller keeps `base` untouched; no needless prefix churn).
    #[must_use]
    pub fn append_prompt_sections(
        &self,
        base: codesmith_agent::models::SystemPrompt,
    ) -> Option<codesmith_agent::models::SystemPrompt> {
        let sections = self
            .prompt_sections
            .lock()
            .expect("prompt sections poisoned");
        if sections.is_empty() {
            return None;
        }
        let joined = sections
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        Some(match base {
            codesmith_agent::models::SystemPrompt::Text(t) => {
                codesmith_agent::models::SystemPrompt::Text(format!("{t}\n\n{joined}"))
            }
            codesmith_agent::models::SystemPrompt::Blocks(mut blocks) => {
                blocks.push(codesmith_agent::models::SystemBlock {
                    block_type: "text".to_string(),
                    text: joined,
                    cache_control: None,
                });
                codesmith_agent::models::SystemPrompt::Blocks(blocks)
            }
        })
    }

    /// Route B — clear all prompt sections (reload drops the generation's
    /// contributions, mirroring `clear_tools`/`clear_providers`).
    pub fn clear_prompt_sections(&self) {
        self.prompt_sections
            .lock()
            .expect("prompt sections poisoned")
            .clear();
    }

    /// The shared message-projection hub. The engine folds transcript
    /// mutations through this `Arc`; `RhaiMod` bakes it into the
    /// `projection_state(key)` native.
    #[must_use]
    pub fn message_projection_hub(&self) -> codesmith_agent::extension::MessageProjectionHubArc {
        codesmith_agent::extension::MessageProjectionHubArc::clone(&self.message_projection_hub)
    }

    /// Drop the generation's message projections (reload). The hub turns
    /// dirty; the engine refolds from the live transcript at the next turn
    /// start, once the new generation has registered.
    pub fn clear_message_projections(&self) {
        self.message_projection_hub.clear();
    }

    /// Route A — point the runner at the host's shared provider registry
    /// (the one the host's client-resolution path reads). Call before
    /// `load`/`bind_core` (host wiring at construction); registrations
    /// already flushed into the previous instance stay there — their
    /// guards reference the instance they registered into.
    pub fn attach_shared_providers(&self, shared: SharedProviderRegistry) {
        *self
            .shared
            .lock()
            .expect("shared provider registry lock poisoned") = shared;
    }

    /// §F5d T4 — MOVE the live `libraries` into `pending_drop` (UI-thread,
    /// reload-time). Safe: takes the `libraries` lock once + `std::mem::take`s
    /// the `Vec` (each `Library` is an owned handle, not a borrowed `Arc`);
    /// the `Library` stays alive until [`drop_pending`](Self::drop_pending)
    /// runs. The main-thread executor's per-turn dylib `Arc`s are unaffected
    /// (they point at the `Arc<Library>` the engine captured this turn; the
    /// runner's own `Vec` move does not touch them). Called from
    /// `reload_extension_runtime` before re-populate loads fresh dylibs.
    /// Idempotent (empty `libraries` → no-op).
    pub fn drain_libraries_to_pending(&self) {
        let mut libs = self.libraries.lock().expect("libraries lock poisoned");
        let drained = std::mem::take(&mut *libs);
        let mut pending = self
            .pending_drop
            .lock()
            .expect("pending_drop lock poisoned");
        pending.extend(drained);
    }

    /// §F5d T4 — DROP the pending `Library`s. Called ONLY from the engine
    /// op-loop top (agent-runtime `engine/mod.rs`) before `match op`, at the
    /// one moment the main-thread `HostAgentExecutor` (the only in-flight
    /// dylib `Arc` holder) is already dropped between turns. Dropping here
    /// unloads the dylibs safely. Idempotent (empty pending → no-op). NEVER
    /// call this from the UI thread — see the `pending_drop` field.
    pub fn drop_pending(&self) {
        // Release the lock before dropping the `Library`s: `Library::drop`
        // runs dylib cleanup (`dlclose`/`FreeLibrary`) which may be slow, and
        // holding the `pending_drop` lock across it would block a concurrent
        // UI-thread `drain_libraries_to_pending`. The move out of the guard
        // is all that needs the lock.
        let drained = {
            let mut pending = self
                .pending_drop
                .lock()
                .expect("pending_drop lock poisoned");
            std::mem::take(&mut *pending)
        };
        drop(drained); // dylibs unloaded, no lock held
    }

    /// Load + configure one extension against a **stub** api. Registrations
    /// queue into `pending_*`. Called by `build_extension_runtime` (Task 9)
    /// for each discovered extension, BEFORE `bind_core`.
    pub async fn load(&self, ext: &dyn Extension) -> Result<(), ExtensionError> {
        let shared = self
            .shared
            .lock()
            .expect("shared provider registry lock poisoned")
            .clone();
        let stub = StubExtensionApi::new(self.generation.clone(), self.pending.clone(), shared);
        ext.configure(&stub).await
    }

    /// §F5b — load a dylib extension (spec §F5b / §7.2). Opens the library,
    /// calls its `codesmith_register_extension`, pushes the `Library` into
    /// `libraries` (must outlive registered contributions' vtables; reload
    /// does not clear — Q1), then runs `configure` via [`load`](Self::load).
    /// The `Extension` Box is dropped after `configure` (registered
    /// contributions are self-contained owned trait objects; vtables live in
    /// the kept `Library`). Lockstep (§8.2) assumed. Mirrors the static
    /// `load` path.
    pub async fn load_dylib(&self, path: &Path) -> Result<(), ExtensionError> {
        let (library, extension) = crate::loader::load_dylib(path)?;
        self.libraries
            .lock()
            .expect("libraries lock poisoned")
            .push(library);
        self.load(&*extension).await
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
        let mut providers = self.providers.lock().unwrap();
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
            handlers.push(RegisteredHandler {
                handler: ph.handler,
                kind_filter: ph.kind_filter,
            });
        }
        for (id, text) in pending.prompt_sections.drain(..) {
            if let Err(e) = self.register_prompt_section(id, text) {
                tracing::warn!(
                    target: "codesmith_extensions",
                    "prompt section rejected at bind: {e}"
                );
            }
        }
        for mp in pending.message_projections.drain(..) {
            if let Err(e) = self
                .message_projection_hub
                .register(&mp.owner, &mp.key, mp.init, mp.fold)
            {
                tracing::warn!(
                    target: "codesmith_extensions",
                    "message projection rejected at bind: {e}"
                );
            }
        }
        for pp in pending.providers.drain(..) {
            let id = pp.factory.id();
            let guard = self
                .shared
                .lock()
                .expect("shared provider registry lock poisoned")
                .register(pp.factory);
            tracing::info!(
                target: "codesmith_extensions",
                "extension provider registered: {}",
                id.as_str(),
            );
            providers.push(guard);
        }
    }

    /// Emit `event` to every bound handler whose variant filter matches,
    /// chaining transforms (spec §4: "一个 handler 的修改对下一个可见").
    /// Returns [`EmitOutcome`] — the final (possibly transformed) event +
    /// the terminal outcome (`Continue` / `Cancel` / `Block`). Each handler
    /// call is wrapped in `catch_unwind` (§8.3) so a panicking handler cannot
    /// tear down the agent loop; its panic is recorded via `tracing` and the
    /// chain continues. A handler `Err` (non-panic) is likewise recorded +
    /// the chain continues (best-effort, §8.3). `Cancel` / `Block`
    /// short-circuit: no further handlers run. `Transform(new)` replaces the
    /// running event and the chain continues with the next handler.
    /// No-op (returns the input event + `Continue`) if `bind_core` has not
    /// run.
    pub async fn emit(&self, event: ExtensionEvent) -> EmitOutcome {
        let ctx = match self.context.lock().unwrap().clone() {
            Some(ctx) => ctx,
            None => {
                return EmitOutcome {
                    event,
                    outcome: HandlerOutcome::Continue,
                };
            }
        };
        // Snapshot the matching handlers under a short lock; dispatch outside
        // the lock so a long-running handler doesn't hold it.
        let kind = event.kind();
        let matching: Vec<Arc<dyn Handler>> = self
            .handlers
            .lock()
            .unwrap()
            .iter()
            .filter(|rh| rh.kind_filter.is_none() || rh.kind_filter == Some(kind))
            .map(|rh| Arc::clone(&rh.handler))
            .collect();
        let mut event = event;
        let mut outcome = HandlerOutcome::Continue;
        for h in matching {
            // §F2a: catch_unwind so a panicking handler can't tear down the
            // agent loop (§8.3 error isolation).
            let result = AssertUnwindSafe(h.handle(&event, &*ctx))
                .catch_unwind()
                .await;
            match result {
                // Panic → record + continue (one handler can't break the chain).
                Err(panic) => {
                    tracing::error!(
                        target: "codesmith_extensions::runner",
                        "extension handler panicked: {panic:?}",
                    );
                    continue;
                }
                // Handler error → record + continue (best-effort, §8.3).
                Ok(Err(err)) => {
                    tracing::error!(
                        target: "codesmith_extensions::runner",
                        "extension handler error: {err}",
                    );
                    continue;
                }
                Ok(Ok(HandlerOutcome::Continue)) => continue,
                Ok(Ok(HandlerOutcome::Transform(new_event))) => {
                    event = new_event;
                    continue;
                }
                // Cancel / Block short-circuit the chain.
                Ok(Ok(c @ HandlerOutcome::Cancel { .. })) => {
                    outcome = c;
                    break;
                }
                Ok(Ok(b @ HandlerOutcome::Block { .. })) => {
                    outcome = b;
                    break;
                }
            }
        }
        EmitOutcome { event, outcome }
    }

    /// Look up a registered command by name (exact match; `:N` conflict
    /// suffixing is §F2 — slice 1 uses first-wins via HashMap insert). Used
    /// by the tui `extension_commands::try_dispatch` (Task 8).
    pub async fn try_dispatch_command(&self, name: &str, args: &str) -> Option<CommandOutput> {
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

    /// §F5d — bound `ExtensionContext` for the host's `ExtensionToolSpecAdapter`
    /// (upcasts the stored `Arc<dyn ExtensionCommandContext>` to
    /// `Arc<dyn ExtensionContext>` via Rust 1.86+ trait-upcasting). `None`
    /// before `bind_core`. Used per-turn by `register_extension_tools`.
    #[must_use]
    pub fn bound_context(&self) -> Option<Arc<dyn ExtensionContext>> {
        self.context
            .lock()
            .expect("context lock poisoned")
            .clone()
            .map(|ctx| -> Arc<dyn ExtensionContext> { ctx })
    }

    /// Names of bound commands (for `/extension list`, Task 8).
    pub fn bound_command_names(&self) -> Vec<String> {
        self.commands.lock().unwrap().keys().cloned().collect()
    }
}

/// Manual `Debug` (the struct holds `dyn` trait-object fields that don't
/// implement `Debug`); mirrors the `SandboxBackend: Debug` supertrait pattern
/// used elsewhere. Shows generation + bound-state counts so `EngineHost`'s
/// `#[derive(Debug)]` keeps working (§F2c surfaces the runner on `EngineHost`).
impl std::fmt::Debug for ExtensionRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tools = self.tools.lock().expect("tools mutex poisoned").len();
        let commands = self.commands.lock().expect("commands mutex poisoned").len();
        let handlers = self.handlers.lock().expect("handlers mutex poisoned").len();
        let libraries = self
            .libraries
            .lock()
            .expect("libraries mutex poisoned")
            .len();
        let pending_drop = self
            .pending_drop
            .lock()
            .expect("pending_drop mutex poisoned")
            .len();
        let providers = self
            .providers
            .lock()
            .expect("providers mutex poisoned")
            .len();
        let bound = self
            .context
            .lock()
            .expect("context mutex poisoned")
            .is_some();
        f.debug_struct("ExtensionRunner")
            .field("generation", &self.generation.load(Ordering::Acquire))
            .field("bound", &bound)
            .field("tools", &tools)
            .field("commands", &commands)
            .field("handlers", &handlers)
            .field("libraries", &libraries)
            .field("pending_drop", &pending_drop)
            .field("providers", &providers)
            .finish()
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
    use codesmith_tools::ToolResult;
    use serde_json::json;
    use std::path::Path;
    use tokio_util::sync::CancellationToken;

    struct Ctx {
        generation: u64,
    }
    #[async_trait::async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &Path {
            Path::new(".")
        }
        fn mode(&self) -> ExtensionMode {
            ExtensionMode::Tui
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn signal(&self) -> CancellationToken {
            CancellationToken::new()
        }
        fn generation(&self) -> u64 {
            self.generation
        }
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
            api.on(Arc::new(RecHandler {
                seen: self.seen.clone(),
            }))?;
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
        ) -> Result<HandlerOutcome, ExtensionError> {
            self.seen.lock().unwrap().push(match event {
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::SessionShutdown => "SessionShutdown",
                _ => "other",
            });
            Ok(HandlerOutcome::Continue)
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
        let _ = runner
            .emit(ExtensionEvent::TurnStart {
                turn_id: "t1".into(),
            })
            .await;
        let _ = runner.emit(ExtensionEvent::SessionShutdown).await;
        let s = seen.lock().unwrap();
        assert_eq!(*s, vec!["TurnStart", "SessionShutdown"]);
    }

    #[tokio::test]
    async fn emit_before_bind_core_is_noop() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner.load(&RecExt { seen: seen.clone() }).await.unwrap();
        // No bind_core — emit must not panic + must not dispatch.
        let _ = runner.emit(ExtensionEvent::SessionShutdown).await;
        assert!(seen.lock().unwrap().is_empty());
    }

    // === §F2a emit chaining / per-variant / catch_unwind ====================

    /// An extension that registers two handlers in deterministic order
    /// (first, then second) so chain ordering is observable.
    struct TwoHandlerExt {
        first: Arc<dyn Handler>,
        second: Arc<dyn Handler>,
    }
    #[async_trait::async_trait]
    impl Extension for TwoHandlerExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("two");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.on(self.first.clone())?;
            api.on(self.second.clone())?;
            Ok(())
        }
    }

    /// Transforms a `ToolResult` event's result to a fixed success string.
    struct TransformResultHandler;
    #[async_trait::async_trait]
    impl Handler for TransformResultHandler {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if let ExtensionEvent::ToolResult(tr) = event {
                let mut tr = tr.clone();
                tr.result = Ok(ToolResult::success("transformed"));
                Ok(HandlerOutcome::Transform(ExtensionEvent::ToolResult(tr)))
            } else {
                Ok(HandlerOutcome::Continue)
            }
        }
    }

    /// Records the `ToolResult` content string it observes.
    struct ObserveResultHandler {
        seen: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Handler for ObserveResultHandler {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if let ExtensionEvent::ToolResult(tr) = event {
                if let Ok(r) = &tr.result {
                    self.seen.lock().unwrap().push(r.content.clone());
                }
            }
            Ok(HandlerOutcome::Continue)
        }
    }

    #[tokio::test]
    async fn f2a_emit_chains_transform_to_next_handler() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let transform: Arc<dyn Handler> = Arc::new(TransformResultHandler);
        let observer: Arc<dyn Handler> = Arc::new(ObserveResultHandler { seen: seen.clone() });
        runner
            .load(&TwoHandlerExt {
                first: transform,
                second: observer,
            })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));

        let original = ExtensionEvent::ToolResult(ToolResultEvent {
            id: "c1".into(),
            name: "echo".into(),
            result: Ok(ToolResult::success("original")),
        });
        let out = runner.emit(original).await;
        // Observer saw the TRANSFORMED result (transform visible to next handler).
        assert_eq!(*seen.lock().unwrap(), vec!["transformed".to_string()]);
        // Final event carries the transformed result.
        match out.event {
            ExtensionEvent::ToolResult(tr) => {
                let r = tr.result.expect("ok result");
                assert_eq!(r.content, "transformed");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        // Transform folds in — terminal outcome is Continue.
        assert!(matches!(out.outcome, HandlerOutcome::Continue));
    }

    struct CancelOnBeforeCompact;
    #[async_trait::async_trait]
    impl Handler for CancelOnBeforeCompact {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if matches!(event, ExtensionEvent::SessionBeforeCompact) {
                Ok(HandlerOutcome::Cancel {
                    reason: "user aborted".into(),
                })
            } else {
                Ok(HandlerOutcome::Continue)
            }
        }
    }

    #[tokio::test]
    async fn f2a_emit_cancel_short_circuits_chain() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        // Cancel first; the second observer must NOT fire (cancel short-circuits).
        runner
            .load(&TwoHandlerExt {
                first: Arc::new(CancelOnBeforeCompact),
                second: Arc::new(RecHandler { seen: seen.clone() }),
            })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let out = runner.emit(ExtensionEvent::SessionBeforeCompact).await;
        assert!(matches!(out.outcome, HandlerOutcome::Cancel { .. }));
        // The observer after the cancel handler never fired.
        assert!(seen.lock().unwrap().is_empty());
    }

    struct BlockOnToolCall;
    #[async_trait::async_trait]
    impl Handler for BlockOnToolCall {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            if matches!(event, ExtensionEvent::ToolCall(_)) {
                Ok(HandlerOutcome::Block {
                    reason: "policy".into(),
                })
            } else {
                Ok(HandlerOutcome::Continue)
            }
        }
    }

    #[tokio::test]
    async fn f2a_emit_block_short_circuits_chain() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner
            .load(&TwoHandlerExt {
                first: Arc::new(BlockOnToolCall),
                second: Arc::new(RecHandler { seen: seen.clone() }),
            })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let out = runner
            .emit(ExtensionEvent::ToolCall(ToolCallEvent {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({}),
            }))
            .await;
        assert!(matches!(out.outcome, HandlerOutcome::Block { .. }));
        assert!(seen.lock().unwrap().is_empty());
    }

    /// An extension that subscribes a handler to ToolCall ONLY (per-variant).
    struct VariantExt {
        seen: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait::async_trait]
    impl Extension for VariantExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("var");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.on_variant(
                ExtensionEventKind::ToolCall,
                Arc::new(RecHandler {
                    seen: self.seen.clone(),
                }),
            )?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn f2a_on_variant_dispatches_only_matching_kind() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner
            .load(&VariantExt { seen: seen.clone() })
            .await
            .unwrap();
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        // ToolCall fires the per-variant handler (RecHandler pushes "other").
        let _ = runner
            .emit(ExtensionEvent::ToolCall(ToolCallEvent {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({}),
            }))
            .await;
        // TurnStart does NOT fire the per-variant handler.
        let _ = runner
            .emit(ExtensionEvent::TurnStart {
                turn_id: "t1".into(),
            })
            .await;
        let s = seen.lock().unwrap();
        assert_eq!(*s, vec!["other"]);
    }

    struct PanickingHandler;
    #[async_trait::async_trait]
    impl Handler for PanickingHandler {
        async fn handle(
            &self,
            _event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            panic!("boom");
        }
    }

    #[test]
    fn f2a_emit_catch_unwind_isolates_panicking_handler() {
        // Dedicated multi-thread runtime (NOT #[tokio::test]) to avoid
        // current-thread panic-abort subtleties + nested-runtime panics
        // (the same lesson as §F1's configure-runtime discovery).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("multi-thread runtime for catch_unwind test");
        rt.block_on(async {
            let runner = ExtensionRunner::new();
            let seen = Arc::new(Mutex::new(Vec::new()));
            // Panicking handler first; the second observer must STILL fire
            // (catch_unwind isolates the panic).
            runner
                .load(&TwoHandlerExt {
                    first: Arc::new(PanickingHandler),
                    second: Arc::new(RecHandler { seen: seen.clone() }),
                })
                .await
                .unwrap();
            runner.bind_core(Arc::new(Ctx { generation: 1 }));
            let out = runner
                .emit(ExtensionEvent::TurnStart {
                    turn_id: "t1".into(),
                })
                .await;
            // Observer after the panicking handler still fired.
            assert_eq!(*seen.lock().unwrap(), vec!["TurnStart"]);
            // Chain continued past the panic → Continue.
            assert!(matches!(out.outcome, HandlerOutcome::Continue));
        });
    }

    /// §F5d T3/T4 test helper — runner with the fixture dylib loaded +
    /// `fixture_echo` bound. Direct `load_dylib` on `env!("CODESMITH_FIXTURE_DYLIB")`
    /// (same compile-time env var as installer.rs:189); skips the install→discover
    /// round-trip (T3/T4 test clear/drain semantics, not the install pipeline).
    ///
    /// Quarantined on Windows — see loader.rs `load_dylib_fixture_…` comment
    /// (fixture dylib unload races with parallel tests → STATUS_ACCESS_VIOLATION).
    #[cfg(not(windows))]
    fn runner_with_fixture_dylib() -> crate::ExtensionRunner {
        let runner = crate::ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let fixture = crate::test_support::fixture_dylib_path();
        rt.block_on(runner.load_dylib(std::path::Path::new(&fixture)))
            .expect("load fixture dylib");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        runner
    }

    #[cfg(not(windows))]
    #[test]
    fn clear_tools_and_clear_commands_empty_registries() {
        let runner = runner_with_fixture_dylib();

        assert!(
            runner
                .bound_tools()
                .iter()
                .any(|(n, _)| n == "fixture_echo"),
            "fixture_echo bound before clear"
        );

        runner.clear_tools();
        assert!(
            runner.bound_tools().is_empty(),
            "tools cleared: {:?}",
            runner
                .bound_tools()
                .iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        );

        // clear_commands: safe on whatever the fixture registered (tool-only →
        // commands empty pre- and post-; assert no panic + stays empty). If the
        // fixture also registers a command, add a "present-then-cleared" assert
        // here (read crates/extensions-fixture-dylib/src/lib.rs to confirm).
        runner.clear_commands();

        // Re-load proves clear is non-destructive to the runner itself.
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let fixture = crate::test_support::fixture_dylib_path();
        rt.block_on(runner.load_dylib(std::path::Path::new(&fixture)))
            .expect("reload");
        runner.bind_core(Arc::new(Ctx { generation: 2 }));
        assert!(
            runner
                .bound_tools()
                .iter()
                .any(|(n, _)| n == "fixture_echo"),
            "fixture_echo re-bound after reload"
        );
    }

    // === §F5d follow-up — clear_commands present→cleared =================
    // T3's `clear_tools_and_clear_commands_empty_registries` covers the
    // tool-only fixture dylib (clear_commands is a safe no-op when no command
    // is registered — it asserts no-panic + stays empty). This fills the
    // coverage gap T3's own comment (:830-833) flags: register a command,
    // assert it is bound (non-empty), then `clear_commands()` empties the
    // map. Mirrors T3's shape (load→bind→assert-present→clear→assert-empty)
    // but for commands, via an in-process command-registering extension (the
    // fixture dylib registers only a tool + handler, so a command-clear test
    // needs its own command contributor).

    /// A contributed slash command registered in-process. Mirrors `EchoCmd`
    /// in `extension_commands.rs` (T2 dispatch fixture) — the symmetric
    /// command-only contributor vs the fixture's tool-only contributor.
    struct EchoCmd;
    #[async_trait::async_trait]
    impl CommandDefinition for EchoCmd {
        fn name(&self) -> &str {
            "clear_cmd_test"
        }
        fn description(&self) -> &str {
            "Echoes args (clear_commands coverage test)."
        }
        async fn run(
            &self,
            _ctx: &dyn ExtensionCommandContext,
            args: &str,
        ) -> Result<CommandOutput, ExtensionError> {
            Ok(CommandOutput::Message(format!("echo:{args}")))
        }
    }

    /// In-process extension registering `EchoCmd` via `api.register_command`
    /// (mirrors `RecExt`'s `api.on` shape, but contributing a command). The
    /// fixture dylib can't serve here (it registers only a tool + handler).
    struct CmdExt;
    #[async_trait::async_trait]
    impl Extension for CmdExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("cmd-clear-ext");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.register_command(Box::new(EchoCmd))?;
            Ok(())
        }
    }

    #[test]
    fn clear_commands_drops_present_command_binding() {
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&CmdExt)).expect("load CmdExt");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));

        // Command present before clear.
        let names = runner.bound_command_names();
        assert!(
            names.iter().any(|n| n == "clear_cmd_test"),
            "clear_cmd_test bound before clear: {names:?}"
        );

        runner.clear_commands();

        // Command gone after clear.
        let after = runner.bound_command_names();
        assert!(after.is_empty(), "commands cleared: {after:?}");
    }

    // === §F5d T4 — two-phase Library drain/drop ===========================

    /// §F5d T4 — the UI-thread MOVE (`drain_libraries_to_pending`) empties
    /// `libraries` into `pending_drop` (the `Library` stays alive — a safe
    /// `Arc`-free `mem::take` under one lock; the live dylib handles are
    /// merely re-homed, not dropped). The engine op-loop-top DROP
    /// (`drop_pending`) then frees them at the one moment the main-thread
    /// `HostAgentExecutor` (the only in-flight dylib `Arc` holder) is already
    /// dropped between turns (spec §4a/§4b). Both ops must be idempotent: a
    /// second drain on an empty `libraries` + a second drop on an empty
    /// `pending_drop` are no-ops (must not panic).
    ///
    /// `libraries` is `Mutex<Vec<Library>>` with no pub count accessor by
    /// design (exposing `Library` would leak `libloading` internals), so the
    /// semantics are proven behaviourally: idempotent + no panic across the
    /// drain→drain→drop→drop sequence.
    #[cfg(not(windows))]
    #[test]
    fn drain_libraries_to_pending_moves_then_drop_pending_empties() {
        let runner = runner_with_fixture_dylib();

        // Drain moves the live `libraries` into `pending_drop` (Library stays
        // alive). A second drain is a no-op (drains an empty Vec).
        runner.drain_libraries_to_pending();
        runner.drain_libraries_to_pending();

        // Drop frees the pending Libraries (dylibs unloaded). A second drop
        // on an empty pending is a no-op (must not panic).
        runner.drop_pending();
        runner.drop_pending();
    }

    // === Route A — provider registration flush / clear / attach ===========

    /// Minimal test-only `LlmClient` + factory (mirrors the shape in
    /// `codesmith-agent`'s provider tests) so runner provider tests can
    /// register a real factory.
    struct EchoClient {
        model: String,
    }
    impl codesmith_agent::llm_client::LlmClient for EchoClient {
        fn provider_name(&self) -> &'static str {
            "echo"
        }
        fn model(&self) -> &str {
            &self.model
        }
        fn create_message(
            &self,
            _request: codesmith_agent::models::MessageRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = anyhow::Result<codesmith_agent::models::MessageResponse>,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async { Err(anyhow::anyhow!("echo mock")) })
        }
        fn create_message_stream(
            &self,
            _request: codesmith_agent::models::MessageRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = anyhow::Result<codesmith_agent::llm_client::StreamEventBox>,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async { Err(anyhow::anyhow!("echo mock")) })
        }
    }

    struct EchoFactory {
        id: codesmith_agent::provider::ProviderId,
    }
    impl codesmith_agent::provider::ProviderFactory for EchoFactory {
        fn id(&self) -> codesmith_agent::provider::ProviderId {
            self.id.clone()
        }
        fn build(
            &self,
            cfg: &codesmith_agent::provider::ProviderConfig,
        ) -> anyhow::Result<codesmith_agent::llm_client::LlmClientHandle> {
            Ok(Arc::new(EchoClient {
                model: cfg.default_model.clone(),
            }))
        }
    }

    /// Registers one provider factory via `api.register_provider` — the
    /// Rust-side (dylib-identical) contribution shape.
    struct ProviderExt;
    #[async_trait::async_trait]
    impl Extension for ProviderExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("provider-ext");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.register_provider(Arc::new(EchoFactory {
                id: codesmith_agent::provider::ProviderId::from("acme-llm"),
            }))?;
            Ok(())
        }
    }

    #[test]
    fn provider_registration_flushes_into_shared_and_clear_removes() {
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&ProviderExt)).expect("load");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));

        let shared = runner.shared_providers();
        let id = codesmith_agent::provider::ProviderId::from("acme-llm");
        assert!(shared.resolve(&id).is_some(), "flushed at bind_core");

        // clear_providers drops the generation's guards → un-register.
        runner.clear_providers();
        assert!(
            shared.resolve(&id).is_none(),
            "clear drops the registration guards"
        );

        // Re-load proves clear is non-destructive to the runner itself.
        rt.block_on(runner.load(&ProviderExt)).expect("reload");
        runner.bind_core(Arc::new(Ctx { generation: 2 }));
        assert!(
            shared.resolve(&id).is_some(),
            "provider re-registered after reload"
        );
    }

    #[test]
    fn attach_shared_providers_repoints_flush_target() {
        // Host wiring shape: create the shared instance the host resolves
        // through, attach it BEFORE load/bind, and the registration lands
        // there (not in the runner's private default).
        let runner = ExtensionRunner::new();
        let host_shared = SharedProviderRegistry::new();
        runner.attach_shared_providers(host_shared.clone());
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&ProviderExt)).expect("load");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));

        let id = codesmith_agent::provider::ProviderId::from("acme-llm");
        assert!(host_shared.resolve(&id).is_some());
        assert!(
            runner.shared_providers().resolve(&id).is_some(),
            "shared_providers() is the attached instance"
        );
    }

    // === Route B — prompt sections =========================================

    #[test]
    fn prompt_sections_register_merge_and_clear() {
        use codesmith_agent::models::SystemPrompt;
        let runner = ExtensionRunner::new();

        // No sections → None (base untouched, no prefix churn).
        assert!(
            runner
                .append_prompt_sections(SystemPrompt::Text("base".into()))
                .is_none()
        );

        runner
            .register_prompt_section("style".into(), "Be terse.".into())
            .unwrap();
        runner
            .register_prompt_section("domain".into(), "Fintech conventions apply.".into())
            .unwrap();
        // Re-registering an id replaces in place (identity), not append.
        runner
            .register_prompt_section("style".into(), "Be VERY terse.".into())
            .unwrap();

        let merged = runner
            .append_prompt_sections(SystemPrompt::Text("base".into()))
            .expect("merged");
        match merged {
            SystemPrompt::Text(t) => {
                assert!(t.starts_with("base"));
                assert!(t.contains("Be VERY terse."), "replaced text wins: {t}");
                assert!(t.contains("Fintech conventions apply."));
                assert!(
                    t.find("VERY").unwrap() < t.find("Fintech").unwrap(),
                    "order kept"
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
        // Blocks base gets an appended block.
        let merged_blocks = runner
            .append_prompt_sections(SystemPrompt::Blocks(Vec::new()))
            .expect("merged blocks");
        assert!(matches!(merged_blocks, SystemPrompt::Blocks(b) if b.len() == 1));

        runner.clear_prompt_sections();
        assert!(
            runner
                .append_prompt_sections(SystemPrompt::Text("base".into()))
                .is_none(),
            "cleared"
        );
    }

    #[test]
    fn prompt_section_validation_and_cap() {
        let runner = ExtensionRunner::new();
        assert!(
            runner
                .register_prompt_section("bad id!".into(), "x".into())
                .is_err()
        );
        assert!(
            runner
                .register_prompt_section("ok".into(), "  ".into())
                .is_err()
        );
        for i in 0..MAX_PROMPT_SECTIONS {
            runner
                .register_prompt_section(format!("s{i}"), "x".into())
                .unwrap();
        }
        assert!(
            runner
                .register_prompt_section("overflow".into(), "x".into())
                .is_err()
        );
    }

    #[test]
    fn prompt_section_flushes_at_bind_core() {
        struct SectionExt;
        #[async_trait::async_trait]
        impl Extension for SectionExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("section-ext");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.register_prompt_section("guide".into(), "Prefer small diffs.".into())?;
                Ok(())
            }
        }
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&SectionExt)).expect("load");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        assert!(
            runner
                .append_prompt_sections(codesmith_agent::models::SystemPrompt::Text("b".into()))
                .is_some(),
            "flushed at bind_core"
        );
    }

    #[test]
    fn provider_registration_before_bind_core_is_not_live() {
        // Pending-only: configure queued the factory but bind_core has not
        // flushed it, so the shared registry does not resolve it yet
        // (mirrors the two-phase tools/commands contract).
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&ProviderExt)).expect("load");
        assert!(
            runner
                .shared_providers()
                .resolve(&codesmith_agent::provider::ProviderId::from("acme-llm"))
                .is_none()
        );
    }
}
