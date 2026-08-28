//! Turn-loop approval gating, split out of `host_executor.rs`
//! (codebase-health §1).
//!
//! Write / code-execution tools are gated behind user permission
//! ([`HostAgentExecutor::request_approval`], seam 3): before running such a
//! tool, the executor emits `Event::ApprovalRequired` (carrying the two
//! fingerprint keys the host uses for approve-for-session / deny-exact dedup,
//! plus the model's intent summary for write tools) and blocks on the
//! approval-decision channel, matching by wire tool id — stale decisions for
//! other ids are dropped — mirroring `handle_deepseek_turn`'s per-tool
//! approval flow. A denied call never runs the tool and feeds back a
//! `permission_denied` error so the model can react (the turn continues). The
//! receiver is
//! `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>` — the
//! first guardrail to use a `tokio::sync::Mutex` (rather than
//! `std::sync::Mutex` like steer / LSP), because the guard must cross the
//! blocking `recv().await` (a std mutex guard isn't `Send`).
//!
//! Approval requirement is derived statically from [`Tool::capabilities`]
//! ([`requires_approval`]; `ExecutesCode` / `WritesFiles` /
//! `RequiresApproval`) — the framework `Tool` trait deliberately carries no
//! per-input approval surface (§E design note); the dynamic override threads
//! in at the wire-in step via the `tool_dispatcher` field. The intent summary
//! shown in the approval view is lifted from the step's text by
//! [`approval_intent_summary`]. Call sites: `turn::batches` (the serial batch
//! approval path) and `run_inner` (the intent-summary lift before the content
//! blocks are moved into `tool_uses`).
//!
//! ## Known gaps in approval (by design)
//!
//! - **cancel-token race** ✅ — production's `await_tool_approval` selects over
//!   `cancel_token.cancelled()` so a cancelled turn breaks out of the approval
//!   wait. This executor now mirrors it: `request_approval`'s `recv().await`
//!   loop is wrapped in a `biased select!` over the cancel token — cancel wins
//!   ⇒ `Err("Request cancelled while awaiting approval")` (fed back as a tool
//!   error; Checkpoint G then surfaces `StopReason::Interrupted`). See the
//!   checkpoint map in `turn::seams`.
//! - **static-only approval derivation** — [`requires_approval`] checks
//!   [`Tool::capabilities`] (`ExecutesCode` / `WritesFiles` /
//!   `RequiresApproval`), mirroring `ToolSpec::approval_requirement()`'s default.
//!   The per-input dynamic override (`ToolSpec::approval_requirement_for_input`,
//!   e.g. `exec_shell rm` ⇒ `Required` vs `exec_shell ls` ⇒ `Auto`) and any
//!   `ToolSpec::approval_requirement()` overrides that declare none of these
//!   capabilities are not visible from the framework `Tool` trait; they thread in
//!   at the wire-in step when the executor consults a `ToolDispatcher`.
//! - **`RetryWithPolicy` treated as `Approved`** — sandbox elevation needs
//!   `ToolDispatcher::execute` with a `sandbox_override` (the host rebuilds the
//!   tool context with elevated sandbox access), which the framework `Tool::run`
//!   path doesn't carry (the `ToolSpecAdapter` runs with a fixed `ToolContext`).
//!   The executor therefore runs the tool with the unchanged context on a
//!   `RetryWithPolicy` decision; the elevation threads in at the wire-in step.

use std::sync::Arc;

use codesmith_agent::models::ContentBlock;
use codesmith_agent::tools::{Tool, ToolCapability};

use crate::engine::approval::ApprovalDecision;
use crate::engine::host_executor::HostAgentExecutor;
use crate::events::Event;
use crate::tools::approval_cache::{build_approval_grouping_key, build_approval_key};
use crate::tools::spec::ApprovalRequirement;

/// Whether a tool requires user approval before execution, derived from its
/// declared capabilities. Mirrors `ToolSpec::approval_requirement()`'s default
/// derivation (`ExecutesCode` ⇒ `Required`, `WritesFiles` ⇒ `Suggest`, both
/// `!= Auto` ⇒ gate) plus an explicit `RequiresApproval` capability — the
/// most faithful static approximation reachable from the framework `Tool`
/// trait (which deliberately carries no `approval_requirement_for_input`
/// surface; see the §E `ToolSpecAdapter` design note).
///
/// **By-design gap:** static only. Production also consults the per-input
/// `ToolSpec::approval_requirement_for_input` (e.g. `exec_shell rm` ⇒ Required
/// vs `exec_shell ls` ⇒ Auto) via the `ToolDispatcher`. That dynamic override
/// and any `ToolSpec::approval_requirement()` overrides that don't declare one
/// of these capabilities are not visible here; they thread in at the wire-in
/// step when the executor consults a `ToolDispatcher`.
pub(crate) fn requires_approval(caps: &[ToolCapability]) -> bool {
    caps.iter().any(|c| {
        matches!(
            c,
            ToolCapability::RequiresApproval
                | ToolCapability::ExecutesCode
                | ToolCapability::WritesFiles
        )
    })
}

/// Cap on the approval intent-summary length. (Relocated from the retired
/// `handle_deepseek_turn`'s `MAX_APPROVAL_INTENT_SUMMARY_CHARS` in the slice
/// 20 §E cutover; the `turn_loop::` original was deleted in slice 49.)
const APPROVAL_INTENT_SUMMARY_MAX_CHARS: usize = 2_000;

/// Extract the model's preceding text this step as an approval "intent summary"
/// — the *why* shown in the approval view before the *what*. Joins the step's
/// `Text` blocks and caps the length. (Relocated from the retired
/// `handle_deepseek_turn`'s `approval_intent_summary` in the slice 20 §E cutover;
/// the `turn_loop::` original was deleted in slice 49 — this is now the single
/// source.)
pub(crate) fn approval_intent_summary(content: &[ContentBlock]) -> Option<String> {
    let mut text = String::new();
    for block in content {
        if let ContentBlock::Text { text: t, .. } = block {
            text.push_str(t);
        }
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars();
    let mut summary: String = chars
        .by_ref()
        .take(APPROVAL_INTENT_SUMMARY_MAX_CHARS)
        .collect();
    if chars.next().is_some() {
        summary.push_str("...");
    }
    Some(summary)
}

impl HostAgentExecutor {
    /// Per-tool approval gate (seam 3). Returns `Ok(())` to proceed with
    /// execution (the tool doesn't require approval, no approval channel was
    /// supplied, or the user approved) or `Err(denial_message)` to skip the
    /// tool and feed back a `permission_denied` error so the model can react
    /// (mirrors `handle_deepseek_turn`'s per-tool approval flow,
    /// `handle_deepseek_turn`).
    ///
    /// The approval requirement is derived statically from [`Tool::capabilities`]
    /// (see [`requires_approval`]); the dynamic per-input override is a by-design
    /// gap. The executor emits `Event::ApprovalRequired` (carrying the two
    /// fingerprint keys the host uses for approve-for-session / deny-exact
    /// dedup, plus the model's intent summary for write tools) and then blocks
    /// on the decision channel, matching by wire tool id — stale decisions for
    /// other ids are dropped (mirrors production's `_ => continue`).
    pub(crate) async fn request_approval(
        &self,
        tool_id: &str,
        name: &str,
        input: &serde_json::Value,
        tool: &Arc<dyn Tool>,
        intent_summary: &Option<String>,
    ) -> Result<(), String> {
        let Some(rx) = &self.approval else {
            return Ok(()); // no approval channel ⇒ gating disabled
        };
        // Per-input approval override (slice 20 §E): when a host dispatcher is
        // attached, consult its `approval_requirement_for` first — a `Some`
        // answer downgrades/upgrades the gate per input (mirrors production's
        // `registry.approval_requirement_for(..)` at handle_deepseek_turn). `None`
        // (no dispatcher, or dispatcher has no opinion) falls back to the
        // static capability gate.
        let approval_required = match self
            .tool_dispatcher
            .as_ref()
            .and_then(|d| d.approval_requirement_for(name, input))
        {
            Some(req) => req != ApprovalRequirement::Auto,
            None => requires_approval(&tool.capabilities()),
        };
        if !approval_required {
            return Ok(()); // dispatcher said Auto, or static gate says no
        }
        let is_read_only = tool.capabilities().contains(&ToolCapability::ReadOnly);
        // Emit the approval request so a host UI can prompt and resolve. The
        // fingerprints are built here (not inside the await) and carried on the
        // event — the runtime emits them, the host owns the dedup sets (same
        // split as production).
        if let Some(tx) = &self.event_tx {
            let approval_key = build_approval_key(name, input).0;
            let approval_grouping_key = build_approval_grouping_key(name, input).0;
            let _ = tx
                .send(Event::ApprovalRequired {
                    id: tool_id.to_string(),
                    tool_name: name.to_string(),
                    description: tool.description().to_string(),
                    input: input.clone(),
                    approval_key,
                    approval_grouping_key,
                    intent_summary: if is_read_only {
                        None
                    } else {
                        intent_summary.clone()
                    },
                })
                .await;
        }
        // Block on the decision channel, matching by tool id. `tokio::sync::Mutex`
        // (not `std::sync::Mutex`) so the guard may cross the blocking
        // `recv().await`. Single consumer ⇒ holding the guard across the await
        // cannot deadlock. The cancel race (mirrors production's
        // `await_tool_approval` select over `cancel_token.cancelled()` at
        // `approval.rs:76-82`) lets a cancelled turn break out of the wait:
        // cancel wins ⇒ `Err("Request cancelled while awaiting approval")`
        // (fed back as a tool error; Checkpoint G in `run_inner` then surfaces
        // `StopReason::Interrupted`).
        let cancel_token = self.cancel_token.clone();
        let mut guard = rx.lock().await;
        loop {
            let cancelled = async {
                match &cancel_token {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                _ = cancelled => {
                    return Err("Request cancelled while awaiting approval".to_string());
                }
                decision = guard.recv() => match decision {
                    Some(ApprovalDecision::Approved { id }) if id == tool_id => return Ok(()),
                    Some(ApprovalDecision::Denied { id }) if id == tool_id => {
                        return Err(format!("Tool '{name}' denied by user"));
                    }
                    // Sandbox elevation needs `ToolDispatcher::execute` with a
                    // `sandbox_override`, which the framework `Tool::run` path
                    // doesn't carry; treat as approved (run with the fixed context).
                    // By-design gap — threads in at the wire-in step.
                    Some(ApprovalDecision::RetryWithPolicy { id, .. }) if id == tool_id => {
                        return Ok(());
                    }
                    Some(_) => continue, // stale id for a different call — ignore
                    None => return Err("Approval channel closed".to_string()),
                }
            }
        }
    }
}
