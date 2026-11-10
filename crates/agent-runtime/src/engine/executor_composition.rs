//! Turn-executor composition — the kernel composition point for every
//! turn loop: the main turn and every in-process sub-agent turn.
//!
//! The engine no longer hard-wires `HostAgentExecutor` into
//! `handle_send_message`: every turn it snapshots its kernel services into a
//! `TurnDeps`, resolves the configured factory from the process-shared
//! `TurnExecutorRegistry` (config key `turn_executor`, default
//! [`BUILTIN_TURN_EXECUTOR_ID`]), and runs whatever the factory built
//! through the dyn-safe [`AgentExecutor`] seam. A sub-agent turn takes the
//! same path through [`run_child_turn`](super::child_turn::run_child_turn):
//! a child-shaped `TurnDeps` (`TurnShape::Child`), the same registry,
//! the same factory (general agent: one factory makes every agent). This is the
//! general-agent kernel-composition model adapted to this
//! codebase: kernel services stay owned by the host, and "which loop runs"
//! is a *selection*, resolved at a defined chokepoint, not a composition
//! of the engine itself.
//!
//! Registration follows the reversible-registration house pattern
//! (`SharedProviderRegistry` / `ProviderRegistration`): upsert by id, an
//! identity-checked RAII guard whose drop un-registers, and
//! effect-at-next-chokepoint semantics — a registration takes effect at the
//! next turn's assembly, never mid-turn (the in-flight executor is
//! refcounted). Replacing an id is refused while turns are in flight on it
//! (the [`crate::turn_lease`] registry; the general agent's `setFactory` conflict
//! semantics) — the error names the count. The builtin factory
//! (`HostAgentExecutorFactory`) carries exactly the construction the
//! engine used to inline, so the default path is the same assembly.
//!
//! Known limitations (deliberate):
//! - **Alternate implementations are compiled-in registrations in this
//!   crate** (the registry is crate-assembled; hosts validate the id, they
//!   do not seed factories). A dylib or host-side factory cannot yet ship a
//!   from-scratch loop: the factory contract would have to name
//!   `TurnDeps`'s runtime-side types across the `codesmith-agent` →
//!   `codesmith-agent-runtime` boundary. Wrapping an existing turn from a
//!   dylib is the around-turn seam (`crates/agent/src/extension.rs`).
//! - **One selection for every agent.** `turn_executor` picks the loop for
//!   the main turn and every in-process child alike; there is no per-type
//!   selection (general-agent plan D2 — no consumer yet). External-provider children
//!   (out of process) run whatever loop their own process selects.
//! - **Approval is loop discipline, not dispatcher-enforced.** A factory
//!   receives the approval router and the cancel/steer channels in
//!   `TurnDeps` and is contractually required to gate tool calls through
//!   them (see `HostAgentExecutor`'s approval phase); tool-internal
//!   trust-boundary validation (path-escape refusals etc.) lives below the
//!   loop and cannot be bypassed by any executor.
//! - A swapped loop that ignores the prefix-cache discipline degrades
//!   caching (perf), not correctness; `BASE_PROMPT` and prompt assembly
//!   stay host-owned.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, RwLock};
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use codesmith_agent::callback::Callback;
use codesmith_agent::executor::{AgentExecutor, AgentExecutorConfig};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::models::{SystemPrompt, Tool, Usage};
use codesmith_agent::tools::ToolSet;
use codesmith_extensions::ExtensionRunner;

use crate::capacity::CapacityController;
use crate::compaction::CompactionConfig;
use crate::config_types::ApiProvider;
use crate::engine::approval::{ApprovalRouter, UserInputDecision};
use crate::engine::capacity_flow::CapacityGateProbe;
use crate::engine::deliverables::DeliverablesProbe;
use crate::engine::host_executor::{
    CapacityProbe, CompactionProbe, HostAgentExecutor, LspProbe, ReinjectProbe, TurnMetaProbe,
};
use crate::engine::result_verifier::VerdictBlock;
use crate::engine::{CapacityDecision, ReplayOutcome, SteerInput, TargetedRefreshOutcome};
use crate::events::Event;
use crate::host_services::SubAgentApi;
use crate::mode::AppMode;
use crate::prefix_cache::PrefixStabilityManager;
use crate::subagent::SubAgentCompletion;
use crate::tool_dispatch::ToolDispatcher;
use crate::tool_state::plan_mode::SharedPlanModeState;
use crate::working_set::WorkingSet;
use codesmith_session::fact_ledger::FactLedger;
use codesmith_session::store::AttemptRecord;

/// The always-registered id of the default production loop
/// (`HostAgentExecutorFactory`). Config `turn_executor` defaults to this.
pub const BUILTIN_TURN_EXECUTOR_ID: &str = "builtin";

/// Which agent a turn is assembled for — the one shape question a factory
/// answers (the builtin factory reads it for the dispatch scope). What a
/// child lacks is an absent (`None`) service in [`TurnDeps`], never a
/// branch in the loop: the general agent is one scope per agent, not a pile of branches
/// per agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnShape {
    /// The engine's own turn: session-level services, `Global` dispatch.
    Main,
    /// A sub-agent turn ([`run_child_turn`](super::child_turn::run_child_turn)).
    Child {
        /// The sub-agent type id — the `Scope::AgentType` it dispatches at.
        agent_type: String,
        /// Spawn depth (1 = a direct child of the main turn).
        depth: u32,
        /// The spawning sub-agent's id; `None` when the main turn spawned it.
        /// Carried for lineage-aware executors; the builtin factory does not
        /// read it.
        parent: Option<String>,
    },
}

/// The compaction / capacity probe sources — the main turn's
/// context-window management. Absent for a sub-agent turn (children run
/// without compaction or the capacity gates, as they always have).
pub(crate) struct TurnContextSources {
    pub compaction: CompactionConfig,
    pub workspace: PathBuf,
    pub fact_ledger: Arc<StdMutex<FactLedger>>,
    pub api_provider: ApiProvider,
    pub model: String,
    /// The one effective context window for `api_provider` + `model`,
    /// resolved by the engine (config override → provider capability →
    /// static table → 128K). Probes budget against this value, never a
    /// second model-name lookup.
    pub effective_context_window: u32,
    pub capacity_controller: Arc<StdMutex<CapacityController>>,
    pub working_set: Arc<StdMutex<WorkingSet>>,
    pub capacity_profile_window: usize,
    pub turn_counter: u64,
}

/// The kernel-service bundle a host snapshots per turn and hands a
/// [`TurnExecutorFactory`] — the composition surface of every turn loop:
/// the engine builds the main turn's ([`TurnShape::Main`]),
/// [`run_child_turn`](super::child_turn::run_child_turn) builds each
/// sub-agent's ([`TurnShape::Child`]), and one factory serves both.
///
/// Every field is an owned clone taken *before* the `&mut Session` borrow
/// (probes observe session state through `Arc`s), so the bundle carries no
/// engine borrow. [`TurnExecutorFactory::build`] consumes it by value: the
/// host builds one bundle per turn and hands it over whole. A factory
/// that needs less simply drops fields it ignores; a future field addition
/// is additive (compiled-in factories recompile in lockstep). The
/// `Option`al services are the main turn's own: a child's bundle carries
/// `None` for each.
///
/// `pub(crate)`: compiled-in factories live in this crate for now (see the
/// module doc's known limitations); promoting a host-side factory tier
/// means naming these runtime types across the crate boundary first.
pub(crate) struct TurnDeps {
    pub shape: TurnShape,

    // --- the four agent-level collaborators (as the reference loop takes) --
    pub client: LlmClientHandle,
    pub tools: Arc<ToolSet>,
    pub callback: Arc<dyn Callback>,
    pub executor_config: AgentExecutorConfig,
    /// Per-turn request overrides (a child's routed model / effort);
    /// `None` sends the client's own model and no effort override.
    pub request_model: Option<String>,
    pub reasoning_effort: Option<String>,

    // --- channels / tokens -------------------------------------------------
    pub tx_event: Option<mpsc::Sender<Event>>,
    /// Mid-turn user input: the engine's steer channel for the main turn, a
    /// child's own `agent_send_input` channel for a sub-agent turn.
    pub rx_steer: Arc<AsyncMutex<mpsc::Receiver<SteerInput>>>,
    pub rx_approval: Option<Arc<ApprovalRouter>>,
    pub rx_user_input: Option<Arc<AsyncMutex<mpsc::Receiver<UserInputDecision>>>>,
    pub rx_subagent_completion:
        Option<Arc<AsyncMutex<mpsc::UnboundedReceiver<SubAgentCompletion>>>>,
    pub cancel_token: CancellationToken,

    // --- probe sources (the builtin factory constructs its probes from these)
    pub lsp: Option<LspProbe>,
    pub turn_meta: Option<TurnMetaProbe>,
    pub reinject: Option<ReinjectProbe>,
    pub deliverables: Option<DeliverablesProbe>,
    /// `[rhai_loop]` settings for the script-driven executor (`None` in
    /// embeds/tests that never select it; the factory applies defaults).
    pub rhai_loop: Option<crate::engine::rhai_executor::RhaiLoopSettings>,
    pub context: Option<TurnContextSources>,

    // --- turn-shape config ---------------------------------------------------
    pub mode: AppMode,
    pub max_subagents: usize,
    /// Sub-agent recursion ceiling (`[runtime] max_spawn_depth`) — the
    /// loop surface's `spawn_child` enforces it loudly. `0` in a child's
    /// own bundle: script-side fan-out is one level in v1.
    pub max_spawn_depth: u32,
    pub plan_mode_state: Option<SharedPlanModeState>,
    pub progressive_disclosure: bool,
    pub stream_idle_timeout: Option<Duration>,
    pub stream_idle_retry_increment: Duration,

    // --- tool surface --------------------------------------------------------
    pub request_catalog: Option<Arc<Vec<Tool>>>,
    pub tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,

    // --- engine-held shared state -------------------------------------------
    pub extension_runner: Option<Arc<ExtensionRunner>>,
    pub prefix_stability: Option<Arc<StdMutex<PrefixStabilityManager>>>,
    pub pending_result_verifications: Option<Arc<StdMutex<Vec<VerdictBlock>>>>,
    pub tool_durations: Option<Arc<StdMutex<HashMap<String, u64>>>>,
    pub subagents: Arc<dyn SubAgentApi>,
    pub session_id: Option<String>,
}

/// A built main turn: the dyn-safe executor the engine runs, plus the
/// harvest handle it reads post-run state through. The builtin factory
/// returns the same `HostAgentExecutor` under both Arcs.
pub(crate) struct BuiltTurn {
    pub executor: Arc<dyn AgentExecutor>,
    pub harvest: Arc<dyn TurnHarvest>,
}

/// Builds a per-turn [`BuiltTurn`] from the engine's kernel-service bundle.
///
/// Factories are compiled-in registrations on the
/// [`TurnExecutorRegistry`] (see the module doc for why dylib from-scratch
/// loops are not yet a tier). `build` is infallible by design: a factory
/// that cannot assemble its loop does not belong in the registry.
pub(crate) trait TurnExecutorFactory: Send + Sync {
    /// Stable selection id (the `turn_executor` config value).
    fn id(&self) -> &'static str;
    /// Assemble this turn's executor + harvest from `deps` (consumed).
    fn build(&self, deps: TurnDeps) -> BuiltTurn;
}

/// The post-run state surface the engine reads after `AgentExecutor::run` —
/// the dyn-safe form of `HostAgentExecutor`'s `take_*` family. Every method
/// defaults to "nothing pending", which is exactly what a loop that runs no
/// compaction/capacity deferrals owes: the engine's post-run cascades are
/// `Option`-gated and simply skip.
pub trait TurnHarvest: Send + Sync {
    /// Discard steers queued before the turn (returns the drain count).
    fn drain_stale_steers<'a>(&'a self) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>> {
        Box::pin(async { 0 })
    }
    fn take_usage(&self) -> Usage {
        Usage::default()
    }
    fn take_reported_model(&self) -> Option<String> {
        None
    }
    fn take_failed_attempts(&self) -> Vec<AttemptRecord> {
        Vec::new()
    }
    fn take_pending_compaction_summary(&self) -> Option<SystemPrompt> {
        None
    }
    fn take_pending_post_compact_cleanup(&self) -> bool {
        false
    }
    fn take_pending_capacity_decision(&self) -> Option<CapacityDecision> {
        None
    }
    fn take_pending_replay_outcome(&self) -> Option<ReplayOutcome> {
        None
    }
    fn take_pending_targeted_refresh_outcome(&self) -> Option<TargetedRefreshOutcome> {
        None
    }
    /// The text the user saw before an interrupt (committed by the engine
    /// with an interruption marker); `None` when nothing streamed.
    fn take_interrupted_text(&self) -> Option<String> {
        None
    }
}

/// Process-shared registry of turn-executor factories, selected by the
/// `turn_executor` config id. Mirrors `SharedProviderRegistry`: upsert by
/// id, identity-checked RAII unregister, lock held only for the map
/// operation (`build` always runs outside it). The paired
/// [`TurnLeaseRegistry`](crate::turn_lease::TurnLeaseRegistry) carries the
/// registry's in-flight accounting (general agent: the agent registry owns both the
/// factory slot and the live agents), so `register` can refuse to replace
/// an id that still has turns running on it.
pub(crate) struct TurnExecutorRegistry {
    inner: Arc<RwLock<HashMap<String, Arc<dyn TurnExecutorFactory>>>>,
    leases: crate::turn_lease::TurnLeaseRegistry,
}

impl TurnExecutorRegistry {
    /// Build a registry over `leases` — the shared registry pairs with
    /// [`shared_turn_leases`](crate::turn_lease::shared_turn_leases);
    /// tests pair with their own instance so parallel tests' leases do
    /// not leak into each other's counts.
    #[must_use]
    pub fn new(leases: crate::turn_lease::TurnLeaseRegistry) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            leases,
        }
    }

    /// A registry seeded with the builtin factory. The seed is a first
    /// registration, never a replacement, so the in-flight guard does not
    /// apply — the shared registry initializes lazily and can do so from
    /// inside the first turn, after that turn's own `builtin` lease exists.
    /// The builtin's guard is leaked: the builtin is process-lifetime and
    /// has nothing to unwind.
    #[must_use]
    pub fn with_builtin(leases: crate::turn_lease::TurnLeaseRegistry) -> Self {
        let registry = Self::new(leases);
        let guard = registry
            .register(Arc::new(HostAgentExecutorFactory))
            .expect("a first registration is never refused");
        std::mem::forget(guard);
        // The script-driven loop ships as a second process-lifetime entry
        // (selection is config `turn_executor = "rhai"`; see
        // `engine::rhai_executor`). Same leaked-guard rule as the builtin.
        let guard = registry
            .register(Arc::new(super::rhai_executor::RhaiTurnExecutorFactory))
            .expect("a first registration is never refused");
        std::mem::forget(guard);
        registry
    }

    /// Register (or replace) a factory, keyed by its id. Replacing an
    /// incumbent fails loud while any turn is still in flight on that id
    /// (the general agent's `setFactory` conflict: a replacement never takes the old
    /// factory's live handles away) — let the turns finish (or drain the
    /// sub-agents via a reload) and retry. A first registration of an id
    /// is not a replacement and is never refused, which is what lets the
    /// shared registry seed the builtin lazily from inside a turn that
    /// already holds a `builtin` lease. Returns the registration guard:
    /// dropping it removes the factory unless a newer registration for
    /// the same id has since replaced it.
    pub fn register(
        &self,
        factory: Arc<dyn TurnExecutorFactory>,
    ) -> Result<TurnExecutorRegistration, String> {
        let id = factory.id().to_string();
        // Lock order: executor map (write) → lease entries; nothing takes
        // them in the other order (`acquire` never touches this map).
        let mut map = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if map.contains_key(&id) {
            let in_flight = self.leases.in_flight_for_executor(&id);
            if in_flight > 0 {
                return Err(format!(
                    "cannot replace turn executor '{id}': {in_flight} turn(s) in flight on it — \
                     wait for them to finish (a /extension reload drains in-flight sub-agents) \
                     and retry"
                ));
            }
        }
        map.insert(id.clone(), factory.clone());
        drop(map);
        Ok(TurnExecutorRegistration {
            registry: self.clone_handle(),
            id,
            factory,
        })
    }

    /// Remove `factory` from `id` only if it is still the registered one
    /// (`Arc::ptr_eq`), so a stale guard cannot un-register a newer factory.
    fn remove_if_current(&self, id: &str, factory: &Arc<dyn TurnExecutorFactory>) {
        let mut w = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = w.get(id)
            && Arc::ptr_eq(current, factory)
        {
            w.remove(id);
        }
    }

    /// Look up the factory registered for `id`, if any.
    #[must_use]
    pub fn resolve(&self, id: &str) -> Option<Arc<dyn TurnExecutorFactory>> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// All registered ids, sorted (stable error messages / listings).
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// Read-only snapshot of every slot with its in-flight census, sorted
    /// by id (general-agent D1 observability — see [`turn_executor_status`]).
    #[must_use]
    pub fn slots(&self) -> Vec<TurnExecutorSlot> {
        self.ids()
            .into_iter()
            .map(|id| {
                let mut main_turns = 0;
                let mut subagent_turns = Vec::new();
                for (depth, count) in self.leases.in_flight_by_depth(&id) {
                    if depth == 0 {
                        main_turns += count;
                    } else {
                        subagent_turns.push((depth, count));
                    }
                }
                TurnExecutorSlot {
                    id,
                    main_turns,
                    subagent_turns,
                }
            })
            .collect()
    }

    fn clone_handle(&self) -> TurnExecutorRegistry {
        TurnExecutorRegistry {
            inner: Arc::clone(&self.inner),
            leases: self.leases.clone(),
        }
    }
}

/// One registered turn-executor slot as an operator sees it (general-agent D1): the
/// id and the turns in flight on it, main turn and sub-agents by spawn
/// depth. Which slot is the session's selection is the host's
/// `turn_executor` config, not registry state — the host marks it. The
/// registry types themselves stay crate-private; this snapshot is the whole
/// public surface `/extension status` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnExecutorSlot {
    pub id: String,
    /// Turns in flight at depth 0 (main turns; the registry is process-wide,
    /// so several engines can each contribute one).
    pub main_turns: usize,
    /// `(spawn depth, count)` for sub-agent turns (depth ≥ 1), depth ascending.
    pub subagent_turns: Vec<(u32, usize)>,
}

/// Snapshot of the process-shared turn-executor registry for status
/// display: one [`TurnExecutorSlot`] per registered id, sorted by id.
#[must_use]
pub fn turn_executor_status() -> Vec<TurnExecutorSlot> {
    shared_turn_executors().slots()
}

/// The registration guard returned by [`TurnExecutorRegistry::register`].
/// Dropping it removes the factory it registered — unless a newer
/// registration for the same id has since replaced it (identity-checked via
/// `Arc::ptr_eq`). Same unload story as `ProviderRegistration`.
pub(crate) struct TurnExecutorRegistration {
    registry: TurnExecutorRegistry,
    id: String,
    factory: Arc<dyn TurnExecutorFactory>,
}

impl Drop for TurnExecutorRegistration {
    fn drop(&mut self) {
        self.registry.remove_if_current(&self.id, &self.factory);
    }
}

/// The process-shared turn-executor registry, seeded with the builtin
/// factory. Hosts resolve here at turn assembly and validate config against
/// it at startup. Paired with the process-shared lease registry
/// (`turn_lease::shared_turn_leases`) so the in-flight guard sees every
/// engine's turns.
pub(crate) fn shared_turn_executors() -> &'static TurnExecutorRegistry {
    static SHARED: OnceLock<TurnExecutorRegistry> = OnceLock::new();
    SHARED.get_or_init(|| {
        TurnExecutorRegistry::with_builtin(crate::turn_lease::shared_turn_leases().clone())
    })
}

/// Resolve the factory for `id`, naming the registered ids and the remedy
/// on a miss (the engine's mid-session loud-error path; startup validation
/// catches the same condition earlier).
pub(crate) fn resolve_turn_executor(id: &str) -> Result<Arc<dyn TurnExecutorFactory>, String> {
    shared_turn_executors().resolve(id).ok_or_else(|| {
        format!(
            "no turn executor registered for '{id}'; registered: [{}] — fix `turn_executor` in \
             config.toml, or reload the extension that registered it",
            shared_turn_executors().ids().join(", ")
        )
    })
}

/// Host-facing startup validation of the `turn_executor` config id: `Ok`
/// when registered, `Err(message)` naming the registered ids and the remedy
/// otherwise. (The registry is crate-assembled for now — see the module
/// doc — so hosts validate; they do not resolve factories.)
pub fn validate_turn_executor_id(id: &str) -> Result<(), String> {
    resolve_turn_executor(id).map(|_| ())
}

/// The default production loop — one loop for every agent: exactly the
/// `HostAgentExecutor` assembly the engine used to inline in
/// `handle_send_message` (slice 20 §E onward) for the main turn, and the
/// same assembly with the main turn's services absent for a sub-agent turn.
/// The shape decides only the dispatch scope. Probes are constructed per
/// build from the `TurnDeps` snapshots, as before.
pub(crate) struct HostAgentExecutorFactory;

/// The per-turn `HostAgentExecutor` construction shared by the builtin
/// factory and the `rhai` loop factory (the scripted loop wraps a host
/// built exactly the way the builtin loop's is — same services, same
/// probes, same builders).
pub(crate) fn build_host_executor(deps: TurnDeps) -> Arc<HostAgentExecutor> {
    let dispatch_scope = match &deps.shape {
        TurnShape::Main => codesmith_agent::extension::Scope::Global,
        TurnShape::Child { agent_type, .. } => {
            codesmith_agent::extension::Scope::AgentType(agent_type.clone())
        }
    };
    let (compaction, capacity, capacity_gate) = match deps.context {
        Some(ctx) => (
            Some(
                CompactionProbe::new(ctx.compaction.clone(), ctx.workspace.clone())
                    .with_fact_ledger(Some(ctx.fact_ledger.clone())),
            ),
            Some(
                CapacityProbe::new(
                    ctx.api_provider,
                    ctx.model.clone(),
                    ctx.effective_context_window,
                    ctx.compaction,
                    ctx.workspace.clone(),
                )
                .with_fact_ledger(Some(ctx.fact_ledger)),
            ),
            Some(CapacityGateProbe::new(
                ctx.capacity_controller,
                ctx.model,
                ctx.effective_context_window,
                ctx.workspace,
                ctx.working_set,
                ctx.capacity_profile_window,
                ctx.turn_counter,
            )),
        ),
        None => (None, None, None),
    };
    Arc::new(
        HostAgentExecutor::new(
            deps.client,
            deps.tools,
            deps.callback,
            deps.executor_config,
            deps.tx_event,
            deps.lsp,
            Some(deps.rx_steer),
            deps.rx_approval,
            compaction,
            capacity,
            deps.rx_subagent_completion,
            Some(deps.cancel_token),
            Some(deps.subagents),
        )
        .with_capacity_gate(capacity_gate)
        .with_turn_mode(deps.mode)
        .with_subagent_cap(deps.max_subagents)
        .with_plan_mode_state(deps.plan_mode_state)
        .with_request_catalog(deps.request_catalog)
        .with_progressive_disclosure(deps.progressive_disclosure)
        .with_tool_dispatcher(deps.tool_dispatcher)
        .with_user_input(deps.rx_user_input)
        .with_turn_meta(deps.turn_meta)
        .with_reinject(deps.reinject)
        .with_extension_runner(deps.extension_runner)
        .with_dispatch_scope(dispatch_scope)
        .with_request_overrides(deps.request_model, deps.reasoning_effort)
        .with_stream_idle_timeout(deps.stream_idle_timeout)
        .with_stream_idle_retry_increment(deps.stream_idle_retry_increment)
        .with_prefix_stability(deps.prefix_stability)
        .with_result_verifications(deps.pending_result_verifications)
        .with_tool_durations(deps.tool_durations)
        .with_session_id(deps.session_id)
        .with_deliverables(deps.deliverables),
    )
}

impl TurnExecutorFactory for HostAgentExecutorFactory {
    fn id(&self) -> &'static str {
        BUILTIN_TURN_EXECUTOR_ID
    }

    fn build(&self, deps: TurnDeps) -> BuiltTurn {
        let executor = build_host_executor(deps);
        let harvest: Arc<dyn TurnHarvest> = executor.clone();
        BuiltTurn { executor, harvest }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-op factory whose `build` assembles a trivial executor + default
    /// harvest (never invoked here — the registry tests only resolve it).
    struct NoopFactory;

    impl TurnExecutorFactory for NoopFactory {
        fn id(&self) -> &'static str {
            "noop-test"
        }
        fn build(&self, _deps: TurnDeps) -> BuiltTurn {
            struct NoopExecutor;
            impl AgentExecutor for NoopExecutor {
                fn run<'a>(
                    &'a self,
                    _history: &'a mut dyn codesmith_agent::memory::ChatHistory,
                    _user_text: String,
                ) -> Pin<
                    Box<
                        dyn Future<Output = anyhow::Result<codesmith_agent::callback::StopReason>>
                            + Send
                            + 'a,
                    >,
                > {
                    Box::pin(async { Ok(codesmith_agent::callback::StopReason::NoToolCalls) })
                }
            }
            struct NoopHarvest;
            impl TurnHarvest for NoopHarvest {}
            BuiltTurn {
                executor: Arc::new(NoopExecutor),
                harvest: Arc::new(NoopHarvest),
            }
        }
    }

    /// A harvest with only default impls reports "nothing pending" on every
    /// surface — the engine's Option-gated post-run cascades all skip.
    #[test]
    fn harvest_defaults_report_nothing_pending() {
        struct OnlyDefaults;
        impl TurnHarvest for OnlyDefaults {}
        let h = OnlyDefaults;
        assert_eq!(h.take_usage(), Usage::default());
        assert_eq!(h.take_reported_model(), None);
        assert!(h.take_failed_attempts().is_empty());
        assert_eq!(h.take_pending_compaction_summary(), None);
        assert!(!h.take_pending_post_compact_cleanup());
        assert!(h.take_pending_capacity_decision().is_none());
        assert!(h.take_pending_replay_outcome().is_none());
        assert!(h.take_pending_targeted_refresh_outcome().is_none());
    }

    #[tokio::test]
    async fn harvest_default_drain_returns_zero() {
        struct OnlyDefaults;
        impl TurnHarvest for OnlyDefaults {}
        assert_eq!(OnlyDefaults.drain_stale_steers().await, 0);
    }

    /// Register → resolve → replace → stale guard cannot un-register the
    /// newer factory → dropping the live guard un-registers.
    #[test]
    fn registry_replace_and_identity_checked_unregister() {
        let registry = TurnExecutorRegistry::new(crate::turn_lease::TurnLeaseRegistry::new());
        let guard_a = registry
            .register(Arc::new(NoopFactory))
            .expect("register a");
        assert_eq!(registry.ids(), vec!["noop-test".to_string()]);

        let guard_b = registry
            .register(Arc::new(NoopFactory))
            .expect("register b");
        drop(guard_a); // stale guard: must NOT remove the newer factory
        assert!(registry.resolve("noop-test").is_some());

        drop(guard_b);
        assert!(registry.resolve("noop-test").is_none());
        assert!(registry.ids().is_empty());
    }

    /// the general agent's `setFactory` conflict semantics: an incumbent id with in-flight
    /// leases is not replaceable; the error names the count (fail loud).
    /// Releasing the leases unblocks the replacement.
    #[test]
    fn register_rejects_replacement_while_turns_are_in_flight() {
        let leases = crate::turn_lease::TurnLeaseRegistry::new();
        let registry = TurnExecutorRegistry::new(leases.clone());
        // The incumbent stays registered for the whole test.
        let _incumbent = registry
            .register(Arc::new(NoopFactory))
            .expect("first registration goes through");

        let in_flight = leases.acquire(crate::turn_lease::TurnLeaseSpec {
            key: crate::turn_lease::TurnLeaseKey {
                agent: "session-x".to_string(),
                generation: 0,
                executor: "noop-test".to_string(),
            },
            depth: 0,
            cancel: CancellationToken::new(),
            dylib_exposed: false,
            scope: None,
        });
        let second = match registry.register(Arc::new(NoopFactory)) {
            Err(message) => message,
            Ok(_) => panic!("in-flight id must not be replaceable"),
        };
        assert!(
            second.contains("cannot replace turn executor 'noop-test'"),
            "{second}"
        );
        assert!(second.contains("1 turn(s) in flight"), "{second}");
        assert!(second.contains("retry"), "names the remedy: {second}");

        drop(in_flight);
        registry
            .register(Arc::new(NoopFactory))
            .expect("registration goes through once the turns finish");
    }

    /// Regression: the builtin seed is a first registration and must never
    /// be refused by the in-flight guard. The shared registry initializes
    /// lazily — possibly from inside the first turn, after that turn's own
    /// `builtin` lease exists — and an unconditional guard refused the
    /// seed there (panicking the init and killing the turn). Only
    /// replacing an incumbent is guarded.
    #[test]
    fn builtin_seed_ignores_in_flight_guard() {
        let leases = crate::turn_lease::TurnLeaseRegistry::new();
        let _turn_already_running = leases.acquire(crate::turn_lease::TurnLeaseSpec {
            key: crate::turn_lease::TurnLeaseKey {
                agent: "session-first-turn".to_string(),
                generation: 0,
                executor: BUILTIN_TURN_EXECUTOR_ID.to_string(),
            },
            depth: 0,
            cancel: CancellationToken::new(),
            dylib_exposed: true,
            scope: None,
        });
        let registry = TurnExecutorRegistry::with_builtin(leases);
        assert!(
            registry.resolve(BUILTIN_TURN_EXECUTOR_ID).is_some(),
            "the builtin seeds even with a builtin turn in flight"
        );
    }

    /// D1 observability: the slot snapshot counts the leases in flight per
    /// executor — main turn and sub-agents by spawn depth — and follows
    /// releases.
    #[test]
    fn slots_report_in_flight_turns_by_depth() {
        let leases = crate::turn_lease::TurnLeaseRegistry::new();
        let registry = TurnExecutorRegistry::with_builtin(leases.clone());
        let lease = |agent: &str, depth: u32| {
            leases.acquire(crate::turn_lease::TurnLeaseSpec {
                key: crate::turn_lease::TurnLeaseKey {
                    agent: agent.to_string(),
                    generation: 0,
                    executor: BUILTIN_TURN_EXECUTOR_ID.to_string(),
                },
                depth,
                cancel: CancellationToken::new(),
                dylib_exposed: false,
                scope: None,
            })
        };
        let slot = |id: &str| TurnExecutorSlot {
            id: id.to_string(),
            main_turns: 0,
            subagent_turns: Vec::new(),
        };
        // The registry now seeds two process-lifetime entries (builtin +
        // the script-driven `rhai` loop).
        let idle = slot(BUILTIN_TURN_EXECUTOR_ID);
        let idle_rhai = slot(crate::engine::rhai_executor::RHAI_TURN_EXECUTOR_ID);
        assert_eq!(registry.slots(), vec![idle.clone(), idle_rhai.clone()]);

        let main = lease("session", 0);
        let child_a = lease("agent-a", 1);
        let child_b = lease("agent-b", 1);
        let grandchild = lease("agent-c", 2);
        assert_eq!(
            registry.slots(),
            vec![
                TurnExecutorSlot {
                    id: BUILTIN_TURN_EXECUTOR_ID.to_string(),
                    main_turns: 1,
                    subagent_turns: vec![(1, 2), (2, 1)],
                },
                idle_rhai.clone(),
            ]
        );

        drop(child_a);
        drop(grandchild);
        assert_eq!(registry.slots()[0].subagent_turns, vec![(1, 1)]);
        drop(main);
        drop(child_b);
        assert_eq!(registry.slots(), vec![idle, idle_rhai]);
    }

    /// The process-shared registry always carries the builtin factory.
    #[test]
    fn shared_registry_seeds_builtin() {
        let ids = shared_turn_executors().ids();
        assert!(ids.iter().any(|id| id == BUILTIN_TURN_EXECUTOR_ID));
        let factory =
            resolve_turn_executor(BUILTIN_TURN_EXECUTOR_ID).expect("builtin is registered");
        assert_eq!(factory.id(), BUILTIN_TURN_EXECUTOR_ID);
    }

    /// A miss names the registered ids and the remedy (loud, actionable).
    #[test]
    fn resolve_miss_names_ids_and_remedy() {
        let Err(err) = resolve_turn_executor("does-not-exist") else {
            panic!("expected the miss to error");
        };
        assert!(
            err.contains("does-not-exist"),
            "names the missing id: {err}"
        );
        assert!(
            err.contains(BUILTIN_TURN_EXECUTOR_ID),
            "names registered ids: {err}"
        );
        assert!(err.contains("turn_executor"), "names the remedy: {err}");
    }
}
