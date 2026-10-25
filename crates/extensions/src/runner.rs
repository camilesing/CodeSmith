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

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codesmith_agent::extension::*;
use codesmith_agent::provider::{ProviderFactory, ProviderRegistration, SharedProviderRegistry};
use futures_util::FutureExt;
use libloading::Library;

use crate::api::StubExtensionApi;

/// Which load path contributed a registration — the §F5d unload-safety
/// tier. Dylib-origin registrations carry code that lives in a
/// `libloading::Library` the runner may `dlclose` on reload (two-phase
/// drop); a detached background sub-agent can outlive that moment, so
/// dylib-origin tools/handlers are **main-turn-only** (see
/// [`ExtensionRunner::mountable_tools_for_subagents`] and the
/// `emit_scoped` origin filter). Script and compiled-in registrations
/// hold statically linked code (or retained script ASTs) and scope
/// everywhere. Stamped by [`ExtensionRunner::load_as`] at load time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationOrigin {
    /// An `inventory`-discovered extension compiled into the binary.
    CompiledIn,
    /// A Rhai script mod (builtin tier or user mod) — adapter-owned code.
    Script,
    /// An installed Rust dylib — unloadable, hence main-turn-only.
    Dylib,
}

impl RegistrationOrigin {
    /// Whether registrations of this origin must stay out of sub-agent
    /// registries and sub-agent-scoped dispatch (the §F5d T4 safety
    /// rule).
    #[must_use]
    pub fn is_dylib(&self) -> bool {
        matches!(self, RegistrationOrigin::Dylib)
    }
}

/// A tool queued by the stub `ExtensionApi` during `configure`, awaiting
/// `bind_core` flush into the host `ToolRegistry`.
pub(crate) struct PendingTool {
    pub tool: Box<dyn ToolDefinition>,
    /// Registration scope — keyed `(scope, name)` at flush, so a
    /// `Global` and an `AgentType(id)` registration of the same name
    /// coexist (the scoped one shadows the global one in that type's
    /// catalog view).
    pub scope: Scope,
    /// Which load path contributed this tool (§F5d unload-safety tier).
    pub origin: RegistrationOrigin,
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

/// Route B (skills) — shared validation (stub-time and live registration
/// paths). Name mirrors the `register_tool` charset; description/body must
/// carry content (a catalogue line and a loadable body are the whole point
/// of a registered skill).
pub(crate) fn validate_skill_registration(
    name: &str,
    description: &str,
    body: &str,
) -> Result<(), ExtensionError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ExtensionError::Config(format!(
            "skill name {name:?} must match [a-zA-Z0-9_-] and be 1-64 chars"
        )));
    }
    if description.trim().is_empty() {
        return Err(ExtensionError::Config(format!(
            "skill {name:?} description must be non-empty"
        )));
    }
    if body.trim().is_empty() {
        return Err(ExtensionError::Config(format!(
            "skill {name:?} body must be non-empty"
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

/// Tool restriction — shared validation (stub-time and live
/// registration paths): a usable scope + at least one non-blank tool
/// name, none blank.
pub(crate) fn validate_tool_restriction(
    scope: &Scope,
    names: &[String],
) -> Result<(), ExtensionError> {
    scope.validate()?;
    if names.is_empty() {
        return Err(ExtensionError::Config(
            "restrict_tools(): names must name at least one tool".into(),
        ));
    }
    if names.iter().any(|n| n.trim().is_empty()) {
        return Err(ExtensionError::Config(
            "restrict_tools(): names must be non-empty tool names".into(),
        ));
    }
    Ok(())
}

/// A handler subscribed during `configure`, with its variant filter
/// (`None` = subscribe-to-all via `on`; `Some(kind)` = per-variant via
/// `on_variant`), its registration scope (`Global` for the plain forms;
/// `AgentType(id)` via `on_variant_scoped` — dispatched only at that
/// scope), and its origin (§F5d unload-safety tier). §F2a.
pub(crate) struct PendingHandler {
    pub handler: Arc<dyn Handler>,
    pub kind_filter: Option<ExtensionEventKind>,
    pub scope: Scope,
    pub origin: RegistrationOrigin,
}

/// A bound handler + its variant filter (None = subscribe-to-all),
/// registration scope (an `AgentType(id)` handler receives
/// only `emit_scoped` dispatches at its scope; `emit` is Global), and
/// origin (a `Dylib` handler is skipped at agent-type scopes — §F5d
/// unload safety). §F2a T7 makes `ExtensionRunner::handlers` a
/// `Vec<RegisteredHandler>`; `bind_core` drains `PendingHandler` into
/// this (carrying all fields through).
#[derive(Clone)]
pub(crate) struct RegisteredHandler {
    pub handler: Arc<dyn Handler>,
    pub kind_filter: Option<ExtensionEventKind>,
    pub scope: Scope,
    pub origin: RegistrationOrigin,
}

/// A deny-only tool restriction:
/// `denied` names are pruned from the **global** extension-tool layer for
/// every view `scope` covers. Never prunes scoped registrations. Used
/// both as the pending queue entry and the bound storage shape.
pub(crate) struct ToolRestriction {
    pub scope: Scope,
    pub denied: Vec<String>,
}

/// The result of [`ExtensionRunner::emit`]: the final (possibly transformed)
/// event + the terminal chain outcome. The host inspects `outcome` at each
/// seam (proceed / cancel / block) and, at transform-capable seams, applies
/// `event`'s actionable field (§F2b wires the host to honor these; §F2a
/// returns them + proves the chain in isolation).
///
/// `#[must_use]` (§F2b) so the host can't silently drop an outcome at a
/// transform/block seam — emit-only sites bind `let _ =`, actionable seams
/// bind `let out =` and inspect `out.outcome` / `out.event`.
#[derive(Debug, Clone)]
#[must_use]
pub struct EmitOutcome {
    /// The event after all handlers (possibly transformed by `Transform`).
    pub event: ExtensionEvent,
    /// Terminal outcome: `Continue` if no handler short-circuited; `Cancel`,
    /// `Block`, or `Ask` if one did. Never `Transform` (folds into `event`).
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
    /// Around-dispatch registrations (tools/execute) queued by the
    /// stub `ExtensionApi`, awaiting the `bind_core` flush.
    pub around_tools: Vec<Arc<dyn AroundToolHandler>>,
    pub providers: Vec<PendingProvider>,
    pub prompt_sections: Vec<(String, String)>,
    pub skills: Vec<codesmith_agent::extension::SkillRegistration>,
    pub message_projections: Vec<PendingMessageProjection>,
    /// Data policies (see [`codesmith_agent::policy`]); last-wins at
    /// flush by registration order (builtin < global < project mods).
    /// `source` is the registering mod's id, stamped at the replay seam.
    pub policies: Vec<codesmith_agent::policy::ResolvedPolicy>,
    /// Builtin command names the stub `ExtensionApi`'s `disable_command`
    /// queued during `configure`, awaiting the `bind_core` flush.
    pub disabled_commands: Vec<String>,
    /// Tool restrictions queued by the stub `ExtensionApi`, awaiting
    /// the `bind_core` flush (deny-only; see [`ToolRestriction`]).
    pub tool_restrictions: Vec<ToolRestriction>,
}

/// A message projection queued by the stub `ExtensionApi`.
pub(crate) struct PendingMessageProjection {
    pub owner: String,
    pub key: String,
    pub init: serde_json::Value,
    pub fold: codesmith_agent::extension::MessageFoldFn,
}

/// Outcome of [`ExtensionRunner::unload_retained_mod`].
#[derive(Debug, Clone)]
#[must_use]
pub struct UnloadModOutcome {
    /// Whether a retained slot existed for the id (false = no-op).
    pub evicted: bool,
    /// How many surviving mods re-registered cleanly.
    pub replayed: usize,
    /// Survivor replay failures + the unbound-context error, if any.
    pub errors: Vec<String>,
}

/// One bound tool: the definition plus the load-path origin that governs
/// whether it may mount into a sub-agent registry (§F5d unload-safety
/// tier — see [`RegistrationOrigin`]).
pub(crate) struct BoundTool {
    pub def: Arc<dyn ToolDefinition>,
    pub origin: RegistrationOrigin,
}

/// The bound tool registry's storage shape: `(scope, name)` → bound tool
/// (registration scope — see [`ExtensionRunner::bound_tools_scoped`]).
pub(crate) type BoundToolMap = HashMap<(Scope, String), BoundTool>;

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
    /// Bound at `bind_core` — the flushed tools, keyed `(scope, name)`
    /// (registration scope): first-wins per key, so a `Global` and an
    /// `AgentType(id)` tool of the same name coexist and the scoped one
    /// shadows the global one in that type's catalog view. Hosts resolve
    /// a view via [`bound_tools_scoped`](Self::bound_tools_scoped) —
    /// [`bound_tools`](Self::bound_tools) is the `Global` view.
    tools: Mutex<BoundToolMap>,
    /// Bound at `bind_core` — deny-only tool restrictions,
    /// intersecting; cleared with the other bound
    /// registrations on reload.
    tool_restrictions: Mutex<Vec<ToolRestriction>>,
    commands: Mutex<HashMap<String, Arc<dyn CommandDefinition>>>,
    /// Bound at `bind_core` — builtin command names disabled by this
    /// generation's extensions (`disable_command`; the "suppress a
    /// builtin" contribution). The framework stores names opaquely — the
    /// host resolves them against its builtin table and warns on unknowns.
    disabled_builtin_commands: Mutex<HashSet<String>>,
    /// Bound at `bind_core` — the flushed handlers, each carrying its variant
    /// filter (`None` = subscribe-to-all via `on`; `Some(kind)` = per-variant
    /// via `on_variant`). T8's `emit` filters on this before dispatch.
    handlers: Mutex<Vec<RegisteredHandler>>,
    /// Bound at `bind_core` — the flushed around-dispatch handlers
    /// (tools/execute), in registration order (outermost first). The
    /// engine snapshots them per tool phase via
    /// [`around_tool_handlers`](Self::around_tool_handlers) and composes
    /// the `next()` chain; `clear_around_tools` / reload drop them.
    around_tools: Mutex<Vec<Arc<dyn AroundToolHandler>>>,
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
    /// Route B (skills) — mod-registered skills in registration order.
    /// `clear_skills` / reload drop the generation's set. Hosts read via
    /// [`Self::registered_skills`] at catalogue render / `load_skill`
    /// snapshot time.
    skills: Mutex<Vec<codesmith_agent::extension::SkillRegistration>>,
    /// Session log folds — mod-registered message projections. The hub is
    /// runner-created but host-shared: the engine folds transcript
    /// mutations through this `Arc` (`fold_message` / `refold_all`),
    /// `projection_state(key)` reads from inside mod scripts, and
    /// `clear_message_projections` / reload drop the generation's entries
    /// (the engine refolds from the transcript on the next turn).
    message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc,
    /// Resolved data policies (kind → [`codesmith_agent::policy::ResolvedPolicy`]),
    /// last-wins by registration order at flush; published to the bound
    /// engine-scoped store (`policy_sink` below, source-stripped) for the
    /// host's mechanism code to read. The source ids stay in this map
    /// only — the resolved-policy observability view reads it.
    /// Deliberately NOT first-wins: last-wins is what makes a user mod
    /// override a builtin policy (load order: builtin < global < project).
    policies: Mutex<
        HashMap<codesmith_agent::policy::PolicyKind, codesmith_agent::policy::ResolvedPolicy>,
    >,
    /// The store the flush publishes into and the reload clear empties
    /// — the engine's [`PolicyStore`] when a host binds one via
    /// [`bind_policy_store`](Self::bind_policy_store). Defaults to the
    /// process-default store, so a standalone runner (tests, embeds
    /// without an engine) keeps the pre-scoping process-wide behaviour.
    policy_sink: Mutex<codesmith_agent::policy::PolicyStore>,
    /// Per-mod lifecycle — retained script mods, in load order. Retention
    /// is what makes single-mod unload cheap: `unload_retained_mod` clears
    /// the bound registrations and replays the survivors from these slots
    /// (adapter re-creation against the retained compiled AST — no
    /// recompile). Only script mods are retained: dylib/compiled-in
    /// `configure` is factory-cheap and their code lifetime is governed by
    /// `libraries` / the inventory. Known limitation: a mod's KV store is
    /// file-backed and deliberately NOT evicted — it is the mod's opt-in
    /// persistent state, the unload reclaims only the compiled form.
    mod_slots: Mutex<Vec<Arc<crate::script::rhai_mod::RhaiMod>>>,
    /// Serializes per-mod transitions (`bind_mod` / `unload_retained_mod`)
    /// so two concurrent manage_mods calls cannot interleave
    /// clear→replay sequences. Tokio mutex (held across `configure`
    /// awaits); guards transitions only — never held across `emit` or
    /// tool dispatch.
    transition_lock: tokio::sync::Mutex<()>,
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
            tool_restrictions: Mutex::new(Vec::new()),
            commands: Mutex::new(HashMap::new()),
            disabled_builtin_commands: Mutex::new(HashSet::new()),
            handlers: Mutex::new(Vec::new()),
            around_tools: Mutex::new(Vec::new()),
            libraries: Mutex::new(Vec::new()),
            pending_drop: Mutex::new(Vec::new()),
            shared: Mutex::new(SharedProviderRegistry::new()),
            providers: Mutex::new(Vec::new()),
            prompt_sections: Mutex::new(Vec::new()),
            skills: Mutex::new(Vec::new()),
            message_projection_hub: codesmith_agent::extension::MessageProjectionHubArc::new(
                codesmith_agent::extension::MessageProjectionHub::new(),
            ),
            policies: Mutex::new(HashMap::new()),
            policy_sink: Mutex::new(codesmith_agent::policy::PolicyStore::process_default()),
            mod_slots: Mutex::new(Vec::new()),
            transition_lock: tokio::sync::Mutex::new(()),
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
    /// Called by `reload` / session-replace / fork / switch. The bump runs
    /// under the `pending` queue lock so the stub api's lock-held
    /// check-generation-then-enqueue step is atomic against it — a stale
    /// registration can never be checked live and then queued.
    pub fn invalidate(&self) {
        let _guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Clear all bound handlers (§F2b T7 live reload). Called by
    /// [`reload_extension_runtime`](crate::reload_extension_runtime) BEFORE
    /// re-discovery so re-binding wouldn't duplicate handlers — `bind_core`'s
    /// drain appends to `handlers`, it doesn't replace. A runtime lifecycle
    /// method; does NOT change the §F2a contract (`HandlerOutcome`/`EmitOutcome`
    /// /`on_variant`/`catch_unwind` stay stable).
    pub fn clear_handlers(&self) {
        self.handlers
            .lock()
            .expect("handlers lock poisoned")
            .clear();
    }

    /// Clear the generation's around-dispatch registrations
    /// (tools/execute). Same reload lifecycle as [`clear_handlers`]; safe
    /// while an in-flight turn holds a snapshot — each handler is
    /// refcounted (`Arc`) and the engine composed its chain at dispatch
    /// time.
    pub fn clear_around_tools(&self) {
        self.around_tools
            .lock()
            .expect("around-tools lock poisoned")
            .clear();
    }

    /// Snapshot the bound around-dispatch handlers in registration order
    /// (outermost first). The engine reads this once per tool phase; an
    /// empty snapshot leaves the dispatch path untouched.
    #[must_use]
    pub fn around_tool_handlers(&self) -> Vec<Arc<dyn AroundToolHandler>> {
        self.around_tools
            .lock()
            .expect("around-tools lock poisoned")
            .clone()
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

    /// Tool restriction — append a deny-only name list for `scope`
    /// (live path; the `bind_core` flush routes queued entries through
    /// here so stub-time and flush-time validation agree). Entries
    /// intersect; nothing re-admits a denied name. Fails loud on a blank
    /// scope id or a blank tool name.
    pub fn restrict_tools(&self, scope: Scope, names: Vec<String>) -> Result<(), ExtensionError> {
        validate_tool_restriction(&scope, &names)?;
        let denied: Vec<String> = names.into_iter().map(|n| n.trim().to_string()).collect();
        self.tool_restrictions
            .lock()
            .expect("tool restrictions lock poisoned")
            .push(ToolRestriction { scope, denied });
        Ok(())
    }

    /// Clear the generation's tool restrictions (reload drops them with
    /// the other bound registrations; surviving mods' replays re-derive
    /// the set).
    pub fn clear_tool_restrictions(&self) {
        self.tool_restrictions
            .lock()
            .expect("tool restrictions lock poisoned")
            .clear();
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

    /// Clear the generation's builtin-command disables. Called with the
    /// other bound registrations on reload; the surviving mods' replays
    /// re-derive the set, so a disabled-then-unloaded mod stops disabling.
    pub fn clear_disabled_commands(&self) {
        self.disabled_builtin_commands
            .lock()
            .expect("disabled commands lock poisoned")
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
        // Re-registering an id replaces its text in place (id is identity) —
        // an in-place replacement does not grow the collection, so the cap
        // only applies when a new entry would actually be added.
        if let Some(slot) = sections.iter_mut().find(|(existing, _)| *existing == id) {
            slot.1 = text;
        } else {
            if sections.len() >= MAX_PROMPT_SECTIONS {
                return Err(ExtensionError::Config(format!(
                    "too many prompt sections (>{MAX_PROMPT_SECTIONS})"
                )));
            }
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

    /// Route B (skills) — register (or, for the same owner+name, replace in
    /// place) a mod-contributed skill. Fails loud at registration on a bad
    /// name/description/body, on exceeding [`MAX_REGISTERED_SKILLS`], or
    /// when the name is already registered by a *different* owner (the
    /// error names the incumbent so the mod author knows who owns the key).
    pub fn register_skill(
        &self,
        skill: codesmith_agent::extension::SkillRegistration,
    ) -> Result<(), ExtensionError> {
        validate_skill_registration(&skill.name, &skill.description, &skill.body)?;
        let mut skills = self.skills.lock().expect("registered skills poisoned");
        if let Some(existing) = skills
            .iter()
            .find(|s| s.name == skill.name && s.owner != skill.owner)
        {
            return Err(ExtensionError::Config(format!(
                "skill {:?} already registered by mod {:?}",
                skill.name, existing.owner
            )));
        }
        // Same owner re-registering replaces in place (owner+name is
        // identity); otherwise append in registration order. The cap only
        // applies when a new entry would actually be added.
        if let Some(slot) = skills.iter_mut().find(|s| s.name == skill.name) {
            *slot = skill;
        } else {
            if skills.len() >= codesmith_agent::extension::MAX_REGISTERED_SKILLS {
                return Err(ExtensionError::Config(format!(
                    "too many registered skills (>{}), rejecting {:?} from mod {:?}",
                    codesmith_agent::extension::MAX_REGISTERED_SKILLS,
                    skill.name,
                    skill.owner
                )));
            }
            skills.push(skill);
        }
        Ok(())
    }

    /// Route B (skills) — snapshot the generation's registered skills
    /// (registration order). Hosts convert + merge these into the
    /// filesystem skill catalogue at render points.
    #[must_use]
    pub fn registered_skills(&self) -> Vec<codesmith_agent::extension::SkillRegistration> {
        self.skills
            .lock()
            .expect("registered skills poisoned")
            .clone()
    }

    /// Route B (skills) — clear all registered skills (reload drops the
    /// generation's contributions).
    pub fn clear_skills(&self) {
        self.skills
            .lock()
            .expect("registered skills poisoned")
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
        let drained = {
            let mut libs = self.libraries.lock().expect("libraries lock poisoned");
            std::mem::take(&mut *libs)
        };
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

    // === Per-mod lifecycle (single-mod load/unload, manual boundary) =======

    /// The bound-registration clear suite the whole-layer reload spells
    /// out: handlers, tools, tool restrictions, commands, providers,
    /// prompt sections, skills, message projections. NOT the dylib
    /// two-phase unload — that stays
    /// [`drain_libraries_to_pending`](Self::drain_libraries_to_pending) +
    /// [`drop_pending`](Self::drop_pending).
    pub fn clear_bound_registrations(&self) {
        self.clear_handlers();
        self.clear_around_tools();
        self.clear_tools();
        self.clear_tool_restrictions();
        self.clear_commands();
        self.clear_disabled_commands();
        self.clear_providers();
        self.clear_prompt_sections();
        self.clear_skills();
        self.clear_message_projections();
        self.clear_policies();
    }

    /// Point the flush/clear at `store` — the engine host binds the
    /// session's [`PolicyStore`](codesmith_agent::policy::PolicyStore) so
    /// this runner's resolved set lands in that engine's scope (the
    /// un-bound default is the process-default store). Call before the
    /// first bind/flush of a generation.
    pub fn bind_policy_store(&self, store: codesmith_agent::policy::PolicyStore) {
        *self.policy_sink.lock().expect("policy sink lock poisoned") = store;
    }

    /// Data policies — clear the runner's resolved set AND the bound
    /// store (the next flush republishes; mechanism code reads compiled
    /// defaults in between).
    pub fn clear_policies(&self) {
        self.policies
            .lock()
            .expect("policies lock poisoned")
            .clear();
        self.policy_sink
            .lock()
            .expect("policy sink lock poisoned")
            .clear();
    }

    /// Retain a loaded script mod for per-mod replay. Same-id retain
    /// replaces in place (idempotent against a host double-load);
    /// otherwise append, preserving load order — the replay flush follows
    /// that order, so first-wins name conflicts keep their original
    /// winners.
    pub fn retain_mod(&self, m: Arc<crate::script::rhai_mod::RhaiMod>) {
        let mut slots = self.mod_slots.lock().expect("mod slots lock poisoned");
        match slots
            .iter()
            .position(|s| s.metadata().id == m.metadata().id)
        {
            Some(i) => slots[i] = m,
            None => slots.push(m),
        }
    }

    /// Drop `id`'s retained compiled form (bound registrations untouched —
    /// pair with [`unload_retained_mod`](Self::unload_retained_mod)).
    /// True when a slot existed. Once its bound adapters are cleared, the
    /// slot was the last owner of the mod's compiled `Engine`/`AST`, so
    /// dropping it is the whole unload.
    pub fn evict_mod(&self, id: &str) -> bool {
        let mut slots = self.mod_slots.lock().expect("mod slots lock poisoned");
        let Some(i) = slots.iter().position(|s| s.metadata().id == id) else {
            return false;
        };
        slots.remove(i);
        true
    }

    /// Ids of retained script mods, in load order.
    #[must_use]
    pub fn retained_mod_ids(&self) -> Vec<String> {
        self.mod_slots
            .lock()
            .expect("mod slots lock poisoned")
            .iter()
            .map(|m| m.metadata().id.to_string())
            .collect()
    }

    /// Drop every retained slot — the whole-layer reload path calls this
    /// before repopulating (fresh compiles re-retain from disk).
    pub fn clear_mod_slots(&self) {
        self.mod_slots
            .lock()
            .expect("mod slots lock poisoned")
            .clear();
    }

    /// Bind one freshly compiled script mod: `configure` (captures
    /// registrations) → flush → retain → generation bump. The registration
    /// set change takes effect for future turns exactly like a reload (the
    /// per-turn dispatcher rebuild + tools-change diff observe it at the
    /// turn boundary). Returns the flush rejections (the same surface
    /// `bind_core` reports); `Err` when the runtime has no bound context
    /// (embeds/tests that skipped the extension layer — fail loud, the
    /// mod is NOT retained half-bound).
    pub async fn bind_mod(
        &self,
        m: Arc<crate::script::rhai_mod::RhaiMod>,
    ) -> Result<Vec<ExtensionError>, ExtensionError> {
        let _guard = self.transition_lock.lock().await;
        self.load_as(&*m, RegistrationOrigin::Script).await?;
        let rejections = self.flush_pending()?;
        self.retain_mod(m);
        self.invalidate();
        Ok(rejections)
    }

    /// Unload one retained script mod without recompiling its peers: evict
    /// the slot, clear every bound registration, replay the survivors'
    /// registrations against their retained compiled ASTs, bump the
    /// generation once. `evicted: false` when `id` has no retained slot
    /// (nothing was live under that id — a no-op). `errors` carries
    /// survivor replay failures and the unbound-context error; the evict
    /// itself always completes — the slot drop is the no-residue unload
    /// guarantee, independent of the survivors' health.
    pub async fn unload_retained_mod(&self, id: &str) -> UnloadModOutcome {
        let _guard = self.transition_lock.lock().await;
        if !self.evict_mod(id) {
            return UnloadModOutcome {
                evicted: false,
                replayed: 0,
                errors: Vec::new(),
            };
        }
        self.clear_bound_registrations();
        let slots: Vec<_> = self
            .mod_slots
            .lock()
            .expect("mod slots lock poisoned")
            .clone();
        let mut errors = Vec::new();
        let mut replayed = 0usize;
        for m in slots {
            let mid = m.metadata().id.to_string();
            match self.load_as(&*m, RegistrationOrigin::Script).await {
                Ok(()) => replayed += 1,
                Err(e) => errors.push(format!("re-register mod {mid}: {e}")),
            }
        }
        if let Err(e) = self.flush_pending() {
            errors.push(e.to_string());
        }
        self.invalidate();
        UnloadModOutcome {
            evicted: true,
            replayed,
            errors,
        }
    }

    /// Load + configure one extension against a **stub** api, stamped as a
    /// compiled-in contribution (the `inventory` tier). Registrations
    /// queue into `pending_*`. Called by `build_extension_runtime` (Task 9)
    /// for each discovered extension, BEFORE `bind_core`.
    pub async fn load(&self, ext: &dyn Extension) -> Result<(), ExtensionError> {
        self.load_as(ext, RegistrationOrigin::CompiledIn).await
    }

    /// `load` with an explicit contribution origin (§F5d unload-safety
    /// tier): the script-mod paths stamp [`RegistrationOrigin::Script`],
    /// [`load_dylib`](Self::load_dylib) stamps
    /// [`RegistrationOrigin::Dylib`]. The stub api carries the stamp onto
    /// every queued registration, and dylib-origin *scoped* registrations
    /// are rejected outright (a scoped tool/handler exists to serve
    /// sub-agent registries and sub-agent-scoped dispatch — exactly the
    /// surfaces dylib contributions cannot reach; failing the load beats a
    /// registration that can never fire).
    pub async fn load_as(
        &self,
        ext: &dyn Extension,
        origin: RegistrationOrigin,
    ) -> Result<(), ExtensionError> {
        let shared = self
            .shared
            .lock()
            .expect("shared provider registry lock poisoned")
            .clone();
        let stub = StubExtensionApi::new(
            self.generation.clone(),
            self.pending.clone(),
            shared,
            origin,
        );
        ext.configure(&stub).await
    }

    /// §F5b — load a dylib extension (spec §F5b / §7.2). Opens the library,
    /// calls its `codesmith_register_extension`, pushes the `Library` into
    /// `libraries` (must outlive registered contributions' vtables; reload
    /// does not clear — Q1), then runs `configure` via
    /// [`load_as`](Self::load_as) stamped
    /// [`RegistrationOrigin::Dylib`] — the unload-safety tier that keeps
    /// dylib contributions main-turn-only. The `Extension` Box is dropped
    /// after `configure` (registered contributions are self-contained
    /// owned trait objects; vtables live in the kept `Library`). Lockstep
    /// (§8.2) assumed. Mirrors the static `load` path.
    pub async fn load_dylib(&self, path: &Path) -> Result<(), ExtensionError> {
        let mut loaded = crate::loader::load_dylib(path)?;
        let extension = loaded.take_extension();
        self.libraries
            .lock()
            .expect("libraries lock poisoned")
            .push(loaded.into_library());
        match extension {
            Some(extension) => self.load_as(&*extension, RegistrationOrigin::Dylib).await,
            None => Ok(()),
        }
    }

    /// Bind the host context + flush `pending_*` into the live registries.
    /// After this, [`emit`](Self::emit) / [`try_dispatch_command`](Self::try_dispatch_command)
    /// are live. The stub→real swap (spec §4) happens here: any later
    /// `register_*` via a captured stub `Arc<dyn ExtensionApi>` would read
    /// a stale generation + return `StaleContext` (slice 1: stubs are
    /// short-lived — dropped after `load` — so this is a §F2 concern; the
    /// generation guard is the stable contract).
    ///
    /// Flush-time rejections (prompt sections at cap, skills at cap or with
    /// a cross-owner conflict, message projections) are logged and returned
    /// so the host can surface them.
    pub fn bind_core(&self, context: Arc<dyn ExtensionCommandContext>) -> Vec<ExtensionError> {
        *self.context.lock().unwrap() = Some(context);
        // The context was just set, so the unbound error is unreachable in
        // practice — surface it anyway rather than discard it.
        self.flush_pending().unwrap_or_else(|e| vec![e])
    }

    /// `bind_core` minus the context swap: drain `pending` into the live
    /// registries against the already-bound context. The per-mod lifecycle
    /// paths (`bind_mod` / `unload_retained_mod`) re-flush through this
    /// after replaying a subset of extensions; `Err` when the runtime was
    /// never bound (embeds/tests that skipped the extension layer).
    fn flush_pending(&self) -> Result<Vec<ExtensionError>, ExtensionError> {
        if self.context.lock().unwrap().is_none() {
            return Err(ExtensionError::Config(
                "extension runtime has no bound context — per-mod transitions require a bound session"
                    .to_string(),
            ));
        }
        // Drain `pending` under its own lock; each registry is flushed below
        // while holding at most that one registry's lock (never across the
        // `shared` registry's lock — see `clear_providers`).
        let drained = {
            let mut pending = self.pending.lock().unwrap();
            std::mem::take(&mut *pending)
        };
        let mut rejections = Vec::new();
        for pt in drained.tools {
            let (scope, origin) = (pt.scope, pt.origin);
            let name = pt.tool.name().to_string();
            let bound = BoundTool {
                def: Arc::from(pt.tool),
                origin,
            };
            // First-wins per (scope, name), matching the documented
            // slice-1 conflict policy: plain `insert` replaces and
            // silently yields last-wins.
            self.tools
                .lock()
                .unwrap()
                .entry((scope, name))
                .or_insert(bound);
        }
        for restriction in drained.tool_restrictions {
            if let Err(e) = self.restrict_tools(restriction.scope, restriction.denied) {
                tracing::warn!(
                    target: "codesmith_extensions",
                    "tool restriction rejected at bind: {e}"
                );
                rejections.push(e);
            }
        }
        for pc in drained.commands {
            let name = pc.command.name().to_string();
            let arc: Arc<dyn CommandDefinition> = Arc::from(pc.command);
            self.commands.lock().unwrap().entry(name).or_insert(arc);
        }
        {
            let mut disabled = self
                .disabled_builtin_commands
                .lock()
                .expect("disabled commands lock poisoned");
            for name in drained.disabled_commands {
                disabled.insert(name);
            }
        }
        {
            let mut handlers = self.handlers.lock().unwrap();
            for ph in drained.handlers {
                handlers.push(RegisteredHandler {
                    handler: ph.handler,
                    kind_filter: ph.kind_filter,
                    scope: ph.scope,
                    origin: ph.origin,
                });
            }
        }
        {
            let mut around = self
                .around_tools
                .lock()
                .expect("around-tools lock poisoned");
            around.extend(drained.around_tools);
        }
        for (id, text) in drained.prompt_sections {
            if let Err(e) = self.register_prompt_section(id, text) {
                tracing::warn!(
                    target: "codesmith_extensions",
                    "prompt section rejected at bind: {e}"
                );
                rejections.push(e);
            }
        }
        for skill in drained.skills {
            if let Err(e) = self.register_skill(skill) {
                tracing::warn!(
                    target: "codesmith_extensions",
                    "registered skill rejected at bind: {e}"
                );
                rejections.push(e);
            }
        }
        for mp in drained.message_projections {
            if let Err(e) = self
                .message_projection_hub
                .register(&mp.owner, &mp.key, mp.init, mp.fold)
            {
                tracing::warn!(
                    target: "codesmith_extensions",
                    "message projection rejected at bind: {e}"
                );
                rejections.push(ExtensionError::Config(e));
            }
        }
        // Data policies: last-wins by registration (drain) order — the
        // load order builtin → global → project resolves overrides before
        // the values are published. The bound store gets the
        // source-stripped kind → config half; the source ids stay in the
        // runner map for the resolved-policy view.
        {
            let mut policies = self.policies.lock().expect("policies lock poisoned");
            for entry in drained.policies {
                policies.insert(entry.kind, entry);
            }
            self.policy_sink
                .lock()
                .expect("policy sink lock poisoned")
                .publish(
                    policies
                        .iter()
                        .map(|(kind, entry)| (*kind, entry.config.clone()))
                        .collect(),
                );
        }
        for pp in drained.providers {
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
            self.providers.lock().unwrap().push(guard);
        }
        Ok(rejections)
    }

    /// Cheap pre-check for hot-path emitters: whether any bound handler
    /// would receive `kind` at the **Global** scope (the main turn's
    /// dispatch scope; `None` kind_filter = subscribe-to-all, so it
    /// counts). Agent-type-scoped handlers do not count — they receive
    /// only `emit_scoped` dispatches at their own scope, never the main
    /// turn's events. Racy with reload by design — it mirrors the
    /// snapshot `emit` itself takes under the same lock and is only ever
    /// used to skip dispatch, never to change emit semantics.
    #[must_use]
    pub fn has_handlers(&self, kind: ExtensionEventKind) -> bool {
        self.handlers.lock().unwrap().iter().any(|rh| {
            rh.scope.is_global() && (rh.kind_filter.is_none() || rh.kind_filter == Some(kind))
        })
    }

    /// Emit `event` to every bound handler whose variant filter matches,
    /// at the **Global** scope — the main turn's dispatch entry point.
    /// Agent-type-scoped handlers never receive these emissions (see
    /// [`emit_scoped`](Self::emit_scoped)). Behavior otherwise as
    /// documented on `emit_scoped`.
    pub async fn emit(&self, event: ExtensionEvent) -> EmitOutcome {
        self.emit_scoped(Scope::Global, event).await
    }

    /// Emit `event` at an explicit registration scope (per-agent
    /// dispatch): handlers registered at `Global` always receive the
    /// event; a handler registered at `AgentType(id)` receives it only
    /// when `scope` is that same type. The main-turn sites call
    /// [`emit`](Self::emit) (Global); a sub-agent loop dispatches with
    /// its own agent type. At agent-type scopes, dylib-origin handlers
    /// are skipped (§F5d unload safety: a detached background child can
    /// outlive a reload's two-phase `Library` drop, so dylib code never
    /// runs from a sub-agent dispatch).
    ///
    /// Chains transforms (spec §4: "一个 handler 的修改对下一个可见").
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
    pub async fn emit_scoped(&self, scope: Scope, event: ExtensionEvent) -> EmitOutcome {
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
        let dylib_excluded = !scope.is_global();
        let matching: Vec<RegisteredHandler> = self
            .handlers
            .lock()
            .unwrap()
            .iter()
            .filter(|rh| !(dylib_excluded && rh.origin.is_dylib()))
            .filter(|rh| rh.scope.covers(&scope))
            .filter(|rh| rh.kind_filter.is_none() || rh.kind_filter == Some(kind))
            .cloned()
            .collect();
        let mut event = event;
        let mut outcome = HandlerOutcome::Continue;
        for rh in matching {
            // The variant filter binds to the RUNNING event kind: a handler
            // earlier in the chain may have `Transform`ed the event to a
            // different kind, which must not still reach subscribers of the
            // pre-chain kind.
            if rh.kind_filter.is_some_and(|filter| filter != event.kind()) {
                continue;
            }
            let h = rh.handler;
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
                // Cancel / Block short-circuit the chain. Ask does too —
                // the host honors it only at the ToolCall seam (approval
                // routing); at any other seam it is ignored like an
                // out-of-place Block, but the chain still stops.
                Ok(Ok(a @ HandlerOutcome::Ask { .. })) => {
                    outcome = a;
                    break;
                }
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
    /// by the CLI `extension_commands::try_dispatch` (Task 8).
    pub async fn try_dispatch_command(&self, name: &str, args: &str) -> Option<CommandOutput> {
        let cmd = self.commands.lock().unwrap().get(name).cloned()?;
        let ctx = self.context.lock().unwrap().clone()?;
        match cmd.run(&*ctx, args).await {
            Ok(output) => Some(output),
            Err(err) => {
                tracing::error!(
                    target: "codesmith_extensions::runner",
                    "extension command {name:?} failed: {err}"
                );
                None
            }
        }
    }

    /// Snapshot of the **Global** extension-tool view (name → def, sorted
    /// by name) for the host's `ExtensionToolSpecAdapter` to wrap +
    /// register into `ToolRegistry` (Task 5/9). This is what the main
    /// turn's `register_extension_tools` mounts: agent-type-scoped tools
    /// never appear here, and a `Global` restrict entry prunes its names.
    pub fn bound_tools(&self) -> Vec<(String, Arc<dyn ToolDefinition>)> {
        self.bound_tools_scoped(&Scope::Global)
    }

    /// The extension-tool view for `scope`:
    /// the `Global` registrations — minus every restriction whose scope
    /// covers `scope` — overlaid by `scope`'s own registrations when it is
    /// an agent type (same name ⇒ the scoped tool shadows the global
    /// one). Sorted by name. Restrictions never prune scoped
    /// registrations (what an agent type declares for itself cannot be
    /// voted away by a deployment restriction), and they apply only to
    /// the extension layer's own tools — builtins are not the extension
    /// layer's to prune.
    #[must_use]
    pub fn bound_tools_scoped(&self, scope: &Scope) -> Vec<(String, Arc<dyn ToolDefinition>)> {
        self.tool_view(scope, true)
    }

    /// The extension-tool view a **sub-agent registry** may mount (5b):
    /// [`bound_tools_scoped`](Self::bound_tools_scoped) for `scope` minus
    /// dylib-origin tools. A detached background sub-agent (an
    /// `agent_open` session) can outlive a reload's two-phase `Library`
    /// drop — §F5d T4's safety argument covers the main-turn executor,
    /// the only in-flight dylib `Arc` holder between turns — so dylib
    /// contributions stay main-turn-only; script/compiled-in
    /// contributions (statically linked code, retained script ASTs)
    /// mount safely. A dylib cannot even register scoped tools (the
    /// stub rejects them at load), so the filter here only ever drops
    /// *global* dylib tools out of the agent-type view.
    #[must_use]
    pub fn mountable_tools_for_subagents(
        &self,
        scope: &Scope,
    ) -> Vec<(String, Arc<dyn ToolDefinition>)> {
        self.tool_view(scope, false)
    }

    /// Shared view derivation: the global layer (minus covering
    /// restrictions, optionally minus dylib-origin tools) overlaid by
    /// `scope`'s own registrations when it is an agent type. Sorted by
    /// name.
    fn tool_view(
        &self,
        scope: &Scope,
        include_dylib: bool,
    ) -> Vec<(String, Arc<dyn ToolDefinition>)> {
        let denied: HashSet<String> = self
            .tool_restrictions
            .lock()
            .expect("tool restrictions lock poisoned")
            .iter()
            .filter(|r| r.scope.covers(scope))
            .flat_map(|r| r.denied.iter().cloned())
            .collect();
        let tools = self.tools.lock().expect("tools lock poisoned");
        let admits = |tool: &BoundTool| include_dylib || !tool.origin.is_dylib();
        let mut view: HashMap<String, Arc<dyn ToolDefinition>> = HashMap::new();
        // Global layer first (restrictions prune it); the scope's own
        // registrations overlay it when the view is an agent type — the
        // Global view has no overlay of its own (its layer IS the base,
        // and re-inserting it would bypass the restriction filter).
        for ((tool_scope, name), tool) in tools.iter() {
            if tool_scope.is_global() && admits(tool) && !denied.contains(name) {
                view.insert(name.clone(), Arc::clone(&tool.def));
            }
        }
        if let Scope::AgentType(_) = scope {
            for ((tool_scope, name), tool) in tools.iter() {
                if tool_scope == scope && admits(tool) {
                    view.insert(name.clone(), Arc::clone(&tool.def));
                }
            }
        }
        let mut out: Vec<(String, Arc<dyn ToolDefinition>)> = view.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
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

    /// The resolved config for one policy kind (the flush-time winner).
    /// Hosts normally read through the ambient policy store
    /// ([`codesmith_agent::policy::published_policy`]) instead — this is
    /// the introspection/testing view.
    #[must_use]
    pub fn bound_policy(
        &self,
        kind: codesmith_agent::policy::PolicyKind,
    ) -> Option<serde_json::Value> {
        self.policies
            .lock()
            .expect("policies lock poisoned")
            .get(&kind)
            .map(|entry| entry.config.clone())
    }

    /// Every resolved policy (the flush-time winner per kind), in
    /// [`codesmith_agent::policy::PolicyKind::ALL`] order — the
    /// observability view behind `manage_mods list` / `/mods list`
    /// (kind → effective config → winning mod id). Read-only; the change
    /// surface stays mod load/unload. Known limitation: reads this
    /// runner's map only — runner-less embeds (the host's `ensure_seeded`
    /// path) resolve nothing here.
    #[must_use]
    pub fn resolved_policies(&self) -> Vec<codesmith_agent::policy::ResolvedPolicy> {
        let policies = self.policies.lock().expect("policies lock poisoned");
        codesmith_agent::policy::PolicyKind::ALL
            .iter()
            .filter_map(|kind| policies.get(kind).cloned())
            .collect()
    }

    /// Names of bound commands (for `/extension list`, Task 8).
    pub fn bound_command_names(&self) -> Vec<String> {
        self.commands.lock().unwrap().keys().cloned().collect()
    }

    /// Whether a builtin command name is disabled by a bound extension
    /// registration. Unknown names simply never match — the host resolves
    /// them against its builtin table and warns at populate.
    #[must_use]
    pub fn is_builtin_command_disabled(&self, name: &str) -> bool {
        self.disabled_builtin_commands
            .lock()
            .expect("disabled commands lock poisoned")
            .contains(name)
    }

    /// The builtin command names disabled by bound extensions (host-side
    /// validation + observability).
    pub fn disabled_builtin_commands(&self) -> Vec<String> {
        self.disabled_builtin_commands
            .lock()
            .expect("disabled commands lock poisoned")
            .iter()
            .cloned()
            .collect()
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
        let mods = self
            .mod_slots
            .lock()
            .expect("mod slots mutex poisoned")
            .len();
        f.debug_struct("ExtensionRunner")
            .field("generation", &self.generation.load(Ordering::Acquire))
            .field("bound", &bound)
            .field("tools", &tools)
            .field("commands", &commands)
            .field("handlers", &handlers)
            .field("libraries", &libraries)
            .field("pending_drop", &pending_drop)
            .field("providers", &providers)
            .field("mods", &mods)
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
            ExtensionMode::Interactive
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
            if let ExtensionEvent::ToolResult(tr) = event
                && let Ok(r) = &tr.result
            {
                self.seen.lock().unwrap().push(r.content.clone());
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

    /// A `Transform` that switches the event kind stops delivery to handlers
    /// subscribed to the pre-chain kind: the variant filter is re-checked
    /// against the running event before each handler call.
    #[tokio::test]
    async fn f2a_transform_to_other_kind_skips_variant_subscriber() {
        struct TransformToolCallToTurnStart;
        #[async_trait::async_trait]
        impl Handler for TransformToolCallToTurnStart {
            async fn handle(
                &self,
                event: &ExtensionEvent,
                _ctx: &dyn ExtensionContext,
            ) -> Result<HandlerOutcome, ExtensionError> {
                if matches!(event, ExtensionEvent::ToolCall(_)) {
                    Ok(HandlerOutcome::Transform(ExtensionEvent::TurnStart {
                        turn_id: "t1".into(),
                    }))
                } else {
                    Ok(HandlerOutcome::Continue)
                }
            }
        }

        struct TransformKindExt {
            seen: Arc<Mutex<Vec<&'static str>>>,
        }
        #[async_trait::async_trait]
        impl Extension for TransformKindExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("transform-kind");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.on(Arc::new(TransformToolCallToTurnStart))?;
                api.on_variant(
                    ExtensionEventKind::ToolCall,
                    Arc::new(RecHandler {
                        seen: self.seen.clone(),
                    }),
                )?;
                Ok(())
            }
        }

        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner
            .load(&TransformKindExt { seen: seen.clone() })
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
        assert!(
            seen.lock().unwrap().is_empty(),
            "variant subscriber must not see the transformed kind"
        );
        assert!(matches!(out.event, ExtensionEvent::TurnStart { .. }));
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

    // === disable_command (builtin suppression) ==============================

    /// The stub→flush→clear round-trip for `disable_command`: queued at
    /// `configure`, live after `bind_core`, cleared with the generation's
    /// other bound registrations (a surviving mod's replay re-adds its own).
    #[test]
    fn disable_command_flushes_and_clears() {
        let runner = crate::ExtensionRunner::new();
        let stub = crate::api::StubExtensionApi::new(
            runner.generation_arc(),
            runner.pending.clone(),
            codesmith_agent::provider::SharedProviderRegistry::new(),
            RegistrationOrigin::CompiledIn,
        );
        stub.disable_command("model".into()).expect("queue");
        assert!(
            !runner.is_builtin_command_disabled("model"),
            "not live before bind_core"
        );
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        assert!(runner.is_builtin_command_disabled("model"));
        runner.clear_disabled_commands();
        assert!(
            !runner.is_builtin_command_disabled("model"),
            "cleared with the generation's registrations"
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

    #[test]
    fn try_dispatch_command_runs_bound_command() {
        // The REPL's mod-command layer dispatches through this path: a
        // bound command runs + returns its output; an unknown name is
        // `None` so the caller falls through to the built-in table.
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&CmdExt)).expect("load CmdExt");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));

        let out = rt
            .block_on(runner.try_dispatch_command("clear_cmd_test", "hi"))
            .expect("bound command dispatches");
        match out {
            CommandOutput::Message(m) => assert_eq!(m, "echo:hi"),
            other => panic!("expected Message, got {other:?}"),
        }
        assert!(
            rt.block_on(runner.try_dispatch_command("no-such", ""))
                .is_none(),
            "unknown command falls through as None"
        );
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
    fn bind_core_returns_flush_rejections() {
        struct OverflowSections;
        #[async_trait::async_trait]
        impl Extension for OverflowSections {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("overflow-sections");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                // The stub validates the section shape but not the cap, so
                // one section too many is first rejected at the flush.
                for i in 0..=MAX_PROMPT_SECTIONS {
                    api.register_prompt_section(format!("s{i}"), "x".into())?;
                }
                Ok(())
            }
        }
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&OverflowSections)).expect("load");
        let rejections = runner.bind_core(Arc::new(Ctx { generation: 1 }));
        assert_eq!(rejections.len(), 1, "overflow surfaced to the host");
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

    fn skill_reg(owner: &str, name: &str) -> codesmith_agent::extension::SkillRegistration {
        codesmith_agent::extension::SkillRegistration {
            owner: owner.to_string(),
            name: name.to_string(),
            description: "does one thing".to_string(),
            body: "# Steps\n1. Do the thing.".to_string(),
            when_to_use: None,
        }
    }

    #[test]
    fn skills_register_replace_and_clear() {
        let runner = ExtensionRunner::new();
        assert!(runner.registered_skills().is_empty());

        runner
            .register_skill(skill_reg("alpha", "commit-helper"))
            .unwrap();
        runner
            .register_skill(skill_reg("beta", "release-notes"))
            .unwrap();
        // Same owner re-registering replaces in place, not appends.
        let mut updated = skill_reg("alpha", "commit-helper");
        updated.body = "# Steps\n1. Updated.".to_string();
        runner.register_skill(updated).unwrap();

        let snapshot = runner.registered_skills();
        assert_eq!(snapshot.len(), 2, "replace, not append");
        let helper = snapshot.iter().find(|s| s.name == "commit-helper").unwrap();
        assert_eq!(helper.body, "# Steps\n1. Updated.");
        assert_eq!(helper.owner, "alpha");

        runner.clear_skills();
        assert!(runner.registered_skills().is_empty(), "cleared");
    }

    #[test]
    fn skills_validation_cap_and_cross_owner_reject() {
        let runner = ExtensionRunner::new();
        let mut bad_name = skill_reg("alpha", "bad name!");
        assert!(runner.register_skill(bad_name.clone()).is_err());
        bad_name.name = "ok".into();
        bad_name.description = "  ".into();
        assert!(runner.register_skill(bad_name).is_err());
        let mut empty_body = skill_reg("alpha", "ok2");
        empty_body.body = String::new();
        assert!(runner.register_skill(empty_body).is_err());

        runner.register_skill(skill_reg("alpha", "shared")).unwrap();
        let err = runner
            .register_skill(skill_reg("beta", "shared"))
            .expect_err("cross-owner name is rejected");
        assert!(
            err.to_string().contains("alpha"),
            "error names the incumbent owner: {err}"
        );

        for i in 0..codesmith_agent::extension::MAX_REGISTERED_SKILLS - 1 {
            runner
                .register_skill(skill_reg("filler", &format!("s{i}")))
                .unwrap();
        }
        assert!(
            runner
                .register_skill(skill_reg("filler", "overflow"))
                .is_err()
        );
    }

    #[test]
    fn skills_flush_at_bind_core() {
        struct SkillExt;
        #[async_trait::async_trait]
        impl Extension for SkillExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("skill-ext");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.register_skill(codesmith_agent::extension::SkillRegistration {
                    owner: "skill-ext".to_string(),
                    name: "inline-skill".to_string(),
                    description: "registered inline".to_string(),
                    body: "Do the thing.".to_string(),
                    when_to_use: Some("the thing comes up".to_string()),
                })?;
                Ok(())
            }
        }
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&SkillExt)).expect("load");
        // Two-phase: pending until bind_core flushes.
        assert!(runner.registered_skills().is_empty());
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let snapshot = runner.registered_skills();
        assert_eq!(snapshot.len(), 1, "flushed at bind_core");
        assert_eq!(snapshot[0].name, "inline-skill");
        assert_eq!(
            snapshot[0].when_to_use.as_deref(),
            Some("the thing comes up")
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

    // === Registration scope (5a): views, restrict, dispatch filter ========

    #[test]
    fn restrict_tools_validation_fails_loud() {
        let runner = ExtensionRunner::new();
        assert!(
            runner.restrict_tools(Scope::Global, Vec::new()).is_err(),
            "empty name list rejected"
        );
        assert!(
            runner
                .restrict_tools(Scope::Global, vec!["  ".into()])
                .is_err(),
            "blank names rejected"
        );
        assert!(
            runner
                .restrict_tools(Scope::AgentType("   ".into()), vec!["t".into()])
                .is_err(),
            "blank scope id rejected"
        );
        runner
            .restrict_tools(Scope::Global, vec!["web_fetch".into()])
            .expect("a usable restriction registers");
    }

    /// A tool whose description tags its origin, so scoped-shadow
    /// assertions can pin WHICH registration a view resolved.
    struct TaggedTool(&'static str, &'static str);
    #[async_trait::async_trait]
    impl ToolDefinition for TaggedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            self.1
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ToolResult, ExtensionError> {
            Ok(ToolResult::success(self.1))
        }
    }

    struct ScopedRegistrationExt;
    #[async_trait::async_trait]
    impl Extension for ScopedRegistrationExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("scoped-reg");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.register_tool(Box::new(TaggedTool("shared", "global-variant")))?;
            api.register_tool(Box::new(TaggedTool("pruned_everywhere", "global")))?;
            api.register_tool_scoped(
                Scope::AgentType("explore".into()),
                Box::new(TaggedTool("shared", "explore-variant")),
            )?;
            api.register_tool_scoped(
                Scope::AgentType("explore".into()),
                Box::new(TaggedTool("explore_only", "explore")),
            )?;
            api.restrict_tools(Scope::Global, vec!["pruned_everywhere".into()])?;
            Ok(())
        }
    }

    fn scoped_reg_runner() -> ExtensionRunner {
        let runner = ExtensionRunner::new();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&ScopedRegistrationExt))
            .expect("load");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        runner
    }

    #[test]
    fn scoped_tool_views_shadow_and_restrict_at_flush() {
        let runner = scoped_reg_runner();
        let names = |v: Vec<(String, Arc<dyn ToolDefinition>)>| -> Vec<String> {
            v.into_iter().map(|(n, _)| n).collect()
        };

        // Global view (main turn): scoped tools never leak in, the global
        // restriction prunes, and the global variant of `shared` survives.
        let global = names(runner.bound_tools());
        assert!(global.contains(&"shared".to_string()));
        assert!(!global.contains(&"explore_only".to_string()), "{global:?}");
        assert!(
            !global.contains(&"pruned_everywhere".to_string()),
            "global restriction prunes the global view: {global:?}"
        );
        let shared_global = runner
            .bound_tools()
            .into_iter()
            .find(|(n, _)| n == "shared")
            .unwrap();
        assert_eq!(shared_global.1.description(), "global-variant");

        // Explore view: scoped shadow wins the shared name, explore-only
        // tools appear, and the Global restriction covers this view too.
        let explore = runner.bound_tools_scoped(&Scope::AgentType("explore".to_string()));
        let explore_names = names(explore.clone());
        assert!(
            explore_names.contains(&"explore_only".to_string()),
            "{explore_names:?}"
        );
        assert!(
            !explore_names.contains(&"pruned_everywhere".to_string()),
            "a Global restriction covers every view: {explore_names:?}"
        );
        let shared_explore = explore.into_iter().find(|(n, _)| n == "shared").unwrap();
        assert_eq!(
            shared_explore.1.description(),
            "explore-variant",
            "the scoped registration shadows the global one in its own view"
        );

        // A different agent type sees the global view shape (no explore
        // tools, restriction still applies).
        let general = names(runner.bound_tools_scoped(&Scope::AgentType("general".to_string())));
        assert!(
            !general.contains(&"explore_only".to_string()),
            "{general:?}"
        );
        assert!(general.contains(&"shared".to_string()));

        // Reload clear drops scoped tools and restrictions together.
        runner.clear_bound_registrations();
        assert!(runner.bound_tools().is_empty());
        assert!(
            runner
                .bound_tools_scoped(&Scope::AgentType("explore".to_string()))
                .is_empty()
        );
    }

    struct ScopeProbeHandler {
        seen: Arc<Mutex<Vec<&'static str>>>,
        label: &'static str,
    }
    #[async_trait::async_trait]
    impl Handler for ScopeProbeHandler {
        async fn handle(
            &self,
            _event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<HandlerOutcome, ExtensionError> {
            self.seen.lock().unwrap().push(self.label);
            Ok(HandlerOutcome::Continue)
        }
    }

    #[test]
    fn emit_scope_filters_handler_dispatch() {
        struct ScopedHandlersExt {
            seen: Arc<Mutex<Vec<&'static str>>>,
        }
        #[async_trait::async_trait]
        impl Extension for ScopedHandlersExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("scoped-handlers");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.on_variant(
                    ExtensionEventKind::TurnStart,
                    Arc::new(ScopeProbeHandler {
                        seen: self.seen.clone(),
                        label: "global",
                    }),
                )?;
                api.on_variant_scoped(
                    ExtensionEventKind::TurnStart,
                    Scope::AgentType("explore".into()),
                    Arc::new(ScopeProbeHandler {
                        seen: self.seen.clone(),
                        label: "explore",
                    }),
                )?;
                Ok(())
            }
        }

        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&ScopedHandlersExt { seen: seen.clone() }))
            .expect("load");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        let turn_start = || ExtensionEvent::TurnStart {
            turn_id: "t1".into(),
        };

        // Main-turn emit (Global): only the global handler runs — an
        // agent-scoped handler never receives another scope's events.
        let _ = rt.block_on(runner.emit(turn_start()));
        assert_eq!(*seen.lock().unwrap(), vec!["global"]);

        // Scoped emit at the matching type: BOTH run (a global handler
        // covers every scope; the explore handler finally receives its
        // own scope's dispatch).
        let _ = rt.block_on(runner.emit_scoped(Scope::AgentType("explore".into()), turn_start()));
        assert_eq!(*seen.lock().unwrap(), vec!["global", "global", "explore"]);

        // Scoped emit at a NON-matching type: the explore handler stays
        // silent.
        let _ = rt.block_on(runner.emit_scoped(Scope::AgentType("general".into()), turn_start()));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["global", "global", "explore", "global"]
        );
    }

    #[test]
    fn has_handlers_counts_global_scope_only() {
        struct ScopedOnlyExt {
            seen: Arc<Mutex<Vec<&'static str>>>,
        }
        #[async_trait::async_trait]
        impl Extension for ScopedOnlyExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("scoped-only");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.on_variant_scoped(
                    ExtensionEventKind::TurnStart,
                    Scope::AgentType("explore".into()),
                    Arc::new(ScopeProbeHandler {
                        seen: self.seen.clone(),
                        label: "explore",
                    }),
                )?;
                Ok(())
            }
        }
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&ScopedOnlyExt { seen }))
            .expect("load");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));
        // The hot-path pre-check mirrors emit (Global): a scoped-only
        // subscription does not make the main turn dispatch.
        assert!(
            !runner.has_handlers(ExtensionEventKind::TurnStart),
            "scoped-only handlers do not count for the Global pre-check"
        );
    }

    // === 5b origin tiering (§F5d unload safety) ==========================

    #[test]
    fn dylib_scoped_registrations_are_rejected_at_load() {
        struct ScopedDylibExt;
        #[async_trait::async_trait]
        impl Extension for ScopedDylibExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("scoped-dylib");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.register_tool_scoped(
                    Scope::AgentType("explore".into()),
                    Box::new(TaggedTool("never_mounts", "dylib")),
                )?;
                Ok(())
            }
        }
        let runner = ExtensionRunner::new();
        let err = tokio::runtime::Runtime::new()
            .expect("rt")
            .block_on(runner.load_as(&ScopedDylibExt, RegistrationOrigin::Dylib))
            .expect_err("a dylib cannot register scoped tools");
        assert!(
            err.to_string().contains("dylib"),
            "the error names the tier: {err}"
        );

        struct ScopedDylibHandlerExt {
            seen: Arc<Mutex<Vec<&'static str>>>,
        }
        #[async_trait::async_trait]
        impl Extension for ScopedDylibHandlerExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("scoped-dylib-handler");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.on_variant_scoped(
                    ExtensionEventKind::TurnStart,
                    Scope::AgentType("explore".into()),
                    Arc::new(ScopeProbeHandler {
                        seen: self.seen.clone(),
                        label: "never_fires",
                    }),
                )?;
                Ok(())
            }
        }
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let err = tokio::runtime::Runtime::new()
            .expect("rt")
            .block_on(runner.load_as(&ScopedDylibHandlerExt { seen }, RegistrationOrigin::Dylib))
            .expect_err("a dylib cannot register scoped handlers");
        assert!(
            err.to_string().contains("dylib"),
            "the error names the tier: {err}"
        );
    }

    #[test]
    fn dylib_tools_and_handlers_stay_main_turn_only() {
        // One dylib-stamped ext (a Global tool + a Global handler) and one
        // compiled-in ext (same shapes) — the tier must keep only the
        // dylib half out of sub-agent surfaces.
        struct MixedExt {
            seen: Arc<Mutex<Vec<&'static str>>>,
            label: &'static str,
        }
        #[async_trait::async_trait]
        impl Extension for MixedExt {
            fn metadata(&self) -> &ExtensionMetadata {
                static M: ExtensionMetadata = ExtensionMetadata::new("mixed-tier");
                &M
            }
            async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
                api.register_tool(Box::new(TaggedTool(
                    Box::leak(format!("{}_tool", self.label).into_boxed_str()),
                    self.label,
                )))?;
                api.on_variant(
                    ExtensionEventKind::TurnStart,
                    Arc::new(ScopeProbeHandler {
                        seen: self.seen.clone(),
                        label: Box::leak(format!("{}_handler", self.label).into_boxed_str()),
                    }),
                )?;
                Ok(())
            }
        }
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&MixedExt {
            seen: seen.clone(),
            label: "compiled",
        }))
        .expect("load compiled-in");
        rt.block_on(runner.load_as(
            &MixedExt {
                seen: seen.clone(),
                label: "dylib",
            },
            RegistrationOrigin::Dylib,
        ))
        .expect("load dylib-stamped");
        runner.bind_core(Arc::new(Ctx { generation: 1 }));

        // Tools: the Global view carries both; the sub-agent-mountable
        // view drops the dylib one.
        let global: Vec<String> = runner.bound_tools().into_iter().map(|(n, _)| n).collect();
        assert!(global.contains(&"compiled_tool".to_string()));
        assert!(global.contains(&"dylib_tool".to_string()));
        let mountable: Vec<String> = runner
            .mountable_tools_for_subagents(&Scope::AgentType("general".to_string()))
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(
            mountable.contains(&"compiled_tool".to_string()),
            "{mountable:?}"
        );
        assert!(
            !mountable.contains(&"dylib_tool".to_string()),
            "dylib-origin tools never mount in sub-agent registries: {mountable:?}"
        );

        // Handlers: the main turn (Global emit) runs both; an agent-type
        // scoped emit runs only the compiled-in one.
        let turn_start = || ExtensionEvent::TurnStart {
            turn_id: "t1".into(),
        };
        let _ = rt.block_on(runner.emit(turn_start()));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["compiled_handler", "dylib_handler"]
        );
        let _ =
            rt.block_on(runner.emit_scoped(Scope::AgentType("general".to_string()), turn_start()));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["compiled_handler", "dylib_handler", "compiled_handler"],
            "the dylib-origin handler is skipped at agent-type scopes"
        );
    }
}
