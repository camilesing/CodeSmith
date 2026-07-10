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
//! reduction) and absorbs the production guardrails slice by slice. Four are in:
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
//!
//! Guardrail status (loop-guard warn/halt, transparent-retry "retrying n/3",
//! steer "Steer input accepted") surfaces over the host's `Event` channel
//! (`event_tx`) — **not** via the framework `Callback`: guardrails are
//! host-side concerns and the `Callback` trait stays untouched per ROADMAP §E.
//!
//! It is **not yet wired into `handle_send_message`**; the production
//! `handle_deepseek_turn` remains the live path — the value of landing it now is
//! the composition proof (the three bridges light up end-to-end inside a real
//! `AgentExecutor::run` driving a real `ToolSpec` over a real `Session`; see the
//! headline test) plus four guardrails absorbed at the seams below.
//!
//! ## Guardrail insertion points
//!
//! The loop has four seams where guardrails are absorbed incrementally:
//!
//! 1. **per-step pre-request** — ✅ **steer drain** (queued user inputs injected
//!    before the request snapshot) + ✅ **LSP flush** (drain pending diagnostics
//!    into a synthetic `user` message); compaction, capacity pre-request,
//!    system-prompt refresh still to come (top of the `loop`).
//! 2. **per-step post-stream** — ✅ **transparent-retry** (re-issue the request
//!    when the stream dies mid-flight before any content commits, up to 3 times);
//!    subagent handoff, thinking-only handling still to come (after the stream
//!    resolves, before tool extraction).
//! 3. **per-tool** — ✅ **loop-guard `record_attempt`** (block the 3rd identical
//!    call) + **`record_outcome`** (warn at 3 / halt at 8 consecutive failures) +
//!    **LSP post-edit collect** (probe diagnostics after a successful edit);
//!    approval, early-tool-start, parallel dispatch still to come (inside the
//!    tool `for` loop).
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
use codesmith_agent::tools::{ToolError, ToolResult, ToolSet};

use super::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
use super::lsp_hooks::edit_file_paths;
use super::summarize_text;
use crate::events::Event;
use crate::host_services::LspManagerApi;
use crate::lsp_diagnostics::{render_blocks as render_lsp_blocks, DiagnosticBlock};

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
}

impl HostAgentExecutor {
/// Construct from the four collaborators + config + an optional guardrail
/// status channel (`None` for embeds that don't surface guardrail status) +
/// an optional [`LspProbe`] (`None` ⇒ LSP collect/flush disabled) + an
/// optional steer input receiver (`None` ⇒ steer drain disabled).
#[must_use]
pub fn new(
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
    event_tx: Option<mpsc::Sender<Event>>,
    lsp: Option<LspProbe>,
    steer: Option<Arc<std::sync::Mutex<mpsc::Receiver<String>>>>,
) -> Self {
        Self {
            client,
            tools,
            callback,
            config,
            event_tx,
            lsp,
            steer,
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
            // inputs injected before the request snapshot); ✅ LSP flush (drain
            // pending diagnostics into a synthetic user message); compaction /
            // capacity / cycle land here later.
            if step >= max_steps {
                callback.on_complete(&StopReason::MaxSteps).await;
                return Ok(StopReason::MaxSteps);
            }
            // Steer drain sits at the very top of the loop (mirrors
            // `turn_loop.rs:300`) — before the LSP flush and the request
            // snapshot, so steered text reaches the model on this step's
            // request. Drains only what's already queued (`try_recv` is
            // non-blocking); never waits for input.
            self.drain_steers(history).await;
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
            // (3) per-tool seam — loop-guard (absorbed); approval /
            // early-tool-start / parallel land here later. `loop_guard_halt` is
            // per-step: a halt short-circuits the tool loop and the whole turn
            // at the (4) seam below.
            let mut loop_guard_halt: Option<String> = None;
            for (id, name, input) in tool_uses {
                callback.on_tool_start(&name, &input).await;
                // loop-guard: block the 3rd identical (name+args) call this turn.
                let (result, blocked) = match loop_guard.record_attempt(&name, &input) {
                    AttemptDecision::Block(message) => {
                        (Ok(block_tool_result(message)), true)
                    }
                    AttemptDecision::Proceed => (
                        match tools.get(&name) {
                            Some(tool) => tool.run(input.clone()).await,
                            None => Err(ToolError::NotAvailable {
                                message: format!("no tool named '{name}'"),
                            }),
                        },
                        false,
                    ),
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
    use codesmith_agent::models::{ContentBlockStart, Delta, MessageDelta, StreamEvent};
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
            }
        }

        /// The `messages` snapshot of each `create_message_stream` call, in call
        /// order.
        fn requests(&self) -> Vec<Vec<Message>> {
            self.requests.lock().unwrap().clone()
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
        ) -> Pin<Box<dyn Future<Output = Result<codesmith_agent::models::MessageResponse>> + Send + '_>>
        {
            Box::pin(async { anyhow::bail!("mock does not implement create_message") })
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
}
