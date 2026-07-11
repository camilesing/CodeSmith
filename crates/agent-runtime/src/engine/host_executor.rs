//! Host-side [`AgentExecutor`] — the designated home for the production turn
//! loop migration (ROADMAP §E "接真引擎").
//!
//! The framework-core [`DefaultAgentExecutor`](codesmith_agent::executor::DefaultAgentExecutor)
//! is the minimal, host-agnostic reference loop. The production `Engine` in
//! this crate carries the real turn loop (`handle_deepseek_turn`, ~2.4k lines)
//! with ten guardrails (compaction / capacity / approval / steer /
//! transparent-retry / early-tool-start / subagent / LSP / loop-guard / cycle).
//! [`HostAgentExecutor`] is the host-side [`AgentExecutor`] impl that will
//! absorb those guardrails slice by slice, eventually replacing
//! `handle_deepseek_turn`. The three host→framework bridges are already in
//! place to compose it:
//!
//! - [`ToolSpecAdapter`](crate::tools::framework_adapter::ToolSpecAdapter) —
//!   production `ToolSpec`+`ToolContext` → framework `Tool` (the `run` path).
//! - [`CallbackBridge`](crate::callback_bridge::CallbackBridge) — `mpsc::Sender<Event>`
//!   + `HookHost` → framework `Callback` (tool-lifecycle hooks).
//! - [`SessionChatHistory`](crate::session_history::SessionChatHistory) —
//!   production `Session` → framework `ChatHistory` (the transcript).
//!
//! ## Absorbed guardrails
//!
//! [`HostAgentExecutor`] runs the LLM↔tool loop (with an inline stream reducer,
//! [`reduce_stream`], that replaced the CORE `accumulate_stream` and emits
//! streaming deltas to `Callback::on_stream_delta` in real time) and absorbs
//! the production guardrails slice by slice. Seven are in:
//!
//! 1. **loop-guard** ([`LoopGuard`]) — the 3rd identical tool call in a turn is
//!    blocked (a `ToolResult` error is fed back instead of executing), and 3 / 8
//!    consecutive failures of the same tool warn / halt the turn. The guard state
//!    is a local `LoopGuard` that persists across steps within one `run` (matching
//!    `turn_loop`). This was the proof that local-state guardrails need no
//!    interior mutability: `&self` suffices, `LoopGuard` is local, and
//!    `mpsc::Sender::send` takes `&self`.
//! 2. **LSP flush** ([`LspProbe`]) — the **first guardrail needing interior
//!    mutability**. After a successful edit (`edit_file` / `write_file`), the
//!    configured [`LspManagerApi`] is probed for diagnostics and the resulting
//!    [`DiagnosticBlock`]s accumulate in `LspProbe.pending` — an
//!    `Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>`, because [`AgentExecutor::run`]
//!    is `&self` while the accumulator is mutated (push on collect, `mem::take` on
//!    flush). The lock is never held across an `await` (collect awaits
//!    `diagnostics_for` outside the lock; flush takes+drops the lock before
//!    `history.push`) — matching the [`CallbackBridge`](crate::callback_bridge::CallbackBridge)
//!    state pattern. Because the `Mutex` lives on the executor struct, pending
//!    diagnostics persist across `run` invocations on the same executor — matching
//!    the production `Engine.pending_lsp_blocks` field semantics (an edit on a turn
//!    that ends before the next request — e.g. a `MaxSteps` halt — surfaces its
//!    diagnostics on the next turn's first pre-request flush).
//! 3. **transparent-retry** ([`stream_with_transparent_retry`]) — the **first
//!    seam-2 guardrail**. When the stream dies mid-flight before any content is
//!    committed (`reduce_stream` returns `StreamReduceOutcome::Empty`), the
//!    executor silently re-issues the SAME request up to
//!    `MAX_STREAM_RETRIES` (3) times before propagating the failure, mirroring
//!    `handle_deepseek_turn`'s outer "stream died with nothing" retry
//!    (`turn_loop.rs:1152-1190`). A stream that dies *after* content was
//!    received returns `Partial` — the partial content is surfaced (not
//!    retried), mirroring production's `any_content_received` guard. A healthy
//!    round resets the budget. The retry counter is a local `u32` that
//!    persists across steps within one `run` (matching loop-guard's local-state
//!    pattern); the retry is transparent to the [`Callback`] (`on_llm_start` /
//!    `on_llm_end` fire once per step, a `Status` event is the only retry
//!    surfacing). See "Known gaps" below for the deferred cancel-token
//!    short-circuit and reactive capacity recovery.
//! 4. **steer** ([`drain_steers`](HostAgentExecutor::drain_steers)) — lets a
//!    user inject additional text input into an in-flight turn. At the top of
//!    each step (before the LLM request), queued steers are drained via
//!    `try_recv` and each becomes a `user` message in the transcript so the
//!    model re-reads them on this step's request — mirroring
//!    `handle_deepseek_turn`'s top-of-loop drain (`turn_loop.rs:300-317`).
//!    The receiver is `Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>`
//!    — interior-mutable because [`AgentExecutor::run`] is `&self` while
//!    `try_recv` takes `&mut self` (same pattern as the LSP flush's `pending`
//!    accumulator; the lock is held only for the synchronous `try_recv`). The
//!    three secondary drain sites (mid-stream buffer, post-stream resume,
//!    blocking `recv` during sub-agent hold) are streaming-lifecycle-specific
//!    and deferred.
//! 5. **approval** ([`request_approval`](HostAgentExecutor::request_approval))
//!    — gates write / code-execution tools behind user permission. Before
//!    running such a tool, the executor emits `Event::ApprovalRequired`
//!    (carrying the two fingerprint keys the host uses for approve-for-session
//!    / deny-exact dedup, plus the model's intent summary for write tools) and
//!    blocks on the approval-decision channel, matching by wire tool id (stale
//!    decisions for other ids are dropped) — mirroring
//!    `handle_deepseek_turn`'s per-tool approval flow
//!    (`turn_loop.rs:2283-2371`). A denied call never runs the tool and feeds
//!    back a `permission_denied` error so the model can react (the turn
//!    continues). The receiver is
//!    `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>` —
//!    the first guardrail to use a `tokio::sync::Mutex` (rather than
//!    `std::sync::Mutex` like steer / LSP), because the guard must cross the
//!    blocking `recv().await` (a std mutex guard isn't `Send`). Approval
//!    requirement is derived statically from [`Tool::capabilities`]
//!    (`ExecutesCode` / `WritesFiles` / `RequiresApproval`) — the framework
//!    `Tool` trait deliberately carries no per-input approval surface (§E
//!    design note); the dynamic override threads in at the wire-in step. See
//!    "Known gaps in approval" below for the cancel-token, sandbox-elevation,
//!    and static-derivation gaps.
//! 6. **compaction** ([`run_compaction`](HostAgentExecutor::run_compaction)) —
//!    keeps the transcript within the model's context window. At the top of
//!    each step (after steer drain, before the LSP flush), the executor runs a
//!    two-stage shrink mirroring `handle_deepseek_turn`'s pre-request
//!    compaction (`turn_loop.rs:378-440`): (a) **micro-compaction** — if the
//!    accumulated tool-result bytes breach the `32KB` cache trigger,
//!    [`micro_compact_messages`] rewrites stale tool results to the cleared
//!    placeholder (no LLM call); then (b) **auto-compaction** — if
//!    [`should_compact`] passes (enough messages past the keep-recent window),
//!    [`compact_messages_safe`] calls the LLM for a summary and replaces the
//!    transcript with the compacted one. Both stages wholesale-replace the
//!    transcript via `ChatHistory::clear()` + `push()` loop (the trait exposes
//!    no bulk replace — this composes its primitives, matching the "don't
//!    change core traits" precedent). The probe carries
//!    `micro_state: Arc<std::sync::Mutex<MicroCompactState>>` and
//!    `circuit_breaker: Arc<std::sync::Mutex<CompactionCircuitBreaker>>` —
//!    `std::sync::Mutex` like steer / LSP (no lock crosses an `await`: messages
//!    are cloned out before the async `compact_messages_safe` call), and
//!    because the `Mutex`es live on the executor struct the breaker / micro
//!    state persist across `run` invocations on the same executor (matching
//!    the production `Engine.micro_compact_state` / `.compaction_circuit_breaker`
//!    fields — a failed compaction on turn N still trips the breaker on turn
//!    N+1). A tripped breaker (3 consecutive failures) throttles further
//!    compaction attempts. See "Known gaps in compaction" below for the
//!    summary-prompt merge, attachment reinject, post-compact cleanup, and
//!    enhancements deferrals.
//! 7. **capacity** ([`run_capacity_preflight`](HostAgentExecutor::run_capacity_preflight))
//!    — the **always-on hard token-budget preflight** (Gate B). After
//!    compaction (so the estimate reflects the just-compacted transcript) and
//!    before the LSP flush, the executor estimates input tokens via
//!    [`estimate_input_tokens_conservative`] and, if the estimate exceeds the
//!    provider's input budget ([`context_input_budget_for_provider`]),
//!    attempts emergency recovery via [`recover_context_overflow`]
//!    (mirrors `turn_loop.rs:463-489`). The recovery cascade runs
//!    micro-compaction (best-effort, fresh state) → forced full LLM compaction
//!    (`compact_messages_safe` with `enabled = true`, lowered
//!    `token_threshold`, zeroed `auto_floor_tokens` — bypassing the
//!    cache-preservation floor) → hard trim of oldest messages (keeping
//!    [`MIN_RECENT_MESSAGES_TO_KEEP`]). On success, the step restarts so the
//!    request snapshot picks up the compacted transcript; on budget exhaustion
//!    (`MAX_CONTEXT_RECOVERY_ATTEMPTS = 2`), the turn hard-fails with
//!    `StopReason::Error`. The probe is stateless — the per-run recovery
//!    counter is a local `u8` (like the transparent-retry counter), resetting
//!    to 0 on a healthy stream round. The opt-in `CapacityController` (Gate A,
//!    off by default since v0.8.11) is deferred. The reactive seam-2 path
//!    (provider context-length rejection → recovery) is absorbed —
//!    `stream_with_transparent_retry` classifies a pre-stream `Err` and runs
//!    the same `recover_context_overflow`. See "Known gaps in capacity" below.
//!
//! Guardrail status (loop-guard warn/halt, transparent-retry "retrying n/3",
//! steer "Steer input accepted", compaction "Compaction completed/failed",
//! capacity "Emergency context compaction …") surfaces over the host's `Event`
//! channel
//! (`event_tx`) — **not** via the framework `Callback`: guardrails are
//! host-side concerns and the `Callback` trait stays untouched per ROADMAP §E.
//!
//! It is **not yet wired into `handle_send_message`**; the production
//! `handle_deepseek_turn` remains the live path — the value of landing it now is
//! the composition proof (the three bridges light up end-to-end inside a real
//! `AgentExecutor::run` driving a real `ToolSpec` over a real `Session`; see the
//! headline test) plus seven guardrails absorbed at the seams below.
//!
//! ## Guardrail insertion points
//!
//! The loop has four seams where guardrails are absorbed incrementally:
//!
//! 1. **per-step pre-request** — ✅ **steer drain** (queued user inputs injected
//!    before the request snapshot) + ✅ **compaction** (micro-compact stale
//!    tool results, then auto-compact via an LLM summary when over threshold)
//!    + ✅ **capacity preflight** (hard token-budget gate + emergency recovery
//!    via forced compaction / hard trim) + ✅ **LSP flush** (drain pending
//!    diagnostics into a synthetic `user` message); system-prompt refresh still
//!    to come (top of the `loop`).
//! 2. **per-step post-stream** — ✅ **inline stream reduction** (the
//!    `reduce_stream` reducer replaced `accumulate_stream`; it emits text /
//!    thinking deltas to `Callback::on_stream_delta` in real time and tracks
//!    `any_content_received` so a stream that dies after content surfaces the
//!    partial turn instead of retrying) + ✅ **transparent-retry** (re-issue the
//!    request when the stream dies before any content commits, up to 3 times);
//!    subagent handoff, thinking-only handling still to come (after the stream
//!    resolves, before tool extraction).
//! 3. **per-tool** — ✅ **loop-guard `record_attempt`** (block the 3rd identical
//!    call) + **`record_outcome`** (warn at 3 / halt at 8 consecutive failures) +
//!    ✅ **approval** (emit `ApprovalRequired` + block on the decision channel
//!    for write/code tools; denied ⇒ `permission_denied` error, tool skipped) +
//!    **LSP post-edit collect** (probe diagnostics after a successful edit);
//!    early-tool-start, parallel dispatch still to come (inside the tool `for`
//!    loop).
//! 4. **per-step post-tool** — ✅ **loop-guard halt short-circuit** (returns
//!    `StopReason::Error`); capacity post-tool checkpoint (opt-in
//!    `CapacityController` Gate A + error-escalation) still to come (after the
//!    tool loop). The hard token-budget preflight (Gate B) is absorbed at seam 1.
//!
//! Streaming deltas (`MessageDelta` / `ThinkingDelta`) now flow through the
//! framework `Callback::on_stream_delta` seam — the inline stream reducer
//! ([`reduce_stream`]) replaced the CORE `accumulate_stream` call, emitting
//! each text/thinking delta to the callback in real time (§E inline-stream-
//! reduction slice). The [`CallbackBridge`] maps them onto the host's
//! `Event::MessageDelta` / `Event::ThinkingDelta` channel. Block-lifecycle
//! events (`MessageStarted` / `ThinkingStarted` / `ThinkingComplete` /
//! `MessageComplete`) and tool-call-start deltas are not yet synthesized —
//! they're deferred to the early-tool-start slice.
//!
//! ## Known gaps in the LSP flush (by design)
//!
//! - **`apply_patch` path derivation deferred** — production derives apply_patch
//!   edited paths via `HostServices::preflight_apply_patch_paths` (which calls
//!   `codesmith-tool-impls`, unreachable from this crate without a circular dep).
//!   This executor handles only `edit_file` / `write_file` (via the shared
//!   [`edit_file_paths`](super::lsp_hooks::edit_file_paths) helper); apply_patch
//!   collects nothing here. The live `handle_deepseek_turn` still covers it; this
//!   wires when the executor connects to a real `HostServices` (or a future
//!   resolver-closure injection).
//! - **no `<turn_meta>` enrichment** on the synthetic flush message — production
//!   wraps it in `user_text_message_with_turn_metadata` (date / model / working
//!   set / skills, read from `session` + `config`). The framework-executor path
//!   carries no turn metadata anywhere yet; that cross-cutting enrichment is its
//!   own future slice.
//! - **no `emit_session_updated`** for the synthetic push — the executor's other
//!   message pushes (assistant / tool result) likewise don't emit it via the
//!   `ChatHistory` path; UI surfacing is deferred to the wire-in step.
//!
//! ## Known gaps in transparent-retry (by design)
//!
//! - **bail-on-error gap closed** ✅ — the inline `reduce_stream` reducer (§E
//!   inline-stream-reduction slice) replaced the CORE `accumulate_stream` call.
//!   It tracks `any_content_received` and returns `StreamReduceOutcome::Partial`
//!   (surface partial content, don't retry) when the stream dies after content
//!   was produced, and `StreamReduceOutcome::Empty` (retry transparently) only
//!   when no content arrived. This mirrors production's
//!   `should_transparently_retry_stream` guard (`streaming.rs:81-87`). The old
//!   `accumulate_stream` dropped partial blocks on the first erroring item, so
//!   the executor retried even when production would ship partial content — that
//!   gap is now closed.
//! - **pre-stream connection errors: reactive recovery absorbed** ✅ — a
//!   `create_message_stream` `Err` is now classified via
//!   `is_context_length_error_message` before propagating. A context-length
//!   rejection triggers `try_recover_context_overflow` (emergency compaction);
//!   on success the step restarts so the request snapshot picks up the
//!   compacted transcript, and on failure (or a non-context-length error) the
//!   turn hard-fails (mirrors `turn_loop.rs:620-633`). Only mid-flight stream
//!   errors (from `reduce_stream`) retry transparently; pre-stream errors are
//!   either recovered or hard-failed.
//! - **no cancel-token short-circuit** — production's
//!   `should_transparently_retry_stream` checks `!cancelled` to abort a retry
//!   loop on a cancelled turn. This executor doesn't hold a `CancellationToken`
//!   yet; the bounded budget (`MAX_STREAM_RETRIES = 3`) prevents an infinite
//!   loop, so a cancelled turn wastes at most 3 quick retries before failing. The
//!   short-circuit threads in at the wire-in step (when the executor connects to
//!   the real `Engine`'s cancel token).
//!
//! ## Known gaps in approval (by design)
//!
//! - **no cancel-token race** — production's `await_tool_approval` selects over
//!   `cancel_token.cancelled()` so a cancelled turn breaks out of the approval
//!   wait. This executor holds no `CancellationToken` yet; it blocks on the
//!   decision channel until a matching decision arrives or the channel closes. A
//!   cancelled turn thus parks at the approval await until the host resolves it
//!   (the bounded test harness pushes a decision; production's stale-drain + the
//!   cancel race thread in at the wire-in step).
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
//!
//! ## Known gaps in compaction (by design)
//!
//! - **summary-prompt merge dropped** — [`compact_messages_safe`] computes a
//!   `CompactionResult` carrying `summary_prompt: Option<SystemPrompt>` (the
//!   rolled-up summary meant to be merged into the system prompt). Production
//!   feeds it through `merge_compaction_summary`, which folds it into
//!   `session.system_prompt`. The framework [`ChatHistory`] exposes no
//!   system-prompt setter (the executor's system prompt is the static
//!   `config.system`), so the summary prompt is computed and **dropped** here.
//!   The LLM still sees the summary in the rolled-up transcript body; only the
//!   system-prompt re-injection is missing. It threads in at the wire-in step
//!   when the executor connects to a `Session` whose system prompt is mutable.
//! - **attachment reinject deferred** — production's
//!   `reinject_compaction_attachments` re-inserts plan / todos / subagents /
//!   read-file snapshots that were compacted out, so the model keeps the
//!   working set. Those attachments are host-coupled (`session.plans` /
//!   `session.todos` / sub-agent state); the framework `ChatHistory` carries
//!   none of it. Deferred to the wire-in step (same pattern as LSP's
//!   `apply_patch` path deferral).
//! - **post-compact cleanup deferred** — production's `post_compact_cleanup`
//!   forces a working-set rebuild and resets per-file cycle state after a
//!   compaction (the transcript the working set was derived from is now stale).
//!   Working-set / cycle state are host-coupled and not reachable through
//!   `ChatHistory`; deferred to the wire-in step.
//! - **enhancements passed `None`** — production's
//!   `build_compaction_enhancements` supplies PreCompact hooks + a
//!   session-memory-first summary seed. None of that surfaces through the
//!   framework `Callback` / `ChatHistory` seam yet; `compact_messages_safe` is
//!   called with `enhancements = None`. Wires in with the PreCompact hook
//!   slice.
//! - **working-set pins/paths passed `None`** — production threads
//!   `external_pins` / `external_working_set_paths` (the host's derived
//!   working set) into `should_compact` / `compact_messages_safe` so the
//!   summarizer preserves pinned files. The executor has no working-set
//!   derivation here; both are `None` (compaction uses the internally-derived
//!   paths, matching `recover_context_overflow`'s forced path). Wires in with
//!   the working-set slice.
//! - **no `emit_session_updated`** — like the LSP flush's synthetic push, the
//!   `clear()` + `push()` replacement doesn't emit a session-updated UI event
//!   via the `ChatHistory` path; UI surfacing is deferred to the wire-in step.
//! - **no backoff delay in tests** — on a compaction failure the executor only
//!   records it with the breaker and surfaces a status event (no sleep);
//!   production adds an exponential backoff before retrying. Non-transient
//!   errors return immediately from `compact_messages_safe`, so the breaker's
//!   consecutive-failure trip (3) is the throttle. The backoff threads in at
//!   the wire-in step.
//!
//! ## Known gaps in capacity (by design)
//!
//! - **responsive compact cascade (Phase 1) deferred** — production's
//!   `recover_context_overflow` runs a four-step responsive cascade
//!   (micro → partial-from → partial-up-to → full) before falling back to
//!   forced full compaction. Partial compaction summarizes only a slice of the
//!   transcript (preserving the prefix cache) — an optimization, not a
//!   correctness path. This executor skips straight to forced full compaction
//!   + hard trim, which is more aggressive (full summary instead of partial)
//!   but always recovers (the hard trim is the ultimate fallback). The
//!   responsive cascade threads in with the inline-stream-reduction slice
//!   (partial compaction needs the responsive state machine, which is
//!   `Session`-internal).
//! - **reactive seam-2 path absorbed** ✅ — production also triggers
//!   `recover_context_overflow` when the provider rejects the request with a
//!   context-length error (`turn_loop.rs:620-633`). This executor now does the
//!   same: `stream_with_transparent_retry` classifies a pre-stream `Err` via
//!   `is_context_length_error_message` and, on a context-length rejection with
//!   recovery budget remaining, runs `recover_context_overflow` and signals
//!   the caller to restart the step. A successful stream open resets the
//!   recovery budget (mirrors `turn_loop.rs:617`). The budget is bounded by
//!   `MAX_CONTEXT_RECOVERY_ATTEMPTS` (2) — in practice a second reactive
//!   recovery in the same turn almost always fails (the first compaction
//!   leaves a short transcript; re-summarizing the single older summary is a
//!   no-op), so the cap is a safety net the preflight path is more likely to
//!   reach than this reactive path.
//! - **opt-in `CapacityController` (Gate A) deferred** — the off-by-default
//!   soft controller (`run_capacity_pre_request_checkpoint` /
//!   `run_capacity_post_tool_checkpoint` / `run_capacity_error_escalation_checkpoint`)
//!   is not absorbed; only the always-on hard preflight (Gate B) is. Gate A
//!   requires the full `CapacityController` state machine (slack window,
//!   recent tool/ref counts, model priors) — a separate, opt-in slice.
//! - **same recovery gaps as compaction** — `recover_context_overflow` calls
//!   `compact_messages_safe` with the same deferred parameters
//!   (`merge_compaction_summary`, `reinject_compaction_attachments`,
//!   `post_compact_cleanup`, `enhancements`, working-set pins/paths all
//!   absent). See "Known gaps in compaction" above.
//! - **no cancel-token short-circuit** — production checks `!cancelled` before
//!   retrying after overflow recovery; this executor has no
//!   `CancellationToken`. The bounded `MAX_CONTEXT_RECOVERY_ATTEMPTS` (2)
//!   prevents an infinite loop; the short-circuit threads in at the wire-in
//!   step.
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use codesmith_agent::callback::{Callback, StopReason, StreamDelta};
use codesmith_agent::executor::{AgentExecutor, AgentExecutorConfig};
use codesmith_agent::llm_client::{LlmClientHandle, StreamEventBox};
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{
    ContentBlock, ContentBlockStart, Delta, Message, MessageDelta, MessageRequest, StreamEvent,
    SystemPrompt, ToolCaller,
};
use codesmith_agent::tools::{Tool, ToolCapability, ToolError, ToolResult, ToolSet};

use super::approval::ApprovalDecision;
use super::context::{
    context_input_budget_for_provider, estimate_input_tokens_conservative,
    is_context_length_error_message, MAX_CONTEXT_RECOVERY_ATTEMPTS,
    MIN_RECENT_MESSAGES_TO_KEEP,
};
use super::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
use super::lsp_hooks::edit_file_paths;
use super::summarize_text;
use crate::compaction::circuit_breaker::CompactionCircuitBreaker;
use crate::compaction::micro_compact::{
    micro_compact_messages, should_trigger_micro_compact, MicroCompactState,
};
use crate::compaction::{compact_messages_safe, should_compact, CompactionConfig};
use crate::config_types::ApiProvider;
use crate::events::Event;
use crate::host_services::LspManagerApi;
use crate::lsp_diagnostics::{render_blocks as render_lsp_blocks, DiagnosticBlock};
use crate::tools::approval_cache::{build_approval_grouping_key, build_approval_key};

/// The `ToolResult` fed back when the loop-guard blocks an identical repeat
/// call (mirrors `turn_loop::loop_guard_block_tool_result`). Duplicated here
/// rather than imported to keep this slice additive — zero production call-site
/// changes; a later cleanup can lift it into `loop_guard` proper as the single
/// source of truth.
fn block_tool_result(message: String) -> ToolResult {
    ToolResult::error(message).with_metadata(serde_json::json!({
        "loop_guard": "identical_tool_call"
    }))
}

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
fn requires_approval(caps: &[ToolCapability]) -> bool {
    caps.iter().any(|c| {
        matches!(
            c,
            ToolCapability::RequiresApproval
                | ToolCapability::ExecutesCode
                | ToolCapability::WritesFiles
        )
    })
}

/// Cap on the approval intent-summary length (mirrors
/// `turn_loop::MAX_APPROVAL_INTENT_SUMMARY_CHARS`).
const APPROVAL_INTENT_SUMMARY_MAX_CHARS: usize = 2_000;

/// Extract the model's preceding text this step as an approval "intent summary"
/// — the *why* shown in the approval view before the *what*. Joins the step's
/// `Text` blocks and caps the length. Mirrors `turn_loop::approval_intent_summary`
/// (which takes an already-extracted `&str`); duplicated here to keep this slice
/// additive (a later cleanup can lift the turn-loop helper to `pub(super)` as the
/// single source, same as the `block_tool_result` dedup note above).
fn approval_intent_summary(content: &[ContentBlock]) -> Option<String> {
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
    let mut summary: String = chars.by_ref().take(APPROVAL_INTENT_SUMMARY_MAX_CHARS).collect();
    if chars.next().is_some() {
        summary.push_str("...");
    }
    Some(summary)
}

/// Bundles the LSP collaborators the executor needs for the post-edit collect /
/// pre-request flush guardrail (§E, mirroring `Engine`'s
/// `run_post_edit_lsp_hook` / `flush_pending_lsp_diagnostics`).
///
/// Carries the **interior-mutable** diagnostics accumulator —
/// `Arc<Mutex<Vec<DiagnosticBlock>>>` — because [`AgentExecutor::run`] takes
/// `&self` while the accumulator is mutated (push on collect, `mem::take` on
/// flush). This mirrors the [`CallbackBridge`](crate::callback_bridge::CallbackBridge)
/// state pattern: a `std::sync::Mutex` whose guard is never held across an
/// `await` (collect awaits `diagnostics_for` *outside* the lock; flush takes
/// and drops the lock before pushing). Because the `Mutex` lives on the
/// executor struct (via this `Option<LspProbe>` field), pending diagnostics
/// persist across `run` invocations on the same executor — matching the
/// production `Engine.pending_lsp_blocks` field semantics (an edit on a turn
/// that ends before the next request — e.g. a `MaxSteps` halt — surfaces its
/// diagnostics on the next turn's first pre-request flush). `None` on the
/// executor ⇒ LSP disabled for this run (collect + flush are no-ops).
pub struct LspProbe {
    manager: Arc<dyn LspManagerApi>,
    /// Workspace root for relativizing edited paths (mirrors
    /// `session.workspace`, which `ChatHistory` does not expose).
    workspace: PathBuf,
    pending: Arc<std::sync::Mutex<Vec<DiagnosticBlock>>>,
}

impl LspProbe {
    /// Construct from the LSP manager + the session workspace. The pending
    /// accumulator starts empty (drained per-step on flush).
    #[must_use]
    pub fn new(manager: Arc<dyn LspManagerApi>, workspace: PathBuf) -> Self {
        Self {
            manager,
            workspace,
            pending: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

/// Bundles the compaction collaborators the executor needs for the per-step
/// auto-compaction guardrail (§E, mirroring `Engine`'s seam-1 micro-compact +
/// `compact_messages_safe` auto-compaction at the top of
/// `handle_deepseek_turn`'s loop — `turn_loop.rs:341-454`).
///
/// Carries two **interior-mutable** state slots —
/// `micro_state` and `circuit_breaker`, both `Arc<std::sync::Mutex<…>>` —
/// because [`AgentExecutor::run`] takes `&self` while compaction mutates them
/// (micro-compact accumulates `bytes_cleared`; the circuit breaker records
/// success/failure). This mirrors the [`LspProbe::pending`] /
/// [`HostAgentExecutor::steer`] pattern: a `std::sync::Mutex` whose guard is
/// never held across an `await` (the LLM summary call happens outside the
/// lock). Because the `Mutex`es live on the executor struct (via this
/// `Option<CompactionProbe>` field), both persist across `run` invocations on
/// the same executor — matching the production `Session.micro_compact_state`
/// / `Session.circuit_breaker` field semantics. `None` on the executor ⇒
/// compaction disabled for this run (micro-compact + auto-compact are no-ops).
///
/// The transcript itself is read/written through [`ChatHistory`]: the compacted
/// messages are applied via `clear()` + `push()`, composing the existing trait
/// surface (no core-trait change). Host-coupled follow-ups
/// (`merge_compaction_summary`, `reinject_compaction_attachments`,
/// `post_compact_cleanup`, working-set pins/paths, `CompactionEnhancements`)
/// are deferred — see "Known gaps in compaction" in the module docs.
pub struct CompactionProbe {
    config: CompactionConfig,
    /// Workspace root for `plan_compaction`'s path normalization (mirrors
    /// `session.workspace`, which `ChatHistory` does not expose).
    workspace: PathBuf,
    micro_state: Arc<std::sync::Mutex<MicroCompactState>>,
    circuit_breaker: Arc<std::sync::Mutex<CompactionCircuitBreaker>>,
}

impl CompactionProbe {
    /// Construct from the compaction config + the session workspace. The
    /// micro-compact state and circuit breaker start fresh (default).
    #[must_use]
    pub fn new(config: CompactionConfig, workspace: PathBuf) -> Self {
        Self {
            config,
            workspace,
            micro_state: Arc::new(std::sync::Mutex::new(MicroCompactState::default())),
            circuit_breaker: Arc::new(std::sync::Mutex::new(
                CompactionCircuitBreaker::new(),
            )),
        }
    }

    /// Borrow the inner circuit breaker (test-only — proves cross-run
    /// persistence of failure tracking).
    #[cfg(test)]
    pub(crate) fn breaker(&self) -> &Arc<std::sync::Mutex<CompactionCircuitBreaker>> {
        &self.circuit_breaker
    }
}

/// Capacity preflight collaborator (§E). Carries the provider + model needed
/// to compute the input-side token budget ([`context_input_budget_for_provider`]),
/// a [`CompactionConfig`] for the forced-compaction recovery path, and the
/// workspace root for [`compact_messages_safe`].
///
/// The probe itself is stateless — the per-run recovery counter
/// (`context_recovery_attempts`) lives as a local in [`HostAgentExecutor::run_inner`],
/// matching the production per-turn counter (`turn_loop.rs:292`). The forced
/// compaction inside [`HostAgentExecutor::recover_context_overflow`] uses a
/// local [`MicroCompactState`] (best-effort pass; the persistent micro-compact
/// state lives on [`CompactionProbe`] if present).
pub struct CapacityProbe {
    api_provider: ApiProvider,
    model: String,
    compaction_config: CompactionConfig,
    workspace: PathBuf,
}

impl CapacityProbe {
    /// Construct from the provider + model (for budget computation), the
    /// compaction config (cloned + forced during recovery), and the workspace
    /// root (for `compact_messages_safe` path normalization).
    #[must_use]
    pub fn new(
        api_provider: ApiProvider,
        model: String,
        compaction_config: CompactionConfig,
        workspace: PathBuf,
    ) -> Self {
        Self {
            api_provider,
            model,
            compaction_config,
            workspace,
        }
    }
}

/// Outcome of the per-step capacity preflight (seam 1).
enum CapacityPreflight {
    /// Within budget (or no probe / unknown model) — proceed with the request.
    Proceed,
    /// Over budget, emergency recovery succeeded — restart the step so the
    /// request snapshot picks up the compacted transcript (mirrors
    /// `turn_loop.rs:484` `continue`).
    RetryStep,
    /// Over budget, recovery budget exhausted — hard-fail the turn (mirrors
    /// `turn_loop.rs:466-470`).
    Fail(String),
}

/// Accumulator for a single content block being built from streaming deltas.
/// Mirrors `accumulate_stream`'s local `BlockBuild` (in
/// `codesmith-agent::executor`) — kept here as a private duplicate so the
/// inline reducer can emit deltas *while* accumulating, which the CORE reducer
/// cannot (it has no `Callback` handle). The `BTreeMap<u32, BlockBuild>` keying
/// preserves wire order (block indices are monotonic but not necessarily
/// contiguous, so `BTreeMap` not `Vec`).
enum BlockBuild {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input_buf: String,
        start_input: serde_json::Value,
        caller: Option<ToolCaller>,
    },
}

/// Finalize an accumulated `BlockBuild` map into assembled `ContentBlock`s.
/// This is the same assembly logic as `accumulate_stream`'s tail — extracted
/// so the inline reducer can call it both on clean completion and on
/// mid-flight error (partial content).
fn finalize_blocks(blocks: BTreeMap<u32, BlockBuild>) -> Vec<ContentBlock> {
    blocks
        .into_values()
        .map(|build| match build {
            BlockBuild::Text(text) => ContentBlock::Text {
                text,
                cache_control: None,
            },
            BlockBuild::Thinking(thinking) => ContentBlock::Thinking { thinking },
            BlockBuild::ToolUse {
                id,
                name,
                input_buf,
                start_input,
                caller,
            } => {
                let input = if !input_buf.is_empty() {
                    serde_json::from_str(&input_buf).unwrap_or(serde_json::Value::Null)
                } else if !start_input.is_null() {
                    start_input
                } else {
                    serde_json::Value::Object(serde_json::Map::new())
                };
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    caller,
                }
            }
        })
        .collect()
}

/// Outcome of the inline stream reducer ([`HostAgentExecutor::reduce_stream`]).
/// Replaces the CORE `accumulate_stream`'s binary `Result<(Vec<ContentBlock>,
/// Option<String>)>` with a three-way result that distinguishes "clean
/// completion" from "partial content + error" from "empty + error" — the
/// distinction drives the transparent-retry decision (only `Empty` retries).
enum StreamReduceOutcome {
    /// Stream completed cleanly (either `MessageStop` seen or the stream ended
    /// without error). The assembled content blocks and stop reason are
    /// available.
    Complete {
        content: Vec<ContentBlock>,
        stop_reason: Option<String>,
    },
    /// The stream produced content (text/thinking/tool deltas arrived) and
    /// then died mid-flight. The partial content assembled so far is available
    /// — the caller should surface it (not retry), matching production's
    /// `any_content_received` guard (`turn_loop.rs:764-834`: once the user has
    /// seen output, retrying double-bills and loses the partial turn).
    Partial {
        content: Vec<ContentBlock>,
        stop_reason: Option<String>,
        error: String,
    },
    /// The stream died before any content was produced (only `MessageStart`
    /// or nothing arrived). Safe to retry transparently — the provider hasn't
    /// billed for output and the user has seen nothing (mirrors
    /// `should_transparently_retry_stream` in `streaming.rs:81`).
    Empty { error: String },
}

/// Outcome of one (possibly retried) stream round in
/// [`HostAgentExecutor::stream_with_transparent_retry`]. Replaces the binary
/// `Result<(Vec<ContentBlock>, Option<String>)>` so the caller can distinguish
/// "content produced — proceed" from "reactive recovery succeeded — restart
/// the step" (the seam-2 reactive context-length recovery signal).
enum StreamRoundOutcome {
    /// The stream produced content (clean completion or partial surfacing).
    /// The assembled blocks and stop reason feed back into the transcript.
    Content {
        content: Vec<ContentBlock>,
        stop_reason: Option<String>,
    },
    /// A pre-stream context-length rejection was classified (via
    /// [`is_context_length_error_message`]) and emergency compaction succeeded
    /// — the caller should `continue` the step loop so the request snapshot
    /// picks up the compacted transcript (mirrors `turn_loop.rs:631-632`).
    RecoveredContextOverflow,
}

/// Host-side [`AgentExecutor`] — the growing home for the production turn loop.
///
/// Construct from the four framework collaborators: an [`LlmClientHandle`], a
/// [`ToolSet`] (built via
/// [`ToolRegistry::to_framework_tool_set`](crate::tools::registry::ToolRegistry::to_framework_tool_set)
/// in production), a [`Callback`] (a [`CallbackBridge`](crate::callback_bridge::CallbackBridge)
/// in production), and an [`AgentExecutorConfig`]. The optional `event_tx`
/// surfaces guardrail status (e.g. loop-guard warn/halt) onto the host's UI
/// `Event` channel — distinct from the `Callback`, which carries the framework
/// loop's own tool-lifecycle hooks. Nothing is mutated on `self` per run; the
/// transcript is mutated in place through [`ChatHistory`].
pub struct HostAgentExecutor {
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
    event_tx: Option<mpsc::Sender<Event>>,
    /// Optional LSP diagnostics probe (§E). `None` ⇒ collect/flush no-op.
    lsp: Option<LspProbe>,
    /// Optional steer input receiver (§E). `None` ⇒ steer drain is a no-op.
    ///
    /// Interior-mutable because [`AgentExecutor::run`] takes `&self` while
    /// `mpsc::Receiver::try_recv` takes `&mut self` — the same
    /// `Arc<std::sync::Mutex<…>>` pattern as [`LspProbe::pending`]. The lock is
    /// held only for the synchronous `try_recv` (never across an `await`),
    /// matching the LSP flush. Steers are drained (consumed) each step, so
    /// unlike diagnostics they don't accumulate — the receiver merely persists
    /// across `run` invocations on the same executor so a steer queued between
    /// turns is picked up on the next turn's first pre-request drain.
    steer: Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>,
    /// Optional approval-decision receiver (§E). `None` ⇒ approval gating is a
    /// no-op (all tools run ungated — for embeds/tests that never prompt).
    ///
    /// Interior-mutable because [`AgentExecutor::run`] takes `&self` while
    /// `mpsc::Receiver::recv` takes `&mut self`. Unlike the steer/LSP fields
    /// (which use `std::sync::Mutex` because their access — `try_recv` / push /
    /// `mem::take` — is synchronous), approval **blocks** on `recv().await`, so
    /// the guard must cross an `await` — a `std::sync::Mutex` guard isn't
    /// `Send` and can't. Hence `tokio::sync::Mutex`, whose guard is `Send` when
    /// the receiver is. The lock is held only by the single consumer (this
    /// executor's approval path); there is no contention. The receiver persists
    /// across `run` invocations on the same executor, matching the production
    /// `Engine.rx_approval` field — a decision queued between turns is matched
    /// on the next turn's per-tool approval await. No `CancellationToken` race
    /// yet (production's `await_tool_approval` also selects on
    /// `cancel_token.cancelled()`); see "Known gaps in approval" below.
    approval: Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>,
    /// Optional compaction probe (§E). `None` ⇒ micro-compact + auto-compact
    /// are no-ops. The probe carries interior-mutable `micro_state` +
    /// `circuit_breaker` (both `Arc<std::sync::Mutex<…>>`, matching the
    /// steer/LSP pattern); the transcript is read/written through `ChatHistory`
    /// (compacted messages applied via `clear()` + `push()`). Persists across
    /// `run` calls (matches `Session.micro_compact_state` / `.circuit_breaker`).
    compaction: Option<CompactionProbe>,
    /// Optional capacity probe (§E). `None` ⇒ token-budget preflight is a
    /// no-op. The probe carries the provider/model (for budget computation
    /// via [`context_input_budget_for_provider`]), a [`CompactionConfig`]
    /// (cloned + forced during emergency recovery), and the workspace root
    /// (for [`compact_messages_safe`]). Unlike [`CompactionProbe`] the probe
    /// is stateless — the per-run recovery counter is a local in `run_inner`,
    /// and the forced-compaction micro-compact pass uses a fresh
    /// [`MicroCompactState`].
    capacity: Option<CapacityProbe>,
}

impl HostAgentExecutor {
/// Construct from the four collaborators + config + an optional guardrail
/// status channel (`None` for embeds that don't surface guardrail status) +
/// an optional [`LspProbe`] (`None` ⇒ LSP collect/flush disabled) + an
/// optional steer input receiver (`None` ⇒ steer drain disabled) + an
/// optional approval-decision receiver (`None` ⇒ approval gating disabled) +
/// an optional [`CompactionProbe`] (`None` ⇒ compaction disabled) + an
/// optional [`CapacityProbe`] (`None` ⇒ capacity preflight disabled).
#[must_use]
pub fn new(
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
    event_tx: Option<mpsc::Sender<Event>>,
    lsp: Option<LspProbe>,
    steer: Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>,
    approval: Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>,
    compaction: Option<CompactionProbe>,
    capacity: Option<CapacityProbe>,
) -> Self {
        Self {
            client,
            tools,
            callback,
            config,
            event_tx,
            lsp,
            steer,
            approval,
            compaction,
            capacity,
        }
    }

    /// Surface a guardrail status message onto the host's UI `Event` channel,
    /// if one was supplied. Guardrails emit here directly rather than through
    /// the framework `Callback` (see the module docs).
    async fn emit_status(&self, message: String) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(Event::status(message)).await;
        }
    }

    /// Inline stream reducer — replaces the CORE `accumulate_stream` call so
    /// the executor can emit streaming deltas to the [`Callback`] in real time
    /// and track `any_content_received` (closing the transparent-retry
    /// bail-on-error gap). This is the §E inline-stream-reduction slice.
    ///
    /// The accumulation logic mirrors `accumulate_stream`
    /// (`codesmith-agent::executor::mod.rs`): a `BTreeMap<u32, BlockBuild>`
    /// keyed by the wire content-block index, with text/thinking deltas
    /// appended to their block's buffer and tool-input JSON deltas buffered
    /// for a final `serde_json::from_str` at assembly time. The key difference
    /// from the CORE reducer is that each text/thinking delta is **also**
    /// forwarded to [`Callback::on_stream_delta`] before being buffered, so
    /// the host's UI lights up as the stream arrives (not after the whole
    /// stream is buffered).
    ///
    /// `any_content_received` flips on the first non-`MessageStart` event —
    /// the moment we cross from "stream not yet productive" (eligible for
    /// transparent retry) into "the model has billed for output" (must
    /// surface). On a mid-flight `Err`, this drives the
    /// [`StreamReduceOutcome`] variant: `Empty` (no content → safe to retry)
    /// vs `Partial` (content received → surface, don't retry). This mirrors
    /// production's `any_content_received` guard in
    /// `should_transparently_retry_stream` (`streaming.rs:81-87`).
    ///
    /// Tool-input JSON deltas (`Delta::InputJsonDelta`) are **not** emitted
    /// to the callback — they're assembled into the `ToolUse` block's input,
    /// which isn't user-visible until `on_llm_end`. Block-lifecycle events
    /// (`MessageStarted` / `ThinkingStarted` / `ThinkingComplete` /
    /// `MessageComplete`) **are** synthesized here, at `ContentBlockStart` /
    /// `ContentBlockStop` for text/thinking blocks — letting the host's UI frame
    /// a block before its first delta and mark it done when its last delta
    /// lands (matching production's `turn_loop.rs:864/874/985/1254`). Production's
    /// `Event::ToolCallStarted` (fired on `ContentBlockStop` for tool blocks) is
    /// not synthesized here yet — it's deferred to the early-tool-start slice
    /// (which needs the tool catalog to validate input before announcing the
    /// call).
    async fn reduce_stream(&self, mut stream: StreamEventBox) -> StreamReduceOutcome {
        let mut blocks: BTreeMap<u32, BlockBuild> = BTreeMap::new();
        let mut stop_reason: Option<String> = None;
        let mut any_content_received = false;

        while let Some(item) = stream.next().await {
            let event = match item {
                Ok(e) => e,
                Err(e) => {
                    // Stream died mid-flight. Whether to retry depends on
                    // whether any content was produced before the error.
                    let error = e.to_string();
                    if any_content_received {
                        let content = finalize_blocks(std::mem::take(&mut blocks));
                        return StreamReduceOutcome::Partial {
                            content,
                            stop_reason,
                            error,
                        };
                    }
                    return StreamReduceOutcome::Empty { error };
                }
            };

            // Flip on the first non-MessageStart event — that's the moment we
            // cross from "stream not yet productive" into "the model has billed
            // for output" (mirrors `turn_loop.rs:770-772`).
            if !any_content_received && !matches!(event, StreamEvent::MessageStart { .. }) {
                any_content_received = true;
            }

            match event {
                StreamEvent::MessageStart { .. } => {}
                StreamEvent::ContentBlockStart {
                    index,
                    content_block,
                } => {
                    let build = match content_block {
                        ContentBlockStart::Text { text } => {
                            // Block-lifecycle: a text block started — let the
                            // host frame the message before its first delta.
                            self.callback
                                .on_stream_delta(&StreamDelta::MessageStarted {
                                    index: index as usize,
                                })
                                .await;
                            BlockBuild::Text(text)
                        }
                        ContentBlockStart::Thinking { thinking } => {
                            self.callback
                                .on_stream_delta(&StreamDelta::ThinkingStarted {
                                    index: index as usize,
                                })
                                .await;
                            BlockBuild::Thinking(thinking)
                        }
                        ContentBlockStart::ToolUse {
                            id,
                            name,
                            input,
                            caller,
                        } => BlockBuild::ToolUse {
                            id,
                            name,
                            input_buf: String::new(),
                            start_input: input,
                            caller,
                        },
                        ContentBlockStart::ServerToolUse { id, name, input } => {
                            BlockBuild::ToolUse {
                                id,
                                name,
                                input_buf: String::new(),
                                start_input: input,
                                caller: None,
                            }
                        }
                    };
                    blocks.insert(index, build);
                }
                StreamEvent::ContentBlockDelta { index, delta } => {
                    if let Some(build) = blocks.get_mut(&index) {
                        match (build, delta) {
                            (BlockBuild::Text(buf), Delta::TextDelta { text }) => {
                                // Forward the delta to the callback before
                                // buffering — the host's UI streams as the
                                // model produces text.
                                self.callback
                                    .on_stream_delta(&StreamDelta::Text {
                                        index: index as usize,
                                        content: text.clone(),
                                    })
                                    .await;
                                buf.push_str(&text);
                            }
                            (
                                BlockBuild::Thinking(buf),
                                Delta::ThinkingDelta { thinking },
                            ) => {
                                self.callback
                                    .on_stream_delta(&StreamDelta::Thinking {
                                        index: index as usize,
                                        content: thinking.clone(),
                                    })
                                    .await;
                                buf.push_str(&thinking);
                            }
                            (
                                BlockBuild::ToolUse { input_buf, .. },
                                Delta::InputJsonDelta { partial_json },
                            ) => {
                                // Tool-input JSON is not user-visible — buffer
                                // for assembly, no callback emission.
                                input_buf.push_str(&partial_json);
                            }
                            // Delta/block kind mismatch — ignore (provider quirk).
                            _ => {}
                        }
                    }
                }
                StreamEvent::ContentBlockStop { index } => {
                    // Block-lifecycle: mark the block done. Production emits
                    // `ThinkingComplete` / `MessageComplete` here (and
                    // `ToolCallStarted` for tool blocks — the latter is
                    // deferred to the early-tool-start slice, which needs the
                    // tool catalog to validate input before announcing the
                    // call). The block is looked up (not removed) so it stays
                    // available for `finalize_blocks` at stream end.
                    if let Some(build) = blocks.get(&index) {
                        match build {
                            BlockBuild::Thinking(_) => {
                                self.callback
                                    .on_stream_delta(&StreamDelta::ThinkingComplete {
                                        index: index as usize,
                                    })
                                    .await;
                            }
                            BlockBuild::Text(_) => {
                                self.callback
                                    .on_stream_delta(&StreamDelta::MessageComplete {
                                        index: index as usize,
                                    })
                                    .await;
                            }
                            BlockBuild::ToolUse { .. } => {
                                // Tool-block lifecycle deferred to
                                // early-tool-start.
                            }
                        }
                    }
                }
                StreamEvent::MessageDelta {
                    delta: MessageDelta { stop_reason: sr, .. },
                    ..
                } => {
                    if sr.is_some() {
                        stop_reason = sr;
                    }
                }
                StreamEvent::MessageStop => break,
                StreamEvent::Ping => {}
            }
        }

        let content = finalize_blocks(blocks);
        StreamReduceOutcome::Complete { content, stop_reason }
    }

    /// (2) per-step post-stream seam — transparent stream retry + reactive
    /// capacity recovery.
    ///
    /// Drives `create_message_stream` + [`reduce_stream`] with a bounded
    /// transparent-retry loop. Only `StreamReduceOutcome::Empty` (the stream
    /// died before any content was produced) is eligible for retry — `Complete`
    /// and `Partial` both surface immediately. This closes the
    /// `accumulate_stream` bail-on-error gap: the old CORE reducer dropped
    /// partial blocks on the first erroring item, so the executor retried even
    /// when production would ship partial content (it tracks
    /// `any_content_received` and skips the retry once the user has seen
    /// output). The inline reducer now makes the same distinction.
    ///
    /// Re-issue the SAME request up to [`MAX_STREAM_RETRIES`] (3) times before
    /// propagating the failure. This mirrors `handle_deepseek_turn`'s outer
    /// "stream died with nothing" retry (`turn_loop.rs:1152-1190`). A healthy
    /// round resets the budget (`turn_loop.rs:1186`), so a bad prior step
    /// doesn't carry over.
    ///
    /// **Reactive context-length recovery (seam 2):** a pre-stream error
    /// (`create_message_stream` returning `Err`) is classified via
    /// [`is_context_length_error_message`] before propagating. A
    /// context-length rejection triggers [`try_recover_context_overflow`] — if
    /// emergency compaction succeeds, the round returns
    /// [`StreamRoundOutcome::RecoveredContextOverflow`] so the caller restarts
    /// the step (the request snapshot picks up the compacted transcript),
    /// mirroring `turn_loop.rs:620-633`. Other pre-stream errors (connection /
    /// auth / timeout) and budget-exhausted / failed-recovery context-length
    /// errors propagate as a hard fail. A successful stream open resets the
    /// reactive recovery budget (mirrors `turn_loop.rs:617`). The retry and
    /// recovery are transparent to the [`Callback`]: `on_llm_start` /
    /// `on_llm_end` fire once per step, and a `Status` event is the only
    /// surfacing (matching production's silent re-issue / recovery).
    ///
    /// # Remaining gap vs production
    ///
    /// The cancel-token short-circuit (production's
    /// `should_transparently_retry_stream` checks `!cancelled`, and
    /// `recover_context_overflow` is guarded by `!cancelled`) is deferred to
    /// the wire-in slice — the bounded budgets (`MAX_STREAM_RETRIES` = 3,
    /// [`MAX_CONTEXT_RECOVERY_ATTEMPTS`] = 2) can't loop forever. Production's
    /// inner mid-flight retry (resetting the stream *inside* the event loop
    /// when no content was received yet, `turn_loop.rs:775-834`) is not
    /// replicated here; this executor uses the simpler outer retry (re-call
    /// `create_message_stream`). The two are functionally equivalent for the
    /// retry decision; the inner retry's advantage is avoiding a redundant
    /// `MessageStart` round-trip, which matters only for latency-sensitive
    /// production paths.
    async fn stream_with_transparent_retry(
        &self,
        client: &LlmClientHandle,
        request: MessageRequest,
        stream_retry_attempts: &mut u32,
        context_recovery_attempts: &mut u8,
        history: &mut dyn ChatHistory,
        system: Option<&SystemPrompt>,
    ) -> Result<StreamRoundOutcome> {
        /// Cap on transparent stream retries — matches `turn_loop`'s
        /// `MAX_STREAM_RETRIES` (3). One initial attempt + 3 retries = 4 total
        /// `create_message_stream` calls before the failure surfaces.
        const MAX_STREAM_RETRIES: u32 = 3;
        loop {
            // A pre-stream error may be a context-length rejection — classify
            // before propagating so reactive recovery (seam 2) can run. Only
            // mid-flight stream errors (from `reduce_stream`) retry
            // transparently; pre-stream errors are either recovered (restart
            // the step) or hard-failed.
            let stream = match client.create_message_stream(request.clone()).await {
                Ok(s) => {
                    // The provider accepted the request — the context was
                    // fine. Reset the reactive recovery budget (mirrors
                    // `turn_loop.rs:617`).
                    *context_recovery_attempts = 0;
                    s
                }
                Err(e) => {
                    let message = e.to_string();
                    if self
                        .try_recover_context_overflow(
                            client,
                            history,
                            system,
                            &message,
                            context_recovery_attempts,
                        )
                        .await
                    {
                        // Recovery succeeded — signal the caller to restart
                        // the step so the request snapshot picks up the
                        // compacted transcript (mirrors `turn_loop.rs:631-632`).
                        // Reset the stream-retry budget too (fresh step).
                        *stream_retry_attempts = 0;
                        return Ok(StreamRoundOutcome::RecoveredContextOverflow);
                    }
                    // Not a context-length error, no probe, budget exhausted,
                    // or recovery failed — hard-fail the turn.
                    return Err(anyhow::anyhow!(message));
                }
            };
            match self.reduce_stream(stream).await {
                StreamReduceOutcome::Complete {
                    content,
                    stop_reason,
                } => {
                    // Healthy round → reset the retry budget so a bad prior
                    // step doesn't carry over (mirrors `turn_loop.rs:1186`).
                    *stream_retry_attempts = 0;
                    return Ok(StreamRoundOutcome::Content { content, stop_reason });
                }
                StreamReduceOutcome::Partial {
                    content,
                    stop_reason,
                    error,
                } => {
                    // Stream died after content was received — surface the
                    // partial content, don't retry (the model has billed for
                    // output; retrying would double-bill and lose the partial
                    // turn). Reset the budget so a bad prior step doesn't
                    // carry over (mirrors `turn_loop.rs:1186`).
                    *stream_retry_attempts = 0;
                    self.emit_status(format!(
                        "Stream interrupted after partial content; surfacing what was received: {error}"
                    ))
                    .await;
                    return Ok(StreamRoundOutcome::Content { content, stop_reason });
                }
                StreamReduceOutcome::Empty { error } => {
                    // Stream died before any content — safe to retry
                    // transparently (no output billed, nothing shown).
                    if *stream_retry_attempts < MAX_STREAM_RETRIES {
                        *stream_retry_attempts = stream_retry_attempts.saturating_add(1);
                        self.emit_status(format!(
                            "Connection interrupted; retrying ({}/{MAX_STREAM_RETRIES})",
                            *stream_retry_attempts,
                        ))
                        .await;
                        continue;
                    }
                    // Budget exhausted → surface the failure.
                    return Err(anyhow::anyhow!(error));
                }
            }
        }
    }

    /// Reactive seam-2 context-length recovery. When the provider rejects a
    /// request with a context-length error (classified via
    /// [`is_context_length_error_message`]), run emergency compaction via
    /// [`recover_context_overflow`] and signal the caller to restart the step
    /// — mirrors `turn_loop.rs:622-632`. Returns `true` only when **all** of:
    /// a [`CapacityProbe`] is present, the error is a context-length
    /// rejection, the recovery budget (`*context_recovery_attempts` bounded by
    /// [`MAX_CONTEXT_RECOVERY_ATTEMPTS`]) allows, the model's budget is known,
    /// **and** [`recover_context_overflow`] succeeded.
    ///
    /// Any miss returns `false` so [`stream_with_transparent_retry`] hard-fails
    /// the turn: a non-context-length error (connection / auth / timeout), a
    /// missing probe (capacity disabled), an unknown model (no budget to judge
    /// recovery against), or budget exhaustion all propagate as a hard fail.
    /// On success the budget is incremented (mirrors `turn_loop.rs:631`); the
    /// reset lives in [`stream_with_transparent_retry`] on a successful stream
    /// open (`turn_loop.rs:617`).
    ///
    /// In practice a second reactive recovery in the same turn almost always
    /// fails: the first compaction leaves a short transcript (summary + recent
    /// tail), and re-summarizing the single older summary message is a no-op
    /// (no shrinkage ⇒ `recover_context_overflow` returns `false`). The
    /// [`MAX_CONTEXT_RECOVERY_ATTEMPTS`] cap is therefore a safety net that the
    /// preflight path (`run_capacity_preflight`) is far more likely to reach
    /// than this reactive path — matching production's defensive `MAX = 2`.
    async fn try_recover_context_overflow(
        &self,
        client: &LlmClientHandle,
        history: &mut dyn ChatHistory,
        system: Option<&SystemPrompt>,
        error_message: &str,
        context_recovery_attempts: &mut u8,
    ) -> bool {
        let Some(probe) = &self.capacity else {
            // No capacity probe ⇒ reactive recovery is disabled (mirrors the
            // preflight's `None` ⇒ `Proceed`, but here a pre-stream error has
            // no in-band fallback — hard-fail).
            return false;
        };
        if !is_context_length_error_message(error_message) {
            return false;
        }
        if *context_recovery_attempts >= MAX_CONTEXT_RECOVERY_ATTEMPTS {
            self.emit_status(format!(
                "Context length exceeded but recovery budget exhausted \
                 ({MAX_CONTEXT_RECOVERY_ATTEMPTS} attempts); failing the turn."
            ))
            .await;
            return false;
        }
        let Some(target_budget) =
            context_input_budget_for_provider(probe.api_provider, &probe.model)
        else {
            // Unknown model ⇒ no budget ⇒ can't judge recovery. Hard-fail
            // (mirrors the preflight's `None` budget ⇒ `Proceed`, but here the
            // provider already rejected — there's nothing to fall through to).
            return false;
        };
        let recovered = self
            .recover_context_overflow(
                client,
                history,
                system,
                target_budget,
                "provider context-length rejection",
            )
            .await;
        if recovered {
            *context_recovery_attempts = context_recovery_attempts.saturating_add(1);
        }
        recovered
    }

    /// (3) per-tool post-edit seam — collect LSP diagnostics after a successful
    /// edit. Mirrors `Engine::run_post_edit_lsp_hook` (`lsp_hooks.rs`): gate on
    /// the master switch, derive the edited path, fetch diagnostics, push onto
    /// the interior-mutable accumulator. Failure is silent — a crashing LSP must
    /// never block the agent. `edit_file`/`write_file` paths come from the
    /// shared [`edit_file_paths`] helper; `apply_patch` path derivation is
    /// deferred (needs `HostServices::preflight_apply_patch_paths`, unreachable
    /// from this crate without the heavy host trait — see module docs).
    async fn collect_lsp_diagnostics(&self, tool_name: &str, input: &serde_json::Value) {
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
    fn flush_pending_lsp_diagnostics(&self, history: &mut dyn ChatHistory) {
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
        // Plain `user` text message — no `<turn_meta>`: the framework-executor
        // path carries no turn metadata anywhere yet (`turn_metadata_block`
        // reads `session`+`config`, a cross-cutting host-side enrichment deferred
        // to its own slice). Pushed via `ChatHistory`, so it lands in the real
        // `Session` transcript ahead of the request snapshot below.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: rendered,
                cache_control: None,
            }],
        });
    }

    /// (1) per-step pre-request seam — drain queued steer inputs into the
    /// transcript as `user` messages so the model sees them before its next
    /// request. Mirrors `handle_deepseek_turn`'s top-of-loop steer drain
    /// (`turn_loop.rs:300-317`): `try_recv` loop → trim → skip-empty → push a
    /// `user` message → emit status. `try_recv` is non-blocking — this only
    /// drains what's already queued; it never waits for new input.
    ///
    /// Unlike production, this does NOT call `working_set.observe_user_message`
    /// (the [`ChatHistory`] trait doesn't expose the working set — that's a
    /// host-side concern deferred to the wire-in step) and does NOT wrap the
    /// steer in `user_text_message_with_turn_metadata` (the framework-executor
    /// path carries no turn metadata anywhere yet — same gap as the LSP
    /// flush). The three secondary drain sites (mid-stream buffer, post-stream
    /// resume, blocking `recv` during sub-agent hold) are
    /// streaming-lifecycle-specific and deferred — they need inline stream
    /// reduction / sub-agent support respectively.
    async fn drain_steers(&self, history: &mut dyn ChatHistory) {
        let Some(rx) = &self.steer else {
            return;
        };
        loop {
            // `try_recv` is synchronous and non-blocking — the std::sync::Mutex
            // guard is taken and dropped within this block, never across an
            // `await` (matching the LSP flush pattern).
            let steer = {
                let mut guard = rx.lock().expect("poisoned");
                match guard.try_recv() {
                    Ok(s) => s,
                    // Empty or disconnected — nothing more to drain this step.
                    Err(_) => break,
                }
            };
            let steer = steer.trim().to_string();
            if steer.is_empty() {
                continue;
            }
            // Compute the status preview before moving `steer` into the
            // message (mirrors production's `steer.clone()` + summarize).
            let status = format!(
                "Steer input accepted: {}",
                summarize_text(&steer, 120)
            );
            history.push(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: steer,
                    cache_control: None,
                }],
            });
            self.emit_status(status).await;
        }
    }

    /// (1) per-step pre-request seam — auto-compaction. Mirrors
    /// `handle_deepseek_turn`'s top-of-loop compaction
    /// (`turn_loop.rs:341-454`): a cheap no-API micro-compact pass (clear old
    /// tool-result bytes) followed, when the token budget is exceeded, by an
    /// LLM summary compaction that replaces the transcript with the pinned
    /// recent tail + summary. Placed after the steer drain and before the LSP
    /// flush (production order: steer → compaction → … → LSP flush → request)
    /// so a fresh LSP diagnostic message survives compaction.
    ///
    /// The transcript is read/written through [`ChatHistory`]: the compacted
    /// messages are applied via `clear()` + `push()`, composing the existing
    /// trait surface (no core-trait change). The `micro_state` /
    /// `circuit_breaker` live on [`CompactionProbe`] as
    /// `Arc<std::sync::Mutex<…>>` (interior-mutable because [`AgentExecutor::run`]
    /// is `&self`); the locks are never held across an `await` — the LLM
    /// summary call happens with the messages cloned out, so no `ChatHistory`
    /// borrow crosses the `await`. Both persist across `run` calls, matching
    /// `Session.micro_compact_state` / `.circuit_breaker`.
    ///
    /// Host-coupled follow-ups are deferred (see "Known gaps in compaction"
    /// in the module docs): `merge_compaction_summary` (the summary system
    /// prompt has nowhere to land — the executor's `system` is static from
    /// `config`), `reinject_compaction_attachments`, `post_compact_cleanup`,
    /// working-set `external_pins` / `external_working_set_paths`, and
    /// `CompactionEnhancements` (PreCompact hooks / session-memory-first).
    async fn run_compaction(&self, client: &LlmClientHandle, history: &mut dyn ChatHistory) {
        let Some(probe) = &self.compaction else {
            return;
        };
        if !probe.config.enabled {
            return;
        }
        // Circuit breaker: a tripped breaker (too many compaction failures)
        // throttles auto-compaction until the recovery timeout elapses. Mirrors
        // `turn_loop.rs:371` (`session.circuit_breaker.should_attempt()`).
        {
            let mut breaker = probe.circuit_breaker.lock().expect("poisoned");
            if !breaker.should_attempt() {
                return;
            }
        }

        // Phase 1 — micro-compaction (no API call): clear content from old
        // tool results (file reads, shell output, …) when a time/byte trigger
        // fires. Mirrors `turn_loop.rs:341-359`. `ChatHistory::messages()` is
        // `&[Message]` (immutable), so clone → mutate → clear+repush.
        {
            let mut state = probe.micro_state.lock().expect("poisoned");
            if should_trigger_micro_compact(history.messages(), &state, false) {
                let mut msgs = history.messages().to_vec();
                let cleared = micro_compact_messages(&mut msgs, &mut state);
                if cleared > 0 {
                    history.clear();
                    for m in msgs {
                        history.push(m);
                    }
                    tracing::info!(
                        "{cleared} bytes cleared by micro-compaction before the request"
                    );
                }
            }
        }

        // Phase 2 — auto-compaction (LLM summary). Gate on `should_compact`
        // (mirrors `turn_loop.rs:380`) BEFORE calling `compact_messages_safe`:
        // the safe wrapper does NOT early-return when under threshold, so
        // without this gate it would summarize an in-budget transcript.
        if !should_compact(
            history.messages(),
            &probe.config,
            Some(&probe.workspace),
            None,
            None,
        ) {
            return;
        }
        // Clone the messages out so no `ChatHistory` borrow crosses the await
        // (the summary call is async — the compacted result is applied after).
        let messages = history.messages().to_vec();
        match compact_messages_safe(
            client.as_ref(),
            &messages,
            &probe.config,
            Some(&probe.workspace),
            None,
            None,
            None,
        )
        .await
        {
            Ok(result) => {
                // Apply the compacted transcript (wholesale replace, mirroring
                // `self.session.messages = result.messages`). Deferred:
                // `merge_compaction_summary` (no system-prompt path via
                // `ChatHistory`), `reinject_compaction_attachments`,
                // `post_compact_cleanup`, `emit_session_updated`.
                history.clear();
                for m in result.messages {
                    history.push(m);
                }
                probe
                    .circuit_breaker
                    .lock()
                    .expect("poisoned")
                    .record_success();
                self.emit_status(format!(
                    "Compaction completed ({} messages after)",
                    history.len()
                ))
                .await;
            }
            Err(e) => {
                probe
                    .circuit_breaker
                    .lock()
                    .expect("poisoned")
                    .record_failure();
                self.emit_status(format!("Compaction failed: {e}")).await;
            }
        }
    }

    /// (1) per-step capacity preflight (seam 1) — hard token-budget gate.
    ///
    /// Estimates input tokens and, if the estimate exceeds the provider's
    /// input budget, attempts emergency recovery (forced compaction + hard
    /// trim). Mirrors `handle_deepseek_turn`'s Gate B
    /// (`turn_loop.rs:463-489`) — the always-on hard token-budget preflight,
    /// **not** the opt-in `CapacityController` (Gate A, off by default since
    /// v0.8.11 — deferred).
    ///
    /// Returns [`CapacityPreflight::Proceed`] when within budget or when
    /// recovery failed but the budget isn't exhausted (the request goes out
    /// anyway, mirroring production's fall-through); [`CapacityPreflight::RetryStep`]
    /// when recovery succeeded (restart the step so the snapshot picks up the
    /// compacted transcript); [`CapacityPreflight::Fail`] when the recovery
    /// budget is exhausted.
    async fn run_capacity_preflight(
        &self,
        client: &LlmClientHandle,
        history: &mut dyn ChatHistory,
        system: Option<&SystemPrompt>,
        context_recovery_attempts: &mut u8,
    ) -> CapacityPreflight {
        let Some(probe) = &self.capacity else {
            return CapacityPreflight::Proceed;
        };
        let Some(target_budget) =
            context_input_budget_for_provider(probe.api_provider, &probe.model)
        else {
            // Unknown model ⇒ no budget ⇒ preflight silently disabled
            // (mirrors `turn_loop.rs:465` — `None` budget skips the gate).
            return CapacityPreflight::Proceed;
        };
        let estimated = estimate_input_tokens_conservative(history.messages(), system);
        if estimated <= target_budget {
            return CapacityPreflight::Proceed;
        }
        // Over budget — check the recovery budget before attempting.
        if *context_recovery_attempts >= MAX_CONTEXT_RECOVERY_ATTEMPTS {
            let msg = format!(
                "Context overflow: estimated ~{estimated} tokens exceeds budget \
                 ~{target_budget} after {MAX_CONTEXT_RECOVERY_ATTEMPTS} recovery attempts."
            );
            self.emit_status(msg.clone()).await;
            return CapacityPreflight::Fail(msg);
        }
        if self
            .recover_context_overflow(client, history, system, target_budget, "preflight token budget")
            .await
        {
            *context_recovery_attempts = context_recovery_attempts.saturating_add(1);
            CapacityPreflight::RetryStep
        } else {
            // Recovery failed but budget not exhausted — fall through; the
            // request goes out and will likely be rejected by the provider
            // (mirrors production: `recover_context_overflow` returning false
            // falls through without `continue`). The reactive seam-2 path
            // (provider context-length rejection → recovery) now catches such
            // a rejection inside `stream_with_transparent_retry`.
            self.emit_status(format!(
                "Context overflow recovery failed; sending request anyway \
                 (~{estimated} vs ~{target_budget} tokens)."
            ))
            .await;
            CapacityPreflight::Proceed
        }
    }

    /// Emergency context-overflow recovery. Mirrors
    /// `Engine::recover_context_overflow` (`engine/mod.rs:1670-1893`),
    /// simplified to two of the production's three phases:
    ///
    /// - **Micro-compact** (best-effort, no API call): clear old tool-result
    ///   content with a fresh [`MicroCompactState`]. Production Phase 1
    ///   (responsive compact cascade: micro → partial-from → partial-up-to →
    ///   full) is deferred — the preflight runs after `run_compaction` in the
    ///   same step, so persistent-state micro-compact already ran; partial
    ///   compaction is a prefix-cache optimization, not a correctness path.
    /// - **Forced full compaction** (Phase 2): [`compact_messages_safe`] with a
    ///   forced config (`enabled = true`, `token_threshold = target - 1`,
    ///   `auto_floor_tokens = 0` — bypassing the cache-preservation floor
    ///   because we're at a hard ceiling).
    /// - **Hard trim** (Phase 3): drop oldest messages from the front while the
    ///   estimate exceeds the budget, always keeping
    ///   [`MIN_RECENT_MESSAGES_TO_KEEP`].
    ///
    /// Returns `true` only if the post-recovery estimate is within budget and
    /// the transcript actually shrank.
    ///
    /// Deferred (same gaps as the compaction slice): `merge_compaction_summary`
    /// (no system-prompt setter on [`ChatHistory`]),
    /// `reinject_compaction_attachments`, `post_compact_cleanup`,
    /// `CompactionEnhancements`, `emit_session_updated`.
    async fn recover_context_overflow(
        &self,
        client: &LlmClientHandle,
        history: &mut dyn ChatHistory,
        system: Option<&SystemPrompt>,
        target_budget: usize,
        reason: &str,
    ) -> bool {
        let Some(probe) = &self.capacity else {
            return false;
        };
        self.emit_status(format!("Emergency context compaction started ({reason})"))
            .await;

        let before_tokens = estimate_input_tokens_conservative(history.messages(), system);
        let before_count = history.len();

        // Phase 1 — best-effort micro-compact (no API call). Production's
        // responsive cascade uses persistent `MicroCompactState`; we use a
        // fresh default — the preflight runs after `run_compaction` (which
        // already micro-compacted with persistent state if a `CompactionProbe`
        // is present), so a second pass with fresh state re-examines all
        // messages but only clears content that hasn't been cleared yet.
        {
            let mut msgs = history.messages().to_vec();
            let mut local_state = MicroCompactState::default();
            let cleared = micro_compact_messages(&mut msgs, &mut local_state);
            if cleared > 0 {
                history.clear();
                for m in msgs {
                    history.push(m);
                }
                let after_micro = estimate_input_tokens_conservative(history.messages(), system);
                if after_micro <= target_budget {
                    self.emit_status(
                        "Emergency recovery: micro-compaction cleared enough context"
                            .to_string(),
                    )
                    .await;
                    return true;
                }
            }
        }

        // Phase 2 — forced full LLM compaction (mirrors mod.rs:1802-1847).
        let mut forced_config = probe.compaction_config.clone();
        forced_config.enabled = true;
        forced_config.token_threshold = forced_config
            .token_threshold
            .min(target_budget.saturating_sub(1))
            .max(1);
        // Bypass the cache-preservation floor — at a hard ceiling we must
        // free budget regardless of cache cost (mirrors mod.rs:1813-1816).
        forced_config.auto_floor_tokens = 0;

        let messages = history.messages().to_vec();
        match compact_messages_safe(
            client.as_ref(),
            &messages,
            &forced_config,
            Some(&probe.workspace),
            None,
            None,
            None,
        )
        .await
        {
            Ok(result) => {
                // Apply the compacted transcript (wholesale replace, mirroring
                // `self.session.messages = result.messages`). Deferred:
                // `merge_compaction_summary` (no system-prompt path via
                // `ChatHistory`), `reinject_compaction_attachments`,
                // `post_compact_cleanup`, `emit_session_updated`.
                if !result.messages.is_empty() || messages.is_empty() {
                    history.clear();
                    for m in result.messages {
                        history.push(m);
                    }
                }
                // summary_prompt discarded — same gap as run_compaction.
            }
            Err(e) => {
                self.emit_status(format!(
                    "Emergency compaction API pass failed: {e}. Falling back to local trim."
                ))
                .await;
            }
        }

        // Phase 3 — hard trim oldest messages (mirrors mod.rs:1852 +
        // trim_oldest_messages_to_budget). `ChatHistory` has no `remove(0)`,
        // so clone → trim Vec → clear + repush (same pattern as compaction).
        let after_compact = estimate_input_tokens_conservative(history.messages(), system);
        if after_compact > target_budget {
            let mut msgs = history.messages().to_vec();
            while msgs.len() > MIN_RECENT_MESSAGES_TO_KEEP
                && estimate_input_tokens_conservative(&msgs, system) > target_budget
            {
                msgs.remove(0);
            }
            history.clear();
            for m in msgs {
                history.push(m);
            }
        }

        let after_tokens = estimate_input_tokens_conservative(history.messages(), system);
        let after_count = history.len();
        let recovered = after_tokens <= target_budget
            && (after_tokens < before_tokens || after_count < before_count);

        if recovered {
            let removed = before_count.saturating_sub(after_count);
            self.emit_status(format!(
                "Emergency compaction complete: {before_count} → {after_count} messages \
                 ({removed} removed), ~{before_tokens} → ~{after_tokens} tokens"
            ))
            .await;
        } else {
            self.emit_status(format!(
                "Emergency context compaction failed to reduce request below model limit \
                 (estimate ~{after_tokens} tokens, budget ~{target_budget})."
            ))
            .await;
        }
        recovered
    }

    /// Per-tool approval gate (seam 3). Returns `Ok(())` to proceed with
    /// execution (the tool doesn't require approval, no approval channel was
    /// supplied, or the user approved) or `Err(denial_message)` to skip the
    /// tool and feed back a `permission_denied` error so the model can react
    /// (mirrors `handle_deepseek_turn`'s per-tool approval flow,
    /// `turn_loop.rs:2283-2371`).
    ///
    /// The approval requirement is derived statically from [`Tool::capabilities`]
    /// (see [`requires_approval`]); the dynamic per-input override is a by-design
    /// gap. The executor emits `Event::ApprovalRequired` (carrying the two
    /// fingerprint keys the host uses for approve-for-session / deny-exact
    /// dedup, plus the model's intent summary for write tools) and then blocks
    /// on the decision channel, matching by wire tool id — stale decisions for
    /// other ids are dropped (mirrors production's `_ => continue`).
    async fn request_approval(
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
        if !requires_approval(&tool.capabilities()) {
            return Ok(()); // tool doesn't require approval
        }
        let is_read_only = tool
            .capabilities()
            .iter()
            .any(|c| *c == ToolCapability::ReadOnly);
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
        // cannot deadlock. No `CancellationToken` race yet — blocks until a
        // matching decision arrives or the channel closes (deferred to wire-in).
        let mut guard = rx.lock().await;
        loop {
            match guard.recv().await {
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

impl AgentExecutor for HostAgentExecutor {
    fn run<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason>> + Send + 'a>> {
        Box::pin(self.run_inner(history, user_text))
    }
}

impl HostAgentExecutor {
    /// The bare LLM↔tool loop. Mirrors `DefaultAgentExecutor::run_inner`; will
    /// grow guardrail seams at the four points noted in the module docs.
    async fn run_inner<'a>(
        &'a self,
        history: &'a mut dyn ChatHistory,
        user_text: String,
    ) -> Result<StopReason> {
        // Cheap Arc clones so the loop body borrows locals, not `&self` fields.
        let client = self.client.clone();
        let tools = self.tools.clone();
        let callback = self.callback.clone();
        let max_steps = self.config.max_steps;
        let max_tokens = self.config.max_tokens;
        let system = self.config.system.clone();
        let temperature = self.config.temperature;

        // Seed the transcript with the user turn.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: user_text,
                cache_control: None,
            }],
        });

        // Loop-guard state persists across steps within this run (one
        // `LoopGuard` per turn, matching `turn_loop`).
        let mut loop_guard = LoopGuard::default();
        let mut step: u32 = 0;
        // Transparent stream-retry counter: re-issue the request when the
        // stream dies mid-flight before any content commits (mirrors
        // `turn_loop.rs:284-292`). Persists across steps within one run;
        // resets to 0 on a healthy round.
        let mut stream_retry_attempts: u32 = 0;
        // Capacity recovery counter: per-turn, bounded by
        // `MAX_CONTEXT_RECOVERY_ATTEMPTS` (2). Increments on each successful
        // emergency compaction; resets to 0 on a healthy stream round
        // (mirrors `turn_loop.rs:292` + the reset at `:617`).
        let mut context_recovery_attempts: u8 = 0;
        loop {
            // (1) per-step pre-request seam — ✅ steer drain (queued user
            // inputs injected before the request snapshot); ✅ compaction
            // (micro-compact + LLM-summary auto-compact, runs after steer and
            // before the LSP flush so a fresh diagnostic message survives
            // compaction); ✅ capacity preflight (hard token-budget gate +
            // emergency recovery, runs after compaction and before the LSP
            // flush so the estimate reflects the just-compacted transcript);
            // ✅ LSP flush (drain pending diagnostics into a synthetic user
            // message); cycle land here later.
            if step >= max_steps {
                callback.on_complete(&StopReason::MaxSteps).await;
                return Ok(StopReason::MaxSteps);
            }
            // Steer drain sits at the very top of the loop (mirrors
            // `turn_loop.rs:300`) — before compaction, the LSP flush, and the
            // request snapshot, so steered text reaches the model on this
            // step's request. Drains only what's already queued (`try_recv` is
            // non-blocking); never waits for input.
            self.drain_steers(history).await;
            // Auto-compaction mirrors `turn_loop.rs:341-454` (steer →
            // compaction → … → LSP flush → request). Runs before the LSP
            // flush so a freshly-collected diagnostic message (pushed by the
            // flush below) is not summarized away.
            self.run_compaction(&client, history).await;
            // Capacity preflight (Gate B — always-on hard token-budget check).
            // Mirrors `turn_loop.rs:463-489`. Runs after compaction so the
            // estimate reflects the just-compacted transcript; before the LSP
            // flush (matching production order: compaction → capacity → LSP
            // flush → request). If recovery succeeds, `continue` restarts the
            // step so the request snapshot picks up the compacted transcript.
            // If the budget is exhausted, hard-fail the turn.
            match self
                .run_capacity_preflight(
                    &client,
                    history,
                    system.as_ref(),
                    &mut context_recovery_attempts,
                )
                .await
            {
                CapacityPreflight::Proceed => {}
                CapacityPreflight::RetryStep => continue,
                CapacityPreflight::Fail(msg) => {
                    callback.on_complete(&StopReason::Error(msg.clone())).await;
                    return Ok(StopReason::Error(msg));
                }
            }
            // LSP flush sits after the max_steps bail so a turn-ending step
            // (e.g. MaxSteps right after an edit) leaves pending diagnostics
            // on the executor for the next turn's first flush — matching the
            // production `Engine.pending_lsp_blocks` field semantics.
            self.flush_pending_lsp_diagnostics(history);

            let api_tools = tools.to_api_tools();
            let request = MessageRequest {
                model: client.model().to_string(),
                messages: history.messages().to_vec(),
                max_tokens,
                system: system.clone(),
                tools: if api_tools.is_empty() {
                    None
                } else {
                    Some(api_tools)
                },
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: None,
                stream: Some(true),
                temperature,
                top_p: None,
            };

            callback.on_llm_start(&request).await;
            // (2) per-step post-stream seam — ✅ transparent-retry (re-issue
            // when the stream dies mid-flight before any content commits);
            // ✅ reactive capacity recovery (a pre-stream context-length
            // rejection triggers emergency compaction and restarts the step).
            // `on_llm_start` fires once per step; retries and recovery are
            // transparent to the Callback. Subagent handoff / thinking-only
            // handling land here later.
            let (content, _stop_reason) = match self
                .stream_with_transparent_retry(
                    &client,
                    request,
                    &mut stream_retry_attempts,
                    &mut context_recovery_attempts,
                    history,
                    system.as_ref(),
                )
                .await
            {
                Ok(StreamRoundOutcome::Content { content, stop_reason }) => {
                    // The reactive recovery budget is reset on a successful
                    // stream open inside `stream_with_transparent_retry`
                    // (mirrors `turn_loop.rs:617`).
                    (content, stop_reason)
                }
                Ok(StreamRoundOutcome::RecoveredContextOverflow) => {
                    // Emergency compaction succeeded on a context-length
                    // rejection — restart the step so the request snapshot
                    // picks up the compacted transcript (mirrors
                    // `turn_loop.rs:631-632`).
                    continue;
                }
                Err(e) => return Err(e),
            };
            callback.on_llm_end(&content).await;

            // Persist the assistant turn.
            history.push(Message {
                role: "assistant".to_string(),
                content: content.clone(),
            });

            // The model's preceding text this step — the "intent summary" the
            // approval view shows for write tools (extracted before `content`
            // is moved into `tool_uses`).
            let intent_summary = approval_intent_summary(&content);

            // Collect tool calls (preserve order).
            let tool_uses: Vec<(String, String, serde_json::Value)> = content
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input, .. } => Some((id, name, input)),
                    _ => None,
                })
                .collect();

            if tool_uses.is_empty() {
                callback.on_complete(&StopReason::NoToolCalls).await;
                return Ok(StopReason::NoToolCalls);
            }

            // Execute each tool sequentially and feed the result back as a
            // `role:"user"` `ToolResult` block (Anthropic/OpenAI-compat shape).
            //
            // (3) per-tool seam — loop-guard (absorbed); ✅ approval (emit
            // `ApprovalRequired` + block on the decision channel for write/code
            // tools; denied ⇒ `permission_denied` error, tool skipped); LSP
            // post-edit collect (absorbed); early-tool-start / parallel land here
            // later. `loop_guard_halt` is per-step: a halt short-circuits the
            // tool loop and the whole turn at the (4) seam below.
            let mut loop_guard_halt: Option<String> = None;
            for (id, name, input) in tool_uses {
                callback.on_tool_start(&name, &input).await;
                // loop-guard: block the 3rd identical (name+args) call this turn.
                let (result, blocked) = match loop_guard.record_attempt(&name, &input) {
                    AttemptDecision::Block(message) => {
                        (Ok(block_tool_result(message)), true)
                    }
                    AttemptDecision::Proceed => {
                        let result = match tools.get(&name) {
                            // approval gate: a tool that requires approval is
                            // gated behind the decision channel; denied ⇒ the
                            // tool never runs and a `permission_denied` error
                            // is fed back so the model can react (turn
                            // continues). Order: loop-guard first (matches
                            // production), then approval.
                            Some(tool) => {
                                match self
                                    .request_approval(&id, &name, &input, tool, &intent_summary)
                                    .await
                                {
                                    Ok(()) => tool.run(input.clone()).await,
                                    Err(denial) => {
                                        Err(ToolError::permission_denied(denial))
                                    }
                                }
                            }
                            None => Err(ToolError::NotAvailable {
                                message: format!("no tool named '{name}'"),
                            }),
                        };
                        (result, false)
                    }
                };
                callback.on_tool_end(&name, &result).await;

                // loop-guard: track consecutive failures of this tool (warn at
                // 3, halt at 8). A guard-blocked call records no outcome — it
                // is an intervention, not an execution, so it doesn't count
                // toward the failure halt.
                if !blocked {
                    let success = result.as_ref().map(|r| r.success).unwrap_or(false);
                    match loop_guard.record_outcome(&name, success) {
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
                // production `output.success && tool_was_executed`); approval /
                // early-tool-start / parallel land here later.
                if !blocked {
                    if let Ok(r) = &result {
                        if r.success {
                            self.collect_lsp_diagnostics(&name, &input).await;
                        }
                    }
                }

                let (content_str, is_error) = match &result {
                    Ok(r) => (r.content.clone(), !r.success),
                    Err(e) => (format!("Error: {e}"), true),
                };
                history.push(Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: content_str,
                        is_error: Some(is_error),
                        content_blocks: None,
                    }],
                });
            }

            // (4) per-step post-tool seam — loop-guard halt (absorbed);
            // capacity post-tool checkpoint (opt-in `CapacityController` Gate A
            // + error-escalation) / cycle land here later. The hard
            // token-budget preflight (Gate B) is absorbed at seam (1).
            if let Some(message) = loop_guard_halt {
                tracing::warn!("{}", message);
                self.emit_status(message.clone()).await;
                callback
                    .on_complete(&StopReason::Error(message.clone()))
                    .await;
                return Ok(StopReason::Error(message));
            }

            callback.on_step(step).await;
            step += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callback_bridge::CallbackBridge;
    use crate::events::Event;
    use crate::hooks::{HookContext, HookEvent, HookHost, HookResult, MessageSubmitOutcome};
    use crate::host_services::LspManagerApi;
    use crate::lsp_config::LspConfig;
    use crate::lsp_diagnostics::{Diagnostic, DiagnosticBlock, Severity};
    use crate::session::Session;
    use crate::session_history::SessionChatHistory;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::spec::{ToolContext, ToolSpec};
    use codesmith_agent::llm_client::{LlmClient, StreamEventBox};
    use codesmith_agent::models::{
        ContentBlockStart, Delta, MessageDelta, MessageResponse, StreamEvent, Usage,
    };
    use codesmith_agent::tools::{ToolCapability, ToolError, ToolResult};
    use std::collections::{HashMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    // === test doubles =======================================================

    /// A `ToolSpec` that echoes its `text` input, stamped with the captured
    /// workspace path so tests can prove the context flowed through the adapter.
    /// (Mirrors `framework_adapter` tests' `EchoSpec`.)
    struct EchoSpec;

    #[async_trait::async_trait]
    impl ToolSpec for EchoSpec {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input, stamped with the workspace path."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult {
                content: format!("{}|{text}", context.workspace.display()),
                success: true,
                metadata: None,
            })
        }
    }

    /// A `ToolSpec` that declares `WritesFiles` (so the §E approval gate fires),
    /// echoes its `path` input, and returns success — lets approval tests prove
    /// the gate runs the tool on approve / skips it with a `permission_denied`
    /// error on deny. Distinct from `EditSpec` (used for the LSP collect seam)
    /// so approval assertions stay decoupled.
    struct WriteSpec;

    #[async_trait::async_trait]
    impl ToolSpec for WriteSpec {
        fn name(&self) -> &str {
            "write_file"
        }
        fn description(&self) -> &str {
            "Writes a file (requires approval)."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::WritesFiles]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            Ok(ToolResult {
                content: format!("wrote:{path}"),
                success: true,
                metadata: None,
            })
        }
    }

    /// A `ToolSpec` standing in for `edit_file` / `write_file`: it succeeds and
    /// reports the edited `path` back in its content, so the §E LSP collect seam
    /// (keyed on tool name `edit_file`/`write_file` + the `path` input field)
    /// fires and the post-edit probe runs.
    struct EditSpec;

    #[async_trait::async_trait]
    impl ToolSpec for EditSpec {
        fn name(&self) -> &str {
            "edit_file"
        }
        fn description(&self) -> &str {
            "Edits a file at `path`; used to drive the LSP post-edit collect seam."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult {
                content: format!("edited:{path}"),
                success: true,
                metadata: None,
            })
        }
    }

    /// Like `EditSpec` (name `edit_file`, reads `path`) but reports a *failed*
    /// edit (`success: false`) so tests can prove the LSP collect seam is gated
    /// on a successful edit (mirrors production `output.success`).
    struct FailingEditSpec;

    #[async_trait::async_trait]
    impl ToolSpec for FailingEditSpec {
        fn name(&self) -> &str {
            "edit_file"
        }
        fn description(&self) -> &str {
            "An edit that fails; the LSP collect seam must skip it."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult {
                content: format!("edit failed:{path}"),
                success: false,
                metadata: None,
            })
        }
    }

    /// A `LlmClient` that pops canned `StreamEvent` lists from a queue, one
    /// per `create_message_stream` call. (Mirrors the bridge tests' `MockLlm`.)
    /// Also records the `messages` of every received request, so tests can
    /// prove the model saw a specific synthetic message (e.g. flushed LSP
    /// diagnostics) before a given call.
    /// A canned response for one `create_message_stream` call.
    ///
    /// - `Events` streams a list of `StreamEvent`s (all `Ok`) — the normal path.
    /// - `StreamErr` yields a single `Err` item, so `reduce_stream` sees an
    ///   error before any content — simulating a mid-flight stream death with no
    ///   content (the #103 "stream died with nothing" case) for the
    ///   transparent-retry tests. Returns `StreamReduceOutcome::Empty`.
    /// - `EventsThenErr` streams the events (all `Ok`) then a trailing `Err` —
    ///   simulates a mid-flight stream death *after* content was produced.
    ///   Returns `StreamReduceOutcome::Partial` (the bail-on-error gap closure:
    ///   partial content is surfaced, not retried).
    /// - `StreamOpenErr` makes `create_message_stream` itself return `Err` —
    ///   simulates a pre-stream provider rejection (e.g. a context-length
    ///   error), the seam-2 reactive-recovery trigger. Distinct from
    ///   `StreamErr`, which opens the stream successfully then yields a
    ///   mid-flight `Err` item (drives transparent retry, not recovery).
    enum MockRound {
        Events(Vec<StreamEvent>),
        StreamErr(String),
        EventsThenErr(Vec<StreamEvent>, String),
        StreamOpenErr(String),
    }

    struct MockLlm {
        rounds: Mutex<VecDeque<MockRound>>,
        requests: Mutex<Vec<Vec<Message>>>,
        /// Canned reply for a non-streaming `create_message` call (used by the
        /// compaction summary path). `None` ⇒ `create_message` bails (the
        /// pre-compaction default, so non-compaction tests are unaffected).
        compaction_reply: Mutex<Option<MessageResponse>>,
        /// When set, `create_message` returns this error instead of the reply
        /// — drives the compaction-failure / circuit-breaker tests.
        compaction_error: Mutex<Option<String>>,
        /// Count of `create_message` calls (compaction summary attempts).
        compaction_calls: Mutex<u32>,
    }

    impl MockLlm {
        /// Convenience: each `Vec<StreamEvent>` becomes a `MockRound::Events`,
        /// popped one per `create_message_stream` call (front first).
        fn new(calls: Vec<Vec<StreamEvent>>) -> Self {
            Self::with_rounds(calls.into_iter().map(MockRound::Events).collect())
        }

        /// Full control: one `MockRound` per `create_message_stream` call, popped
        /// in order (front first). Use this to inject `StreamErr` rounds.
        fn with_rounds(rounds: Vec<MockRound>) -> Self {
            Self {
                rounds: Mutex::new(rounds.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
                compaction_reply: Mutex::new(None),
                compaction_error: Mutex::new(None),
                compaction_calls: Mutex::new(0),
            }
        }

        /// The `messages` snapshot of each `create_message_stream` call, in call
        /// order.
        fn requests(&self) -> Vec<Vec<Message>> {
            self.requests.lock().unwrap().clone()
        }

        /// Make `create_message` (the compaction summary call) return a canned
        /// `MessageResponse` whose text is `summary`. The summary is what
        /// `compact_messages` writes back as the compaction result.
        fn with_compaction_summary(self, summary: &str) -> Self {
            *self.compaction_reply.lock().unwrap() = Some(MessageResponse {
                id: "compaction".to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: summary.to_string(),
                    cache_control: None,
                }],
                model: "mock-v0".to_string(),
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                container: None,
                usage: Usage::default(),
            });
            self
        }

        /// Make `create_message` (the compaction summary call) return a
        /// non-transient error — drives the circuit-breaker failure path.
        fn with_compaction_error(self, message: &str) -> Self {
            *self.compaction_error.lock().unwrap() = Some(message.to_string());
            self
        }

        /// How many `create_message` (compaction summary) calls were made.
        fn compaction_calls(&self) -> u32 {
            *self.compaction_calls.lock().unwrap()
        }
    }

    impl LlmClient for MockLlm {
        fn provider_name(&self) -> &'static str {
            "mock"
        }
        fn model(&self) -> &str {
            "mock-v0"
        }
        fn create_message(
            &self,
            _request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
            // Locks are taken and dropped here (sync, before the async block) so
            // no guard crosses an `await` — the returned future captures only
            // owned data (`reply` / `error`).
            *self.compaction_calls.lock().unwrap() += 1;
            let reply = self.compaction_reply.lock().unwrap().clone();
            let error = self.compaction_error.lock().unwrap().clone();
            Box::pin(async move {
                if let Some(msg) = error {
                    anyhow::bail!("{msg}");
                }
                reply.ok_or_else(|| {
                    anyhow::anyhow!("mock does not implement create_message")
                })
            })
        }
        fn create_message_stream(
            &self,
            request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<StreamEventBox>> + Send + '_>> {
            self.requests.lock().unwrap().push(request.messages.clone());
            let next = self.rounds.lock().unwrap().pop_front();
            Box::pin(async move {
                let round = next.unwrap_or(MockRound::Events(vec![]));
                match round {
                    MockRound::Events(events) => Ok(Box::pin(
                        futures_util::stream::iter(events.into_iter().map(Ok)),
                    )
                        as StreamEventBox),
                    MockRound::StreamErr(msg) => Ok(Box::pin(
                        futures_util::stream::iter(vec![Err(anyhow::anyhow!(msg))]),
                    )
                        as StreamEventBox),
                    MockRound::EventsThenErr(events, msg) => {
                        let mut items: Vec<Result<StreamEvent>> =
                            events.into_iter().map(Ok).collect();
                        items.push(Err(anyhow::anyhow!(msg)));
                        Ok(Box::pin(futures_util::stream::iter(items)) as StreamEventBox)
                    }
                    // Pre-stream rejection: `create_message_stream` itself
                    // returns `Err` (the stream never opens). Drives the
                    // seam-2 reactive-recovery tests. The request was already
                    // recorded above, so `requests()` still sees this call.
                    MockRound::StreamOpenErr(msg) => Err(anyhow::anyhow!(msg)),
                }
            })
        }
    }

    /// A `LspManagerApi` test double. Owns an `LspConfig` (lent via `config()`)
    /// and returns a canned `DiagnosticBlock` per `diagnostics_for` call, while
    /// recording every (file, edit_seq) it was probed with. `enabled(false)`
    /// short-circuits the collect seam at the master switch before any probe.
    struct FakeLsp {
        config: LspConfig,
        diagnostics: Option<DiagnosticBlock>,
        calls: Mutex<Vec<(PathBuf, u64)>>,
    }

    impl FakeLsp {
        /// Enabled LSP that returns `block` for every probed file.
        fn returning(block: DiagnosticBlock) -> Arc<Self> {
            Arc::new(Self {
                config: LspConfig {
                    enabled: true,
                    ..LspConfig::default()
                },
                diagnostics: Some(block),
                calls: Mutex::new(Vec::new()),
            })
        }

        /// Disabled LSP — `config().enabled == false`, so the collect seam
        /// early-returns before probing.
        fn disabled() -> Arc<Self> {
            Arc::new(Self {
                config: LspConfig {
                    enabled: false,
                    ..LspConfig::default()
                },
                diagnostics: None,
                calls: Mutex::new(Vec::new()),
            })
        }

        /// The `(file, edit_seq)` pairs `diagnostics_for` was probed with.
        fn calls(&self) -> Vec<(PathBuf, u64)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl LspManagerApi for FakeLsp {
        fn config(&self) -> &LspConfig {
            &self.config
        }
        async fn diagnostics_for(&self, file: &Path, edit_seq: u64) -> Option<DiagnosticBlock> {
            self.calls
                .lock()
                .unwrap()
                .push((file.to_path_buf(), edit_seq));
            self.diagnostics.clone()
        }
    }

    /// A `HookHost` test double that records every `execute` call.
    /// (Mirrors `callback_bridge` tests' `RecordingHookHost`.)
    #[derive(Default)]
    struct RecordingHookHost {
        calls: std::sync::Arc<Mutex<Vec<(HookEvent, HookContext)>>>,
    }

    impl RecordingHookHost {
        fn calls(&self) -> Vec<(HookEvent, HookContext)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl HookHost for RecordingHookHost {
        fn execute(&self, event: HookEvent, context: &HookContext) -> Vec<HookResult> {
            self.calls
                .lock()
                .unwrap()
                .push((event, context.clone()));
            Vec::new()
        }
        fn execute_pre_compact_hook(&self, _context: &HookContext) -> Option<String> {
            None
        }
        fn execute_message_submit_transform(
            &self,
            _context: &HookContext,
            _original_text: &str,
        ) -> MessageSubmitOutcome {
            MessageSubmitOutcome::unchanged()
        }
        fn has_hooks_for_event(&self, _event: HookEvent) -> bool {
            true
        }
        fn is_enabled(&self) -> bool {
            true
        }
        fn session_id(&self) -> &str {
            "test"
        }
        fn collect_shell_env(&self, _context: &HookContext) -> HashMap<String, String> {
            HashMap::new()
        }
    }

    fn test_template() -> HookContext {
        HookContext::new()
            .with_session_id("test")
            .with_workspace(PathBuf::from("/tmp/codesmith-test"))
            .with_model("mock-v0")
    }

    // === stream-event builders (mirroring the bridge tests) =================

    fn text_block(idx: u32, body: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::Text {
                    text: String::new(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::TextDelta {
                    text: body.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn tool_use_block(idx: u32, id: &str, name: &str, input_json: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: serde_json::Value::Null,
                    caller: None,
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::InputJsonDelta {
                    partial_json: input_json.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    fn finish(stop: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(stop.to_string()),
                    stop_sequence: None,
                },
                usage: None,
            },
            StreamEvent::MessageStop,
        ]
    }

    /// One-file `DiagnosticBlock` with a single ERROR line, the canned payload
    /// `FakeLsp::returning` hands back per probe.
    fn error_diag_block(file: &str, line: u32, column: u32, message: &str) -> DiagnosticBlock {
        DiagnosticBlock {
            file: PathBuf::from(file),
            items: vec![Diagnostic {
                line,
                column,
                severity: Severity::Error,
                message: message.to_string(),
            }],
        }
    }

    /// Drain all events currently buffered in `rx` into a `Vec`.
    fn drain(rx: &mut mpsc::Receiver<Event>) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    fn fresh_session() -> Session {
        Session::new(
            "mock-v0".to_string(),
            PathBuf::from("/tmp/codesmith-test"),
            false,
            false,
            PathBuf::from("/tmp/codesmith-test/notes.md"),
            PathBuf::from("/tmp/codesmith-test/mcp.json"),
        )
    }

    // === tests ==============================================================

    #[tokio::test]
    async fn host_executor_drives_full_bridge_trio() {
        // Registry with a real ToolSpec → framework ToolSet via the adapter.
        let tmp = tempdir().expect("tempdir");
        let workspace_stamp = tmp.path().display().to_string();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        // Real Session → framework ChatHistory.
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);

        // CallbackBridge: mock Event channel + RecordingHookHost.
        let (tx, mut rx) = mpsc::channel(256);
        let hooks = Arc::new(RecordingHookHost::default());
        let callback: Arc<dyn Callback> = Arc::new(CallbackBridge::new(
            Some(tx),
            Some(hooks.clone()),
            test_template(),
        ));

        // Call 1: text + tool_use(echo). Call 2: text-only -> NoToolCalls.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "echo world".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, assistant(text+tooluse), user(toolresult), assistant(text)]
        assert_eq!(history.len(), 4);
        // The same bytes live on the underlying Session.
        assert_eq!(sess.messages.len(), 4);

        // The ToolResult carries the captured context's workspace path —
        // proof the ToolSpec flowed through ToolSpecAdapter into the loop.
        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert!(
                    content.starts_with(&workspace_stamp),
                    "context stamped: {content}"
                );
                assert!(content.ends_with("|world"));
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        // Event channel: ToolCallStarted + ToolCallComplete with matching ids.
        let events = drain(&mut rx);
        let started = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallStarted { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .expect("ToolCallStarted emitted");
        let complete = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallComplete { id, name, result } => {
                    Some((id.clone(), name.clone(), result.clone()))
                }
                _ => None,
            })
            .expect("ToolCallComplete emitted");
        assert_eq!(started.0, complete.0, "bridge ids correlate");
        assert_eq!(started.1, "echo");
        assert_eq!(started.2, serde_json::json!({"text":"world"}));
        assert_eq!(complete.1, "echo");
        match complete.2 {
            Ok(r) => {
                assert!(r.content.ends_with("|world"));
                assert!(r.success);
            }
            Err(e) => panic!("expected Ok ToolResult, got Err: {e}"),
        }

        // HookHost: ToolCallBefore + ToolCallAfter with full context.
        let calls = hooks.calls();
        assert_eq!(calls.len(), 2, "one Before + one After");
        assert_eq!(calls[0].0, HookEvent::ToolCallBefore);
        assert_eq!(calls[1].0, HookEvent::ToolCallAfter);
        assert_eq!(calls[0].1.tool_name.as_deref(), Some("echo"));
        assert_eq!(calls[0].1.session_id.as_deref(), Some("test"));
        assert_eq!(calls[1].1.tool_name.as_deref(), Some("echo"));
        assert_eq!(calls[1].1.tool_result.as_deref().unwrap().ends_with("|world"), true);
        assert_eq!(calls[1].1.tool_success, Some(true));
    }

    #[tokio::test]
    async fn host_executor_missing_tool_records_error_result() {
        // Empty ToolSet -> "ghost" lookup fails with NotAvailable.
        let tools = Arc::new(ToolSet::new());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call = text_block(0, "calling ghost");
        call.extend(tool_use_block(1, "t1", "ghost", r#"{}"#));
        call.extend(finish("tool_use"));
        let mut finish_call = text_block(0, "ok");
        finish_call.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call, finish_call])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult {
                content,
                is_error,
                ..
            } => {
                assert!(content.starts_with("Error:"));
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn host_executor_exhausts_steps() {
        // Mock always returns a tool call -> hits MaxSteps.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let make_call = || {
            let mut c = text_block(0, "looping");
            c.extend(tool_use_block(1, "t1", "echo", r#"{"text":"x"}"#));
            c.extend(finish("tool_use"));
            c
        };

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![make_call(), make_call(), make_call()])),
            tools,
            callback,
            AgentExecutorConfig {
                max_steps: 2,
                ..AgentExecutorConfig::default()
            },
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::MaxSteps);
        // user + (assistant + toolresult) x2 = 1 + 2*2 = 5
        assert_eq!(history.len(), 5);
    }

    // === loop-guard (seam 3 + 4) ===========================================

    #[tokio::test]
    async fn loop_guard_blocks_third_identical_call() {
        let tmp = tempdir().expect("tempdir");
        let workspace_stamp = tmp.path().display().to_string();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Three identical echo calls, then a text-only turn that ends the run.
        let call = || {
            let mut c = text_block(0, "again");
            c.extend(tool_use_block(1, "t1", "echo", r#"{"text":"x"}"#));
            c.extend(finish("tool_use"));
            c
        };
        let mut done = text_block(0, "done");
        done.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call(), call(), call(), done])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, asst, tr(echo), asst, tr(echo), asst, tr(block), asst] = 8.
        assert_eq!(history.len(), 8);

        // First two tool results are real echo output (workspace-stamped) —
        // proof the tool actually ran twice.
        for &idx in &[2usize, 4] {
            match &sess.messages[idx].content[0] {
                ContentBlock::ToolResult { content, is_error, .. } => {
                    assert!(
                        content.starts_with(&workspace_stamp),
                        "echo ran at msg[{idx}]: {content}"
                    );
                    assert_eq!(*is_error, Some(false));
                }
                other => panic!("msg[{idx}] not ToolResult: {other:?}"),
            }
        }
        // Third is the loop-guard block — echo did NOT run, error, block message.
        match &sess.messages[6].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert!(
                    !content.starts_with(&workspace_stamp),
                    "echo must not run on the blocked call: {content}"
                );
                assert!(
                    content.contains("already been made 3 times"),
                    "block message: {content}"
                );
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("msg[6] not ToolResult: {other:?}"),
        }
    }

    #[tokio::test]
    async fn loop_guard_warns_at_three_failures() {
        // No tools registered — every tool call hits "ghost" (NotAvailable),
        // which counts as a failure for the loop-guard.
        let tools = Arc::new(ToolSet::new());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Vary the args each call so `record_attempt` (keyed on name+args) never
        // blocks; `record_outcome` is keyed on name only, so failures still
        // accumulate toward the warn threshold (3).
        let failing = |n: u64| {
            let mut c = text_block(0, "trying");
            c.extend(tool_use_block(1, "t1", "ghost", &format!(r#"{{"n":{n}}}"#)));
            c.extend(finish("tool_use"));
            c
        };
        let mut done = text_block(0, "done");
        done.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![failing(1), failing(2), failing(3), done])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        let events = drain(&mut rx);
        let warned = events.iter().any(|e| {
            matches!(
                e,
                Event::Status { message } if message.contains("failed 3 consecutive times")
            )
        });
        assert!(warned, "expected a warn status event, got: {events:?}");
    }

    #[tokio::test]
    async fn loop_guard_halts_after_eight_failures() {
        let tools = Arc::new(ToolSet::new()); // ghost

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let failing = |n: u64| {
            let mut c = text_block(0, "trying");
            c.extend(tool_use_block(1, "t1", "ghost", &format!(r#"{{"n":{n}}}"#)));
            c.extend(finish("tool_use"));
            c
        };
        // 8 distinct-arg failures → the 8th triggers Halt.
        let calls: Vec<Vec<StreamEvent>> = (1..=8).map(failing).collect();

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(calls)),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        let msg = match reason {
            StopReason::Error(m) => m,
            other => panic!("expected Error, got {other:?}"),
        };
        assert!(
            msg.contains("failed 8 consecutive times"),
            "halt message: {msg}"
        );

        let events = drain(&mut rx);
        let halted = events.iter().any(|e| {
            matches!(
                e,
                Event::Status { message } if message.contains("failed 8 consecutive times")
            )
        });
        assert!(halted, "expected a halt status event, got: {events:?}");
    }

    // === LSP flush (seam 1 + 3) ==========================================

    /// Helper: does any message in `sess` carry a `<diagnostics` text block?
    fn has_diagnostics_msg(sess: &Session) -> bool {
        sess.messages.iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text.contains("<diagnostics"))
            })
        })
    }

    #[tokio::test]
    async fn lsp_collect_then_flush_feeds_model() {
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EditSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let fake = FakeLsp::returning(error_diag_block("foo.rs", 12, 8, "missing semicolon"));
        let probe = LspProbe::new(fake.clone(), tmp.path().to_path_buf());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Call 1: edit_file -> tool_use (collect probes LSP). Call 2: text -> end.
        let mut call1 = text_block(0, "editing");
        call1.extend(tool_use_block(1, "t1", "edit_file", r#"{"path":"foo.rs"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let mock = Arc::new(MockLlm::new(vec![call1, call2]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            Some(probe),
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "edit foo.rs".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user, asst(text+tooluse), user(toolresult), user(<diagnostics>), asst]
        assert_eq!(history.len(), 5);
        assert_eq!(sess.messages[3].role.as_str(), "user");
        match &sess.messages[3].content[0] {
            ContentBlock::Text { text, .. } => {
                assert!(text.contains("<diagnostics"), "rendered block: {text}");
                assert!(text.contains("missing semicolon"));
                assert!(text.contains("foo.rs"));
            }
            other => panic!("expected diagnostics Text block, got {other:?}"),
        }

        // The model actually saw it — call2's request snapshot included it.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2);
        let saw_diag = reqs[1].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text.contains("<diagnostics"))
            })
        });
        assert!(saw_diag, "call2 request must include diagnostics: {reqs:?}");

        // Probed once, for the edited file (relativized to the workspace).
        assert_eq!(fake.calls().len(), 1);
        assert!(
            fake.calls()[0].0.ends_with("foo.rs"),
            "probed path: {:?}",
            fake.calls()[0].0
        );
    }

    #[tokio::test]
    async fn lsp_disabled_skips_collect() {
        // Unit check of the master-switch gate inside `collect_lsp_diagnostics`.
        let fake = FakeLsp::disabled();
        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![])),
            Arc::new(ToolSet::new()),
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig::default(),
            None,
            Some(LspProbe::new(fake.clone(), PathBuf::from("/tmp/ws"))),
            None,
            None,
            None,
            None,
        );
        executor
            .collect_lsp_diagnostics("edit_file", &serde_json::json!({"path":"foo.rs"}))
            .await;
        assert!(
            fake.calls().is_empty(),
            "disabled LSP must not be probed: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn lsp_skips_non_edit_tool() {
        // Non-edit tool name → no path derivation → no probe.
        let fake = FakeLsp::returning(error_diag_block("foo.rs", 1, 1, "x"));
        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![])),
            Arc::new(ToolSet::new()),
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig::default(),
            None,
            Some(LspProbe::new(fake.clone(), PathBuf::from("/tmp/ws"))),
            None,
            None,
            None,
            None,
        );
        executor
            .collect_lsp_diagnostics("echo", &serde_json::json!({"text":"hi"}))
            .await;
        assert!(
            fake.calls().is_empty(),
            "non-edit tool must not probe LSP: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn lsp_skips_failed_edit() {
        // The loop's success gate (r.success) must skip collect on a failed edit.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(FailingEditSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let fake = FakeLsp::returning(error_diag_block("foo.rs", 1, 1, "stale"));
        let probe = LspProbe::new(fake.clone(), tmp.path().to_path_buf());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call1 = text_block(0, "editing");
        call1.extend(tool_use_block(1, "t1", "edit_file", r#"{"path":"foo.rs"}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call1, call2])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            Some(probe),
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "edit foo.rs".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert!(
            fake.calls().is_empty(),
            "failed edit must not probe LSP: {:?}",
            fake.calls()
        );
        assert!(!has_diagnostics_msg(&sess), "no diagnostics message expected");
    }

    #[tokio::test]
    async fn lsp_apply_patch_paths_deferred() {
        // apply_patch path derivation is deferred (needs HostServices) — collect
        // must not probe even though config is enabled. Pins the gap; flips when
        // the executor later wires a real HostServices.
        let fake = FakeLsp::returning(error_diag_block("a.rs", 1, 1, "x"));
        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![])),
            Arc::new(ToolSet::new()),
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig::default(),
            None,
            Some(LspProbe::new(fake.clone(), PathBuf::from("/tmp/ws"))),
            None,
            None,
            None,
            None,
        );
        executor
            .collect_lsp_diagnostics("apply_patch", &serde_json::json!({"patch":"x"}))
            .await;
        assert!(
            fake.calls().is_empty(),
            "apply_patch must not probe LSP yet: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn lsp_cross_turn_persistence_via_shared_state() {
        // THE interior-mutability proof: `pending_lsp_blocks` (Arc<Mutex<Vec>>)
        // persists across `run()` calls on the SAME executor. run1 edits then hits
        // MaxSteps (max_steps:1) before flushing, leaving pending non-empty; run2
        // on a fresh session flushes those leftovers into its first request.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EditSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let fake = FakeLsp::returning(error_diag_block("foo.rs", 12, 8, "missing semicolon"));
        let probe = LspProbe::new(fake.clone(), tmp.path().to_path_buf());

        let mock = Arc::new(MockLlm::new(vec![
            // run1: edit -> tool_use (then MaxSteps halts before a 2nd request).
            {
                let mut c = text_block(0, "editing");
                c.extend(tool_use_block(1, "t1", "edit_file", r#"{"path":"foo.rs"}"#));
                c.extend(finish("tool_use"));
                c
            },
            // run2: text-only -> end (NoToolCalls).
            {
                let mut c = text_block(0, "ok");
                c.extend(finish("end_turn"));
                c
            },
        ]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            Arc::new(codesmith_agent::callback::NoopCallback),
            AgentExecutorConfig {
                max_steps: 1,
                ..AgentExecutorConfig::default()
            },
            None,
            Some(probe),
            None,
            None,
            None,
            None,
        );

        // Run 1: edits foo.rs (collect pushes to pending), then MaxSteps halts
        // before the next step's flush (the max_steps bail precedes the flush
        // seam), so pending carries over.
        let mut sess_a = fresh_session();
        let mut history_a = SessionChatHistory::new(&mut sess_a);
        let reason = executor
            .run(&mut history_a, "edit foo.rs".to_string())
            .await
            .expect("run1");
        assert_eq!(reason, StopReason::MaxSteps);
        assert!(!has_diagnostics_msg(&sess_a), "run1 must not flush before MaxSteps");

        // Run 2: SAME executor, FRESH session. The first pre-request flush must
        // drain run1's leftover pending into run2's transcript — impossible with
        // a per-run local Vec; proves the Arc<Mutex<Vec>> persists across runs.
        let mut sess_b = fresh_session();
        let mut history_b = SessionChatHistory::new(&mut sess_b);
        let reason = executor
            .run(&mut history_b, "next turn".to_string())
            .await
            .expect("run2");
        assert_eq!(reason, StopReason::NoToolCalls);

        // sess_b: [user_text, <diagnostics flush>, asst] — from run1's edit.
        assert_eq!(history_b.len(), 3);
        assert_eq!(sess_b.messages[1].role.as_str(), "user");
        match &sess_b.messages[1].content[0] {
            ContentBlock::Text { text, .. } => {
                assert!(text.contains("<diagnostics"), "flush block: {text}");
                assert!(text.contains("missing semicolon"));
            }
            other => panic!("expected diagnostics flush msg, got {other:?}"),
        }
        // And the model saw it in run2's (only) request.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2, "run1 + run2 each fired one request");
        let saw = reqs[1].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text.contains("<diagnostics"))
            })
        });
        assert!(saw, "run2 request must include leftover diagnostics: {reqs:?}");
    }

    // === transparent-retry (seam 2) =======================================
    //
    // The production `handle_deepseek_turn` silently re-issues a request when
    // the chunked-transfer stream dies mid-flight before any content was
    // committed (the #103 "stream died with nothing" retry, `turn_loop.rs`
    // :1152). `HostAgentExecutor` absorbs that at the (2) post-stream seam:
    // `accumulate_stream` returns `Err` on the first erroring stream item
    // (dropping any partial blocks — so an `Err` means "no actionable content
    // committed"), and the executor re-sends the same request up to
    // `MAX_STREAM_RETRIES` (3) times before propagating the failure. A healthy
    // round resets the budget. (Cancel-token short-circuit is deferred to the
    // wire-in slice; the bounded budget can't loop forever.)

    /// All `Event::Status` messages drained from `rx`, in arrival order.
    fn statuses(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Status { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn transparent_retry_recovers_after_stream_error() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // First round: stream dies mid-flight (no content). Second round: a
        // clean text+end_turn turn that ends the run.
        let mut ok = text_block(0, "recovered");
        ok.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::with_rounds(vec![
            MockRound::StreamErr("connection reset".into()),
            MockRound::Events(ok),
        ]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run should recover via transparent retry");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The request was issued twice: the failed attempt + the retry.
        assert_eq!(mock.requests().len(), 2, "initial attempt + one retry");

        // A status surfaced the retry, and no partial assistant message was
        // committed before the retry (only the seed user + the recovered turn).
        let msgs = statuses(&drain(&mut rx));
        assert!(
            msgs.iter().any(|m| m.contains("retrying (1/3")),
            "expected a retry status, got: {msgs:?}"
        );
        assert_eq!(history.len(), 2, "[user, assistant(text recovered)]");
    }

    #[tokio::test]
    async fn transparent_retry_exhausts_budget_then_fails() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Four consecutive stream deaths — budget is 3 retries (4 attempts).
        let mock = Arc::new(MockLlm::with_rounds(vec![
            MockRound::StreamErr("die 1".into()),
            MockRound::StreamErr("die 2".into()),
            MockRound::StreamErr("die 3".into()),
            MockRound::StreamErr("die 4".into()),
        ]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            None,
        );

        let err = executor
            .run(&mut history, "go".to_string())
            .await
            .expect_err("budget exhausted should surface the failure");

        // 1 initial attempt + 3 retries = 4 create_message_stream calls.
        assert_eq!(mock.requests().len(), 4, "initial + 3 retries");
        assert!(
            err.to_string().contains("die 4"),
            "last attempt's error should propagate: {err}"
        );

        // Three retry statuses (1/3, 2/3, 3/3) — the 4th attempt fails outright.
        let msgs = statuses(&drain(&mut rx));
        assert_eq!(msgs.len(), 3, "one status per retry: {msgs:?}");
        assert!(msgs[0].contains("1/3"), "{}", msgs[0]);
        assert!(msgs[1].contains("2/3"), "{}", msgs[1]);
        assert!(msgs[2].contains("3/3"), "{}", msgs[2]);
        // No assistant turn was committed — only the seed user message.
        assert_eq!(history.len(), 1);
    }

    #[tokio::test]
    async fn transparent_retry_resets_budget_across_steps() {
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Step 1: die then recover (text + tool_use(echo)); Step 2: die then
        // recover (text + end_turn). Both steps retry exactly once — proving
        // the budget reset between steps (else step 2's retry would be 2/3).
        let mut step1 = text_block(0, "step one");
        step1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"a"}"#));
        step1.extend(finish("tool_use"));
        let mut step2 = text_block(0, "done");
        step2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::with_rounds(vec![
            MockRound::StreamErr("die s1".into()),
            MockRound::Events(step1),
            MockRound::StreamErr("die s2".into()),
            MockRound::Events(step2),
        ]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // 2 calls per step (die + retry) = 4 total.
        assert_eq!(mock.requests().len(), 4, "die+recover per step");

        // Both retry statuses numbered 1/3 — the budget reset between steps.
        let msgs = statuses(&drain(&mut rx));
        assert_eq!(msgs.len(), 2, "one retry per step: {msgs:?}");
        assert!(
            msgs.iter().all(|m| m.contains("retrying (1/3")),
            "both steps' first retry must be 1/3 (budget reset): {msgs:?}"
        );
    }

    #[tokio::test]
    async fn transparent_retry_skips_clean_empty_stream() {
        // A stream that completes cleanly but produced no content blocks is
        // NOT a "stream died" situation (production gates on `stream_errors >
        // 0`). The executor must not retry it — it surfaces NoToolCalls.
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mock = Arc::new(MockLlm::with_rounds(vec![MockRound::Events(vec![])]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Exactly one request — no retry on a clean (error-free) empty stream.
        assert_eq!(mock.requests().len(), 1, "clean empty stream must not retry");
        assert!(statuses(&drain(&mut rx)).is_empty(), "no retry status");
    }

    // === steer (seam 1) ==================================================
    //
    // The production `handle_deepseek_turn` drains queued steer inputs at the
    // very top of each step (`turn_loop.rs:300-317`) — before the LLM request
    // snapshot — so the user's in-flight text reaches the model this step.
    // `HostAgentExecutor` absorbs that at the (1) pre-request seam:
    // `drain_steers` does a non-blocking `try_recv` loop, trimming and pushing
    // each as a `user` message, emitting a status per accepted input. The
    // receiver is `Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>` —
    // interior-mutable because `AgentExecutor::run` is `&self` while
    // `try_recv` takes `&mut self`. Only the pre-request drain is absorbed;
    // the mid-stream buffer / post-stream resume / blocking `recv` during
    // sub-agent hold are streaming-lifecycle-specific and deferred.

    /// Create a steer channel pair: the sender for tests to enqueue steers, and
    /// the interior-mutable receiver the executor expects.
    fn steer_channel() -> (mpsc::Sender<String>, Arc<Mutex<mpsc::Receiver<String>>>) {
        let (tx, rx) = mpsc::channel::<String>(64);
        (tx, Arc::new(Mutex::new(rx)))
    }

    /// Create an approval channel pair: the sender for tests to push
    /// `ApprovalDecision`s (matched by wire tool id), and the interior-mutable
    /// receiver the executor expects. Uses `tokio::sync::Mutex` (unlike
    /// `steer_channel`'s `std::sync::Mutex`) because the approval await blocks
    /// on `recv().await` — the guard must cross an `await`.
    fn approval_channel() -> (
        mpsc::Sender<ApprovalDecision>,
        Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>,
    ) {
        let (tx, rx) = mpsc::channel::<ApprovalDecision>(64);
        (tx, Arc::new(tokio::sync::Mutex::new(rx)))
    }

    #[tokio::test]
    async fn steer_drain_injects_queued_input_before_request() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();
        // Pre-queue two steers before run starts.
        tx_steer.send("remember this".to_string()).await.unwrap();
        tx_steer.send("and also this".to_string()).await.unwrap();

        let mut ok = text_block(0, "acknowledged");
        ok.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![ok]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            Some(rx_steer),
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "start".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user(seed), user(steer1), user(steer2), assistant]
        assert_eq!(history.len(), 4);
        assert_eq!(sess.messages[1].role.as_str(), "user");
        assert_eq!(sess.messages[2].role.as_str(), "user");
        match &sess.messages[1].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "remember this"),
            other => panic!("expected steer Text, got {other:?}"),
        }
        match &sess.messages[2].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "and also this"),
            other => panic!("expected steer Text, got {other:?}"),
        }

        // The model saw both steers in its (only) request.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 1);
        let saw1 = reqs[0].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text == "remember this")
            })
        });
        let saw2 = reqs[0].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text == "and also this")
            })
        });
        assert!(saw1, "request must include first steer: {reqs:?}");
        assert!(saw2, "request must include second steer: {reqs:?}");
    }

    #[tokio::test]
    async fn steer_none_is_noop() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call = text_block(0, "hello");
        call.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![call])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None, // no steer receiver
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // [user(seed), assistant] — no extra steer messages.
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    async fn steer_skips_empty_and_whitespace() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();
        // Empty / whitespace-only strings must all be skipped (trimmed to "").
        tx_steer.send(String::new()).await.unwrap();
        tx_steer.send("   ".to_string()).await.unwrap();
        tx_steer.send("\t\n".to_string()).await.unwrap();

        let mut ok = text_block(0, "nothing steered");
        ok.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![ok])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            Some(rx_steer),
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // [user(seed), assistant] — no steer messages (all were empty).
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    async fn steer_emits_status_per_accepted_input() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();
        tx_steer.send("first steer".to_string()).await.unwrap();
        tx_steer.send("second steer".to_string()).await.unwrap();

        let mut ok = text_block(0, "ok");
        ok.extend(finish("end_turn"));

        let executor = HostAgentExecutor::new(
            Arc::new(MockLlm::new(vec![ok])),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            Some(rx_steer),
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        let accepted: Vec<_> = statuses(&drain(&mut rx))
            .iter()
            .filter(|s| s.contains("Steer input accepted"))
            .cloned()
            .collect();
        assert_eq!(
            accepted.len(),
            2,
            "one status per accepted steer: {accepted:?}"
        );
    }

    #[tokio::test]
    async fn steer_picks_up_input_queued_between_runs() {
        // THE receiver-persistence proof: the steer receiver lives on the
        // executor struct (Arc<Mutex<Receiver>>), not as a per-run local — so a
        // steer queued between two runs on the SAME executor is picked up on
        // the second run's first pre-request drain (mirrors the LSP
        // cross-turn persistence test pattern).
        let tools = Arc::new(ToolSet::new());
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();

        // Run 1: text-only "first" + end_turn. Run 2: text-only "second" + end_turn.
        let mut run1 = text_block(0, "first turn");
        run1.extend(finish("end_turn"));
        let mut run2 = text_block(0, "second turn");
        run2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![run1, run2]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            Some(rx_steer),
            None,
            None,
            None,
        );

        // Run 1: no steers queued — clean text-only turn.
        let mut sess_a = fresh_session();
        let mut history_a = SessionChatHistory::new(&mut sess_a);
        let reason = executor
            .run(&mut history_a, "start".to_string())
            .await
            .expect("run1");
        assert_eq!(reason, StopReason::NoToolCalls);
        // [user(seed), assistant] — no steer messages.
        assert_eq!(history_a.len(), 2);

        // Queue a steer between runs.
        tx_steer
            .send("steered between runs".to_string())
            .await
            .unwrap();

        // Run 2: SAME executor, FRESH session — the steer is picked up on the
        // first pre-request drain. A per-run local receiver couldn't do this.
        let mut sess_b = fresh_session();
        let mut history_b = SessionChatHistory::new(&mut sess_b);
        let reason = executor
            .run(&mut history_b, "next".to_string())
            .await
            .expect("run2");
        assert_eq!(reason, StopReason::NoToolCalls);

        // sess_b: [user(seed), user(steer), assistant]
        assert_eq!(history_b.len(), 3);
        assert_eq!(sess_b.messages[1].role.as_str(), "user");
        match &sess_b.messages[1].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "steered between runs"),
            other => panic!("expected steer Text, got {other:?}"),
        }
        // And the model saw it in run2's request.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2, "run1 + run2 each fired one request");
        let saw = reqs[1].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text == "steered between runs")
            })
        });
        assert!(saw, "run2 request must include the steer: {reqs:?}");
    }

    // === approval ===========================================================

    /// Build a `WriteSpec` tool registry → framework `ToolSet`. `WriteSpec`
    /// declares `WritesFiles`, so the §E approval gate fires for it.
    fn write_tools() -> Arc<ToolSet> {
        let mut registry = ToolRegistry::new(ToolContext::new(PathBuf::from("/tmp/ws")));
        registry.register(Arc::new(WriteSpec));
        Arc::new(registry.to_framework_tool_set())
    }

    /// Round 1: the model explains intent then calls `write_file` (id `call_1`).
    fn write_call() -> Vec<StreamEvent> {
        let mut call = text_block(0, "writing the file now");
        call.extend(tool_use_block(1, "call_1", "write_file", r#"{"path":"/tmp/x"}"#));
        call.extend(finish("tool_use"));
        call
    }

    /// Round 2: a clean text turn ending the loop.
    fn end_call() -> Vec<StreamEvent> {
        let mut call = text_block(0, "done");
        call.extend(finish("end_turn"));
        call
    }

    #[tokio::test]
    async fn approval_approved_runs_tool() {
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (tx_approval, rx_approval) = approval_channel();
        // Pre-push an Approved decision matching the tool_use id.
        tx_approval
            .send(ApprovalDecision::Approved {
                id: "call_1".to_string(),
            })
            .await
            .unwrap();

        let mock = Arc::new(MockLlm::new(vec![write_call(), end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            write_tools(),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            Some(rx_approval),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "please write the file".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // [user(seed), assistant(text+tool_use), user(tool_result), assistant]
        assert_eq!(history.len(), 4);
        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert_eq!(content, "wrote:/tmp/x", "approved tool must run");
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_denied_skips_tool_with_permission_error() {
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (tx_approval, rx_approval) = approval_channel();
        tx_approval
            .send(ApprovalDecision::Denied {
                id: "call_1".to_string(),
            })
            .await
            .unwrap();

        let mock = Arc::new(MockLlm::new(vec![write_call(), end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            write_tools(),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            Some(rx_approval),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "please write the file".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The tool never ran: the result is a permission-denied error fed back
        // to the model (turn continues).
        assert_eq!(history.len(), 4);
        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert!(!content.contains("wrote:"), "tool must not have run: {content}");
                assert!(content.contains("denied"), "must be a denial: {content}");
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_none_skips_gating() {
        // No approval channel ⇒ WriteSpec runs ungated (no event, no blocking).
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mock = Arc::new(MockLlm::new(vec![write_call(), end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            write_tools(),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None, // no approval channel
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "please write the file".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert_eq!(content, "wrote:/tmp/x", "ungated tool must run");
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_readonly_tool_skips_gating() {
        // EchoSpec (ReadOnly) + an approval channel with NO decision pushed: a
        // read-only tool must not hit the gate (else `recv()` would block).
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (_tx_approval, rx_approval) = approval_channel(); // no decision pushed

        let tmp = tempdir().expect("tempdir");
        let workspace_stamp = tmp.path().display().to_string();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut call1 = text_block(0, "echoing");
        call1.extend(tool_use_block(1, "call_1", "echo", r#"{"text":"hi"}"#));
        call1.extend(finish("tool_use"));
        let mock = Arc::new(MockLlm::new(vec![call1, end_call()]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            Some(rx_approval),
            None,
            None,
        );

        // If the gate wrongly fires, recv() blocks → the timeout fails the test.
        let reason = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            executor.run(&mut history, "echo hi".to_string()),
        )
        .await
        .expect("read-only tool must skip the approval gate (no hang)")
        .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Echo ran, stamped with the workspace path (context flowed through).
        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert!(
                    content.starts_with(&workspace_stamp),
                    "echo ran ungated: {content}"
                );
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_emits_approval_required_event() {
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (tx_event, mut rx_event) = mpsc::channel(256);
        let (tx_approval, rx_approval) = approval_channel();
        tx_approval
            .send(ApprovalDecision::Approved {
                id: "call_1".to_string(),
            })
            .await
            .unwrap();

        let mock = Arc::new(MockLlm::new(vec![write_call(), end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            write_tools(),
            callback,
            AgentExecutorConfig::default(),
            Some(tx_event),
            None,
            None,
            Some(rx_approval),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "please write the file".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Drain the event channel; find the ApprovalRequired.
        let mut found: Option<Event> = None;
        while let Ok(ev) = rx_event.try_recv() {
            if matches!(ev, Event::ApprovalRequired { .. }) {
                found = Some(ev);
            }
        }
        let ev = found.expect("an ApprovalRequired event was emitted");
        match ev {
            Event::ApprovalRequired {
                id,
                tool_name,
                description,
                approval_key,
                approval_grouping_key,
                intent_summary,
                ..
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(tool_name, "write_file");
                assert_eq!(description, "Writes a file (requires approval).");
                assert!(!approval_key.is_empty(), "fingerprint must be built");
                assert!(!approval_grouping_key.is_empty());
                // write tool (not read-only) → the model's preceding text is attached.
                assert!(
                    intent_summary.as_ref().is_some_and(|s| s.contains("writing")),
                    "intent summary must carry the model's text: {intent_summary:?}"
                );
            }
            _ => unreachable!("matched ApprovalRequired above"),
        }
    }

    #[tokio::test]
    async fn approval_retry_with_policy_treated_as_approved() {
        // RetryWithPolicy carries a sandbox policy the framework `Tool::run` path
        // can't honor; the by-design treatment is Approved (tool runs with the
        // fixed context). Sandbox elevation threads in at the wire-in step.
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (tx_approval, rx_approval) = approval_channel();
        tx_approval
            .send(ApprovalDecision::RetryWithPolicy {
                id: "call_1".to_string(),
                policy: crate::sandbox::SandboxPolicy::default(),
            })
            .await
            .unwrap();

        let mock = Arc::new(MockLlm::new(vec![write_call(), end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            write_tools(),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            Some(rx_approval),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "please write the file".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        match &sess.messages[2].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert_eq!(content, "wrote:/tmp/x", "tool ran on RetryWithPolicy");
                assert_eq!(*is_error, Some(false));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    // === compaction helpers ===============================================

    fn compaction_config_low_threshold() -> CompactionConfig {
        CompactionConfig {
            enabled: true,
            token_threshold: 100,
            model: "mock-v0".to_string(),
            cache_summary: false,
            auto_floor_tokens: 0,
        }
    }

    fn compaction_config_high_threshold() -> CompactionConfig {
        CompactionConfig {
            enabled: true,
            token_threshold: 967_000,
            ..CompactionConfig::default()
        }
    }

    fn compaction_config_disabled() -> CompactionConfig {
        CompactionConfig {
            enabled: false,
            ..CompactionConfig::default()
        }
    }

    /// Seed `n` alternating user/assistant text messages (~200 chars each) so
    /// the transcript exceeds a low compaction threshold.
    fn seed_text_messages(sess: &mut Session, n: usize) {
        let body = "x".repeat(200);
        for i in 0..n {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            sess.add_message(Message {
                role: role.to_string(),
                content: vec![ContentBlock::Text {
                    text: format!("{i}: {body}"),
                    cache_control: None,
                }],
            });
        }
    }

    /// Seed a `file_read` tool call + a >32 KB tool result so micro-compaction's
    /// byte trigger fires (`estimate_compactable_bytes >= 32 KB`).
    fn seed_large_file_read(sess: &mut Session) {
        sess.add_message(Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "fr1".to_string(),
                name: "file_read".to_string(),
                input: serde_json::json!({"path": "a.rs"}),
                caller: None,
            }],
        });
        sess.add_message(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "fr1".to_string(),
                content: "x".repeat(33_000),
                is_error: None,
                content_blocks: None,
            }],
        });
    }

    // === compaction tests =================================================

    #[tokio::test]
    async fn compaction_none_is_noop() {
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // No probe ⇒ no create_message call at all.
        assert_eq!(mock.compaction_calls(), 0);
    }

    #[tokio::test]
    async fn compaction_disabled_skips_even_when_over_threshold() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 12);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            Some(CompactionProbe::new(
                compaction_config_disabled(),
                PathBuf::from("/tmp/codesmith-test"),
            )),
            None,
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // enabled=false ⇒ run_compaction bails before any LLM call, even though
        // the transcript (13 seeded + user text) would exceed a low threshold.
        assert_eq!(mock.compaction_calls(), 0);
        // Transcript untouched by compaction: 12 seeded + user text + assistant.
        assert_eq!(history.len(), 14);
    }

    #[tokio::test]
    async fn micro_compact_clears_old_tool_results() {
        let mut sess = fresh_session();
        seed_large_file_read(&mut sess);
        // A couple of trailing text messages so the transcript is well-formed.
        sess.add_message(Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "analysis done".to_string(),
                cache_control: None,
            }],
        });
        sess.add_message(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "follow up".to_string(),
                cache_control: None,
            }],
        });
        let transcript_len_before = sess.messages.len();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            // High threshold ⇒ auto-compaction won't fire; only micro-compact.
            Some(CompactionProbe::new(
                compaction_config_high_threshold(),
                PathBuf::from("/tmp/codesmith-test"),
            )),
            None,
        );
        let reason = executor
            .run(&mut history, "what did the file say".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Micro-compact is a no-API pass — no LLM call.
        assert_eq!(mock.compaction_calls(), 0);
        // The 33 KB file_read result is now the cleared placeholder. Mirrors
        // `micro_compact::CLEARED_PLACEHOLDER` (private in that module). Read
        // via `history.messages()` (not `sess.messages`) — `history` holds the
        // `&mut Session` borrow, so `sess` can't be borrowed again here.
        let placeholder = "[tool result cleared for context economy]";
        match &history.messages()[1].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, placeholder, "tool result was micro-compacted");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        // Message count is unchanged — micro-compact clears content in place,
        // it doesn't drop messages (unlike the LLM-summary auto-compact).
        assert_eq!(history.len(), transcript_len_before + 2);
    }

    #[tokio::test]
    async fn auto_compact_summarizes_when_over_threshold() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 12);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(
            MockLlm::new(vec![end_call()]).with_compaction_summary("Conversation summary."),
        );
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            Some(CompactionProbe::new(
                compaction_config_low_threshold(),
                PathBuf::from("/tmp/codesmith-test"),
            )),
            None,
        );
        let reason = executor
            .run(&mut history, "continue".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // One create_message call for the compaction summary.
        assert_eq!(mock.compaction_calls(), 1);
        // The transcript shrank: 12 seeded + user text (13) → compacted
        // (recent tail + summary), well under 13.
        assert!(
            history.len() < 13,
            "transcript compacted: {} < 13",
            history.len()
        );
        // The stream request saw the *compacted* transcript, not the 13-message
        // original — proof the summary was applied before the request snapshot.
        assert!(
            mock.requests()[0].len() < 13,
            "stream request used compacted transcript: {} < 13",
            mock.requests()[0].len()
        );
    }

    #[tokio::test]
    async fn compaction_circuit_breaker_records_failure() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 12);
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock =
            Arc::new(MockLlm::new(vec![end_call()]).with_compaction_error("mock compaction failure"));
        let probe = CompactionProbe::new(
            compaction_config_low_threshold(),
            PathBuf::from("/tmp/codesmith-test"),
        );
        let breaker = probe.breaker().clone();
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            Some(probe),
            None,
        );
        let reason = executor
            .run(&mut history, "continue".to_string())
            .await
            .expect("run");
        // The turn still completes — a failed compaction is caught, not fatal.
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 1);
        assert_eq!(breaker.lock().unwrap().consecutive_failures(), 1);
        let statuses = statuses(&drain(&mut rx));
        assert!(
            statuses.iter().any(|s| s.contains("Compaction failed")),
            "compaction-failure status emitted: {statuses:?}"
        );
    }

    #[tokio::test]
    async fn compaction_cross_turn_circuit_breaker_persistence() {
        // Interior-mutability proof: the Arc<Mutex<CompactionCircuitBreaker>>
        // on the executor persists across `run` calls (matching
        // `Session.circuit_breaker`). A per-run local breaker would reset to 0
        // each run; here run1 records failure #1 and run2 (same executor, new
        // session) records failure #2.
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(
            MockLlm::new(vec![end_call(), end_call()]).with_compaction_error("mock compaction failure"),
        );
        let probe = CompactionProbe::new(
            compaction_config_low_threshold(),
            PathBuf::from("/tmp/codesmith-test"),
        );
        let breaker = probe.breaker().clone();
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            Some(probe),
            None,
        );

        // run1: seed an over-threshold transcript → compaction attempted → fails.
        let mut sess1 = fresh_session();
        seed_text_messages(&mut sess1, 12);
        let mut history1 = SessionChatHistory::new(&mut sess1);
        let reason1 = executor
            .run(&mut history1, "turn one".to_string())
            .await
            .expect("run");
        assert_eq!(reason1, StopReason::NoToolCalls);
        assert_eq!(breaker.lock().unwrap().consecutive_failures(), 1);

        // run2: SAME executor (same probe → same breaker), NEW session.
        let mut sess2 = fresh_session();
        seed_text_messages(&mut sess2, 12);
        let mut history2 = SessionChatHistory::new(&mut sess2);
        let reason2 = executor
            .run(&mut history2, "turn two".to_string())
            .await
            .expect("run");
        assert_eq!(reason2, StopReason::NoToolCalls);
        // A per-run-local breaker would be 1 here; persistence ⇒ 2.
        assert_eq!(breaker.lock().unwrap().consecutive_failures(), 2);
        assert_eq!(mock.compaction_calls(), 2);
    }

    // === capacity helpers ================================================

    fn capacity_probe(api_provider: ApiProvider, model: &str) -> CapacityProbe {
        CapacityProbe::new(
            api_provider,
            model.to_string(),
            CompactionConfig::default(),
            PathBuf::from("/tmp/codesmith-test"),
        )
    }

    // === capacity tests ==================================================

    #[tokio::test]
    async fn capacity_none_is_noop() {
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // No capacity probe ⇒ no preflight, no compaction call.
        assert_eq!(mock.compaction_calls(), 0);
    }

    #[tokio::test]
    async fn capacity_within_budget_proceeds() {
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        // Ollama / "llama2" → context_window 8192, budget = 8192 − 4096 − 1024
        // = 3072 tokens. A single "hello" (~17 tokens) is far under budget.
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 0);
    }

    #[tokio::test]
    async fn capacity_over_budget_recovers_via_compaction() {
        let mut sess = fresh_session();
        // 40 text messages × ~200 chars ≈ 40 × 75 × 1.5 + framing ≈ 3615
        // tokens > 3072 budget (Ollama / "llama2").
        seed_text_messages(&mut sess, 40);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(
            MockLlm::new(vec![end_call()]).with_compaction_summary("SUMMARY"),
        );
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // One forced-compaction `create_message` call during recovery.
        assert_eq!(mock.compaction_calls(), 1);
        // Transcript shrank: 40 seeded + user + assistant ≪ 42.
        assert!(history.len() < 42);
    }

    #[tokio::test]
    async fn capacity_over_budget_recovers_via_hard_trim() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 40);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        // Compaction fails → hard trim is the fallback.
        let mock = Arc::new(
            MockLlm::new(vec![end_call()]).with_compaction_error("mock compaction error"),
        );
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Compaction was attempted (1 call) but failed; hard trim saved the turn.
        assert_eq!(mock.compaction_calls(), 1);
        // Hard trim removed oldest messages until under budget (keeping ≥ 4).
        // The transcript shrank from 40 seeded + user text = 41 to well under
        // that, then the assistant turn added 1. Exact count depends on the
        // trim loop; the proof is the reduction + the turn succeeded.
        assert!(history.len() < 42, "history len = {}", history.len());
    }

    #[tokio::test]
    async fn capacity_micro_compact_clears_tool_results_in_recovery() {
        let mut sess = fresh_session();
        // A >32 KB `file_read` tool result pushes the transcript over budget.
        // CompactionProbe is None, so run_compaction doesn't micro-compact —
        // only the capacity recovery's best-effort micro-compact runs.
        seed_large_file_read(&mut sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let reason = executor
            .run(&mut history, "what did the file say".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Micro-compact cleared the tool result before forced compaction was
        // needed — no LLM call.
        assert_eq!(mock.compaction_calls(), 0);
        // The tool result is now the cleared placeholder.
        let placeholder = "[tool result cleared for context economy]";
        match &history.messages()[1].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, placeholder, "tool result was micro-compacted");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn capacity_recovery_fails_proceeds_with_request() {
        let mut sess = fresh_session();
        // Ten large text messages + user text = 11 messages, well over the
        // 3072 budget. Compaction is attempted (enough messages beyond the
        // keep-recent window for plan_compaction to produce summarize
        // indices) but the mock errors. Hard trim can't bring 4 × 10K-char
        // messages under 3072, so recovery fails — the request goes out
        // anyway (Proceed), and the mock returns a canned reply.
        let body = "x".repeat(10_000);
        for _ in 0..10 {
            sess.add_message(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: body.clone(),
                    cache_control: None,
                }],
            });
        }
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(
            MockLlm::new(vec![end_call()]).with_compaction_error("mock compaction error"),
        );
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        // Recovery failed but the budget wasn't exhausted (attempts = 0 <
        // MAX), so the request went out (Proceed) and the mock replied.
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 1);
    }

    // === reactive capacity recovery (seam 2) ===============================
    //
    // When the provider rejects a request with a context-length error before
    // the stream opens, the executor classifies it (`is_context_length_error_message`)
    // and triggers `recover_context_overflow`, then restarts the step so the
    // request snapshot picks up the compacted transcript (mirrors
    // `turn_loop.rs:620-633`). `MockRound::StreamOpenErr` makes
    // `create_message_stream` itself return `Err` (a pre-stream rejection),
    // distinct from `StreamErr` (mid-flight death → transparent retry).

    /// A context-length rejection recovers via emergency compaction and the
    /// turn proceeds (happy path). The transcript is seeded under the local
    /// budget estimate so the preflight (seam 1) does not fire — the rejection
    /// comes from the provider, exercising the reactive seam-2 path.
    #[tokio::test]
    async fn reactive_recovery_recovers_on_context_length_error() {
        let mut sess = fresh_session();
        // 10 text messages (~750 tokens) ≪ the 3072 Ollama/llama2 budget, so
        // the preflight proceeds; the provider "rejects" with a context-length
        // error (its real count exceeds its limit).
        seed_text_messages(&mut sess, 10);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let ctx_len_msg = "This model's maximum context length is 128000 tokens, \
                           however you requested 200000 tokens.";
        let mock = Arc::new(
            MockLlm::with_rounds(vec![
                MockRound::StreamOpenErr(ctx_len_msg.to_string()),
                MockRound::Events(end_call()),
            ])
            .with_compaction_summary("SUMMARY"),
        );
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run should succeed after reactive recovery");
        assert_eq!(reason, StopReason::NoToolCalls);
        // The reactive recovery ran one forced-compaction LLM call.
        assert_eq!(mock.compaction_calls(), 1);
        // create_message_stream was called twice: the rejected attempt, then
        // the post-recovery successful round.
        assert_eq!(mock.requests().len(), 2);
        // The transcript was compacted (10 seeded + user → a few messages).
        assert!(history.len() <= 6, "history len = {}", history.len());
    }

    /// A non-context-length pre-stream error (timeout) is not recovered — it
    /// hard-fails the turn immediately, with no compaction attempt.
    #[tokio::test]
    async fn reactive_recovery_non_context_length_error_hard_fails() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 10);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        // "Connection timed out" classifies as Timeout, not context-length.
        let mock = Arc::new(MockLlm::with_rounds(vec![
            MockRound::StreamOpenErr("Connection timed out".to_string()),
        ]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let err = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect_err("non-context-length error should hard-fail");
        assert!(
            err.to_string().contains("Connection timed out"),
            "err = {err}"
        );
        // No recovery attempted for a non-context-length error.
        assert_eq!(mock.compaction_calls(), 0);
    }

    /// A context-length rejection where emergency compaction fails (and the
    /// transcript can't be trimmed below the local estimate) hard-fails the
    /// turn — recovery failure is not a fall-through-to-Proceed here (unlike
    /// the preflight path), because the provider already rejected the request.
    #[tokio::test]
    async fn reactive_recovery_failed_hard_fails() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 10);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let ctx_len_msg = "This model's maximum context length is 128000 tokens.";
        // Compaction errors, and the transcript is already under the local
        // estimate so the hard-trim can't shrink it → recovery fails.
        let mock = Arc::new(
            MockLlm::with_rounds(vec![MockRound::StreamOpenErr(ctx_len_msg.to_string())])
                .with_compaction_error("mock compaction error"),
        );
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let err = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect_err("recovery failure should hard-fail");
        assert!(
            err.to_string().contains("maximum context length"),
            "err = {err}"
        );
        // Compaction was attempted (1 create_message call) but failed.
        assert_eq!(mock.compaction_calls(), 1);
    }

    /// Without a capacity probe, reactive recovery is disabled — a
    /// context-length rejection hard-fails (no in-band fallback).
    #[tokio::test]
    async fn reactive_recovery_without_capacity_probe_hard_fails() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 10);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let ctx_len_msg = "This model's maximum context length is 128000 tokens.";
        let mock = Arc::new(MockLlm::with_rounds(vec![
            MockRound::StreamOpenErr(ctx_len_msg.to_string()),
        ]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None, // no capacity probe ⇒ reactive recovery disabled
        );
        let err = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect_err("no probe ⇒ context-length error hard-fails");
        assert!(
            err.to_string().contains("maximum context length"),
            "err = {err}"
        );
        assert_eq!(mock.compaction_calls(), 0);
    }

    /// Reactive recovery surfaces status events on the host's `Event` channel
    /// (the "Emergency context compaction started/complete" guardrail
    /// surfacing), proving the host UI sees the recovery happen.
    #[tokio::test]
    async fn reactive_recovery_surfaces_status_events() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 10);
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let ctx_len_msg = "This model's maximum context length is 128000 tokens.";
        let mock = Arc::new(
            MockLlm::with_rounds(vec![
                MockRound::StreamOpenErr(ctx_len_msg.to_string()),
                MockRound::Events(end_call()),
            ])
            .with_compaction_summary("SUMMARY"),
        );
        let executor = HostAgentExecutor::new(
            mock.clone(),
            Arc::new(ToolSet::new()),
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            Some(capacity_probe(ApiProvider::Ollama, "llama2")),
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run should succeed after reactive recovery");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 1);
        // The Event channel received the compaction-started + complete
        // status messages.
        let events = drain(&mut rx);
        let recovery_surfaced = events.iter().any(|e| match e {
            Event::Status { message, .. } => message.contains("compaction"),
            _ => false,
        });
        assert!(
            recovery_surfaced,
            "expected a compaction status event, got: {events:?}"
        );
    }

    // === inline stream reduction (§E) =======================================
    //
    // The inline `reduce_stream` replaced `accumulate_stream` so the executor
    // emits streaming deltas to `Callback::on_stream_delta` in real time and
    // tracks `any_content_received` (closing the transparent-retry bail-on-error
    // gap). These tests prove: (1) text/thinking deltas flow to the callback,
    // (2) a stream that dies after content surfaces the partial turn (no retry),
    // (3) end-to-end delta flow through `CallbackBridge` → `Event::MessageDelta`.

    /// A `Callback` that records every `on_stream_delta` call, so tests can
    /// prove the executor emitted specific text/thinking deltas during streaming.
    struct DeltaRecorder {
        deltas: Arc<std::sync::Mutex<Vec<StreamDelta>>>,
    }

    impl DeltaRecorder {
        fn new() -> Self {
            Self {
                deltas: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
        fn deltas(&self) -> Vec<StreamDelta> {
            self.deltas.lock().unwrap().clone()
        }
    }

    impl Callback for DeltaRecorder {
        fn on_stream_delta<'a>(
            &'a self,
            delta: &'a StreamDelta,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            let deltas = self.deltas.clone();
            let delta = delta.clone();
            Box::pin(async move {
                deltas.lock().unwrap().push(delta);
            })
        }
    }

    /// Build a thinking-block stream-event sequence (mirrors `text_block`).
    fn thinking_block(idx: u32, body: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::Thinking {
                    thinking: String::new(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: idx,
                delta: Delta::ThinkingDelta {
                    thinking: body.to_string(),
                },
            },
            StreamEvent::ContentBlockStop { index: idx },
        ]
    }

    #[tokio::test]
    async fn stream_emits_text_deltas_to_callback() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let recorder = Arc::new(DeltaRecorder::new());
        let callback: Arc<dyn Callback> = recorder.clone();

        // Two text blocks in one stream: "hello " then "world".
        let mut call = text_block(0, "hello ");
        call.extend(text_block(1, "world"));
        call.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Two text deltas were emitted — one per ContentBlockDelta. Lifecycle
        // events (MessageStarted/MessageComplete) now interleave, so filter for
        // the content deltas rather than asserting positional indices.
        let deltas = recorder.deltas();
        let text_deltas: Vec<_> = deltas
            .iter()
            .filter_map(|d| match d {
                StreamDelta::Text { index, content } => Some((*index, content.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(text_deltas.len(), 2, "two text deltas: {deltas:?}");
        assert_eq!(text_deltas[0], (0, "hello ".to_string()));
        assert_eq!(text_deltas[1], (1, "world".to_string()));
    }

    #[tokio::test]
    async fn stream_emits_thinking_deltas_to_callback() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let recorder = Arc::new(DeltaRecorder::new());
        let callback: Arc<dyn Callback> = recorder.clone();

        // A thinking block followed by a text block.
        let mut call = thinking_block(0, "pondering");
        call.extend(text_block(1, "answer"));
        call.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The thinking delta was emitted (the text delta too). Filter for the
        // thinking content delta (lifecycle events interleave).
        let deltas = recorder.deltas();
        let thinking_deltas: Vec<_> = deltas
            .iter()
            .filter_map(|d| match d {
                StreamDelta::Thinking { index, content } => Some((*index, content.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(thinking_deltas.len(), 1, "one thinking delta: {deltas:?}");
        assert_eq!(thinking_deltas[0], (0, "pondering".to_string()));
    }

    #[tokio::test]
    async fn stream_partial_content_surfaces_without_retry() {
        // The bail-on-error gap closure: a stream that produces text content
        // then dies with an Err returns StreamReduceOutcome::Partial — the
        // partial text is surfaced (not retried). Before the inline reducer,
        // accumulate_stream dropped partial blocks and the executor retried.
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Stream emits a text block ("partial answer") then dies with an Err.
        let mut partial = text_block(0, "partial answer");
        // Don't add finish — the stream dies before MessageStop.
        let mock = Arc::new(MockLlm::with_rounds(vec![MockRound::EventsThenErr(
            partial,
            "connection reset".into(),
        )]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run should surface partial content");
        // Partial text has no tool calls → NoToolCalls.
        assert_eq!(reason, StopReason::NoToolCalls);

        // The request was issued only ONCE — no retry (content was received).
        assert_eq!(mock.requests().len(), 1, "partial content must not retry");

        // The partial text was committed to the transcript.
        assert_eq!(history.len(), 2, "[user, assistant(partial)]");
        match &history.messages()[1].content[0] {
            ContentBlock::Text { text, .. } => {
                assert_eq!(text, "partial answer", "partial text surfaced");
            }
            other => panic!("expected Text block, got {other:?}"),
        }

        // A status surfaced the partial-content interruption.
        let msgs = statuses(&drain(&mut rx));
        assert!(
            msgs.iter()
                .any(|m| m.contains("partial content") || m.contains("Stream interrupted")),
            "expected a partial-content status, got: {msgs:?}"
        );
    }

    #[tokio::test]
    async fn stream_deltas_flow_through_callback_bridge() {
        // End-to-end: executor → CallbackBridge → Event::MessageDelta /
        // Event::ThinkingDelta on the host's Event channel.
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let bridge = Arc::new(CallbackBridge::new(Some(tx), None, HookContext::new()));
        let callback: Arc<dyn Callback> = bridge;

        // A thinking block + a text block + finish.
        let mut call = thinking_block(0, "reasoning");
        call.extend(text_block(1, "output"));
        call.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The Event channel received a ThinkingDelta then a MessageDelta.
        let events = drain(&mut rx);
        let thinking = events.iter().find_map(|e| match e {
            Event::ThinkingDelta { index, content } => Some((*index, content.clone())),
            _ => None,
        });
        let text = events.iter().find_map(|e| match e {
            Event::MessageDelta { index, content } => Some((*index, content.clone())),
            _ => None,
        });
        let (t_idx, t_content) =
            thinking.expect("Event::ThinkingDelta should have been emitted");
        let (x_idx, x_content) =
            text.expect("Event::MessageDelta should have been emitted");
        assert_eq!(t_idx, 0);
        assert_eq!(t_content, "reasoning");
        assert_eq!(x_idx, 1);
        assert_eq!(x_content, "output");
    }

    // (4) Block-lifecycle events — the inline reducer synthesizes
    // MessageStarted/ThinkingStarted at ContentBlockStart and
    // ThinkingComplete/MessageComplete at ContentBlockStop, interleaved with
    // the content deltas. This is the §E block-lifecycle slice.

    /// Render a `StreamDelta` sequence as readable tags so a lifecycle-ordering
    /// assertion reads as a list of block-boundary + content events.
    fn delta_tags(deltas: &[StreamDelta]) -> Vec<String> {
        deltas
            .iter()
            .map(|d| match d {
                StreamDelta::Text { index, content } => {
                    format!("Text({index}, {content:?})")
                }
                StreamDelta::Thinking { index, content } => {
                    format!("Thinking({index}, {content:?})")
                }
                StreamDelta::MessageStarted { index } => {
                    format!("MessageStarted({index})")
                }
                StreamDelta::ThinkingStarted { index } => {
                    format!("ThinkingStarted({index})")
                }
                StreamDelta::ThinkingComplete { index } => {
                    format!("ThinkingComplete({index})")
                }
                StreamDelta::MessageComplete { index } => {
                    format!("MessageComplete({index})")
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn stream_emits_block_lifecycle_events() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let recorder = Arc::new(DeltaRecorder::new());
        let callback: Arc<dyn Callback> = recorder.clone();

        // A thinking block(0) followed by a text block(1) + finish.
        let mut call = thinking_block(0, "pondering");
        call.extend(text_block(1, "answer"));
        call.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The full interleaved sequence: lifecycle markers frame each block
        // around its content delta. ThinkingStarted fires before the thinking
        // delta; ThinkingComplete fires at ContentBlockStop(0); then the text
        // block's Started/Complete bracket its delta.
        let tags = delta_tags(&recorder.deltas());
        assert_eq!(
            tags,
            vec![
                "ThinkingStarted(0)".to_string(),
                "Thinking(0, \"pondering\")".to_string(),
                "ThinkingComplete(0)".to_string(),
                "MessageStarted(1)".to_string(),
                "Text(1, \"answer\")".to_string(),
                "MessageComplete(1)".to_string(),
            ],
            "lifecycle + content sequence: {tags:?}"
        );
    }

    #[tokio::test]
    async fn stream_lifecycle_events_flow_through_callback_bridge() {
        // End-to-end: executor → CallbackBridge → Event::ThinkingStarted /
        // ThinkingComplete / MessageStarted / MessageComplete on the host's
        // Event channel (mirrors `stream_deltas_flow_through_callback_bridge`).
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let bridge = Arc::new(CallbackBridge::new(Some(tx), None, HookContext::new()));
        let callback: Arc<dyn Callback> = bridge;

        let mut call = thinking_block(0, "reasoning");
        call.extend(text_block(1, "output"));
        call.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call]));

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The four lifecycle Events arrived on the channel, with the right
        // block indices.
        let events = drain(&mut rx);
        let started = events.iter().find_map(|e| match e {
            Event::MessageStarted { index } => Some(*index),
            _ => None,
        });
        let complete = events.iter().find_map(|e| match e {
            Event::MessageComplete { index } => Some(*index),
            _ => None,
        });
        let t_started = events.iter().find_map(|e| match e {
            Event::ThinkingStarted { index } => Some(*index),
            _ => None,
        });
        let t_complete = events.iter().find_map(|e| match e {
            Event::ThinkingComplete { index } => Some(*index),
            _ => None,
        });
        assert_eq!(
            t_started.expect("Event::ThinkingStarted"),
            0,
            "thinking block started: {events:?}"
        );
        assert_eq!(
            t_complete.expect("Event::ThinkingComplete"),
            0,
            "thinking block completed: {events:?}"
        );
        assert_eq!(
            started.expect("Event::MessageStarted"),
            1,
            "text block started: {events:?}"
        );
        assert_eq!(
            complete.expect("Event::MessageComplete"),
            1,
            "text block completed: {events:?}"
        );

        // Ordering: ThinkingComplete(0) precedes MessageStarted(1) — a block
        // completes before the next one starts.
        let t_complete_pos = events
            .iter()
            .position(|e| matches!(e, Event::ThinkingComplete { index: 0 }));
        let msg_started_pos = events
            .iter()
            .position(|e| matches!(e, Event::MessageStarted { index: 1 }));
        match (t_complete_pos, msg_started_pos) {
            (Some(a), Some(b)) => assert!(
                a < b,
                "ThinkingComplete(0) at {a} should precede MessageStarted(1) at {b}: {events:?}"
            ),
            _ => panic!("lifecycle events missing: {events:?}"),
        }
    }
}
