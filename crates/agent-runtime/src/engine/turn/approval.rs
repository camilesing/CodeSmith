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
//! receiver is an [`ApprovalRouter`] (`Option<Arc<ApprovalRouter>>`) — a
//! `tokio::sync::Mutex` around the receiver plus a parked-decision buffer, so
//! the parent turn's gate and every running sub-agent can wait on one channel
//! at the same time without consuming each other's answers.
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
//! - **path grants live for the process, not the session** — approving an
//!   out-of-workspace path (`Event::PathAccessRequired`) calls
//!   `ToolDispatcher::grant_external_path`, which appends to the shared live
//!   trust list (`ToolContext::trusted_external_paths`). That covers the
//!   rest of this run, including sub-agents sharing the context. Writing it
//!   to `~/.codesmith/workspace-trust.json` is the host's decision (only the
//!   host knows whether the user answered "remember"), and a dispatch path
//!   with no approval channel never asks — it keeps the plain refusal.
//! - **cancel-token race** ✅ — production's `await_tool_approval` selects over
//!   `cancel_token.cancelled()` so a cancelled turn breaks out of the approval
//!   wait. This executor now mirrors it: `request_approval`'s `recv().await`
//!   loop is wrapped in a `biased select!` over the cancel token — cancel wins
//!   ⇒ `Err("Request cancelled while awaiting approval")` (fed back as a tool
//!   error; Checkpoint G then surfaces `StopReason::Interrupted`). See the
//!   checkpoint map in `turn::seams`.
//! - **static-only approval derivation** — [`requires_approval`] checks
//!   [`Tool::capabilities`] (`ExecutesCode` / `WritesFiles` /
//!   `RequiresApproval`), mirroring `ToolSpec::approval_requirement()`'s default
//!   derivation. The per-input dynamic override (`ToolSpec::approval_requirement_for_input`,
//!   e.g. `exec_shell rm` ⇒ `Required` vs `exec_shell ls` ⇒ `Auto`) and any
//!   `ToolSpec::approval_requirement()` overrides that declare none of these
//!   capabilities are not visible from the framework `Tool` trait; they thread in
//!   at the wire-in step when the executor consults a `ToolDispatcher`.
//! - **one-shot sandbox elevation threads through the gate** — an approved
//!   `RetryWithPolicy` decision returns the policy to the caller
//!   ([`request_approval`] ⇒ `Ok(Some(policy))`), which re-dispatches the
//!   tool through `ToolDispatcher::execute` with it as the `sandbox_override`.
//!   That wire-in (the registry's override arm) rejects any policy that is
//!   not a strict widening of the baseline, so a wider sandbox policy can
//!   only ever take effect through this approved path — there is no bypass.
//!   Without a dispatcher attached the elevation cannot thread in; the tool
//!   runs with its fixed context (plain-approval semantics).

use std::path::PathBuf;
use std::sync::Arc;

use codesmith_agent::models::ContentBlock;
use codesmith_agent::tools::{Tool, ToolCapability};

use crate::engine::approval::{ApprovalDecision, ApprovalWaitError};
use crate::engine::host_executor::HostAgentExecutor;
use crate::events::Event;
use crate::tool_dispatch::ToolDispatcher;
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

/// Whether a call needs the user's approval before it may run: a host
/// dispatcher's per-input answer wins, and an out-of-workspace path counts as
/// requiring approval even when the dispatcher answers `Auto` — the path
/// question replaces the generic one (`request_approval`).
///
/// The single source of this decision, read by three call sites that must
/// agree: `request_approval` (which asks), `turn::batches` phase-1 planning
/// (which keeps the call out of an approval-free parallel batch) and
/// `turn::stream`'s speculative dispatch (which must not run a gated call
/// before the answer).
pub(crate) fn input_requires_approval(
    dispatcher: Option<&Arc<dyn ToolDispatcher>>,
    name: &str,
    input: &serde_json::Value,
    caps: &[ToolCapability],
) -> bool {
    if dispatcher
        .and_then(|d| d.escaped_path_for(name, input))
        .is_some()
    {
        return true;
    }
    match dispatcher.and_then(|d| d.approval_requirement_for(name, input)) {
        Some(req) => req != ApprovalRequirement::Auto,
        None => requires_approval(caps),
    }
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
    /// Per-tool approval gate (seam 3). Returns `Ok(None)` to proceed with
    /// execution (the tool doesn't require approval, no approval channel was
    /// supplied, or the user approved) or `Err(denial_message)` to skip the
    /// tool and feed back a `permission_denied` error so the model can react
    /// (mirrors `handle_deepseek_turn`'s per-tool approval flow,
    /// `handle_deepseek_turn`). `Ok(Some(policy))` is an approved one-shot
    /// sandbox escalation: the caller must re-dispatch the tool through
    /// `ToolDispatcher::execute` with the policy as its `sandbox_override`
    /// — the single wire-in where a wider sandbox policy can take effect
    /// (the registry rejects overrides that are not a strict widening of
    /// the baseline, so the escalation cannot narrow or launder anything).
    ///
    /// The requirement is the per-input answer from the attached dispatcher
    /// (falling back to the static [`Tool::capabilities`] gate when it has no
    /// opinion), except that an out-of-workspace path is always required —
    /// see the `escaped` branch below. In an interactive host an escaped path
    /// is asked about as itself (`Event::PathAccessRequired`) rather than as
    /// an opaque tool call; otherwise the tool is refused by its own
    /// `resolve_path`. Either way the executor blocks on the decision
    /// channel, matching by wire tool id — stale decisions for other ids are
    /// dropped (mirrors production's `_ => continue`).
    pub(crate) async fn request_approval(
        &self,
        tool_id: &str,
        name: &str,
        input: &serde_json::Value,
        tool: &Arc<dyn Tool>,
        intent_summary: &Option<String>,
    ) -> Result<Option<crate::sandbox::SandboxPolicy>, String> {
        let Some(router) = &self.approval else {
            return Ok(None); // no approval channel ⇒ gating disabled
        };
        // Path-access question (the user trusts, or refuses, an
        // out-of-workspace directory) instead of a bare refusal. The registry
        // merges this into `Required`; asking is kept unconditional here so a
        // dispatcher that reports the path but answers `Auto` still prompts.
        let escaped = self
            .tool_dispatcher
            .as_ref()
            .and_then(|d| d.escaped_path_for(name, input));
        // The gate itself is the shared predicate — the same one planning and
        // speculative dispatch read — so the answer that asks, the answer that
        // classifies, and the answer that decides whether to start early cannot
        // drift apart. `escaped` is computed here as well because the question
        // and the grant need the path itself, not just the boolean.
        let approval_required = input_requires_approval(
            self.tool_dispatcher.as_ref(),
            name,
            input,
            &tool.capabilities(),
        );
        if !approval_required {
            return Ok(None); // dispatcher said Auto, or static gate says no
        }
        let is_read_only = tool.capabilities().contains(&ToolCapability::ReadOnly);
        // Emit the request so a host UI can prompt and resolve. The
        // fingerprints are built here (not inside the await) and carried on the
        // event — the runtime emits them, the host owns the dedup sets (same
        // split as production). A path question carries the path instead: the
        // answer is about that directory, and the host persists it (or not)
        // through `workspace_trust` rather than through an approval
        // fingerprint.
        if let Some(tx) = &self.event_tx {
            if let Some(path) = escaped.as_ref() {
                let _ = tx
                    .send(Event::PathAccessRequired {
                        id: tool_id.to_string(),
                        tool_name: name.to_string(),
                        path: path.clone(),
                    })
                    .await;
            } else {
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
        }
        // Block on the decision for this tool-call id. `ApprovalRouter` parks
        // answers addressed to other waiters (a sub-agent asking at the same
        // time) instead of dropping them; with a single waiter it is the raw
        // mutex+receiver this replaced. The cancel race (mirrors production's
        // `await_tool_approval` select over `cancel_token.cancelled()` at
        // `approval.rs:76-82`) lets a cancelled turn break out of the wait:
        // cancel wins ⇒ `Err("Request cancelled while awaiting approval")`
        // (fed back as a tool error; Checkpoint G in `run_inner` then surfaces
        // `StopReason::Interrupted`).
        let decision = router
            .wait(tool_id, self.cancel_token.as_ref())
            .await
            .map_err(|err| match err {
                ApprovalWaitError::Cancelled => {
                    "Request cancelled while awaiting approval".to_string()
                }
                ApprovalWaitError::Closed => "Approval channel closed".to_string(),
            })?;
        match decision {
            ApprovalDecision::Approved { .. } => {
                self.grant_escaped_path(escaped.as_ref());
                Ok(None)
            }
            ApprovalDecision::Denied { .. } => Err(match escaped.as_ref() {
                // The refusal is about one directory, and the model must not
                // answer it by pointing the user at `/trust add` again — that
                // is what was just declined.
                Some(path) => format!(
                    "The user declined access to '{}' (outside the workspace, not trusted). \
                     Do not retry this call or ask for that path again.",
                    path.display()
                ),
                None => format!("Tool '{name}' denied by user"),
            }),
            // An approved one-shot sandbox escalation: carry the policy back
            // so the caller re-dispatches the tool through the dispatcher's
            // override wire-in (see the module docs). Without a dispatcher
            // the caller cannot thread it in and falls back to plain
            // approval semantics.
            ApprovalDecision::RetryWithPolicy { policy, .. } => {
                self.grant_escaped_path(escaped.as_ref());
                Ok(Some(policy))
            }
        }
    }

    /// Trust a path the user just approved for the rest of this process, so
    /// the call that triggered the question passes its own `resolve_path`.
    /// No-op when the call wasn't about an out-of-workspace path, or when no
    /// dispatcher (and therefore no trust list) is attached.
    fn grant_escaped_path(&self, escaped: Option<&PathBuf>) {
        if let (Some(dispatcher), Some(path)) = (self.tool_dispatcher.as_ref(), escaped) {
            dispatcher.grant_external_path(path.clone());
        }
    }
}
