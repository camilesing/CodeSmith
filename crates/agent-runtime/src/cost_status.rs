//! Process-wide cost-accrual side-channel (#526).
//!
//! Background LLM calls outside the main turn-complete path
//! (compaction summaries, seam recompaction, cycle briefings) used
//! to drop their token usage on the floor — the dashboard's
//! session-cost only saw the parent turn's tokens, so a long
//! session that triggered compaction or cycle-restart under-reported
//! cost by however many tokens those background calls consumed.
//!
//! Background callers call [`report`] after each `client.create_message`;
//! the REPL's turn-complete handler calls [`drain`] and folds the taken
//! amount into the session's background (sub-agent) cost bucket, which
//! persists via `SessionCostSnapshot`.
//!
//! Single-consumer resolution: the one draining consumer is the REPL's
//! turn-complete fold, and the CLI runs one session per process — the
//! "provably single-consumer" arm of the original precondition. Flows
//! outside any session (doctor probes) accrue to the same pool and are
//! drained by the next turn-complete of the process that ran them.
//! App-server hosts many engines per process and deliberately do **not**
//! drain: their reports accumulate in the pool exactly as they did when
//! it was write-only (app-server surfaces usage through its own event
//! accounting, not this side-channel). If that ever needs a drain, the
//! pool must first be keyed by session/engine id — do not add a second
//! draining consumer without that.
//!
//! Why a side-channel and not a plumbed callback: the leaky callers
//! (`compaction.rs`, `seam_manager.rs`, `cycle_manager.rs`) are
//! engine-internal machinery without a direct handle to app state
//! or the engine's event channel. A side-channel keeps the change
//! surface tiny — one new `report` line per call site — and any
//! future background caller (summarizers, retrieval helpers) gets
//! accrued for free without further plumbing.

use std::sync::{Mutex, OnceLock};

use crate::models::Usage;
use crate::pricing::CostEstimate;

static PENDING: OnceLock<Mutex<CostEstimate>> = OnceLock::new();

fn cell() -> &'static Mutex<CostEstimate> {
    PENDING.get_or_init(|| Mutex::new(CostEstimate::default()))
}

/// Background callers report their LLM usage here. Computes the
/// cost via [`crate::pricing::calculate_turn_cost_estimate_from_usage`] and
/// adds it to the pending pool. Cheap; takes a short-lived lock
/// and returns. No-op on models the pricing table doesn't know.
pub fn report(model: &str, usage: &Usage) {
    let Some(cost) = crate::pricing::calculate_turn_cost_estimate_from_usage(model, usage) else {
        // A pricing-table miss makes this accrual a silent no-op — the
        // exact under-reporting leak (#526) this module exists to prevent.
        // Surface the coverage hole so a newly added model can't silently
        // reintroduce it (background calls are rare, so per-miss logging
        // is low-frequency).
        tracing::warn!(
            model,
            "cost_status: model missing from the pricing table; background usage not accrued"
        );
        return;
    };
    if !cost.is_positive() {
        return;
    }
    // Poison recovery instead of `if let Ok`: dropping the Err arm would
    // silently swallow accrued cost for the process lifetime after any
    // future panic while the lock is held (the module exists to prevent
    // exactly that class of silent loss). `CostEstimate` carries no
    // invariant the guard could violate.
    let mut pending = cell().lock().unwrap_or_else(|e| e.into_inner());
    pending.usd += cost.usd;
    pending.cny += cost.cny;
}

/// Take the whole pending pool, resetting it to zero. The single draining
/// consumer is the REPL's turn-complete fold (see the module docs); tests
/// may also take it.
pub fn drain() -> CostEstimate {
    let mut pending = cell().lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *pending)
}

/// Reset the pool to zero without consuming. Test-only helper for
/// suites that share the static and need to start from a known
/// state.
#[cfg(test)]
pub fn reset_for_tests() {
    let mut pending = cell().lock().unwrap_or_else(|e| e.into_inner());
    *pending = CostEstimate::default();
}

/// Cross-suite serialization for tests that touch the process-wide pool.
/// The whole test binary runs in parallel while `PENDING` is shared, so any
/// suite whose machinery transitively calls [`report`] (compaction /
/// cycle-manager / purge tests with stub clients that return usage) must
/// hold this guard around the reporting span, joining the same domain as
/// this module's own tests — a module-local lock cannot see them.
#[cfg(test)]
pub(crate) fn cost_pool_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_usage() -> Usage {
        Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        }
    }

    #[test]
    fn report_adds_to_pool_and_drain_returns_then_resets() {
        let _g = cost_pool_test_lock();
        reset_for_tests();
        report("deepseek-v4-flash", &small_usage());
        let first = drain();
        assert!(first.usd > 0.0, "expected positive USD cost, got {first:?}");
        assert!(first.cny > 0.0, "expected positive CNY cost, got {first:?}");
        let second = drain();
        assert_eq!(
            second,
            CostEstimate::default(),
            "draining the pool must reset it"
        );
    }

    #[test]
    fn report_skips_unknown_models() {
        let _g = cost_pool_test_lock();
        reset_for_tests();
        // NIM-hosted models intentionally have no DeepSeek pricing.
        report("deepseek-ai/deepseek-v4-pro", &small_usage());
        assert_eq!(drain(), CostEstimate::default());
    }

    #[test]
    fn report_accumulates_across_multiple_calls() {
        let _g = cost_pool_test_lock();
        reset_for_tests();
        report("deepseek-v4-flash", &small_usage());
        report("deepseek-v4-flash", &small_usage());
        let total = drain();
        // Two equal reports — total must be 2× a single report.
        let single = crate::pricing::calculate_turn_cost_estimate_from_usage(
            "deepseek-v4-flash",
            &small_usage(),
        )
        .unwrap();
        assert!((total.usd - 2.0 * single.usd).abs() < 1e-12);
        assert!((total.cny - 2.0 * single.cny).abs() < 1e-12);
    }
}
