//! Turn-loop phase modules split out of `host_executor.rs`
//! (codebase-health §1, step 1).
//!
//! The per-step machinery of the turn loop is organized by phase so that
//! `host_executor.rs` keeps the step loop itself plus the cross-cutting
//! guardrails it owns directly:
//!
//! - [`stream`] — inline stream reduction, transparent retry, and early
//!   speculative tool dispatch (`reduce_stream` + early-tool-start).
//! - [`batches`] — tool-call planning and batch execution
//!   (`execute_tool_batches`).
//! - [`approval`] — the per-tool approval gate (`request_approval` + the
//!   static capability gate / intent-summary helpers).
//! - [`seams`] — the cross-cutting seams: the cancel-token checkpoint helper
//!   and the steer push / drain methods (the checkpoint map lives in its
//!   module docs).
//! - [`postprocess`] — the post-stream tail helpers: the sub-agent reaping
//!   gate + sentinel builder, the thinking-only emit predicate, and the LSP
//!   diagnostics collect / flush pair (the drain / hold control flow stays
//!   inline in `run_inner`).
//!
//! The submodules are private; the items `host_executor` still needs cross
//! over through the `pub(crate)` re-exports below.
//!
//! Not to be confused with the crate-root [`crate::turn`] module (host-side
//! turn bookkeeping) — this one is `engine`'s internal turn-loop
//! decomposition.

mod approval;
mod batches;
mod postprocess;
mod seams;
mod stream;

pub(crate) use approval::approval_intent_summary;
pub(crate) use postprocess::{
    should_emit_thinking_only_status, should_hold_turn_for_subagents,
    subagent_completion_runtime_message,
};
pub(crate) use stream::{EarlyToolTask, StreamRoundOutcome};
