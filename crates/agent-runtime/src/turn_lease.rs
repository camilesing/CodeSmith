//! In-flight turn leases — structural ownership of running turns
//! (DSH alignment, iteration A).
//!
//! Every running turn holds a [`TurnLease`] for its lifetime: the main
//! turn acquires one at assembly in
//! `engine::handle_send_message`, and each in-process sub-agent run
//! acquires one in `run_subagent_task` (the CLI). The lease is keyed by
//! the agent identity, the extension generation the turn was assembled
//! under, and the turn-executor id it runs on. The process-shared
//! [`TurnLeaseRegistry`] makes "who runs what, under which generation" an
//! explicit, queryable fact instead of a comment-stated invariant, so the
//! three consumers can act on it:
//!
//! - **Reload drain** (`crates/cli/src/core/engine.rs`:
//!   `reload_extension_runtime`): before the two-phase dylib unload,
//!   cancel the in-flight sub-agent turns of the runtime being reloaded —
//!   reason-tagged (DSH `disposed`), deepest spawn depth first — and wait
//!   a bounded time for them to release ([`TurnLeaseRegistry::drain`]).
//!   Depth 0 is never selected: from the REPL no main turn is in flight,
//!   and from the `manage_mods` tool the main turn is the initiator, so
//!   draining it would be a self-wait (the DSH self-lineage exclusion).
//! - **Executor replacement guard**
//!   ([`TurnExecutorRegistry::register`](crate::engine::executor_composition::TurnExecutorRegistry::register)):
//!   an id with in-flight leases cannot be replaced (DSH `setFactory`
//!   conflict semantics); the error names the count.
//! - **dlclose safety net** (engine op-loop top): `drop_pending` is
//!   deferred while a lease assembled under a *retired* generation may
//!   still hold dylib-origin handles
//!   ([`TurnLeaseRegistry::stale_dylib_holders`]).
//!
//! Known limitations (deliberate, iteration A):
//! - Only the main turn and in-process sub-agents hold leases. External
//!   -provider children (out of process, no in-process handles) and
//!   AgentTeams teammates (`run_teammate_loop`) do not.
//! - `dylib_exposed` is declared by the holder, not derived: the main
//!   turn declares it whenever a runner is bound; sub-agents declare
//!   `false` by the 5b rule (dylib-origin contributions never enter
//!   sub-agent dispatch).
//! - The drain is a blocking wait (the reload path runs under
//!   `spawn_blocking`); there is no async form.
//! - Drain scope is the `ExtensionRunner` identity (`Weak::ptr_eq`), so a
//!   multi-engine host's reload drains only the turns of the runtime it
//!   reloads. Leases without a runner are never in any drain scope (they
//!   still count toward the executor replacement guard, which is
//!   process-wide by design).

use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use codesmith_extensions::ExtensionRunner;

/// What a lease is keyed on: the agent identity (session id for the main
/// turn, sub-agent id otherwise), the extension generation the turn was
/// assembled under, and the turn-executor id it runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnLeaseKey {
    /// Session id (main turn) or sub-agent id.
    pub agent: String,
    /// Extension generation at assembly; `0` when no runner is bound.
    pub generation: u64,
    /// The `turn_executor` config value the turn runs on.
    pub executor: String,
}

/// Everything a holder declares when acquiring a lease.
pub struct TurnLeaseSpec {
    pub key: TurnLeaseKey,
    /// 0 = main turn; n = sub-agent spawn depth.
    pub depth: u32,
    /// Cancelled by [`TurnLeaseRegistry::drain`]; the holder's loop must
    /// observe it and release.
    pub cancel: CancellationToken,
    /// Whether this holder may hold dylib-origin `Arc`s (see the module
    /// doc's known limitations — declared, not derived).
    pub dylib_exposed: bool,
    /// The extension runtime the turn was assembled under; drains and the
    /// dlclose gate select by this identity. `None` = no runner bound.
    pub scope: Option<Weak<ExtensionRunner>>,
}

/// One registered in-flight turn.
struct LeaseEntry {
    id: u64,
    spec: TurnLeaseSpec,
    /// Set by `drain` before the cancel token fires, so a holder that
    /// observes cancellation can read back why it was stopped.
    cancel_reason: Option<&'static str>,
}

/// What a drain did: which leases released within the bound, and which
/// were still held when it elapsed (cancelled, but not awaited).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrainOutcome {
    pub drained: Vec<TurnLeaseKey>,
    pub outstanding: Vec<TurnLeaseKey>,
}

struct Inner {
    entries: Mutex<Vec<LeaseEntry>>,
    released: Condvar,
    next_id: std::sync::atomic::AtomicU64,
}

/// Registry of in-flight turn leases. Cheap to clone (one `Arc`); the
/// production instance is [`shared_turn_leases`].
#[derive(Clone)]
pub struct TurnLeaseRegistry {
    inner: Arc<Inner>,
}

impl Default for TurnLeaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnLeaseRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(Vec::new()),
                released: Condvar::new(),
                next_id: std::sync::atomic::AtomicU64::new(1),
            }),
        }
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, Vec<LeaseEntry>> {
        self.inner.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register an in-flight turn. The returned [`TurnLease`] releases on
    /// drop; dropping it immediately is a bug, hence `#[must_use]`.
    #[must_use]
    pub fn acquire(&self, spec: TurnLeaseSpec) -> TurnLease {
        let id = self
            .inner
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.lock_entries().push(LeaseEntry {
            id,
            spec,
            cancel_reason: None,
        });
        TurnLease {
            registry: self.clone(),
            id,
        }
    }

    /// Leases currently held on `executor` (across every scope — a factory
    /// replacement is process-wide).
    pub fn in_flight_for_executor(&self, executor: &str) -> usize {
        self.lock_entries()
            .iter()
            .filter(|e| e.spec.key.executor == executor)
            .count()
    }

    /// Leases in `scope` that may still hold dylib-origin handles from a
    /// generation older than `current_generation`. The dlclose safety
    /// net: while this is non-zero, the runner's `drop_pending` must wait.
    pub fn stale_dylib_holders(
        &self,
        scope: &Arc<ExtensionRunner>,
        current_generation: u64,
    ) -> usize {
        let scope = Arc::downgrade(scope);
        self.lock_entries()
            .iter()
            .filter(|e| {
                e.spec.dylib_exposed
                    && e.spec.key.generation < current_generation
                    && e.spec
                        .scope
                        .as_ref()
                        .is_some_and(|s| Weak::ptr_eq(s, &scope))
            })
            .count()
    }

    /// Cancel and await the selected leases of `scope`, deepest spawn
    /// depth first (children before parents), with a reason and a bounded
    /// wait. `select` receives each candidate's key and depth and decides
    /// whether it is drained — the lineage policy (which holder is the
    /// initiator, or absent) belongs to the caller, not the registry.
    ///
    /// Blocking: runs on a `spawn_blocking` thread (or plain thread), not
    /// inside an async context. A lease that does not release within
    /// `timeout` is reported in `outstanding` — the caller proceeds and
    /// says so; the lease stays cancelled and releases when its holder
    /// reaches the next checkpoint. Leases acquired after the drain's
    /// snapshot are not awaited; their holders observe parent-linked
    /// cancel tokens (a detached straggler holds no dylib handles by the
    /// 5b rule), so proceeding stays unload-safe.
    pub fn drain(
        &self,
        scope: &Arc<ExtensionRunner>,
        select: impl Fn(&TurnLeaseKey, u32) -> bool,
        reason: &'static str,
        timeout: Duration,
    ) -> DrainOutcome {
        let scope_weak = Arc::downgrade(scope);
        let deadline = Instant::now() + timeout;
        // Snapshot the selected leases under the lock (key clone + token
        // clone; the entry itself stays mutable for the reason latch).
        let mut selected: Vec<(u64, TurnLeaseKey, u32, CancellationToken)> = {
            let entries = self.lock_entries();
            entries
                .iter()
                .filter(|e| {
                    e.spec
                        .scope
                        .as_ref()
                        .is_some_and(|s| Weak::ptr_eq(s, &scope_weak))
                        && select(&e.spec.key, e.spec.depth)
                })
                .map(|e| {
                    (
                        e.id,
                        e.spec.key.clone(),
                        e.spec.depth,
                        e.spec.cancel.clone(),
                    )
                })
                .collect()
        };
        selected.sort_by(|a, b| b.2.cmp(&a.2)); // deepest first
        let mut outcome = DrainOutcome::default();
        if selected.is_empty() {
            return outcome;
        }
        // One round per distinct depth, deepest first. The reason is
        // latched under the lock, the token cancelled outside it (a
        // holder woken by the cancel may want the lock to release), then
        // the round is awaited — bounded by the shared deadline, so
        // later rounds still cancel even when an earlier one consumed it.
        let mut round_start = 0;
        while round_start < selected.len() {
            let depth = selected[round_start].2;
            let round_end = selected[round_start..]
                .iter()
                .position(|e| e.2 != depth)
                .map_or(selected.len(), |p| round_start + p);
            let round = &selected[round_start..round_end];
            {
                let mut entries = self.lock_entries();
                for (id, ..) in round {
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == *id) {
                        entry.cancel_reason = Some(reason);
                    }
                }
            }
            for (_, _, _, token) in round {
                token.cancel();
            }
            let mut entries = self.lock_entries();
            loop {
                if !round
                    .iter()
                    .any(|(id, ..)| entries.iter().any(|e| e.id == *id))
                {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let (guard, _) = self
                    .inner
                    .released
                    .wait_timeout(entries, deadline - now)
                    .unwrap_or_else(|e| e.into_inner());
                entries = guard;
            }
            for (id, key, ..) in round {
                if entries.iter().any(|e| e.id == *id) {
                    outcome.outstanding.push(key.clone());
                } else {
                    outcome.drained.push(key.clone());
                }
            }
            drop(entries);
            round_start = round_end;
        }
        outcome
    }
}

/// RAII in-flight lease; dropping it releases the registry entry and
/// wakes a draining reload.
pub struct TurnLease {
    registry: TurnLeaseRegistry,
    id: u64,
}

impl TurnLease {
    /// Why this lease was cancelled, if a drain latched a reason before
    /// cancelling. A holder that observes cancellation reads this to
    /// explain the stop.
    pub fn cancel_reason(&self) -> Option<&'static str> {
        self.registry
            .lock_entries()
            .iter()
            .find(|e| e.id == self.id)
            .and_then(|e| e.cancel_reason)
    }
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        self.registry.lock_entries().retain(|e| e.id != self.id);
        self.registry.inner.released.notify_all();
    }
}

/// The process-shared lease registry. The engine, the CLI's sub-agent
/// runs, the reload path and the turn-executor registry all read this one
/// instance; tests that assert counts use their own `TurnLeaseRegistry`.
pub fn shared_turn_leases() -> &'static TurnLeaseRegistry {
    static SHARED: std::sync::OnceLock<TurnLeaseRegistry> = std::sync::OnceLock::new();
    SHARED.get_or_init(TurnLeaseRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(
        runner: &Arc<ExtensionRunner>,
        agent: &str,
        depth: u32,
        cancel: CancellationToken,
        dylib_exposed: bool,
        generation: u64,
    ) -> TurnLeaseSpec {
        TurnLeaseSpec {
            key: TurnLeaseKey {
                agent: agent.to_string(),
                generation,
                executor: "builtin".to_string(),
            },
            depth,
            cancel,
            dylib_exposed,
            scope: Some(Arc::downgrade(runner)),
        }
    }

    #[test]
    fn acquire_and_drop_track_in_flight_per_executor() {
        let registry = TurnLeaseRegistry::new();
        let runner = Arc::new(ExtensionRunner::new());
        let a = registry.acquire(spec(&runner, "a", 1, CancellationToken::new(), false, 0));
        let b = registry.acquire(spec(&runner, "b", 1, CancellationToken::new(), false, 0));
        assert_eq!(registry.in_flight_for_executor("builtin"), 2);
        assert_eq!(registry.in_flight_for_executor("other"), 0);
        drop(a);
        assert_eq!(registry.in_flight_for_executor("builtin"), 1);
        drop(b);
        assert_eq!(registry.in_flight_for_executor("builtin"), 0);
    }

    /// Drain cancels deepest first, latches the reason before the token
    /// fires, and waits for each depth's release before the next.
    #[test]
    fn drain_cancels_deepest_first_and_waits_for_release() {
        let registry = TurnLeaseRegistry::new();
        let runner = Arc::new(ExtensionRunner::new());
        let order: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let mut holders = Vec::new();
        for depth in [1u32, 2] {
            let token = CancellationToken::new();
            let lease = registry.acquire(spec(
                &runner,
                &format!("agent-{depth}"),
                depth,
                token.clone(),
                false,
                0,
            ));
            let order = Arc::clone(&order);
            holders.push(std::thread::spawn(move || {
                while !token.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(2));
                }
                // The reason was latched before the token fired.
                assert_eq!(lease.cancel_reason(), Some("test reload"));
                order.lock().unwrap().push(depth);
                drop(lease);
            }));
        }
        let outcome = registry.drain(
            &runner,
            |_, depth| depth >= 1,
            "test reload",
            Duration::from_secs(5),
        );
        for holder in holders {
            holder.join().expect("holder thread");
        }
        assert!(outcome.outstanding.is_empty(), "{outcome:?}");
        let agents: Vec<&str> = outcome.drained.iter().map(|k| k.agent.as_str()).collect();
        assert_eq!(agents, vec!["agent-2", "agent-1"], "children first");
        assert_eq!(*order.lock().unwrap(), vec![2, 1]);
    }

    /// A lease that never releases: cancelled + reported outstanding when
    /// the bound elapses; the registry still counts it until its holder
    /// drops it.
    #[test]
    fn drain_times_out_and_reports_outstanding() {
        let registry = TurnLeaseRegistry::new();
        let runner = Arc::new(ExtensionRunner::new());
        let token = CancellationToken::new();
        let lease = registry.acquire(spec(&runner, "stuck", 1, token.clone(), false, 0));
        let outcome = registry.drain(
            &runner,
            |_, depth| depth >= 1,
            "test reload",
            Duration::from_millis(50),
        );
        assert!(token.is_cancelled(), "the drain still cancels");
        assert_eq!(lease.cancel_reason(), Some("test reload"));
        assert!(outcome.drained.is_empty(), "{outcome:?}");
        assert_eq!(
            outcome.outstanding,
            vec![TurnLeaseKey {
                agent: "stuck".to_string(),
                generation: 0,
                executor: "builtin".to_string(),
            }]
        );
        assert_eq!(
            registry.in_flight_for_executor("builtin"),
            1,
            "the lease is held until its holder drops it"
        );
        drop(lease);
        assert_eq!(registry.in_flight_for_executor("builtin"), 0);
    }

    /// Depth 0 (the main turn — initiator or absent) and other scopes are
    /// never drained by a scope's reload.
    #[test]
    fn drain_skips_depth_zero_and_other_scopes() {
        let registry = TurnLeaseRegistry::new();
        let runner = Arc::new(ExtensionRunner::new());
        let other = Arc::new(ExtensionRunner::new());
        let main_token = CancellationToken::new();
        let foreign_token = CancellationToken::new();
        let _main = registry.acquire(spec(&runner, "main", 0, main_token.clone(), true, 0));
        let _foreign = registry.acquire(spec(
            &other,
            "other-child",
            1,
            foreign_token.clone(),
            false,
            0,
        ));
        let outcome = registry.drain(
            &runner,
            |_, depth| depth >= 1,
            "test reload",
            Duration::from_millis(20),
        );
        assert!(
            outcome.drained.is_empty() && outcome.outstanding.is_empty(),
            "{outcome:?}"
        );
        assert!(!main_token.is_cancelled());
        assert!(!foreign_token.is_cancelled());
    }

    /// The dlclose gate: only dylib-exposed leases assembled under a
    /// retired generation of THIS runner defer `drop_pending`.
    #[test]
    fn stale_dylib_holders_counts_retired_generation_only() {
        let registry = TurnLeaseRegistry::new();
        let runner = Arc::new(ExtensionRunner::new());
        let other = Arc::new(ExtensionRunner::new());

        let stale = registry.acquire(spec(&runner, "main", 0, CancellationToken::new(), true, 3));
        assert_eq!(registry.stale_dylib_holders(&runner, 4), 1);
        assert_eq!(
            registry.stale_dylib_holders(&runner, 3),
            0,
            "same generation is current"
        );
        drop(stale);
        assert_eq!(registry.stale_dylib_holders(&runner, 4), 0);

        let sub = registry.acquire(spec(&runner, "sub", 1, CancellationToken::new(), false, 3));
        assert_eq!(
            registry.stale_dylib_holders(&runner, 4),
            0,
            "sub-agents declare no dylib exposure (5b)"
        );
        drop(sub);

        let foreign = registry.acquire(spec(
            &other,
            "foreign",
            0,
            CancellationToken::new(),
            true,
            1,
        ));
        assert_eq!(
            registry.stale_dylib_holders(&runner, 4),
            0,
            "another runner's lease is out of scope"
        );
        drop(foreign);
    }
}
