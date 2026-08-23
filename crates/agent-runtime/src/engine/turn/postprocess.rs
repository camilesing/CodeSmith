//! Turn-loop post-stream phase — the sub-agent completion reaping helpers,
//! the thinking-only tail predicate, and the LSP diagnostics collect / flush
//! pair — split out of `host_executor.rs` (codebase-health §1).
//!
//! The post-stream *control flow* stays inline in `run_inner`
//! (`host_executor.rs`): the mid-stream steer flush, the non-blocking
//! completion drain, the blocking hold (Checkpoint E's `biased select!` over
//! cancel / completion `recv()` / steer `recv()`), the sentinel injection +
//! resume, and the thinking-only status emit at the clean no-tool-calls tail
//! are woven into the step-loop locals (`pending_steers`, `step`, `history`,
//! `callback`, …) and three distinct control-flow exits, so this module
//! takes only the pieces that stand alone:
//!
//! - `should_hold_turn_for_subagents` — the blocking-hold gate (run when the
//!   non-blocking drain found nothing but children may still be running).
//! - `subagent_completion_runtime_message` — the sentinel user message
//!   injected per drained completion.
//! - `should_emit_thinking_only_status` — the clean-end gate for the
//!   thinking-only status.
//! - [`HostAgentExecutor::collect_lsp_diagnostics`] (seam 3, post-edit) and
//!   [`HostAgentExecutor::flush_pending_lsp_diagnostics`] (seam 1,
//!   pre-request) — the self-contained LSP diagnostics pair over the `lsp`
//!   probe (the probe type itself stays in `host_executor.rs`; the wire-in
//!   constructs it there).
//!
//! The "known gaps in thinking-only handling" / "known gaps in subagent"
//! inventories stay in the `host_executor.rs` module docs — that behavior is
//! still inline there; the LSP-flush inventory moved with the pair, below.
//!
//! ## Known gaps in the LSP flush (by design)
//!
//! - **`apply_patch` path derivation deferred** — production derives apply_patch
//!   edited paths via `HostServices::preflight_apply_patch_paths` (which calls
//!   `codesmith-tool-impls`, unreachable from this crate without a circular dep).
//!   This executor handles only `edit_file` / `write_file` (via the shared
//!   [`edit_file_paths`](crate::engine::lsp_hooks::edit_file_paths) helper); apply_patch
//!   collects nothing here. The live `handle_deepseek_turn` still covers it; this
//!   wires when the executor connects to a real `HostServices` (or a future
//!   resolver-closure injection).
//! - **`<turn_meta>` enrichment closed** ✅ — when a `TurnMetaProbe` is wired
//!   in (production), the synthetic flush message is wrapped via
//!   `enrich_user_text_message` (date / model / working set / skills, read
//!   from the `Arc`-shared `WorkingSet`), matching production's
//!   `user_text_message_with_turn_metadata`. Embeds/tests (`probe` absent) push
//!   plain text — the pre-slice-22 behavior. No `observe_user_message` for
//!   diagnostics (no user-intent path tokens).
//! - **no `emit_session_updated`** for the synthetic push — the executor's other
//!   message pushes (assistant / tool result) likewise don't emit it via the
//!   `ChatHistory` path; UI surfacing is deferred to the wire-in step.

use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message};

use crate::engine::host_executor::HostAgentExecutor;
use crate::engine::lsp_hooks::edit_file_paths;
use crate::lsp_diagnostics::render_blocks as render_lsp_blocks;

/// Decide whether the turn should hold (block) for still-running sub-agents
/// when the non-blocking completion drain found nothing (mirrors
/// `handle_deepseek_turn`). Hold fires when there are already-queued
/// completions OR children still running — so the turn waits for a child to
/// finish rather than ending prematurely.
pub(crate) fn should_hold_turn_for_subagents(
    queued_completions: usize,
    running_children: usize,
) -> bool {
    queued_completions > 0 || running_children > 0
}

/// Decide whether to surface the "thinking-only" status at the clean
/// no-tool-calls tail (issue #1727). Mirrors the retired
/// `handle_deepseek_turn` pure helper exactly: emit only on a *clean* end —
/// tool uses empty, no turn error already surfaced, not cancelled, no pending
/// steers (the turn is about to resume), not holding for sub-agents (the turn
/// is held open). Any of those suppresses the notice so a resume path or an
/// already-surfaced error/cancel is never followed by a spurious "turn ended"
/// status. The flag itself is captured earlier (at the persist site) but the
/// emit is deferred to the tail — see `HostAgentExecutor::run_inner`
/// (slice 39 §E).
pub(crate) fn should_emit_thinking_only_status(
    tool_uses_empty: bool,
    turn_error_is_none: bool,
    cancelled: bool,
    steers_pending: bool,
    holding_for_subagents: bool,
) -> bool {
    tool_uses_empty && turn_error_is_none && !cancelled && !steers_pending && !holding_for_subagents
}

/// Build the `<codesmith:runtime_event kind="subagent_completion">` sentinel
/// user message injected into the transcript when a sub-agent completes (§E
/// slice 16/18, mirroring the retired `handle_deepseek_turn`'s drain path).
///
/// Role is `"user"`, not `"system"`: some OpenAI-compatible backends apply a
/// strict chat template (e.g. vLLM serving Qwen3) that requires any system
/// message to be messages[0]. A system message appended mid-conversation
/// makes the template raise "System message must be at the beginning",
/// which surfaces as a 400 BadRequest and breaks the whole sub-agent
/// hand-off in the parent turn. The `visibility="internal"` tag already
/// tells the model this is a runtime event rather than user input, so the
/// role carries no semantic weight here — only template-compatibility cost.
///
/// Relocated from `turn_loop.rs` in slice 49 §E (module convergence — that
/// file was the retired `handle_deepseek_turn` home and is now deleted; this
/// is the sole production caller's module).
pub(crate) fn subagent_completion_runtime_message(payload: &str) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: format!(
                "<codesmith:runtime_event kind=\"subagent_completion\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. Use the sub-agent completion \
data below to continue coordinating the current task. Do not tell the user they \
pasted sentinels, do not explain the sentinel protocol, and do not quote the raw \
XML unless the user explicitly asks to debug sub-agent internals.\n\n\
{payload}\n\
</codesmith:runtime_event>"
            ),
            cache_control: None,
        }],
    }
}

impl HostAgentExecutor {
    /// (3) per-tool post-edit seam — collect LSP diagnostics after a successful
    /// edit. Mirrors `Engine::run_post_edit_lsp_hook` (`lsp_hooks.rs`): gate on
    /// the master switch, derive the edited path, fetch diagnostics, push onto
    /// the interior-mutable accumulator. Failure is silent — a crashing LSP must
    /// never block the agent. `edit_file`/`write_file` paths come from the
    /// shared [`edit_file_paths`] helper; `apply_patch` path derivation is
    /// deferred (needs `HostServices::preflight_apply_patch_paths`, unreachable
    /// from this crate without the heavy host trait — see module docs).
    pub(crate) async fn collect_lsp_diagnostics(&self, tool_name: &str, input: &serde_json::Value) {
        let Some(probe) = &self.lsp else {
            return;
        };
        if !probe.manager.config().enabled {
            return;
        }
        let paths = match tool_name {
            "edit_file" | "write_file" => edit_file_paths(input),
            // apply_patch: deferred (needs HostServices); non-edit tools: nothing to probe.
            _ => Vec::new(),
        };
        for path in paths {
            let absolute = if path.is_absolute() {
                path
            } else {
                probe.workspace.join(&path)
            };
            // `edit_seq` is log-correlation only (production uses `turn_counter`);
            // this executor doesn't track a turn counter, so 0 suffices.
            if let Some(block) = probe.manager.diagnostics_for(&absolute, 0).await {
                probe.pending.lock().expect("poisoned").push(block);
            }
        }
    }

    /// (1) per-step pre-request seam — drain the pending LSP diagnostics into a
    /// synthetic `user` message so the model sees compile errors before its next
    /// reasoning step. Mirrors `Engine::flush_pending_lsp_diagnostics`
    /// (`lsp_hooks.rs`): `mem::take` the accumulator, render, push. No-op when
    /// nothing is pending or when LSP is disabled. Synchronous — the mutex guard
    /// is taken and dropped before `history.push`, never held across an `await`.
    pub(crate) fn flush_pending_lsp_diagnostics(&self, history: &mut dyn ChatHistory) {
        let Some(probe) = &self.lsp else {
            return;
        };
        let blocks = std::mem::take(&mut *probe.pending.lock().expect("poisoned"));
        if blocks.is_empty() {
            return;
        }
        let rendered = render_lsp_blocks(&blocks);
        if rendered.is_empty() {
            return;
        }
        // When a `TurnMetaProbe` is wired in (production), wrap the rendered
        // diagnostics in a `<turn_meta>` block — matching production's
        // `user_text_message_with_turn_metadata` for the LSP flush (enrich
        // only: no `observe_user_message`, since diagnostics carry no
        // user-intent path tokens). Embeds/tests (`None`) push plain text
        // (the pre-slice-22 behavior). Pushed via `ChatHistory`, so it lands
        // in the real `Session` transcript ahead of the request snapshot.
        let message = match &self.turn_meta {
            Some(probe) => probe.enrich_user_text_message(rendered),
            None => Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: rendered,
                    cache_control: None,
                }],
            },
        };
        history.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slice 39 §E: the pure decision helper emits the "thinking-only"
    /// status only on a genuinely clean end — mirroring the retired
    /// `handle_deepseek_turn` (issue #1727). Each suppression condition
    /// (tool uses pending, turn error already shown, cancelled, steer
    /// pending, sub-agents running) flips the result to `false`. Production
    /// pinned exactly these six cases at the helper level (it had no
    /// end-to-end test for the tail); this keeps that contract.
    #[test]
    fn should_emit_thinking_only_status_only_on_clean_end() {
        // Thinking-only response, turn genuinely ending → surface a status.
        assert!(should_emit_thinking_only_status(
            true, true, false, false, false
        ));
        // Tool uses still pending → no thinking-only status.
        assert!(!should_emit_thinking_only_status(
            false, true, false, false, false
        ));
        // A turn_error was already surfaced → don't double-report.
        assert!(!should_emit_thinking_only_status(
            true, false, false, false, false
        ));
        // Request was cancelled → cancellation status already covers it.
        assert!(!should_emit_thinking_only_status(
            true, true, true, false, false
        ));
        // A steer is pending → the turn will resume; emitting now is spurious.
        assert!(!should_emit_thinking_only_status(
            true, true, false, true, false
        ));
        // Sub-agents still running → the turn is held open; do not claim it ended.
        assert!(!should_emit_thinking_only_status(
            true, true, false, false, true
        ));
    }

    /// §E slice 49 — relocated from turn_loop.rs (module convergence). Pure-fn
    /// unit test for the sentinel-message format the `host_executor` tests
    /// exercise end-to-end via the executor.
    #[test]
    fn subagent_completion_handoff_is_internal_user_message() {
        let message = subagent_completion_runtime_message(
            "Build passed\n<codesmith:subagent.done>{\"agent_id\":\"agent_a\"}</codesmith:subagent.done>",
        );

        // Must be "user", not "system": a system message appended mid-stream
        // trips strict chat templates (vLLM/Qwen3) into a 400 BadRequest
        // ("System message must be at the beginning"). The internal-event
        // framing lives in the text + visibility tag, not the role.
        assert_eq!(message.role, "user");
        let text = match &message.content[0] {
            ContentBlock::Text { text, .. } => text,
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(text.contains("internal runtime event, not user input"));
        assert!(text.contains("Do not tell the user they pasted sentinels"));
        assert!(text.contains("<codesmith:subagent.done>"));
        assert!(text.contains("Build passed"));
    }
}
