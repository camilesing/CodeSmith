//! Process-wide cost-accrual side-channel (#526).
//!
//! Background LLM calls outside the main turn-complete path
//! (compaction summaries, seam recompaction, cycle briefings) used
//! to drop their token usage on the floor — the dashboard's
//! session-cost only saw the parent turn's tokens, so a long
//! session that triggered compaction or cycle-restart under-reported
//! cost by however many tokens those background calls consumed.
//!
//! Background callers call [`report`] after each `client.create_message`.
//!
//! Known limitation: no production consumer drains the pool yet. The
//! TUI render loop that was to fold the pending amount into session
//! cost was removed in 0.9.0, so the pool is currently write-only —
//! reported amounts accumulate but do not reach any display. The
//! `report` call sites are the accrual hooks for whenever a consumer
//! lands; until then they keep the usage observable in one place
//! instead of scattered (or dropped) across callers.
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
        return;
    };
    if !cost.is_positive() {
        return;
    }
    if let Ok(mut pending) = cell().lock() {
        pending.usd += cost.usd;
        pending.cny += cost.cny;
    }
}

/// Take the pending cost, resetting the pool to zero. Test-only:
/// production has no drain consumer yet — see the module docs.
#[cfg(test)]
fn take_for_tests() -> CostEstimate {
    let mut pending = cell().lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *pending)
}

/// Reset the pool to zero without consuming. Test-only helper for
/// suites that share the static and need to start from a known
/// state.
#[cfg(test)]
pub fn reset_for_tests() {
    if let Ok(mut pending) = cell().lock() {
        *pending = CostEstimate::default();
    }
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

    /// Tests run in parallel and share the static — serialize the
    /// ones that touch the pool through this mutex so concurrent
    /// `report`/`drain` doesn't make assertions racy.
    fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn report_adds_to_pool_and_take_returns_then_resets() {
        let _g = serial_lock();
        reset_for_tests();
        report("deepseek-v4-flash", &small_usage());
        let first = take_for_tests();
        assert!(first.usd > 0.0, "expected positive USD cost, got {first:?}");
        assert!(first.cny > 0.0, "expected positive CNY cost, got {first:?}");
        let second = take_for_tests();
        assert_eq!(
            second,
            CostEstimate::default(),
            "taking the pool must reset it"
        );
    }

    #[test]
    fn report_skips_unknown_models() {
        let _g = serial_lock();
        reset_for_tests();
        // NIM-hosted models intentionally have no DeepSeek pricing.
        report("deepseek-ai/deepseek-v4-pro", &small_usage());
        assert_eq!(take_for_tests(), CostEstimate::default());
    }

    #[test]
    fn report_accumulates_across_multiple_calls() {
        let _g = serial_lock();
        reset_for_tests();
        report("deepseek-v4-flash", &small_usage());
        report("deepseek-v4-flash", &small_usage());
        let total = take_for_tests();
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
