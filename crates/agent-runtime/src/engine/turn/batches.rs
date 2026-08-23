//! Turn-loop tool phase: plan the parsed tool calls into batches and execute
//! them (seam 3, per-tool dispatch).
//!
//! Split out of `engine/host_executor.rs` (codebase-health §1, step 1),
//! together with `stream.rs`. [`HostAgentExecutor::execute_tool_batches`] runs
//! the four phases that used to live inline in `run_inner`: planning
//! (loop-guard `record_attempt`, speculative early-task pops, per-input
//! approval gating), batch classification via `plan_tool_execution_batches`
//! (the production classifier in `engine::dispatch`), per-batch dispatch
//! (`Parallel` batches concurrently via `FuturesUnordered`, `Serial` batches
//! one-by-one with approval gating), and the sequential post-batch pass
//! (loop-guard outcomes, LSP collect, read-file observe, error taxonomy,
//! `ToolResult` transcript push). The method was extracted verbatim — the
//! parameters are exactly the step-loop locals this phase reads or mutates.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;

use codesmith_agent::callback::Callback;
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message};
use codesmith_agent::tools::{Tool, ToolCapability, ToolError, ToolResult, ToolSet};

use super::approval::requires_approval;
use super::stream::{EarlyToolTask, early_start_safe};
use crate::engine::dispatch::{ToolExecutionBatch, ToolExecutionPlan, plan_tool_execution_batches};
use crate::engine::host_executor::HostAgentExecutor;
use crate::engine::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
use crate::error_taxonomy::{ErrorCategory, ErrorEnvelope};
use crate::tools::spec::ApprovalRequirement;

/// The `ToolResult` fed back when the loop-guard blocks an identical repeat
/// call (mirrors `handle_deepseek_turn`'s `loop_guard_block_tool_result`). Duplicated here
/// rather than imported to keep this slice additive — zero production call-site
/// changes; a later cleanup can lift it into `loop_guard` proper as the single
/// source of truth.
fn block_tool_result(message: String) -> ToolResult {
    ToolResult::error(message).with_metadata(serde_json::json!({
        "loop_guard": "identical_tool_call"
    }))
}

/// Per-tool dispatch outcome carried from the batch-dispatch phase to the
/// sequential post-batch phase (slice 40 §E — seam-3 parallel dispatch).
/// `blocked` marks loop-guard interventions (a guard-blocked call records no
/// `record_outcome` / LSP / read-file, mirroring the prior sequential loop's
/// `!blocked` guard). Local struct instead of `dispatch::ToolExecOutcome` to
/// avoid that type's unused `started_at` / `context_patch` fields and to carry
/// the `blocked` flag the post-batch pass needs.
struct DispatchedTool {
    index: usize,
    id: String,
    name: String,
    input: serde_json::Value,
    result: Result<ToolResult, ToolError>,
    blocked: bool,
}

impl HostAgentExecutor {
    /// Execute the parsed tool calls and feed each result back as a
    /// `role:"user"` `ToolResult` block (Anthropic/OpenAI-compat shape).
    ///
    /// (3) per-tool seam — ✅ loop-guard; ✅ approval; ✅ early-tool-start
    /// (reuse a speculatively-started task spawned at `ContentBlockStop`
    /// during streaming if the args still match; otherwise abort + run
    /// fresh); ✅ LSP post-edit collect; ✅ parallel dispatch (slice 40 §E).
    /// `plan_tool_execution_batches` groups consecutive parallel-safe
    /// (read-only, no-approval) tool_uses into a single `Parallel` batch
    /// run concurrently via `FuturesUnordered`; each unsafe tool becomes
    /// its own `Serial` batch (approval / write / blocked). Outcomes are
    /// index-preserving (a pre-allocated array written by `plan.index`),
    /// and `record_outcome` / LSP / read-file / error-escalation / push
    /// `ToolResult` are deferred to a sequential post-batch pass.
    /// `on_tool_start`/`on_tool_end` fire per-batch LIFO (starts in index
    /// order before dispatch, ends in reverse order after) so the
    /// `CallbackBridge`'s pending-stack pairing stays correct. Deferred:
    /// `multi_tool_use.parallel` parsing (host concern — the framework
    /// executor receives flat `tool_uses` from `reduce_stream`) and
    /// `tool_exec_lock` (unnecessary for single-loop dispatch — a
    /// `Parallel` batch drains before the next `Serial` batch starts).
    /// `loop_guard_halt` is per-step: a halt short-circuits the tool loop
    /// and the whole turn at the (4) seam in `run_inner`.
    // Signature mirrors `run_inner`'s step locals 1:1 (behavior-preserving
    // extraction); collapsing into a params struct is left for a later pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_tool_batches(
        &self,
        history: &mut dyn ChatHistory,
        tool_uses: Vec<(String, String, serde_json::Value)>,
        early_tasks: &mut HashMap<String, EarlyToolTask>,
        loop_guard: &mut LoopGuard,
        tools: &Arc<ToolSet>,
        callback: &Arc<dyn Callback>,
        extension: &Option<Arc<codesmith_extensions::ExtensionRunner>>,
        intent_summary: &Option<String>,
        step_error_count: &mut usize,
        step_error_categories: &mut Vec<ErrorCategory>,
    ) -> Option<String> {
        let mut loop_guard_halt: Option<String> = None;
        let n = tool_uses.len();

        // --- Phase 1: planning (sequential) — build a `ToolExecutionPlan`
        // per tool_use, pop speculative `early_tasks`, and run the loop-guard
        // `record_attempt` (the guard is per-tool, in order, so deferring it
        // would mis-count identical calls). `early_for_plan` / `tool_for_plan`
        // are parallel arrays keyed by `plan.index` — the `ToolExecutionPlan`
        // struct's own `blocked_error` field is left `None` (the framework
        // executor tracks speculative early-start tasks in its own distinct
        // `EarlyToolTask` type + `early_tasks` map, not on the plan).
        let mut plans: Vec<ToolExecutionPlan> = Vec::with_capacity(n);
        let mut early_for_plan: Vec<Option<EarlyToolTask>> = Vec::with_capacity(n);
        let mut tool_for_plan: Vec<Option<Arc<dyn Tool>>> = Vec::with_capacity(n);
        for (i, (id, name, input)) in tool_uses.into_iter().enumerate() {
            // loop-guard: block the 3rd identical (name+args) call this turn.
            let guard_result = match loop_guard.record_attempt(&name, &input) {
                AttemptDecision::Block(message) => {
                    // Abort any speculatively-started task — the call
                    // won't execute (Drop aborts the `JoinHandle`).
                    early_tasks.remove(&id);
                    Some(block_tool_result(message))
                }
                AttemptDecision::Proceed => None,
            };
            // Pop the speculative early-start task (if any) for reuse / abort
            // at dispatch time.
            let early_task = early_tasks.remove(&id);
            let tool = tools.get(&name).cloned();
            let caps: Vec<ToolCapability> =
                tool.as_ref().map(|t| t.capabilities()).unwrap_or_default();
            let read_only = caps.contains(&ToolCapability::ReadOnly);
            // Per-input approval override (mirrors `request_approval`'s
            // own logic in `turn::approval`): a host dispatcher's
            // `Required` / `Suggest` downgrades/upgrades the gate per
            // input; `Auto` or `None` falls back to the static capability gate.
            let approval_required = match self
                .tool_dispatcher
                .as_ref()
                .and_then(|d| d.approval_requirement_for(&name, &input))
            {
                Some(req) => req != ApprovalRequirement::Auto,
                None => requires_approval(&caps),
            };
            plans.push(ToolExecutionPlan {
                index: i,
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                caller: None,
                interactive: false,
                approval_required,
                approval_description: String::new(),
                // Framework `Tool` doesn't expose `supports_parallel` /
                // `interactive` directly; assume `true` / `false` so the
                // classifier's predicate reduces to `read_only &&
                // !approval_required` — the same gate as `early_start_safe`.
                supports_parallel: true,
                read_only,
                stream_early_start_safe: early_start_safe(&caps),
                blocked_error: None,
                guard_result: guard_result.clone(),
            });
            early_for_plan.push(early_task);
            tool_for_plan.push(tool);
        }

        // --- Phase 2: batch classification (reuses the production classifier).
        let batches = plan_tool_execution_batches(plans);

        // --- Phase 3: per-batch dispatch. A `Parallel` batch runs its plans
        // concurrently via `FuturesUnordered` (each future is `'static` — it
        // owns an `Arc<dyn Tool>` clone, matching the early-start spawn
        // site's `async move { tool.run(input).await }` pattern); a `Serial`
        // batch runs one tool with approval gating (borrows `&self`).
        let mut outcomes: Vec<Option<DispatchedTool>> = (0..n).map(|_| None).collect();
        for batch in batches {
            match batch {
                ToolExecutionBatch::Parallel(batch_plans) => {
                    // `on_tool_start` in index order before dispatch (LIFO
                    // push — `CallbackBridge` stashes each on its pending
                    // stack).
                    // §F2b T1 — honor `Block` at `ToolCall` (parallel arm):
                    // record per-plan block reasons here, then skip dispatch
                    // for those indices (mirrors the loop-guard blocked path).
                    let mut ext_blocks: HashMap<usize, String> = HashMap::new();
                    for plan in &batch_plans {
                        callback
                            .on_tool_start(&plan.id, &plan.name, &plan.input)
                            .await;
                        if let Some(runner) = &extension {
                            let out = runner
                                .emit(codesmith_agent::extension::ExtensionEvent::ToolCall(
                                    codesmith_agent::extension::ToolCallEvent {
                                        id: plan.id.clone(),
                                        name: plan.name.clone(),
                                        input: plan.input.clone(),
                                    },
                                ))
                                .await;
                            if let codesmith_agent::extension::HandlerOutcome::Block { reason } =
                                out.outcome
                            {
                                ext_blocks.insert(plan.index, reason);
                            }
                        }
                    }
                    let mut futs: FuturesUnordered<
                        Pin<Box<dyn Future<Output = DispatchedTool> + Send>>,
                    > = FuturesUnordered::new();
                    for plan in &batch_plans {
                        let tool = tool_for_plan[plan.index]
                            .clone()
                            .expect("parallel-safe plan has a registered read-only tool");
                        let early = early_for_plan[plan.index].take();
                        let guard = plan.guard_result.clone();
                        // §F2b T1 — the per-plan extension Block reason
                        // (if a handler blocked this `ToolCall`), cloned
                        // into the `'static` future.
                        let ext_block_reason = ext_blocks.get(&plan.index).cloned();
                        // §F2b T3 — capture the runner into the `'static`
                        // future so ToolExecutionStart/End can bracket the
                        // tool run from inside `async move`.
                        let ext = extension.clone();
                        let id = plan.id.clone();
                        let name = plan.name.clone();
                        let input = plan.input.clone();
                        let index = plan.index;
                        futs.push(Box::pin(async move {
                            let blocked = guard.is_some() || ext_block_reason.is_some();
                            let result = if let Some(g) = guard {
                                // Loop-guard blocked this call — don't run
                                // the tool (the speculative task was already
                                // aborted in planning).
                                Ok(g)
                            } else if let Some(reason) = ext_block_reason {
                                // §F2b T1 — an extension blocked this
                                // `ToolCall`; skip dispatch and feed back a
                                // blocked (failed) result (an intervention,
                                // not an execution failure — does not count
                                // toward the error-escalation halt).
                                Ok(ToolResult::error(reason))
                            } else {
                                // Early-tool-start reuse: await the
                                // speculatively-started task if the model
                                // didn't revise the args; otherwise abort
                                // (Drop) and run fresh.
                                // §F2b T3 — ToolExecutionStart/End bracket
                                // the actual tool run (mirrors the serial arm).
                                if let Some(runner) = &ext {
                                    let _ = runner
                                        .emit(
                                            codesmith_agent::extension::ExtensionEvent::ToolExecutionStart,
                                        )
                                        .await;
                                }
                                let r = match early {
                                    Some(mut early)
                                        if early.name == name
                                            && early.input == input =>
                                    {
                                        let handle = early
                                            .handle
                                            .take()
                                            .expect("handle present until consumed");
                                        match handle.await {
                                            Ok(result) => result,
                                            Err(join_err) => Err(ToolError::execution_failed(
                                                format!(
                                                    "Early tool execution task failed: {join_err}"
                                                ),
                                            )),
                                        }
                                    }
                                    Some(_) => {
                                        // Args revised after the block closed
                                        // — the dropped `EarlyToolTask` (Drop
                                        // aborts) cleans up the orphaned task.
                                        tool.run(input.clone()).await
                                    }
                                    None => tool.run(input.clone()).await,
                                };
                                if let Some(runner) = &ext {
                                    let _ = runner
                                        .emit(
                                            codesmith_agent::extension::ExtensionEvent::ToolExecutionEnd,
                                        )
                                        .await;
                                }
                                r
                            };
                            DispatchedTool {
                                index,
                                id,
                                name,
                                input,
                                result,
                                blocked,
                            }
                        }));
                    }
                    // Index-preserving drain — completion order is
                    // irrelevant; each outcome is written at its `index`.
                    while let Some(outcome) = futs.next().await {
                        let index = outcome.index;
                        outcomes[index] = Some(outcome);
                    }
                    // `on_tool_end` in reverse index order (LIFO pop).
                    // §F2b T1 — honor `Transform` at `ToolResult`: emit
                    // BEFORE `on_tool_end` so the callback + downstream
                    // transcript see the (possibly transformed) result, and
                    // write the final result back into `outcomes` so Phase-4
                    // pushes the transformed (not original) `ToolResult`.
                    for plan in batch_plans.iter().rev() {
                        let original_result = outcomes[plan.index]
                            .as_ref()
                            .expect("outcome populated by the FuturesUnordered drain")
                            .result
                            .clone();
                        let final_result = if let Some(runner) = &extension {
                            let out = runner
                                .emit(codesmith_agent::extension::ExtensionEvent::ToolResult(
                                    codesmith_agent::extension::ToolResultEvent {
                                        id: plan.id.clone(),
                                        name: plan.name.clone(),
                                        result: original_result.clone(),
                                    },
                                ))
                                .await;
                            match out.event {
                                codesmith_agent::extension::ExtensionEvent::ToolResult(tr) => {
                                    tr.result
                                }
                                // Out-of-place transform (the terminal
                                // event is not `ToolResult`) → Continue.
                                _ => original_result,
                            }
                        } else {
                            original_result
                        };
                        callback.on_tool_end(&plan.name, &final_result).await;
                        outcomes[plan.index]
                            .as_mut()
                            .expect("outcome populated by the FuturesUnordered drain")
                            .result = final_result;
                    }
                }
                ToolExecutionBatch::Serial(plan) => {
                    let idx = plan.index;
                    callback
                        .on_tool_start(&plan.id, &plan.name, &plan.input)
                        .await;
                    // §F2b T1 — honor `Block` at `ToolCall` (serial arm):
                    // capture the block reason, then skip approval/`tool.run`
                    // below (mirrors the loop-guard blocked path — a failed
                    // result, not an execution error).
                    let ext_blocked_reason: Option<String> = if let Some(runner) = &extension {
                        let out = runner
                            .emit(codesmith_agent::extension::ExtensionEvent::ToolCall(
                                codesmith_agent::extension::ToolCallEvent {
                                    id: plan.id.clone(),
                                    name: plan.name.clone(),
                                    input: plan.input.clone(),
                                },
                            ))
                            .await;
                        if let codesmith_agent::extension::HandlerOutcome::Block { reason } =
                            out.outcome
                        {
                            Some(reason)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    // approval gate: a tool that requires approval is gated
                    // behind the decision channel; denied ⇒ the tool never
                    // runs and a `permission_denied` error is fed back so the
                    // model can react (turn continues). Order: loop-guard
                    // first (matches production), then approval, then
                    // early-start reuse.
                    let (result, blocked) = if let Some(guard) = &plan.guard_result {
                        (Ok(guard.clone()), true)
                    } else if let Some(reason) = ext_blocked_reason {
                        // §F2b T1 — extension blocked this `ToolCall`; skip
                        // approval/`tool.run` and feed back a blocked result.
                        (Ok(ToolResult::error(reason)), true)
                    } else {
                        // §F2b T3 — ToolExecutionStart/End bracket the
                        // serial dispatch (approval + run).
                        if let Some(runner) = &extension {
                            let _ = runner
                                .emit(
                                    codesmith_agent::extension::ExtensionEvent::ToolExecutionStart,
                                )
                                .await;
                        }
                        let r = match &tool_for_plan[idx] {
                            Some(tool) => {
                                match self
                                    .request_approval(
                                        &plan.id,
                                        &plan.name,
                                        &plan.input,
                                        tool,
                                        intent_summary,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        // Early-tool-start reuse (same shape
                                        // as the parallel arm, but sequential).
                                        match early_for_plan[idx].take() {
                                            Some(mut early)
                                                if early.name == plan.name
                                                    && early.input == plan.input =>
                                            {
                                                let handle = early
                                                    .handle
                                                    .take()
                                                    .expect("handle present until consumed");
                                                match handle.await {
                                                    Ok(result) => (result, false),
                                                    Err(join_err) => (
                                                        Err(ToolError::execution_failed(format!(
                                                            "Early tool execution task failed: {join_err}"
                                                        ))),
                                                        false,
                                                    ),
                                                }
                                            }
                                            Some(_) => {
                                                // Args revised after the block
                                                // closed — can't reuse; the
                                                // dropped `EarlyToolTask` (Drop
                                                // aborts) cleans up the orphaned
                                                // speculative task.
                                                (tool.run(plan.input.clone()).await, false)
                                            }
                                            None => (tool.run(plan.input.clone()).await, false),
                                        }
                                    }
                                    Err(denial) => {
                                        // Approval denied — abort any
                                        // speculative task (defensive:
                                        // early-start-safe tools don't
                                        // require approval, so this path has
                                        // none, but `Drop` is cheap).
                                        early_for_plan[idx].take();
                                        (Err(ToolError::permission_denied(denial)), false)
                                    }
                                }
                            }
                            None => {
                                // No tool registered — abort any speculative
                                // task (`reduce_stream` only spawns for
                                // registered tools, so this is defensive).
                                early_for_plan[idx].take();
                                (
                                    Err(ToolError::NotAvailable {
                                        message: format!("no tool named '{}'", plan.name),
                                    }),
                                    false,
                                )
                            }
                        };
                        // §F2b T3 — ToolExecutionEnd brackets the
                        // serial dispatch.
                        if let Some(runner) = &extension {
                            let _ = runner
                                .emit(codesmith_agent::extension::ExtensionEvent::ToolExecutionEnd)
                                .await;
                        }
                        r
                    };
                    // §F2b T1 — honor `Transform` at `ToolResult`: emit
                    // BEFORE `on_tool_end` so the callback + downstream
                    // transcript see the (possibly transformed) result;
                    // write the final result into `outcomes` so Phase-4
                    // pushes the transformed `ToolResult`.
                    let final_result = if let Some(runner) = &extension {
                        let out = runner
                            .emit(codesmith_agent::extension::ExtensionEvent::ToolResult(
                                codesmith_agent::extension::ToolResultEvent {
                                    id: plan.id.clone(),
                                    name: plan.name.clone(),
                                    result: result.clone(),
                                },
                            ))
                            .await;
                        match out.event {
                            codesmith_agent::extension::ExtensionEvent::ToolResult(tr) => tr.result,
                            // Out-of-place transform → Continue.
                            _ => result,
                        }
                    } else {
                        result
                    };
                    callback.on_tool_end(&plan.name, &final_result).await;
                    outcomes[idx] = Some(DispatchedTool {
                        index: idx,
                        id: plan.id.clone(),
                        name: plan.name.clone(),
                        input: plan.input.clone(),
                        result: final_result,
                        blocked,
                    });
                }
            }
        }

        // --- Phase 4: post-batch processing (sequential, index order).
        // `record_outcome` / LSP collect / read-file observe /
        // error-escalation / push `ToolResult` are deferred to here so they
        // run after every concurrent batch has drained — behavior-preserving
        // w.r.t. the prior sequential loop (the loop-guard's failure halt is
        // checked at the (4) seam below, after the tool loop, so deferring
        // `record_outcome` to this pass does not change the halt decision).
        let ordered: Vec<DispatchedTool> = outcomes
            .into_iter()
            .map(|o| o.expect("every plan slot filled by the batch dispatch"))
            .collect();
        for o in &ordered {
            let blocked = o.blocked;
            // loop-guard: track consecutive failures of this tool (warn at
            // 3, halt at 8). A guard-blocked call records no outcome — it
            // is an intervention, not an execution, so it doesn't count
            // toward the failure halt.
            if !blocked {
                let success = o.result.as_ref().map(|r| r.success).unwrap_or(false);
                match loop_guard.record_outcome(&o.name, success) {
                    OutcomeDecision::Continue => {}
                    OutcomeDecision::Warn(message) => {
                        tracing::warn!("{}", message);
                        self.emit_status(message).await;
                    }
                    OutcomeDecision::Halt(message) => {
                        loop_guard_halt.get_or_insert(message);
                    }
                }
            }

            // (3) per-tool seam — loop-guard (absorbed); ✅ LSP post-edit
            // collect (only on a successful, non-blocked edit — mirrors
            // production `output.success && tool_was_executed`); ✅ read_file
            // observe (records the compacted/sanitized output into
            // `recent_read_files` for post-compaction reinject); ✅ parallel
            // dispatch (slice 40 §E — post-batch).
            if !blocked {
                if let Ok(r) = &o.result {
                    if r.success {
                        self.collect_lsp_diagnostics(&o.name, &o.input).await;
                        self.record_read_file_result(&o.name, &o.input, r);
                    }
                } else if let Err(e) = &o.result {
                    // Error-escalation tracking (slice 34 §E): categorize a
                    // dispatch error via the shared taxonomy (the retired
                    // `handle_deepseek_turn`). Only `Err(ToolError)` counts —
                    // `Ok(ToolResult { success: false })` is a failed result,
                    // not a dispatch error (faithful to production).
                    let envelope: ErrorEnvelope = e.clone().into();
                    *step_error_count += 1;
                    step_error_categories.push(envelope.category);
                }
            }

            let (content_str, is_error) = match &o.result {
                Ok(r) => (r.content.clone(), !r.success),
                Err(e) => (format!("Error: {e}"), true),
            };
            history.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: o.id.clone(),
                    content: content_str,
                    is_error: Some(is_error),
                    content_blocks: None,
                }],
            });
        }

        // Abort any speculatively-started tasks that weren't consumed by
        // the tool loop (e.g. a tool block completed during streaming but
        // didn't survive into the parsed `tool_uses`, or an args-mismatch
        // path left an orphaned task). `Drop` on each `EarlyToolTask`
        // aborts the spawned task. In normal operation every spawned task
        // is consumed or aborted within the loop, so this is defensive.
        early_tasks.clear();

        loop_guard_halt
    }
}
