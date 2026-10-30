//! Turn-executor composition — the kernel composition point for the main
//! turn loop.
//!
//! The engine no longer hard-wires `HostAgentExecutor` into
//! `handle_send_message`: every turn it snapshots its kernel services into a
//! [`TurnDeps`], resolves the configured factory from the process-shared
//! [`TurnExecutorRegistry`] (config key `turn_executor`, default
//! [`BUILTIN_TURN_EXECUTOR_ID`]), and runs whatever the factory built
//! through the dyn-safe [`AgentExecutor`] seam. This is the deepseek-harness
//! (DSH) kernel-composition model adapted to this codebase: kernel services
//! stay owned by the host, and "which loop runs" is a *selection*, resolved
//! at a defined chokepoint, not a composition of the engine itself.
//!
//! Registration follows the reversible-registration house pattern
//! (`SharedProviderRegistry` / `ProviderRegistration`): upsert by id, an
//! identity-checked RAII guard whose drop un-registers, and
//! effect-at-next-chokepoint semantics — a registration takes effect at the
//! next turn's assembly, never mid-turn (the in-flight executor is
//! refcounted). The builtin factory (`HostAgentExecutorFactory`) carries
//! exactly the construction the engine used to inline, so the default path
//! is the same assembly.
//!
//! Known limitations (deliberate):
//! - **Alternate implementations are compiled-in registrations in this
//!   crate** (the registry is crate-assembled; hosts validate the id, they
//!   do not seed factories). A dylib or host-side factory cannot yet ship a
//!   from-scratch loop: the factory contract would have to name
//!   `TurnDeps`'s runtime-side types across the `codesmith-agent` →
//!   `codesmith-agent-runtime` boundary. Wrapping an existing turn from a
//!   dylib is the around-turn seam (`crates/agent/src/extension.rs`).
//! - **Main turns only.** Sub-agent loops build their own in-loop dispatch
//!   and do not consult this registry; a session's children run the builtin
//!   machinery regardless of the selected main-turn executor.
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
use crate::compaction::fact_ledger::FactLedger;
use crate::config_types::ApiProvider;
use crate::engine::approval::{ApprovalRouter, UserInputDecision};
use crate::engine::capacity_flow::CapacityGateProbe;
use crate::engine::deliverables::DeliverablesProbe;
use crate::engine::host_executor::{
    CapacityProbe, CompactionProbe, HostAgentExecutor, LspProbe, ReinjectProbe, TurnMetaProbe,
};
use crate::engine::result_verifier::VerdictBlock;
use crate::engine::{CapacityDecision, ReplayOutcome, TargetedRefreshOutcome};
use crate::events::Event;
use crate::host_services::SubAgentApi;
use crate::mode::AppMode;
use crate::prefix_cache::PrefixStabilityManager;
use crate::session_store::AttemptRecord;
use crate::subagent::SubAgentCompletion;
use crate::tool_dispatch::ToolDispatcher;
use crate::tool_state::plan_mode::SharedPlanModeState;
use crate::working_set::WorkingSet;

/// The always-registered id of the default production loop
/// ([`HostAgentExecutorFactory`]). Config `turn_executor` defaults to this.
pub const BUILTIN_TURN_EXECUTOR_ID: &str = "builtin";

/// The kernel-service bundle the engine snapshots per turn and hands a
/// [`TurnExecutorFactory`] — the composition surface of the main turn loop.
///
/// Every field is an owned clone taken *before* the `&mut Session` borrow
/// (probes observe session state through `Arc`s), so the bundle carries no
/// engine borrow. [`TurnExecutorFactory::build`] consumes it by value: the
/// engine builds one bundle per turn and hands it over whole. A factory
/// that needs less simply drops fields it ignores; a future field addition
/// is additive (compiled-in factories recompile in lockstep).
///
/// `pub(crate)`: compiled-in factories live in this crate for now (see the
/// module doc's known limitations); promoting a host-side factory tier
/// means naming these runtime types across the crate boundary first.
pub(crate) struct TurnDeps {
    // --- the four agent-level collaborators (as the reference loop takes) --
    pub client: LlmClientHandle,
    pub tools: Arc<ToolSet>,
    pub callback: Arc<dyn Callback>,
    pub executor_config: AgentExecutorConfig,

    // --- channels / tokens -------------------------------------------------
    pub tx_event: Option<mpsc::Sender<Event>>,
    pub rx_steer: Arc<AsyncMutex<mpsc::Receiver<String>>>,
    pub rx_approval: Arc<ApprovalRouter>,
    pub rx_user_input: Arc<AsyncMutex<mpsc::Receiver<UserInputDecision>>>,
    pub rx_subagent_completion: Arc<AsyncMutex<mpsc::UnboundedReceiver<SubAgentCompletion>>>,
    pub cancel_token: CancellationToken,

    // --- probe sources (the builtin factory constructs its probes from these)
    pub lsp: Option<LspProbe>,
    pub turn_meta: Option<TurnMetaProbe>,
    pub reinject: Option<ReinjectProbe>,
    pub deliverables: Option<DeliverablesProbe>,
    pub compaction: CompactionConfig,
    pub workspace: PathBuf,
    pub fact_ledger: Arc<StdMutex<FactLedger>>,
    pub api_provider: ApiProvider,
    pub model: String,
    pub capacity_controller: Arc<StdMutex<CapacityController>>,
    pub working_set: Arc<StdMutex<WorkingSet>>,
    pub capacity_profile_window: usize,
    pub turn_counter: u64,

    // --- turn-shape config ---------------------------------------------------
    pub mode: AppMode,
    pub max_subagents: usize,
    pub plan_mode_state: SharedPlanModeState,
    pub progressive_disclosure: bool,
    pub stream_idle_timeout: Option<Duration>,
    pub stream_idle_retry_increment: Duration,

    // --- tool surface --------------------------------------------------------
    pub request_catalog: Option<Arc<Vec<Tool>>>,
    pub tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,

    // --- engine-held shared state -------------------------------------------
    pub extension_runner: Option<Arc<ExtensionRunner>>,
    pub prefix_stability: Option<Arc<StdMutex<PrefixStabilityManager>>>,
    pub pending_result_verifications: Arc<StdMutex<Vec<VerdictBlock>>>,
    pub tool_durations: Arc<StdMutex<HashMap<String, u64>>>,
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
}

/// Process-shared registry of turn-executor factories, selected by the
/// `turn_executor` config id. Mirrors `SharedProviderRegistry`: upsert by
/// id, identity-checked RAII unregister, lock held only for the map
/// operation (`build` always runs outside it).
#[derive(Default)]
pub(crate) struct TurnExecutorRegistry {
    inner: Arc<RwLock<HashMap<String, Arc<dyn TurnExecutorFactory>>>>,
}

impl TurnExecutorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a factory, keyed by its id. Returns the
    /// registration guard: dropping it removes the factory unless a newer
    /// registration for the same id has since replaced it.
    #[must_use]
    pub fn register(&self, factory: Arc<dyn TurnExecutorFactory>) -> TurnExecutorRegistration {
        let id = factory.id().to_string();
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), factory.clone());
        TurnExecutorRegistration {
            registry: self.clone_handle(),
            id,
            factory,
        }
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

    fn clone_handle(&self) -> TurnExecutorRegistry {
        TurnExecutorRegistry {
            inner: Arc::clone(&self.inner),
        }
    }
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
/// it at startup.
pub(crate) fn shared_turn_executors() -> &'static TurnExecutorRegistry {
    static SHARED: OnceLock<TurnExecutorRegistry> = OnceLock::new();
    SHARED.get_or_init(|| {
        let registry = TurnExecutorRegistry::new();
        // The builtin's guard must never drop — it would un-register the
        // builtin at the end of this statement. Leak it: the builtin is
        // process-lifetime by design and has nothing to unwind.
        let guard = registry.register(Arc::new(HostAgentExecutorFactory));
        std::mem::forget(guard);
        registry
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

/// The default production loop: exactly the `HostAgentExecutor` assembly the
/// engine used to inline in `handle_send_message` (slice 20 §E onward),
/// moved here unchanged. Probes are constructed per build from the
/// `TurnDeps` snapshots, as before.
pub(crate) struct HostAgentExecutorFactory;

impl TurnExecutorFactory for HostAgentExecutorFactory {
    fn id(&self) -> &'static str {
        BUILTIN_TURN_EXECUTOR_ID
    }

    fn build(&self, deps: TurnDeps) -> BuiltTurn {
        let executor = Arc::new(
            HostAgentExecutor::new(
                deps.client,
                deps.tools,
                deps.callback,
                deps.executor_config,
                deps.tx_event,
                deps.lsp,
                Some(deps.rx_steer),
                Some(deps.rx_approval),
                Some(
                    CompactionProbe::new(deps.compaction.clone(), deps.workspace.clone())
                        .with_fact_ledger(Some(deps.fact_ledger.clone())),
                ),
                Some(
                    CapacityProbe::new(
                        deps.api_provider,
                        deps.model.clone(),
                        deps.compaction.clone(),
                        deps.workspace.clone(),
                    )
                    .with_fact_ledger(Some(deps.fact_ledger.clone())),
                ),
                Some(deps.rx_subagent_completion),
                Some(deps.cancel_token),
                Some(deps.subagents),
            )
            .with_capacity_gate(Some(CapacityGateProbe::new(
                deps.capacity_controller,
                deps.model,
                deps.workspace.clone(),
                deps.working_set,
                deps.capacity_profile_window,
                deps.turn_counter,
            )))
            .with_turn_mode(deps.mode)
            .with_subagent_cap(deps.max_subagents)
            .with_plan_mode_state(Some(deps.plan_mode_state))
            .with_request_catalog(deps.request_catalog)
            .with_progressive_disclosure(deps.progressive_disclosure)
            .with_tool_dispatcher(deps.tool_dispatcher)
            .with_user_input(Some(deps.rx_user_input))
            .with_turn_meta(deps.turn_meta)
            .with_reinject(deps.reinject)
            .with_extension_runner(deps.extension_runner)
            .with_stream_idle_timeout(deps.stream_idle_timeout)
            .with_stream_idle_retry_increment(deps.stream_idle_retry_increment)
            .with_prefix_stability(deps.prefix_stability)
            .with_result_verifications(Some(deps.pending_result_verifications))
            .with_tool_durations(deps.tool_durations)
            .with_session_id(deps.session_id)
            .with_deliverables(deps.deliverables),
        );
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
        let registry = TurnExecutorRegistry::new();
        let guard_a = registry.register(Arc::new(NoopFactory));
        assert_eq!(registry.ids(), vec!["noop-test".to_string()]);

        let guard_b = registry.register(Arc::new(NoopFactory));
        drop(guard_a); // stale guard: must NOT remove the newer factory
        assert!(registry.resolve("noop-test").is_some());

        drop(guard_b);
        assert!(registry.resolve("noop-test").is_none());
        assert!(registry.ids().is_empty());
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
