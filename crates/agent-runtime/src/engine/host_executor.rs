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
//! [`HostAgentExecutor`] runs the LLM↔tool loop (reusing
//! [`accumulate_stream`](codesmith_agent::executor::accumulate_stream) for stream
//! reduction) and absorbs the production guardrails slice by slice. Six are in:
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
//!    committed (`accumulate_stream` returns `Err`), the executor silently re-issues
//!    the SAME request up to `MAX_STREAM_RETRIES` (3) times before propagating the
//!    failure, mirroring `handle_deepseek_turn`'s outer "stream died with nothing"
//!    retry (`turn_loop.rs:1152-1190`). A healthy round resets the budget. The
//!    retry counter is a local `u32` that persists across steps within one `run`
//!    (matching loop-guard's local-state pattern); the retry is transparent to the
//!    [`Callback`] (`on_llm_start` / `on_llm_end` fire once per step, a `Status`
//!    event is the only retry surfacing). See "Known tradeoffs" below for the
//!    `accumulate_stream` bail-on-error gap and the deferred cancel-token
//!    short-circuit.
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
//!
//! Guardrail status (loop-guard warn/halt, transparent-retry "retrying n/3",
//! steer "Steer input accepted", compaction "Compaction completed/failed")
//! surfaces over the host's `Event` channel
//! (`event_tx`) — **not** via the framework `Callback`: guardrails are
//! host-side concerns and the `Callback` trait stays untouched per ROADMAP §E.
//!
//! It is **not yet wired into `handle_send_message`**; the production
//! `handle_deepseek_turn` remains the live path — the value of landing it now is
//! the composition proof (the three bridges light up end-to-end inside a real
//! `AgentExecutor::run` driving a real `ToolSpec` over a real `Session`; see the
//! headline test) plus six guardrails absorbed at the seams below.
//!
//! ## Guardrail insertion points
//!
//! The loop has four seams where guardrails are absorbed incrementally:
//!
//! 1. **per-step pre-request** — ✅ **steer drain** (queued user inputs injected
//!    before the request snapshot) + ✅ **compaction** (micro-compact stale
//!    tool results, then auto-compact via an LLM summary when over threshold)
//!    + ✅ **LSP flush** (drain pending diagnostics into a synthetic `user`
//!    message); capacity pre-request, system-prompt refresh still to come
//!    (top of the `loop`).
//! 2. **per-step post-stream** — ✅ **transparent-retry** (re-issue the request
//!    when the stream dies mid-flight before any content commits, up to 3 times);
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
//!    `StopReason::Error`); capacity post-tool still to come (after the tool loop).
//!
//! Streaming deltas (`MessageDelta` / `ThinkingDelta`) will continue to flow
//! over the `Event` channel directly, emitted by an inline stream reducer (a
//! later slice replaces the `accumulate_stream` call) — they have no `Callback`
//! method and stay off the `Callback` path (see `callback_bridge` docs).
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
//! ## Known tradeoffs in transparent-retry (by design)
//!
//! - **`accumulate_stream` bail-on-error** — the shared core reducer returns `Err`
//!   on the first erroring stream item and drops any partially-accumulated blocks,
//!   so an `Err` always means "no actionable content committed". This makes the
//!   retry fire even when production would ship partial content (it tracks
//!   `any_content_received` inline and skips the retry once the user has seen
//!   output). Since the partial content is lost here, retrying is the only
//!   recovery path; the double-billing concern (the provider billed for the
//!   partial output) is provider-side, not user-visible. Inline stream reduction
//!   (a later §E slice that replaces the `accumulate_stream` call) closes this
//!   gap.
//! - **pre-stream connection errors not retried** — `create_message_stream`
//!   returning `Err` (connection refused / auth / context-length) propagates as a
//!   hard fail here. Production treats those as context-recovery or hard-fail (a
//!   separate guardrail, deferred); only mid-flight stream errors retry.
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
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use codesmith_agent::callback::{Callback, StopReason};
use codesmith_agent::executor::{accumulate_stream, AgentExecutor, AgentExecutorConfig};
use codesmith_agent::llm_client::LlmClientHandle;
use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message, MessageRequest};
use codesmith_agent::tools::{Tool, ToolCapability, ToolError, ToolResult, ToolSet};

use super::approval::ApprovalDecision;
use super::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
use super::lsp_hooks::edit_file_paths;
use super::summarize_text;
use crate::compaction::circuit_breaker::CompactionCircuitBreaker;
use crate::compaction::micro_compact::{
    micro_compact_messages, should_trigger_micro_compact, MicroCompactState,
};
use crate::compaction::{compact_messages_safe, should_compact, CompactionConfig};
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
}

impl HostAgentExecutor {
/// Construct from the four collaborators + config + an optional guardrail
/// status channel (`None` for embeds that don't surface guardrail status) +
/// an optional [`LspProbe`] (`None` ⇒ LSP collect/flush disabled) + an
/// optional steer input receiver (`None` ⇒ steer drain disabled) + an
/// optional approval-decision receiver (`None` ⇒ approval gating disabled) +
/// an optional [`CompactionProbe`] (`None` ⇒ compaction disabled).
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

    /// (2) per-step post-stream seam — transparent stream retry.
    ///
    /// Drives `create_message_stream` + `accumulate_stream` with a bounded
    /// transparent-retry loop. When the stream dies mid-flight before any
    /// content is committed — `accumulate_stream` returns `Err` (it drops any
    /// partial blocks on the first erroring item, so an `Err` means "no
    /// actionable content committed") — re-issue the SAME request up to
    /// [`MAX_STREAM_RETRIES`] (3) times before propagating the failure. This
    /// mirrors `handle_deepseek_turn`'s outer "stream died with nothing"
    /// retry (`turn_loop.rs:1152-1190`). A healthy round resets the budget
    /// (`turn_loop.rs:1186`), so a bad prior step doesn't carry over.
    ///
    /// Pre-stream connection errors (`create_message_stream` returning `Err`)
    /// are **not** retried here — production treats those as hard-fail /
    /// context-recovery (a separate guardrail, deferred). Only mid-flight
    /// stream errors retry. The retry is transparent to the [`Callback`]:
    /// `on_llm_start` / `on_llm_end` fire once per step, and a `Status` event
    /// is the only retry surfacing (matching production's silent re-issue).
    ///
    /// # Known tradeoff vs production
    ///
    /// `accumulate_stream` bails on the first erroring stream item and drops
    /// accumulated blocks, so this retries even when production would ship
    /// partial content (it tracks `any_content_received` inline and skips the
    /// retry when the user has already seen output). Inline stream reduction
    /// (a later §E slice that replaces the `accumulate_stream` call) closes
    /// that gap; until then retrying is the only recovery path since the
    /// partial content is lost. The cancel-token short-circuit (production's
    /// `should_transparently_retry_stream` checks `!cancelled`) is deferred
    /// to the wire-in slice — the bounded budget can't loop forever.
    async fn stream_with_transparent_retry(
        &self,
        client: &LlmClientHandle,
        request: MessageRequest,
        stream_retry_attempts: &mut u32,
    ) -> Result<(Vec<ContentBlock>, Option<String>)> {
        /// Cap on transparent stream retries — matches `turn_loop`'s
        /// `MAX_STREAM_RETRIES` (3). One initial attempt + 3 retries = 4 total
        /// `create_message_stream` calls before the failure surfaces.
        const MAX_STREAM_RETRIES: u32 = 3;
        loop {
            // Pre-stream connection errors are hard-fails (context recovery is
            // a separate guardrail). Only mid-flight stream errors retry.
            let stream = client.create_message_stream(request.clone()).await?;
            match accumulate_stream(stream).await {
                Ok(outcome) => {
                    // Healthy round → reset the retry budget so a bad prior
                    // step doesn't carry over (mirrors `turn_loop.rs:1186`).
                    *stream_retry_attempts = 0;
                    return Ok(outcome);
                }
                Err(e) => {
                    // Stream died mid-flight. `accumulate_stream` drops partial
                    // blocks on error, so no actionable content was committed.
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
                    return Err(e);
                }
            }
        }
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
        loop {
            // (1) per-step pre-request seam — ✅ steer drain (queued user
            // inputs injected before the request snapshot); ✅ compaction
            // (micro-compact + LLM-summary auto-compact, runs after steer and
            // before the LSP flush so a fresh diagnostic message survives
            // compaction); ✅ LSP flush (drain pending diagnostics into a
            // synthetic user message); capacity / cycle land here later.
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
            // when the stream dies mid-flight before any content commits).
            // `on_llm_start` fires once per step; retries are transparent to
            // the Callback. Subagent handoff / thinking-only handling land here
            // later.
            let (content, _stop_reason) = self
                .stream_with_transparent_retry(&client, request, &mut stream_retry_attempts)
                .await?;
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
            // capacity / LSP post-edit land here later.
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
    /// - `StreamErr` yields a single `Err` item, so `accumulate_stream` returns
    ///   `Err` — simulating a mid-flight stream death (the #103 "stream died
    ///   with nothing" case) for the transparent-retry tests.
    enum MockRound {
        Events(Vec<StreamEvent>),
        StreamErr(String),
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
}
