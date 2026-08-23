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
//!
//! Later steps of the split will add `approval`, `seams`, and `postprocess`.
//! The submodules are private; the items `host_executor` still needs cross
//! over through the `pub(crate)` re-exports below.
//!
//! Not to be confused with the crate-root [`crate::turn`] module (host-side
//! turn bookkeeping) — this one is `engine`'s internal turn-loop
//! decomposition.

mod batches;
mod stream;

pub(crate) use stream::{EarlyToolTask, StreamRoundOutcome};
