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
//! the production guardrails slice by slice. Ten are in:
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
//!    surfacing). See "Known gaps" below for reactive capacity recovery.
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
//!    three secondary drain sites are all absorbed ✅: the **mid-stream buffer**
//!    ([`reduce_stream`](HostAgentExecutor::reduce_stream) `try_recv`s into a
//!    per-step `pending_steers` vec after each stream event, flushed post-stream
//!    / post-tool via [`flush_pending_steers`](HostAgentExecutor::flush_pending_steers)),
//!    the **post-stream resume** (the no-tool-calls arm flushes + resumes), and
//!    the **blocking `recv` during sub-agent hold** (the hold's own `biased
//!    select!` arms).
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
//!    "Known gaps in approval" below for the sandbox-elevation and
//!    static-derivation gaps.
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
//!    post-compact cleanup (25c) and enhancements deferrals (summary-prompt
//!    merge absorbed ✅ in slice 25a §E; attachment reinject absorbed ✅ in
//!    slice 25b §E).
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
//! 8. **early-tool-start** ([`early_start_safe`] + [`EarlyToolTask`]) — the
//!    **second seam-2 guardrail**. When a tool block reaches
//!    `ContentBlockStop` mid-stream, [`reduce_stream`] finalizes its input and
//!    — if [`early_start_safe`] passes (read-only + no approval + no
//!    code-exec / file-write) — `tokio::spawn`s the tool immediately so its
//!    result is ready by the time the executor reaches the tool loop, mirroring
//!    `handle_deepseek_turn`'s `early_tool_start_safe` + `early_tool_tasks`
//!    map (`turn_loop.rs:975-1135` spawn, `1598-1803` reuse). The tool loop
//!    pops the task by wire `id`, re-verifies name + input (the model could in
//!    principle revise args after the block closed), and awaits the
//!    `JoinHandle` to reuse the result instead of re-running the tool; an
//!    args mismatch, a loop-guard block, a denied approval, or a
//!    `NotAvailable` pops + `Drop`-aborts the orphaned task. The map is a
//!    per-step local `HashMap` (not an executor field — unlike LSP / steer /
//!    approval / compaction / capacity, this guardrail has no cross-step state),
//!    so the constructor signature is unchanged; on a `continue` (capacity
//!    `RetryStep` / reactive `RecoveredContextOverflow`) the stream either
//!    never opened or died before any content ⇒ no `ContentBlockStop` ⇒ the
//!    map is empty. [`EarlyToolTask::Drop`] aborts the `JoinHandle` so an
//!    unreused task never leaks a background task (aborting a completed task is
//!    a no-op, so the reuse path's await-then-drop is safe). See "Known gaps in
//!    early-tool-start" below for the ToolCallStarted bridge dedup, the
//!    spawn-time loop-guard, and the per-input approval.
//! 9. **subagent post-stream drain** (`subagent` field) — the **third seam-2
//!    guardrail**. When the model finishes a step with no tool calls, any child
//!    sub-agent completions that arrived (queued during inference or between
//!    turns) are drained via `try_recv` and injected as
//!    `<codesmith:runtime_event kind="subagent_completion">` user messages
//!    (the sentinel contract in `prompts/base.md`), then the turn resumes —
//!    mirroring `handle_deepseek_turn`'s non-blocking completion drain
//!    (`turn_loop.rs:1317-1397` + the late drain at `1501-1532`). The executor
//!    has no thinking-only / goal-continuation / REPL branches, so this single
//!    drain site (the `tool_uses.is_empty()` arm) covers both production drains.
//!    The receiver is
//!    `Option<Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<SubAgentCompletion>>>>` —
//!    `tokio::sync::Mutex` (not `std::sync::Mutex`) because the blocking hold's
//!    `biased select!` calls `recv().await` across the guard (same rationale as
//!    the steer receiver's migration), and it persists across `run` invocations
//!    (matching `Engine.rx_subagent_completion`). The **blocking hold** for
//!    still-running children (`should_hold_turn_for_subagents` + a `biased
//!    select!` over cancel / completion `recv().await` / steer `recv().await`)
//!    is **absorbed ✅** — it uses [`SubAgentApi::running_count`] (the
//!    [`Self::subagent_api`] field) and the `tokio::sync::Mutex` receiver; the
//!    hold's own `select!` cancel arm is **Checkpoint E** (the executor's
//!    `CancellationToken` is absorbed — see guardrail 10 — and the hold's cancel
//!    race is wired there). The steer arm of the same `select!` closes the steer
//!    post-stream resume gap (see below). `ContextPatch` apply
//!    (tighten-only `auto_approve`/`trust_mode`) is still deferred — it mutates
//!    `Session` state not reachable through `ChatHistory`, and production
//!    hardcodes `context_patch: None` today. See "Known gaps in subagent" below.
//! 10. **cancel-token** (`cancel_token` field + [`is_cancelled`]) — the
//!     **first cross-cutting guardrail**. An optional
//!     [`CancellationToken`](tokio_util::sync::CancellationToken) (`None` ⇒ all
//!     cancel checks are no-ops) mirrors production's seven turn-cancellation
//!     checkpoints. When set, a cancelled turn surfaces
//!     [`StopReason::Interrupted`](codesmith_agent::callback::StopReason::Interrupted)
//!     (distinct from `Error`) so the host can show "cancelled" rather than
//!     "failed". **Checkpoint A** (loop-top, before `max_steps`) bounds all
//!     `continue` loops (capacity `RetryStep`, reactive `RecoveredContextOverflow`,
//!     subagent resume); **Checkpoint B** (stream-open race) races the token
//!     against `create_message_stream` in a `biased select!` so a cancelled turn
//!     aborts before the stream opens; **Checkpoint C** (`Empty` arm) aborts a
//!     transparent retry; **Checkpoint D** (post-stream `Complete`/`Partial`)
//!     discards already-produced content; **Checkpoint G** (post-tool-loop, before
//!     `loop_guard_halt`) lets a tool-triggered cancel take priority over a
//!     loop-guard halt; the **approval cancel race** breaks out of the blocking
//!     `recv().await` via `select!` (the tool records an error result, then
//!     Checkpoint G catches the cancel); and the **steer stale-drain**
//!     ([`drain_stale_steers`](HostAgentExecutor::drain_stale_steers)) is a
//!     `pub` host-side method — the host calls it before `run` (mirrors
//!     `handle_send_message`'s `while rx_steer.try_recv().is_ok() {}`), not
//!     inside the turn loop. The subagent **blocking hold** cancel race
//!     (Checkpoint E) is **absorbed ✅** — it lives in the hold's own `biased
//!     select!` cancel arm. See "Known gaps in subagent" below for per-guardrail
//!     cancel status.
//!
//! Guardrail status (loop-guard warn/halt, transparent-retry "retrying n/3",
//! steer "Steer input accepted", compaction "Compaction completed/failed",
//! capacity "Emergency context compaction …", subagent "Resuming turn with N
//! sub-agent completion(s)") surfaces over the host's `Event` channel
//! (`event_tx`) — **not** via the framework `Callback`: guardrails are
//! host-side concerns and the `Callback` trait stays untouched per ROADMAP §E.
//!
//! It is **not yet wired into `handle_send_message`**; the production
//! `handle_deepseek_turn` remains the live path — the value of landing it now is
//! the composition proof (the three bridges light up end-to-end inside a real
//! `AgentExecutor::run` driving a real `ToolSpec` over a real `Session`; see the
//! headline test) plus eight guardrails absorbed at the seams below.
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
//!    request when the stream dies before any content commits, up to 3 times) +
//!    ✅ **early-tool-start** (at `ContentBlockStop` for a tool block, finalize
//!    the input and `tokio::spawn` a read-only tool so its result is ready by
//!    the tool loop) + ✅ **subagent post-stream drain** (when the model returns
//!    no tool calls, `try_recv`-drain queued child completions, inject each as a
//!    sentinel `user` message, and resume the turn) + ✅ **subagent blocking
//!    hold** (when the non-blocking drain found nothing but children are still
//!    running, block on a `biased select!` over cancel / completion `recv().await`
//!    / steer `recv().await`, emitting "Waiting on N sub-agent(s)"); thinking-only
//!    handling still to come (after the stream resolves, before tool extraction /
//!    turn end).
//! 3. **per-tool** — ✅ **loop-guard `record_attempt`** (block the 3rd identical
//!    call) + **`record_outcome`** (warn at 3 / halt at 8 consecutive failures) +
//!    ✅ **approval** (emit `ApprovalRequired` + block on the decision channel
//!    for write/code tools; denied ⇒ `permission_denied` error, tool skipped) +
//!    ✅ **early-tool-start reuse** (pop a speculatively-started task by id;
//!    reuse if name+input match, else `Drop`-abort + run fresh) +
//!    **LSP post-edit collect** (probe diagnostics after a successful edit);
//!    parallel dispatch still to come (inside the tool `for` loop).
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
//! `MessageComplete`) **are** synthesized at `ContentBlockStart` /
//! `ContentBlockStop` for text/thinking blocks (§E block-lifecycle slice),
//! letting the host's UI frame a block before its first delta and mark it done
//! when its last delta lands. Tool-call-start events (`StreamDelta::ToolCallStarted`,
//! fired on `ContentBlockStop` for tool blocks) announce a tool call as soon as
//! its input is finalized — the `Callback::on_tool_start` trait carries the
//! wire id, so the `CallbackBridge` uses the real wire id (no `bridge-{n}`
//! synthesis) and deduplicates the stream-time emission against the
//! execute-time `on_tool_start`.
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
//! - **`<turn_meta>` enrichment closed** ✅ — when a [`TurnMetaProbe`] is wired
//!   in (production), the synthetic flush message is wrapped via
//!   `enrich_user_text_message` (date / model / working set / skills, read
//!   from the `Arc`-shared `WorkingSet`), matching production's
//!   `user_text_message_with_turn_metadata`. Embeds/tests (`probe` absent) push
//!   plain text — the pre-slice-22 behavior. No `observe_user_message` for
//!   diagnostics (no user-intent path tokens).
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
//! - **cancel-token short-circuit** ✅ — production's
//!   `should_transparently_retry_stream` checks `!cancelled` to abort a retry
//!   loop on a cancelled turn. This executor now mirrors it: Checkpoint C (the
//!   `Empty` arm) checks `is_cancelled()` and returns
//!   `StreamRoundOutcome::Interrupted` (no retry); Checkpoint B (stream-open
//!   race) and Checkpoint D (post-stream `Complete`/`Partial`) are also wired.
//!   See guardrail 10 (cancel-token) for the full checkpoint map.
//!
//! ## Known gaps in approval (by design)
//!
//! - **cancel-token race** ✅ — production's `await_tool_approval` selects over
//!   `cancel_token.cancelled()` so a cancelled turn breaks out of the approval
//!   wait. This executor now mirrors it: `request_approval`'s `recv().await`
//!   loop is wrapped in a `biased select!` over the cancel token — cancel wins
//!   ⇒ `Err("Request cancelled while awaiting approval")` (fed back as a tool
//!   error; Checkpoint G then surfaces `StopReason::Interrupted`). See
//!   guardrail 10 (cancel-token).
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
//! - **summary-prompt merge absorbed ✅ (post-`run`, slice 25a §E)** —
//!   [`compact_messages_safe`] computes a `CompactionResult` carrying
//!   `summary_prompt: Option<SystemPrompt>` (the rolled-up summary meant to be
//!   merged into the system prompt). Production feeds it through
//!   `merge_compaction_summary`, which folds it into `session.system_prompt`.
//!   The framework [`ChatHistory`] exposes no system-prompt setter (the
//!   executor's system prompt is the static `config.system`), so the summary is
//!   recorded into the executor's
//!   [`pending_compaction_summary`](HostAgentExecutor::take_pending_compaction_summary)
//!   slot (accumulated across a turn's compactions via
//!   [`crate::compaction::merge_system_prompts`]) and the host folds it into
//!   `session.system_prompt` after `run` returns — behavior-equivalent to
//!   merging mid-`run` since the executor's system prompt is a static snapshot
//!   (the merged summary only matters for the *next* turn's re-snapshot). The
//!   LLM also still sees the summary in the rolled-up transcript body.
//! - **attachment reinject absorbed ✅ (during `run`, slice 25b §E)** —
//!   production's `reinject_compaction_attachments` re-inserts plan / todos /
//!   subagents / read-file snapshots that were compacted out, so the model
//!   keeps the working set. Those attachments are host-coupled
//!   (`session.plans` / `session.todos` / sub-agent state); the framework
//!   [`ChatHistory`] carries none of it, so the executor reaches them through a
//!   [`ReinjectProbe`] (`plan_state` / `todos` / `recent_read_files` `Arc`
//!   clones, the last mirroring slice 22's `working_set` Arc-ification) plus
//!   its own `subagent_api` (`live_running_snapshots`) and `turn_meta`
//!   (the `<turn_meta>` enrich block). It fires right after the transcript
//!   replace in both full-compact `Ok(result)` arms of [`run_compaction`] /
//!   [`recover_context_overflow`] — `None` budget for auto-compact (dedup +
//!   push only), `Some(target_budget)` for the hard-ceiling recovery (dedup +
//!   budget trial + push). The micro-compact arms are untouched (they clear
//!   tool-result content in place, not attachment messages). See
//!   [`HostAgentExecutor::reinject_compaction_attachments`].
//!   - **read-file observe site absorbed ✅** — `recent_read_files` is now
//!     populated *during* `run` by the (3) per-tool seam: when a `read_file`
//!     tool succeeds, the executor feeds the compacted/sanitized output
//!     (`compact_tool_result_for_context` — strips hidden Unicode attacks via
//!     `partially_sanitize_unicode`, HackerOne #3086545) into
//!     [`ReinjectProbe::record_read_file_result`], which dedup-by-path + push +
//!     trim on the shared `Arc<VecDeque<RecentReadFile>>`. Mirrors the retired
//!     `turn_loop.rs:2523-2525`. No-op when `reinject` is `None` (no probe ⇒ no
//!     `Arc` to write ⇒ the data would never be read by a reinject). The
//!     `run_compaction` reinject's provider-budget (currently `None`; passing
//!     `context_input_budget_for_provider` needs threading `api_provider` /
//!     `model`) remains a refinement.
//! - **post-compact cleanup absorbed ✅ (post-`run`, slice 25c §E)** — production's
//!   `post_compact_cleanup` force-rebuilds the working set and resets per-file
//!   cycle state (`micro_compact_state` / `circuit_breaker` /
//!   `last_system_prompt_hash`) after a compaction (the transcript the working
//!   set was derived from is now stale). The three plain `Session` fields are
//!   only reachable through `&mut Session`, which the executor can't hold
//!   during `run`; the reset only matters for the next turn anyway (like the
//!   25a summary merge), so the executor records a [`signal_post_compact_cleanup`]
//!   slot and the host runs the existing `post_compact_cleanup(&mut
//!   self.session)` free fn post-`run` (single source — no mid-run `CleanupProbe`
//!   or Arc-ification of the plain fields needed; the probe's
//!   `circuit_breaker` / `micro_state` are fresh-each-turn anyway since the
//!   executor is constructed per-turn, so resetting the session's vestigial
//!   fields is harmless and matches production intent). The signal fires on
//!   any *non-merge* compaction (pre-request micro / recovery micro /
//!   hard-trim) and NOT on the full-compact arms (which record a
//!   `summary_prompt` for the 25a merge), preserving the production XOR
//!   `full→merge`, `micro/partial→cleanup`. The host runs merge FIRST then
//!   cleanup (mirrors production's `partial→both` order at `mod.rs:1901` →
//!   `:1905`); when both fire, cleanup clears the `last_system_prompt_hash`
//!   merge just set → next turn re-assembles. Of the four resets, only two
//!   are live-meaningful (`last_system_prompt_hash = None` forces next-turn
//!   `refresh_system_prompt` re-assembly; `working_set.force_rebuild()` clears
//!   stale entries); the `micro_compact_state` / `circuit_breaker` resets hit
//!   vestigial session fields the live compaction path doesn't read (the live
//!   path uses the probe's divorced slots).
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
//! - **`emit_session_updated` on the post-`run` closure ✅ (slice 25a/25c §E)**
//!   — the host emits a `SessionUpdated` UI refresh alongside the post-`run`
//!   compaction closure (the 25a summary merge and/or the 25c cleanup, when
//!   either signal fired). The mid-`run` transcript replacements (`clear()` +
//!   `push()`) in `run_compaction` / `recover_context_overflow` / `reinject`
//!   still don't emit per-phase (they go through `ChatHistory::push`, which
//!   `SessionChatHistory` fans out as `SessionUpdated` in production but is a
//!   no-op in the executor's own tests); the post-`run` emit is the
//!   authoritative refresh.
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
//! - **same recovery closure as compaction** — `recover_context_overflow`
//!   shares `run_compaction`'s absorbed post-compact paths: the
//!   `merge_compaction_summary` slot (slice 25a §E — recorded into
//!   [`HostAgentExecutor::take_pending_compaction_summary`] for the host to
//!   fold post-`run`), the `reinject_compaction_attachments` path (slice 25b
//!   §E — fires right after the transcript replace with
//!   `Some(target_budget)`; dedup + budget trial + push), **and** the
//!   `post_compact_cleanup` signal (slice 25c §E — the recovery-micro and
//!   hard-trim arms set [`HostAgentExecutor::take_pending_post_compact_cleanup`]
//!   for the host to run the cleanup free fn post-`run`). Still deferred (see
//!   "Known gaps in compaction" above): `enhancements`, and working-set
//!   pins/paths.
//! - **cancel-token short-circuit** ✅ — production checks `!cancelled` before
//!   retrying after overflow recovery. This executor's `CancellationToken` is
//!   now absorbed: the reactive-recovery `continue` (restart step) is bounded
//!   by Checkpoint A (loop-top gate), so a cancel that landed during recovery
//!   is caught before the next step. The `recover_context_overflow` function
//!   itself has no internal cancel check (mirrors production); Checkpoint A is
//!   the bound. See guardrail 10 (cancel-token).
//!
//! ## Known gaps in early-tool-start (by design)
//!
//! - **`ToolCallStarted` emitted at stream time ✅** — [`reduce_stream`] fires
//!   `StreamDelta::ToolCallStarted` on `ContentBlockStop` for tool blocks
//!   (carrying the wire id), so the UI shows "calling X" before the tool
//!   executes. The `Callback::on_tool_start` trait now carries the wire `id`,
//!   so the [`CallbackBridge`] uses the real wire id (no more `bridge-{n}`
//!   synthesis) and deduplicates: the stream-time `ToolCallStarted` marks the
//!   id as announced, and the execute-time `on_tool_start` skips re-emitting.
//! - **static-only safety gate** — [`early_start_safe`] checks
//!   [`Tool::capabilities`] (`ReadOnly` present AND none of
//!   `{RequiresApproval, ExecutesCode, WritesFiles}`), mirroring the final
//!   composite clause of `turn_loop::early_tool_start_safe`. Production
//!   additionally checks `metadata.is_read_only &&
//!   metadata.supports_parallel && !is_interactive && validate_input().is_ok()
//!   && approval_requirement_for(...) == Auto` plus a tool-catalog allowlist
//!   (not-MCP / not-code-execution / not-tool-search). Those per-input /
//!   per-metadata surfaces are not reachable from the framework `Tool` trait;
//!   they thread in at the wire-in step (§E design note, same gap as
//!   [`requires_approval`]). The practical effect: a read-only tool whose
//!   per-input validation would reject the args is spawned speculatively and
//!   the (rejected) result is discarded at execute time — wasted work, not a
//!   correctness bug.
//! - **no loop-guard at spawn time** — production's `early_tool_start_safe`
//!   consults the `LoopGuard` so a 3rd-identical call isn't speculatively
//!   started (it'd be blocked at execute time anyway). This executor spawns
//!   without consulting `LoopGuard` to avoid threading a `&mut LoopGuard`
//!   through the streaming path (which would double-count the attempt — once at
//!   spawn, once at execute). The execute-time `record_attempt` is the single
//!   source of truth: a speculatively-started task for a call that the
//!   loop-guard blocks is popped + `Drop`-aborted in the tool loop (the
//!   `AttemptDecision::Block` arm). So the wasted work is a spawned task that's
//!   immediately aborted — cheap for a read-only tool.
//! - **no per-input approval / interactive checks** — `early_start_safe`
//!   statically excludes `RequiresApproval`-tagged tools, but a tool that's
//!   `Auto`-by-default yet `Required` for a specific input (e.g. `exec_shell rm`)
//!   isn't visible from `capabilities()`. Such a tool would be speculatively
//!   started and, if the execute-time approval comes back `Required`, the task
//!   is popped + `Drop`-aborted (the approval `Err(denial)` arm). Again wasted
//!   work, not a correctness bug. Threads in with the per-input approval
//!   surface at the wire-in step.
//! - **cancel-token short-circuit** — by design (N/A): production's
//!   `early_tool_start_safe` doesn't check cancel at spawn time either; a
//!   cancelled turn still spawns speculative tasks for tool blocks that close
//!   before the stream errors, but the `early_tasks.clear()` at the end of the
//!   step (and `Drop` on map drop) aborts them. The work is bounded by the
//!   number of tool blocks in the partial stream. Checkpoint A (loop-top) and
//!   Checkpoint D (post-stream) bound the turn before the next step's
//!   speculative dispatch.
//!
//! ## Known gaps in subagent (by design)
//!
//! - **blocking hold absorbed ✅** — production, when the model returns no
//!   tool calls *and* no completion is queued *but* children are still running
//!   (`should_hold_turn_for_subagents(0, running)`), blocks on a `biased
//!   select!` (`turn_loop.rs:1330-1368`) over cancel / completion `recv().await`
//!   / steer `recv().await`, emitting "Waiting on N sub-agent(s)". This executor
//!   now mirrors that: the `subagent_api` field supplies `running_count().await`,
//!   and the steer + subagent receivers are `tokio::sync::Mutex` (matching
//!   approval), so the `biased select!` arms can `recv().await` across the guard.
//!   The hold's cancel arm is **Checkpoint E** (absorbed — guardrail 10). The
//!   steer arm resumes mid-turn with the steered text (closing the steer
//!   post-stream resume gap). Batched completions behind the first `recv()` are
//!   drained by a `try_recv` loop after the `select!` (mirrors
//!   `turn_loop.rs:1342-1345`).
//! - **`ContextPatch` apply deferred** — production drains each completion's
//!   `context_patch` and applies them **tighten-only** (`auto_approve` /
//!   `trust_mode` → `false`; loosen attempts rejected) to `Session` +
//!   `config.trust_mode` (`turn_loop.rs:1373-1388`). `ChatHistory` exposes no
//!   `auto_approve` / `trust_mode` setter (host-coupled, same gap class as
//!   compaction's working-set / cycle-state reinject), so the patches are
//!   dropped. Production hardcodes `context_patch: None` at every
//!   `emit_parent_completion` site today, so this is a safe no-op; it matters
//!   only when a future child sets a by-value patch. Threads in at the wire-in
//!   step (`Session` reachable).
//! - **no `<turn_meta>` enrichment (by design)** — the synthetic sentinel message
//!   uses the plain `subagent_completion_runtime_message` (role `user`, no
//!   `user_text_message_with_turn_metadata` wrapper), matching production: the
//!   sentinel is a runtime-event marker, not user intent, so it carries no
//!   `<turn_meta>`. (The steer / LSP-flush pushes are now enriched ✅ when a
//!   [`TurnMetaProbe`] is present; the sentinel is deliberately not.)
//! - **steer mid-stream buffer + post-stream resume absorbed ✅** — production
//!   declares `pending_steers` before the stream loop (`turn_loop.rs:683`),
//!   `try_recv`s into it after every polled event (`:721-731`, emitting "Steer
//!   input queued"), then flushes at two points: post-stream no-tools (`:1297-
//!   1307`, flush + resume) and post-tool (`:2632-2637`, flush + fall through).
//!   This executor now mirrors all three: [`reduce_stream`](HostAgentExecutor::reduce_stream)
//!   buffers steers during streaming, the no-tool-calls arm flushes + resumes
//!   (before the sub-agent drain), the post-tool arm flushes, and the blocking
//!   hold's `biased select!` steer arm injects a steer arriving during the hold.
//!   Without the mid-stream buffer, steers arriving during the last step's
//!   streaming would be discarded by the next turn's stale drain.
//!
//! See `ARCHITECTURE.md` ("Framework-core agent seam") and `ROADMAP.md` §E.

use std::collections::{BTreeMap, HashMap};
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
    SystemPrompt, ToolCaller, Usage,
};
use codesmith_agent::tools::{Tool, ToolCapability, ToolError, ToolResult, ToolSet};

use super::approval::ApprovalDecision;
use super::context::{
    compact_tool_result_for_context, context_input_budget_for_provider,
    estimate_input_tokens_conservative, is_context_length_error_message,
    MAX_CONTEXT_RECOVERY_ATTEMPTS, MIN_RECENT_MESSAGES_TO_KEEP,
};
use super::loop_guard::{AttemptDecision, LoopGuard, OutcomeDecision};
use super::lsp_hooks::edit_file_paths;
use super::summarize_text;
use super::turn_loop::subagent_completion_runtime_message;
use crate::subagent::SubAgentCompletion;
use tokio_util::sync::CancellationToken;
use crate::compaction::circuit_breaker::CompactionCircuitBreaker;
use crate::compaction::micro_compact::{
    micro_compact_messages, should_trigger_micro_compact, MicroCompactState,
};
use crate::compaction::{compact_messages_safe, should_compact, CompactionConfig};
use crate::config_types::ApiProvider;
use crate::events::Event;
use crate::host_services::LspManagerApi;
use crate::lsp_diagnostics::{render_blocks as render_lsp_blocks, DiagnosticBlock};
use crate::tool_dispatch::ToolDispatcher;
use crate::tools::approval_cache::{build_approval_grouping_key, build_approval_key};
use crate::tools::spec::ApprovalRequirement;
use crate::working_set::WorkingSet;

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

/// Whether a tool is a safe candidate for **early speculative dispatch**
/// (early-tool-start, §E): the read-only, parallel-safe, no-approval, no-side-
/// effect tools whose results can be pre-computed during streaming and reused
/// at execute time (mirrors `turn_loop::early_tool_start_safe`'s final composite
/// gate). The framework `Tool` trait exposes only `capabilities()`, so this is a
/// **static approximation**: `ReadOnly` present AND none of `{RequiresApproval,
/// ExecutesCode, WritesFiles}`. Production additionally checks
/// `metadata.is_read_only && metadata.supports_parallel && !is_interactive &&
/// validate_input().is_ok() && approval_requirement_for(...) == Auto` plus a
/// tool-catalog allowlist (not-MCP / not-code-execution / not-tool-search) —
/// those per-input / per-metadata surfaces are not reachable from the framework
/// `Tool` and thread in at the wire-in step (§E design note, same gap as
/// [`requires_approval`]). `Network` / `Sandboxable` capabilities are not
/// disqualifying (a read-only network fetch is safe to start early).
fn early_start_safe(caps: &[ToolCapability]) -> bool {
    let read_only = caps.iter().any(|c| *c == ToolCapability::ReadOnly);
    read_only && !requires_approval(caps)
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

/// Decide whether the turn should hold (block) for still-running sub-agents
/// when the non-blocking completion drain found nothing (mirrors
/// `turn_loop.rs:2846-2848`). Hold fires when there are already-queued
/// completions OR children still running — so the turn waits for a child to
/// finish rather than ending prematurely.
fn should_hold_turn_for_subagents(
    queued_completions: usize,
    running_children: usize,
) -> bool {
    queued_completions > 0 || running_children > 0
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
/// surface (no core-trait change). Host-coupled follow-ups that **are**
/// absorbed: `merge_compaction_summary` (slice 25a §E —
/// [`HostAgentExecutor::take_pending_compaction_summary`]) and
/// `reinject_compaction_attachments` (slice 25b §E — [`ReinjectProbe`]).
/// Still deferred (see "Known gaps in compaction" in the module docs):
/// `post_compact_cleanup` (25c), working-set pins/paths,
/// `CompactionEnhancements`.
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

/// Post-compaction attachment re-inject collaborator (slice 25b §E). Carries
/// `Arc` clones of the three host-state sources that [`HostAgentExecutor`]'s
/// mid-`run` reinject path needs but cannot reach through `&mut dyn
/// ChatHistory` (the executor has no `&mut Session` during `run`):
///
/// - `plan_state` — `SharedPlanState = Arc<tokio::sync::Mutex<PlanState>>`,
///   already `Arc` on `EngineConfig`; cloned at wire-in (before the `&mut
///   session` borrow).
/// - `todos` — `SharedTodoList = Arc<tokio::sync::Mutex<TodoList>>`, same.
/// - `recent_read_files` — `Arc<std::sync::Mutex<VecDeque<RecentReadFile>>>`,
///   Arc-ified on `Session` in slice 25b (mirroring `working_set` / slice 22).
///
/// Sub-agent state needs **no probe** — the executor already holds a
/// `subagent_api: Option<Arc<dyn SubAgentApi>>` with `live_running_snapshots`.
/// The `<turn_meta>` enrich block is built via the existing [`TurnMetaProbe`].
///
/// Matches the established `LspProbe` / `CompactionProbe` / `CapacityProbe` /
/// [`TurnMetaProbe`] pattern: an `Option<…>` field on the executor, attached
/// via a `.with_reinject(Some(probe))` builder (precedent: `.with_turn_meta`).
pub struct ReinjectProbe {
    plan_state: crate::tool_state::plan::SharedPlanState,
    todos: crate::tool_state::todo::SharedTodoList,
    recent_read_files: Arc<std::sync::Mutex<std::collections::VecDeque<crate::session::RecentReadFile>>>,
    /// Model id used by [`compact_tool_result_for_context`] (model-dependent
    /// context limits) when observing a `read_file` result into
    /// `recent_read_files`. Mirrors the retired `turn_loop.rs` call
    /// `compact_tool_result_for_context(&self.session.model, …)`.
    model: String,
}

impl ReinjectProbe {
    /// Construct from `Arc` clones of the host-state sources. The host
    /// (`Engine::handle_send_message`) calls this *before* the
    /// `&mut self.session` borrow held by `SessionChatHistory`, snapshotting
    /// live handles (not values) so the executor reads current state at
    /// reinject time (plans / todos / subagents change during a turn). `model`
    /// is a point-in-time snapshot of `session.model` (immutable for a turn)
    /// feeding the read_file observe site (slice 25b §E follow-on).
    #[must_use]
    pub fn new(
        plan_state: crate::tool_state::plan::SharedPlanState,
        todos: crate::tool_state::todo::SharedTodoList,
        recent_read_files: Arc<std::sync::Mutex<std::collections::VecDeque<crate::session::RecentReadFile>>>,
        model: String,
    ) -> Self {
        Self {
            plan_state,
            todos,
            recent_read_files,
            model,
        }
    }

    /// The model id carried by this probe (for
    /// [`compact_tool_result_for_context`]).
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    /// Record a successful `read_file` output into the shared
    /// `recent_read_files` queue (mirrors [`Session::record_read_file_result`]
    /// via the shared [`record_read_file_result_into`]). Called by the
    /// executor's tool-result path when a `read_file` tool succeeds, so the
    /// data is live for the next compaction reinject.
    pub fn record_read_file_result(&self, input: &serde_json::Value, output_for_context: &str) {
        crate::session::record_read_file_result_into(&self.recent_read_files, input, output_for_context);
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

/// `<turn_meta>` enrichment probe (§E). Carries an `Arc` clone of the
/// session's `WorkingSet` so it can observe + enrich messages pushed *during*
/// `executor.run` — the window in which `&mut self.session` is borrowed by
/// [`SessionChatHistory`] and the host cannot reach the live working set /
/// config directly. The probe is constructed at executor-build time (before
/// the borrow) and `Arc`-shared with `Session::working_set`, matching the
/// established `LspProbe` / `CompactionProbe` / `CapacityProbe` pattern.
///
/// Two enrichment responsibilities (faithful to the retired production
/// `handle_deepseek_turn` push sites):
/// - `observe_user_message` — for **steer** pushes (records the steer text so
///   its paths enter the working set before the next `<turn_meta>` read).
/// - `enrich_user_text_message` — for **steer + LSP flush** pushes (wraps the
///   text in a `<turn_meta>` block + raw text `user` message).
///
/// The subagent-completion sentinel push is **not** enriched (plain user
/// text), matching production — see `HostAgentExecutor::run_inner`.
pub struct TurnMetaProbe {
    working_set: Arc<std::sync::Mutex<WorkingSet>>,
    workspace: PathBuf,
    skills_dir: PathBuf,
    model: String,
    auto_model: bool,
    reasoning_effort: Option<String>,
    reasoning_effort_auto: bool,
}

impl TurnMetaProbe {
    /// Construct from the session-shared working set `Arc` and the config /
    /// session model-routing fields. The model fields are snapshot values at
    /// executor-build time (set in `Engine::handle_send_message` and equal to
    /// the routed `model` param), matching production's session-model variant
    /// of `user_text_message_with_turn_metadata`.
    #[must_use]
    pub fn new(
        working_set: Arc<std::sync::Mutex<WorkingSet>>,
        workspace: PathBuf,
        skills_dir: PathBuf,
        model: String,
        auto_model: bool,
        reasoning_effort: Option<String>,
        reasoning_effort_auto: bool,
    ) -> Self {
        Self {
            working_set,
            workspace,
            skills_dir,
            model,
            auto_model,
            reasoning_effort,
            reasoning_effort_auto,
        }
    }

    /// Observe a steer's text against the shared working set (records the
    /// message turn + extracts path tokens so the next `<turn_meta>` read
    /// reflects them). Mirrors production's
    /// `WorkingSet::observe_user_message(text, &workspace)` call for steer
    /// pushes; deliberately **not** called for LSP flush or subagent
    /// sentinel pushes (no path/turn semantics for those).
    pub fn observe_user_message(&self, text: &str) {
        self.working_set
            .lock()
            .expect("working_set poisoned")
            .observe_user_message(text, &self.workspace);
    }

    /// Build a `user` message whose first content block is the `<turn_meta>`
    /// block (date / model route / working-set summary / matched conditional
    /// skills) and whose second is the raw `text`. The working set is locked
    /// only for the duration of the synchronous read (std `Mutex`, not held
    /// across `.await`); the `conditional_skills` fs walk is also sync, so the
    /// std mutex matches the `LspProbe` / `CompactionProbe` precedent.
    pub fn enrich_user_text_message(&self, text: String) -> Message {
        let working_set = self.working_set.lock().expect("working_set poisoned");
        super::turn_meta::user_text_message_with_turn_metadata(
            &working_set,
            &self.workspace,
            &self.skills_dir,
            text,
            &self.model,
            self.auto_model,
            self.reasoning_effort.as_deref(),
            self.reasoning_effort_auto,
        )
    }

    /// Build the standalone `<turn_meta>` [`ContentBlock`] (date / model route /
    /// working-set summary / matched conditional skills) from the probe's
    /// snapshotted state. Slice 25b §E: used by the mid-`run` reinject path to
    /// prepend a `<turn_meta>` block to each re-inject candidate (matching
    /// slice 24's host-side `[turn_meta, content]` shape). Same std `Mutex`
    /// lock-then-release precedent as [`enrich_user_text_message`].
    pub fn turn_metadata_block(&self) -> ContentBlock {
        let working_set = self.working_set.lock().expect("working_set poisoned");
        super::turn_meta::turn_metadata_block(
            &working_set,
            &self.workspace,
            &self.skills_dir,
            &self.model,
            self.auto_model,
            self.reasoning_effort.as_deref(),
            self.reasoning_effort_auto,
        )
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

/// Resolve a tool block's final input from its accumulated `input_buf`
/// (streamed `InputJsonDelta` fragments) with a fallback to the
/// `start_input` carried by `ContentBlockStart::ToolUse`, and finally to an
/// empty object — the same logic as the CORE `accumulate_stream`'s tail.
/// Extracted so both [`finalize_blocks`] (at stream end) and the early-start
/// spawn (at `ContentBlockStop`, mid-stream) finalize identically.
fn finalize_tool_input(input_buf: &str, start_input: &serde_json::Value) -> serde_json::Value {
    if !input_buf.is_empty() {
        serde_json::from_str(input_buf).unwrap_or(serde_json::Value::Null)
    } else if !start_input.is_null() {
        start_input.clone()
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    }
}

/// A speculatively-dispatched read-only tool task started during streaming
/// (early-tool-start, §E). When a `ContentBlockStop` for a tool block lands
/// mid-stream, [`reduce_stream`](HostAgentExecutor::reduce_stream) checks
/// [`early_start_safe`] and, if the tool qualifies, spawns it on the runtime
/// so its result is ready by the time the executor reaches the tool loop. At
/// execute time the executor pops the task by wire `id`, re-verifies the
/// name + input (the model could in principle revise args after the block
/// closed), and awaits the `JoinHandle` to reuse the result instead of
/// re-running the tool (mirrors `turn_loop`'s `early_tool_tasks` map +
/// `EarlyToolTask`).
///
/// `Drop` aborts the spawned task so an unreused / orphaned task (e.g. a
/// block that didn't survive into the parsed `tool_uses`, or a call blocked
/// by the loop-guard / denied approval at execute time) never leaks a
/// background task. Aborting a task that already completed is a no-op, so the
/// reuse path (await then drop) is safe. The handle is wrapped in `Option` so
/// the reuse path can [`Option::take`] it out for `.await` — a type that
/// implements `Drop` can't otherwise let a field be moved out of it.
struct EarlyToolTask {
    /// Tool name (re-verified at execute time).
    name: String,
    /// Finalized input (re-verified at execute time).
    input: serde_json::Value,
    /// The speculative task. `Some` until the reuse path [`Option::take`]s
    /// it for `.await`; aborted on every other path (via `Drop`).
    handle: Option<tokio::task::JoinHandle<Result<ToolResult, ToolError>>>,
}

impl Drop for EarlyToolTask {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            // `abort` takes `&self`; safe on a completed task (no-op).
            handle.abort();
        }
    }
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
                let input = finalize_tool_input(&input_buf, &start_input);
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

/// `Option<u32>` saturating add — mirrors `turn.rs::add_optional_usage`
/// (`Some` + `Some` → saturating add; `Some` + `None` → keep the `Some`;
/// `None` + `None` → `None`). Defined inline here rather than re-exported
/// from `turn.rs` to keep the slice self-contained; the two can be unified
/// into a shared `Usage::add` later.
fn add_optional_usage(total: Option<u32>, delta: Option<u32>) -> Option<u32> {
    match (total, delta) {
        (Some(t), Some(d)) => Some(t.saturating_add(d)),
        (Some(t), None) => Some(t),
        (None, Some(d)) => Some(d),
        (None, None) => None,
    }
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
        /// Token usage captured for this stream (slice 21 §E):
        /// `MessageStart` sets input tokens, `MessageDelta` overwrites with
        /// the latest cumulative usage (replace-within-stream — mirrors the
        /// retired `turn_loop.rs:838` / `:1137-1141`). The caller adds it to
        /// the per-turn total (mirrors `turn_loop.rs:1193`
        /// `turn.add_usage`).
        usage: Usage,
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
        /// Same capture as [`StreamReduceOutcome::Complete`]'s `usage` — the
        /// partial stream's usage up to the mid-flight death (surfaced, not
        /// retried, so its tokens still count).
        usage: Usage,
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
        /// Per-stream usage threaded up from [`reduce_stream`] (slice 21 §E)
        /// — `run_inner` adds it to the per-turn total via
        /// [`HostAgentExecutor::accumulate_usage`].
        usage: Usage,
    },
    /// A pre-stream context-length rejection was classified (via
    /// [`is_context_length_error_message`]) and emergency compaction succeeded
    /// — the caller should `continue` the step loop so the request snapshot
    /// picks up the compacted transcript (mirrors `turn_loop.rs:631-632`).
    RecoveredContextOverflow,
    /// The turn was cancelled during the stream phase (Checkpoint B race at
    /// stream-open, Checkpoint C guard in the transparent-retry `Empty` arm,
    /// or Checkpoint D post-stream gate). The caller should surface
    /// [`StopReason::Interrupted`] — mirroring production's
    /// `TurnOutcomeStatus::Interrupted` (`turn_loop.rs:294-298`, `:607-614`,
    /// `:1147-1150`).
    Interrupted,
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
    /// `mpsc::Receiver::try_recv`/`recv` takes `&mut self`. Uses
    /// `tokio::sync::Mutex` (not `std::sync::Mutex`) so the guard may cross the
    /// blocking `recv().await` in the sub-agent blocking hold's `biased select!`
    /// steer arm — the same rationale as the `approval` field. The pre-request
    /// drain (`try_recv`) is non-blocking and the lock is uncontended (single
    /// consumer), so the tokio mutex is a no-cost upgrade there. Steers are
    /// drained (consumed) each step, so unlike diagnostics they don't
    /// accumulate — the receiver merely persists across `run` invocations on
    /// the same executor so a steer queued between turns is picked up on the
    /// next turn's first pre-request drain.
    steer: Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>,
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
    /// on the next turn's per-tool approval await. The cancel race is absorbed
    /// ✅ (production's `await_tool_approval` also selects on
    /// `cancel_token.cancelled()` — see guardrail 10 / "Known gaps in
    /// approval" below).
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
    /// Optional sub-agent completion receiver (§E). `None` ⇒ the post-stream
    /// completion drain is a no-op (the turn ends on the first no-tool-call
    /// round). When the model finishes a step with no tool calls, queued
    /// child completions are drained via `try_recv` and injected as
    /// `<codesmith:runtime_event kind="subagent_completion">` user messages
    /// (the sentinel contract documented in `prompts/base.md`), then the turn
    /// resumes — mirroring `handle_deepseek_turn`'s non-blocking completion
    /// drain (`turn_loop.rs:1317-1397`). The **blocking hold** for
    /// still-running children (`should_hold_turn_for_subagents` + a `biased
    /// select!` over cancel / completion `recv().await` / steer `recv().await`)
    /// is absorbed ✅ — it needs [`SubAgentApi::running_count`] (the
    /// [`Self::subagent_api`] field) and a `tokio::sync::Mutex` receiver (the
    /// guard must cross the `recv().await`, same rationale as the `approval`
    /// field; the steer receiver migrated from `std::sync::Mutex` in this same
    /// slice). The `CancellationToken` is absorbed (guardrail 10); the hold's
    /// own `select!` cancel arm is Checkpoint E. Interior-mutable because
    /// [`AgentExecutor::run`] takes `&self` while
    /// `mpsc::Receiver::try_recv`/`recv` takes `&mut self`. The non-blocking
    /// drain holds the lock only for the synchronous `try_recv` (never across
    /// an `await`); the blocking hold holds it across `recv().await` (single
    /// consumer, no contention). The receiver persists across `run`
    /// invocations on the same executor, matching the production
    /// `Engine.rx_subagent_completion` field — a completion that arrives
    /// between turns is surfaced on the next turn's post-stream drain.
    /// `ContextPatch` apply (tighten-only `auto_approve`/`trust_mode`) is
    /// deferred — it mutates `Session` state not reachable through
    /// `ChatHistory`, and production hardcodes `context_patch: None` today.
    subagent: Option<Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<SubAgentCompletion>>>>,
    /// Optional sub-agent manager handle (§E). `None` ⇒ the blocking hold is
    /// disabled (no `running_count` to check). Injected as `Arc<dyn
    /// SubAgentApi>` — the minimal surface (mirrors [`LspProbe`]'s `Arc<dyn
    /// LspManagerApi>` pattern, not full [`HostServices`]). `SubAgentApi` is
    /// `#[async_trait]` + `Send + Sync`, already a dependency of this crate.
    /// Used only for `running_count().await` in the blocking-hold gate.
    subagent_api: Option<Arc<dyn crate::host_services::SubAgentApi>>,
    /// Optional cancel token (§E). `None` ⇒ cancel checks are no-ops
    /// (`is_cancelled()` returns `false`). When `Some`, mirrors production
    /// `handle_deepseek_turn`'s seven cancel checkpoints: (A) loop-top gate,
    /// (B) stream-open race, (C) transparent-retry `!cancelled` guard,
    /// (D) post-stream gate, (G) post-tool-loop final gate, plus the approval
    /// `select!` race and the steer stale-drain at `run_inner` start. The
    /// sub-agent blocking hold race (Checkpoint E) is absorbed ✅ — it lives
    /// in the hold's own `biased select!` cancel arm. Early-tool-start spawn
    /// has no production cancel guard (bounded by `early_tasks.clear()`/`Drop`).
    cancel_token: Option<CancellationToken>,
    /// Optional host tool-dispatcher (slice 20 §E) for per-input approval
    /// overrides. `None` (default; all embeds/tests) ⇒ `request_approval`
    /// falls back to the static [`requires_approval`] capability gate. When
    /// `Some` (production wire-in), consults
    /// [`ToolDispatcher::approval_requirement_for`] first — mirroring
    /// production's `registry.approval_requirement_for(..)` (turn_loop.rs
    /// ~1700). A `Some(Auto)` answer downgrades an `ExecutesCode` tool to
    /// no-approval for a specific safe input.
    tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    /// Optional `<turn_meta>` enrichment probe (slice 22 §E). `None` (default;
    /// all embeds/tests) ⇒ steer / LSP-flush pushes emit plain user text with
    /// no working-set observe, preserving the pre-slice-22 behavior. When
    /// `Some` (production wire-in), the probe — an `Arc` clone of the
    /// session's `WorkingSet` constructed before the `&mut self.session`
    /// borrow held by `SessionChatHistory` — observes steer text and wraps
    /// steer / LSP-flush pushes in `<turn_meta>` during `executor.run`,
    /// matching the retired production `handle_deepseek_turn` push sites.
    /// The subagent-completion sentinel push is **never** enriched (plain
    /// text, matching production).
    turn_meta: Option<TurnMetaProbe>,
    /// Post-compaction attachment re-inject probe (slice 25b §E). Carries
    /// `Arc` clones of `config.plan_state` / `config.todos` /
    /// `session.recent_read_files` (snapshotted at wire-in before the
    /// `&mut session` borrow) so [`reinject_compaction_attachments`] can
    /// re-insert plan / todos / subagents / read_files candidates DURING
    /// `run`, right after [`run_compaction`] / [`recover_context_overflow`]
    /// replace the transcript. `None` ⇒ reinject is a no-op (embeds/tests
    /// that don't opt in — matches the absent-probe precedent for the other
    /// probes). Sub-agent state comes from the existing [`subagent_api`]
    /// field (no probe needed for it); the `<turn_meta>` enrich block comes
    /// from [`turn_meta`].
    reinject: Option<ReinjectProbe>,
    /// Per-turn token usage accumulated across streams (slice 21 §E). The
    /// inline stream reducer ([`reduce_stream`]) captures `MessageStart` +
    /// `MessageDelta` usage (replace-within-stream — the latest cumulative
    /// value wins, mirroring the retired `turn_loop.rs:838` /
    /// `:1137-1141`); `run_inner` adds each completed stream's usage here
    /// (mirrors `turn_loop.rs:1193` `turn.add_usage(&usage)`). The host
    /// reads it back via [`HostAgentExecutor::take_usage`] after `run`
    /// returns — the executor is constructed fresh per turn, so there is
    /// no cross-turn leakage. `std::sync::Mutex` (not tokio): accumulation
    /// is synchronous field arithmetic, and the lock is never held across
    /// an `await` (matches the `LspProbe` / `CompactionProbe` precedent).
    usage: std::sync::Mutex<Usage>,

    /// `result.summary_prompt` from `run_compaction` / `recover_context_overflow`
    /// (slice 25a §E) — accumulated across a turn's compactions via
    /// [`crate::compaction::merge_system_prompts`] (mirrors production's
    /// `merge_compaction_summary` folding each into
    /// `session.compaction_summary_prompt`). The host reads it back via
    /// [`HostAgentExecutor::take_pending_compaction_summary`] after `run`
    /// returns — the merge is deferred to post-`run` because the executor's
    /// system prompt is a static `config.system` snapshot (taken before the
    /// `&mut session` borrow), so any `session.system_prompt` mutation during
    /// `run` is invisible to the same turn's requests; the merged summary only
    /// matters for the next turn (which re-snapshots). `std::sync::Mutex` (not
    /// tokio): accumulation is a synchronous merge fn call, and the lock is
    /// never held across an `await` (matches the `usage` / `LspProbe` /
    /// `CompactionProbe` precedent).
    pending_compaction_summary: std::sync::Mutex<Option<SystemPrompt>>,

    /// Signal that a *non-merge* compaction changed the transcript during
    /// `run` (slice 25c §E) — set by [`Self::signal_post_compact_cleanup`]
    /// from the micro-compact arms of [`run_compaction`] /
    /// [`recover_context_overflow`] and the hard-trim arm of
    /// [`recover_context_overflow`] (i.e. any compaction that does NOT
    /// produce a `summary_prompt` to merge). The host reads it back via
    /// [`HostAgentExecutor::take_pending_post_compact_cleanup`] after `run`
    /// returns and runs [`crate::compaction::post_compact_cleanup`] on
    /// `&mut self.session` — the production closure that force-rebuilds the
    /// working set + resets `last_system_prompt_hash` / `circuit_breaker` /
    /// `micro_compact_state` after a compaction (the transcript the working
    /// set was derived from is now stale). Deferred to post-`run` (like the
    /// 25a summary merge) because the three plain `Session` fields it resets
    /// are only reachable through `&mut Session`, which the executor can't
    /// hold during `run`; the reset only matters for the next turn anyway.
    /// Reuses the existing free fn (single source). The *full-compact* arms
    /// record a `summary_prompt` (merge path, 25a) and deliberately do NOT
    /// set this slot — preserving the production XOR `full→merge`,
    /// `micro/partial→cleanup`. `std::sync::Mutex<bool>` (sync, never held
    /// across an `await`; matches `usage` / `pending_compaction_summary`).
    pending_post_compact_cleanup: std::sync::Mutex<bool>,
}

impl HostAgentExecutor {
/// Construct from the four collaborators + config + an optional guardrail
/// status channel (`None` for embeds that don't surface guardrail status) +
/// an optional [`LspProbe`] (`None` ⇒ LSP collect/flush disabled) + an
/// optional steer input receiver (`None` ⇒ steer drain disabled) + an
/// optional approval-decision receiver (`None` ⇒ approval gating disabled) +
/// an optional [`CompactionProbe`] (`None` ⇒ compaction disabled) + an
/// optional [`CapacityProbe`] (`None` ⇒ capacity preflight disabled) + an
/// optional sub-agent completion receiver (`None` ⇒ the post-stream
/// completion drain is disabled — the turn ends on the first no-tool-call
/// round) + an optional [`CancellationToken`] (`None` ⇒ cancel checks are
/// no-ops) + an optional sub-agent manager handle (`None` ⇒ the blocking hold
/// for still-running children is disabled — no `running_count` to check).
#[must_use]
pub fn new(
    client: LlmClientHandle,
    tools: Arc<ToolSet>,
    callback: Arc<dyn Callback>,
    config: AgentExecutorConfig,
    event_tx: Option<mpsc::Sender<Event>>,
    lsp: Option<LspProbe>,
    steer: Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>,
    approval: Option<Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>>,
    compaction: Option<CompactionProbe>,
    capacity: Option<CapacityProbe>,
    subagent: Option<Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<SubAgentCompletion>>>>,
    cancel_token: Option<CancellationToken>,
    subagent_api: Option<Arc<dyn crate::host_services::SubAgentApi>>,
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
        subagent,
        cancel_token,
        subagent_api,
        tool_dispatcher: None,
        turn_meta: None,
        reinject: None,
        usage: std::sync::Mutex::new(Usage::default()),
        pending_compaction_summary: std::sync::Mutex::new(None),
        pending_post_compact_cleanup: std::sync::Mutex::new(false),
    }
}

    /// Opt into per-input approval consultation (slice 20 §E). The production
    /// wire-in calls this after [`new`] with `plan.tool_registry.clone()` so
    /// [`request_approval`] consults [`ToolDispatcher::approval_requirement_for`]
    /// before the static capability gate. Embeds/tests skip it — the field
    /// defaults to `None`. Consumes and returns `self` (builder).
    #[must_use]
    pub fn with_tool_dispatcher(
        mut self,
        tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    ) -> Self {
        self.tool_dispatcher = tool_dispatcher;
        self
    }

    /// Opt into `<turn_meta>` enrichment for steer / LSP-flush pushes (slice
    /// 22 §E). The production wire-in calls this after [`new`] with a
    /// [`TurnMetaProbe`] built from an `Arc` clone of `session.working_set`
    /// plus the config / session model-routing fields (snapshotted before the
    /// `&mut self.session` borrow held by `SessionChatHistory`). Embeds /
    /// tests skip it — the field defaults to `None`, so push sites emit plain
    /// user text (the pre-slice-22 behavior) and all 78 existing tests stay
    /// unchanged. Consumes and returns `self` (builder).
    #[must_use]
    pub fn with_turn_meta(mut self, turn_meta: Option<TurnMetaProbe>) -> Self {
        self.turn_meta = turn_meta;
        self
    }

    /// Opt into post-compaction attachment re-inject (slice 25b §E). The
    /// production wire-in calls this after [`new`] with a [`ReinjectProbe`]
    /// built from `Arc` clones of `config.plan_state` / `config.todos` /
    /// `session.recent_read_files` (snapshotted before the `&mut self.session`
    /// borrow) so [`Self::reinject_compaction_attachments`] can re-insert plan /
    /// todos / subagents / read_files candidates during `run`, right after
    /// [`run_compaction`] / [`recover_context_overflow`] replace the
    /// transcript. Embeds / tests skip it — the field defaults to `None`, so
    /// the mid-`run` reinject is a no-op (the pre-slice-25b behavior) and
    /// existing tests stay unchanged. Consumes and returns `self` (builder).
    #[must_use]
    pub fn with_reinject(mut self, reinject: Option<ReinjectProbe>) -> Self {
        self.reinject = reinject;
        self
    }

    /// Read back the per-turn token usage accumulated by the inline stream
    /// reducer (slice 21 §E). The host calls this after `run` returns to
    /// populate `turn.usage` — the end-of-turn handoff the retired
    /// `turn_loop.rs:1193` (`turn.add_usage(&usage)`) used to do inline. The
    /// executor is constructed fresh each turn, so this starts at zero and
    /// reflects only the current turn's streams. Clones under the lock (cheap;
    /// read once).
    #[must_use]
    pub fn take_usage(&self) -> Usage {
        self.usage.lock().expect("usage mutex poisoned").clone()
    }

    /// Read back the accumulated compaction `summary_prompt` recorded by
    /// `run_compaction` / `recover_context_overflow` (slice 25a §E). The host
    /// calls this after `run` returns to fold the summary into
    /// `session.system_prompt` via `Engine::merge_compaction_summary`
    /// (behavior-equivalent to merging mid-`run` since the executor's system
    /// prompt is a static snapshot — the merged summary only matters for the
    /// next turn's snapshot). Drains the slot; the executor is constructed
    /// fresh per turn, so no cross-turn leakage. Mirrors the [`take_usage`]
    /// read-back pattern.
    #[must_use]
    pub fn take_pending_compaction_summary(&self) -> Option<SystemPrompt> {
        self.pending_compaction_summary
            .lock()
            .expect("pending_compaction_summary mutex poisoned")
            .take()
    }

    /// Accumulate a compaction `summary_prompt` into the pending slot (slice
    /// 25a §E). Called from the `Ok(result)` arms of `run_compaction` and
    /// `recover_context_overflow` instead of discarding `result.summary_prompt`.
    /// Folds via [`crate::compaction::merge_system_prompts`] so multiple
    /// compactions in one turn accumulate (mirrors production's
    /// `merge_compaction_summary` folding each into
    /// `session.compaction_summary_prompt`). Sync; the current value is read
    /// out, merged, and written back inside one synchronous critical section
    /// (no `await` crosses the guard).
    fn record_compaction_summary(&self, summary: Option<SystemPrompt>) {
        let mut guard = self
            .pending_compaction_summary
            .lock()
            .expect("pending_compaction_summary mutex poisoned");
        let current = guard.take();
        *guard = crate::compaction::merge_system_prompts(current.as_ref(), summary);
    }

    /// Read back whether a *non-merge* compaction changed the transcript
    /// during `run` (slice 25c §E). The host calls this after `run` returns
    /// to decide whether to run [`crate::compaction::post_compact_cleanup`]
    /// on `&mut self.session` (force-rebuild working set + reset
    /// `last_system_prompt_hash` / `circuit_breaker` / `micro_compact_state`).
    /// Drains the slot (one-shot); the executor is constructed fresh per turn,
    /// so no cross-turn leakage. Mirrors the [`take_pending_compaction_summary`]
    /// read-back pattern. Idempotent: multiple non-merge compactions in one
    /// turn collapse to a single cleanup.
    #[must_use]
    pub fn take_pending_post_compact_cleanup(&self) -> bool {
        std::mem::replace(
            &mut *self
                .pending_post_compact_cleanup
                .lock()
                .expect("pending_post_compact_cleanup mutex poisoned"),
            false,
        )
    }

    /// Signal that a *non-merge* compaction changed the transcript (slice
    /// 25c §E). Called from the micro-compact arms of `run_compaction` /
    /// `recover_context_overflow` and the hard-trim arm of
    /// `recover_context_overflow`. The *full-compact* arms do NOT call this
    /// — they record a `summary_prompt` (merge path, 25a) instead, so the
    /// host's post-`run` closure preserves the production XOR (`full→merge`,
    /// `micro/partial→cleanup`). Sync; the boolean is set inside one
    /// synchronous critical section (no `await` crosses the guard).
    fn signal_post_compact_cleanup(&self) {
        *self
            .pending_post_compact_cleanup
            .lock()
            .expect("pending_post_compact_cleanup mutex poisoned") = true;
    }

    /// Add one completed stream's usage to the per-turn total (slice 21 §E).
    /// Mirrors `TurnContext::add_usage` (`turn.rs:106`):
    /// `input_tokens` / `output_tokens` saturating-add, the optional cache /
    /// reasoning fields add when both sides are present. (`reasoning_replay_
    /// tokens` / `server_tool_use` are intentionally not accumulated —
    /// `add_usage` omits them too, so production's `turn.usage` never carried
    /// them; this stays faithful.) The `add_optional_usage` helper duplicates
    /// `turn.rs`'s private one — to be unified into a shared `Usage::add`
    /// later, same class of "lift later" duplication as
    /// `approval_intent_summary` / `block_tool_result`.
    fn accumulate_usage(&self, delta: &Usage) {
        let mut acc = self.usage.lock().expect("usage mutex poisoned");
        acc.input_tokens = acc.input_tokens.saturating_add(delta.input_tokens);
        acc.output_tokens = acc.output_tokens.saturating_add(delta.output_tokens);
        acc.prompt_cache_hit_tokens = add_optional_usage(
            acc.prompt_cache_hit_tokens,
            delta.prompt_cache_hit_tokens,
        );
        acc.prompt_cache_miss_tokens = add_optional_usage(
            acc.prompt_cache_miss_tokens,
            delta.prompt_cache_miss_tokens,
        );
        acc.reasoning_tokens = add_optional_usage(acc.reasoning_tokens, delta.reasoning_tokens);
    }

    /// Surface a guardrail status message onto the host's UI `Event` channel,
    /// if one was supplied. Guardrails emit here directly rather than through
    /// the framework `Callback` (see the module docs).
    async fn emit_status(&self, message: String) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(Event::status(message)).await;
        }
    }

    /// Whether the turn has been cancelled. `None` cancel token ⇒ never
    /// cancelled (embeds/tests that don't need cancellation). Mirrors production
    /// `self.cancel_token.is_cancelled()` checks at every turn-loop checkpoint.
    fn is_cancelled(&self) -> bool {
        self.cancel_token.as_ref().map_or(false, |t| t.is_cancelled())
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
    /// lands (matching production's `turn_loop.rs:864/874/985/1254`).
    ///
    /// **Early speculative dispatch (early-tool-start):** when a tool block
    /// reaches `ContentBlockStop`, its input is finalized and — if the tool is
    /// [`early_start_safe`] — a [`tokio::spawn`] runs it immediately so its
    /// result is ready by the time the executor reaches the tool loop. The
    /// `JoinHandle` is stored in `early_tasks` keyed by the wire tool id; the
    /// tool loop pops it by id, re-verifies name + input, and awaits it to
    /// reuse the result (mirrors `turn_loop`'s `early_tool_tasks` map +
    /// `early_tool_start_safe`). `Event::ToolCallStarted` is also fired on
    /// `ContentBlockStop` for tool blocks via `StreamDelta::ToolCallStarted`
    /// (carrying the wire id) — the `CallbackBridge` deduplicates this
    /// stream-time emission against the execute-time `on_tool_start`.
    async fn reduce_stream(
        &self,
        mut stream: StreamEventBox,
        early_tasks: &mut HashMap<String, EarlyToolTask>,
        pending_steers: &mut Vec<String>,
    ) -> StreamReduceOutcome {
        let mut blocks: BTreeMap<u32, BlockBuild> = BTreeMap::new();
        let mut stop_reason: Option<String> = None;
        let mut any_content_received = false;
        // Per-stream usage (slice 21 §E): `MessageStart` sets input tokens,
        // `MessageDelta` overwrites with the latest cumulative usage
        // (replace-within-stream — mirrors the retired `turn_loop.rs:838` /
        // `:1137-1141`). Returned on `Complete`/`Partial` so the caller adds
        // it to the per-turn total (mirrors `turn_loop.rs:1193`). `Empty`
        // carries none — a retried stream's partial usage is dropped, matching
        // production's `continue` before `turn.add_usage`.
        let mut usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            ..Usage::default()
        };

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
                            usage,
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

            // Mid-stream steer buffer (mirrors `turn_loop.rs:721-731`): drain
            // any steers that arrived while the stream was producing. These are
            // buffered (not injected mid-stream) and flushed by `run_inner`
            // post-stream / post-tool — without this, steers arriving during the
            // last step's streaming would be discarded by the next turn's stale
            // drain. The tokio mutex guard is taken and dropped within the `{ }`
            // block before `emit_status().await` — no lock crosses an `await`
            // (matching `drain_steers`).
            if let Some(rx) = &self.steer {
                loop {
                    let steer = {
                        let mut guard = rx.lock().await;
                        match guard.try_recv() {
                            Ok(s) => s,
                            Err(_) => break,
                        }
                    };
                    let steer = steer.trim().to_string();
                    if steer.is_empty() {
                        continue;
                    }
                    pending_steers.push(steer.clone());
                    self.emit_status(format!(
                        "Steer input queued: {}",
                        summarize_text(&steer, 120)
                    ))
                    .await;
                }
            }

            match event {
                StreamEvent::MessageStart { message } => {
                    // Replace the running usage with the message's usage
                    // (input tokens + cache tokens arrive here for Anthropic
                    // / OpenAI). Mirrors the retired `turn_loop.rs:838`
                    // `usage = message.usage;`.
                    usage = message.usage;
                }
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
                    // deferred, see the `reduce_stream` doc). The block is
                    // looked up (not removed) so it stays available for
                    // `finalize_blocks` at stream end.
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
                            BlockBuild::ToolUse {
                                id,
                                name,
                                input_buf,
                                start_input,
                                ..
                            } => {
                                // Finalize the tool input now that all
                                // `InputJsonDelta` fragments have arrived.
                                let finalized_input =
                                    finalize_tool_input(input_buf, start_input);

                                // Announce the tool call at stream-time —
                                // production fires `Event::ToolCallStarted` on
                                // `ContentBlockStop` for tool blocks (carrying
                                // the wire id) so the UI can show "calling X"
                                // before the tool actually executes. The
                                // `CallbackBridge` marks the id as announced so
                                // the execute-time `on_tool_start` skips
                                // re-emitting (dedup).
                                self.callback
                                    .on_stream_delta(&StreamDelta::ToolCallStarted {
                                        id: id.clone(),
                                        name: name.clone(),
                                        input: finalized_input.clone(),
                                    })
                                    .await;

                                // Early speculative dispatch: if the tool is
                                // early-start-safe (read-only, no approval),
                                // spawn it now so its result is ready by the
                                // tool loop (mirrors `turn_loop`'s
                                // `early_tool_start_safe` + spawn at
                                // `ContentBlockStop`). `tokio::spawn` returns
                                // immediately (non-blocking); the stream keeps
                                // consuming. `Drop` on `EarlyToolTask` aborts
                                // an unreused task so nothing leaks.
                                if let Some(tool) = self.tools.get(name) {
                                    if early_start_safe(&tool.capabilities()) {
                                        let tool = Arc::clone(tool);
                                        let input = finalized_input.clone();
                                        let handle = tokio::spawn(async move {
                                            tool.run(input).await
                                        });
                                        early_tasks.insert(
                                            id.clone(),
                                            EarlyToolTask {
                                                name: name.clone(),
                                                input: finalized_input,
                                                handle: Some(handle),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                StreamEvent::MessageDelta {
                    delta: MessageDelta { stop_reason: sr, .. },
                    usage: delta_usage,
                } => {
                    if let Some(u) = delta_usage {
                        // Replace — the latest cumulative usage wins (mirrors
                        // the retired `turn_loop.rs:1137-1141`
                        // `if let Some(u) = delta_usage { usage = u; }`).
                        usage = u;
                    }
                    if sr.is_some() {
                        stop_reason = sr;
                    }
                }
                StreamEvent::MessageStop => break,
                StreamEvent::Ping => {}
            }
        }

        let content = finalize_blocks(blocks);
        StreamReduceOutcome::Complete {
            content,
            stop_reason,
            usage,
        }
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
    /// # Cancel-token checkpoints (absorbed)
    ///
    /// The cancel-token short-circuits are **absorbed** — Checkpoint B (stream-
    /// open race), Checkpoint C (`!cancelled` in the transparent-retry `Empty`
    /// arm, mirroring `should_transparently_retry_stream`), and Checkpoint D
    /// (post-stream gate, discarding even cleanly-completed content if the turn
    /// was cancelled mid-stream). All return [`StreamRoundOutcome::Interrupted`]
    /// so `run_inner` surfaces [`StopReason::Interrupted`]. The loop-top gate
    /// (Checkpoint A in `run_inner`) bounds the capacity/reactive `continue`
    /// loops on a cancelled turn. Production's inner mid-flight retry (resetting
    /// the stream *inside* the event loop when no content was received yet,
    /// `turn_loop.rs:775-834`) is not replicated here; this executor uses the
    /// simpler outer retry (re-call `create_message_stream`). The two are
    /// functionally equivalent for the retry decision; the inner retry's
    /// advantage is avoiding a redundant `MessageStart` round-trip, which
    /// matters only for latency-sensitive production paths.
    async fn stream_with_transparent_retry(
        &self,
        client: &LlmClientHandle,
        request: MessageRequest,
        stream_retry_attempts: &mut u32,
        context_recovery_attempts: &mut u8,
        history: &mut dyn ChatHistory,
        system: Option<&SystemPrompt>,
        early_tasks: &mut HashMap<String, EarlyToolTask>,
        pending_steers: &mut Vec<String>,
    ) -> Result<StreamRoundOutcome> {
        /// Cap on transparent stream retries — matches `turn_loop`'s
        /// `MAX_STREAM_RETRIES` (3). One initial attempt + 3 retries = 4 total
        /// `create_message_stream` calls before the failure surfaces.
        const MAX_STREAM_RETRIES: u32 = 3;
        // Clone the token once per round so the cancel future owns a local
        // (not a `&self` borrow) — avoids borrow-checker conflicts with the
        // `self.emit_status` / `self.try_recover_context_overflow` calls in the
        // select arms. `CancellationToken::clone` is a cheap Arc bump.
        let cancel_token = self.cancel_token.clone();
        loop {
            // Checkpoint B — stream-open cancel race (mirrors
            // `turn_loop.rs:607-614`): race the cancel token against
            // `create_message_stream` so a cancelled turn aborts before the
            // stream even opens. `biased` so cancel wins if both are ready.
            // A pre-stream error may be a context-length rejection — classify
            // before propagating so reactive recovery (seam 2) can run. Only
            // mid-flight stream errors (from `reduce_stream`) retry
            // transparently; pre-stream errors are either recovered (restart
            // the step) or hard-failed.
            let stream = tokio::select! {
                biased;
                _ = async {
                    match &cancel_token {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    self.emit_status("Request cancelled".to_string()).await;
                    return Ok(StreamRoundOutcome::Interrupted);
                }
                result = client.create_message_stream(request.clone()) => match result {
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
                }
            };
            match self.reduce_stream(stream, early_tasks, pending_steers).await {
                StreamReduceOutcome::Complete {
                    content,
                    stop_reason,
                    usage,
                } => {
                    // Checkpoint D — post-stream cancel gate (mirrors
                    // `turn_loop.rs:1147-1150`): even a cleanly-completed
                    // stream is discarded if the turn was cancelled mid-stream.
                    if self.is_cancelled() {
                        self.emit_status("Request cancelled".to_string()).await;
                        return Ok(StreamRoundOutcome::Interrupted);
                    }
                    // Healthy round → reset the retry budget so a bad prior
                    // step doesn't carry over (mirrors `turn_loop.rs:1186`).
                    *stream_retry_attempts = 0;
                    return Ok(StreamRoundOutcome::Content { content, stop_reason, usage });
                }
                StreamReduceOutcome::Partial {
                    content,
                    stop_reason,
                    error,
                    usage,
                } => {
                    // Checkpoint D — post-stream cancel gate. Same as Complete:
                    // if cancelled, discard the partial content and return
                    // `Interrupted`.
                    if self.is_cancelled() {
                        self.emit_status("Request cancelled".to_string()).await;
                        return Ok(StreamRoundOutcome::Interrupted);
                    }
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
                    return Ok(StreamRoundOutcome::Content { content, stop_reason, usage });
                }
                StreamReduceOutcome::Empty { error } => {
                    // Checkpoint C — transparent-retry `!cancelled` guard
                    // (mirrors `should_transparently_retry_stream` in
                    // `streaming.rs:81-87`): if the turn was cancelled, don't
                    // retry — return `Interrupted` (not `Err`).
                    if self.is_cancelled() {
                        self.emit_status("Request cancelled".to_string()).await;
                        return Ok(StreamRoundOutcome::Interrupted);
                    }
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

    /// (3) per-tool observe seam — record a successful `read_file` output into
    /// the shared `recent_read_files` queue so a later compaction can re-inject
    /// a concise reminder. Mirrors the retired `turn_loop.rs:2523-2525`:
    ///
    /// ```text
    /// let output_for_context = compact_tool_result_for_context(&model, name, &output);
    /// if output.success && name == "read_file" {
    ///     self.session.record_read_file_result(&tool_input, &output_for_context);
    /// }
    /// ```
    ///
    /// Feeds the *compacted / sanitized* form (not raw content) so hidden
    /// Unicode-character attacks (HackerOne #3086545) are stripped before the
    /// preview is retained — a security property the raw-content path would
    /// lose. Synchronous; the lock is taken and dropped before returning,
    /// never held across an `await`. No-op when `reinject` is `None` (no
    /// probe ⇒ no `recent_read_files` Arc ⇒ the data would never be read by a
    /// reinject, so skipping is correct), when the tool isn't `read_file`, or
    /// when the result didn't succeed.
    fn record_read_file_result(&self, name: &str, input: &serde_json::Value, result: &ToolResult) {
        if name != "read_file" || !result.success {
            return;
        }
        let Some(probe) = &self.reinject else {
            return;
        };
        let output_for_context = compact_tool_result_for_context(probe.model(), name, result);
        probe.record_read_file_result(input, &output_for_context);
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

    /// (1) per-step pre-request seam — drain queued steer inputs into the
    /// transcript as `user` messages so the model sees them before its next
    /// request. Mirrors `handle_deepseek_turn`'s top-of-loop steer drain
    /// (`turn_loop.rs:300-317`): `try_recv` loop → trim → skip-empty → push a
    /// `user` message → emit status. `try_recv` is non-blocking — this only
    /// drains what's already queued; it never waits for new input.
    ///
    /// When a [`TurnMetaProbe`] is wired in (production), this mirrors
    /// production's `working_set.observe_user_message(text, &workspace)` +
    /// `user_text_message_with_turn_metadata` wrap for each drained steer
    /// (observe before the move, then enrich). Embeds/tests (`probe` absent)
    /// push plain text — the pre-slice-22 behavior. The mid-stream buffer
    /// drain site (inside [`reduce_stream`](HostAgentExecutor::reduce_stream)'s
    /// `try_recv` + [`flush_pending_steers`](HostAgentExecutor::flush_pending_steers))
    /// is now absorbed ✅; the blocking `recv` during the sub-agent hold is
    /// enriched via the hold's own steer arm.
    /// Push a steer message into the transcript, observing against the shared
    /// working set and wrapping in `<turn_meta>` when a [`TurnMetaProbe`] is
    /// present (production); plain text otherwise. Shared by the pre-request
    /// drain ([`drain_steers`](Self::drain_steers)), the sub-agent blocking-hold
    /// steer arm, and the mid-stream buffer flush
    /// ([`flush_pending_steers`](Self::flush_pending_steers)) — single source for
    /// the observe + enrich + push logic so the three push sites cannot drift.
    /// Sync: [`ChatHistory::push`] is sync and `observe_user_message` /
    /// `enrich_user_text_message` are sync (the lock on the shared working set
    /// is never held across an `await`).
    fn push_steer_message(&self, steer: String, history: &mut dyn ChatHistory) {
        let message = match &self.turn_meta {
            Some(probe) => {
                probe.observe_user_message(&steer);
                probe.enrich_user_text_message(steer)
            }
            None => Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: steer,
                    cache_control: None,
                }],
            },
        };
        history.push(message);
    }

    /// Flush mid-stream-buffered steers into the transcript (observe + enrich +
    /// push). No status — "Steer input queued" was already emitted during
    /// buffering in [`reduce_stream`](Self::reduce_stream). Returns the count
    /// flushed so callers can decide whether to resume the turn. Mirrors
    /// production's `pending_steers.drain(..)` at `turn_loop.rs:1297-1307`
    /// (post-stream no-tools — caller resumes on a fresh step) and
    /// `:2632-2637` (post-tool — caller falls through to the next step).
    fn flush_pending_steers(
        &self,
        pending: &mut Vec<String>,
        history: &mut dyn ChatHistory,
    ) -> usize {
        let count = pending.len();
        for steer in pending.drain(..) {
            self.push_steer_message(steer, history);
        }
        count
    }

    async fn drain_steers(&self, history: &mut dyn ChatHistory) {
        let Some(rx) = &self.steer else {
            return;
        };
        loop {
            // `try_recv` is synchronous and non-blocking — the tokio mutex
            // guard is taken and dropped within this block, never across an
            // `await` (matching the LSP flush pattern). The lock is
            // uncontended (single consumer) so `.await` is effectively instant.
            let steer = {
                let mut guard = rx.lock().await;
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
            self.push_steer_message(steer, history);
            self.emit_status(status).await;
        }
    }

    /// Drain stale steer inputs from a previous (possibly cancelled) turn.
    /// Mirrors production's `while self.rx_steer.try_recv().is_ok() {}` at the
    /// start of `handle_send_message` (`engine/mod.rs:1013-1014`) — a per-turn
    /// reset so steers queued during an interrupted previous turn don't leak
    /// into the new turn. Unlike [`drain_steers`](Self::drain_steers), this
    /// **discards** (does not inject into the transcript): stale steers are not
    /// the user's intent for this turn. Async — the tokio mutex guard is
    /// taken and dropped within the loop, never across another `await` (the
    /// steer receiver migrated from `std::sync::Mutex` to `tokio::sync::Mutex`
    /// in the blocking-hold slice so the `biased select!` steer arm can hold
    /// the guard across `recv().await`).
    ///
    /// **Host-side concern:** the host calls this BEFORE
    /// [`AgentExecutor::run`], not inside the turn loop. Calling it inside
    /// `run_inner` would discard steers the host queued for the current turn
    /// before calling `run`.
    pub async fn drain_stale_steers(&self) {
        let Some(rx) = &self.steer else {
            return;
        };
        let mut guard = rx.lock().await;
        while guard.try_recv().is_ok() {}
    }

    /// Re-inject post-compaction attachment messages (plan / todos /
    /// subagents / read_files) into the transcript DURING `run`, right after
    /// [`run_compaction`] / [`recover_context_overflow`] replace it via
    /// `history.clear()` + `push(result.messages)` (slice 25b §E). Mirrors
    /// production's `Engine::reinject_compaction_attachments` but reads from
    /// the [`ReinjectProbe`] (`plan_state` / `todos` / `recent_read_files`
    /// `Arc` clones) + [`subagent_api`] (`live_running_snapshots`) +
    /// [`turn_meta`] (the `<turn_meta>` enrich block) + [`ChatHistory`] (the
    /// live transcript for dedup / budget trial / push) — the executor has no
    /// `&mut Session` during `run`.
    ///
    /// Per-candidate loop (matches production's slice-24 shape): enrich
    /// FIRST (prepend `<turn_meta>` so byte-stable equality makes dedup
    /// work), then dedup against `history.messages()`, then budget-trial (if
    /// `target_input_budget` is `Some`) against `history.messages()` + the
    /// static `config.system` snapshot (the same system prompt the request
    /// uses), then `history.push`. Each immutable `history.messages()` read
    /// ends (NLL) before the mutable `history.push`, and reads see prior
    /// pushes (live) — matching production's `session.messages` semantics.
    ///
    /// `target_input_budget`: `None` for the auto-compact path (best-effort
    /// dedup + push — auto-compact isn't at a hard ceiling); `Some(budget)`
    /// for the context-overflow recovery path (at the hard ceiling, mirror
    /// production's `Some(target_budget)`). Returns the count pushed.
    async fn reinject_compaction_attachments(
        &self,
        history: &mut dyn ChatHistory,
        target_input_budget: Option<usize>,
    ) -> usize {
        // No probe ⇒ no-op (embeds/tests that don't opt in — matches the
        // absent-probe precedent for the other probes).
        let Some(probe) = &self.reinject else {
            return 0;
        };
        // Plan (`Arc<tokio::Mutex<PlanState>>` — async lock, consistent with
        // production's `self.config.plan_state.lock().await.snapshot()`).
        let plan_snapshot = probe.plan_state.lock().await.snapshot();
        let plan_summary =
            crate::compaction::attachment_reinject::format_plan_reinject_summary(&plan_snapshot);
        // Todos (`Arc<tokio::Mutex<TodoList>>` — async lock).
        let todo_snapshot = probe.todos.lock().await.snapshot();
        let todo_summary =
            crate::compaction::attachment_reinject::format_todo_reinject_summary(&todo_snapshot);
        let mut candidates: Vec<Message> = Vec::new();
        if let Some(message) = crate::compaction::attachment_reinject::reinject_plan_attachment(
            plan_summary.as_deref().unwrap_or(""),
        ) {
            candidates.push(message);
        }
        if let Some(todo_summary) = todo_summary {
            candidates.push(
                crate::compaction::attachment_reinject::compaction_reinject_message(format!(
                    "Active todos resumed after context compaction:\n\n{todo_summary}"
                )),
            );
        }
        // Subagents — the executor already holds a `SubAgentApi` (no probe);
        // `live_running_snapshots` is on the trait.
        if let Some(api) = &self.subagent_api {
            let snapshots = api.live_running_snapshots().await;
            let summaries =
                crate::compaction::attachment_reinject::summarize_subagents(&snapshots);
            if let Some(message) =
                crate::compaction::attachment_reinject::reinject_subagent_attachments(&summaries)
            {
                candidates.push(message);
            }
        }
        // Read files (`Arc<std::sync::Mutex<VecDeque<…>>>` — sync lock,
        // matching `working_set`; held only for the synchronous clone-out).
        let read_files = probe
            .recent_read_files
            .lock()
            .expect("recent_read_files poisoned")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(message) =
            crate::compaction::attachment_reinject::reinject_read_file_attachments(&read_files)
        {
            candidates.push(message);
        }
        // Enrich: prepend the `<turn_meta>` block (slice 24 shape) — built
        // from the TurnMetaProbe's snapshotted state. Absent probe ⇒ no
        // enrich (plain candidates, matching the pre-slice-24 behavior).
        let turn_meta_block = self.turn_meta.as_ref().map(|p| p.turn_metadata_block());
        let mut injected = 0usize;
        for mut candidate in candidates {
            if let Some(block) = &turn_meta_block {
                candidate.content.insert(0, block.clone());
            }
            // Dedup (live transcript).
            if history.messages().iter().any(|message| message == &candidate) {
                continue;
            }
            // Budget trial (only on the recovery path).
            if let Some(budget) = target_input_budget {
                let mut trial = history.messages().to_vec();
                trial.push(candidate.clone());
                if estimate_input_tokens_conservative(&trial, self.config.system.as_ref())
                    > budget
                {
                    continue;
                }
            }
            history.push(candidate);
            injected = injected.saturating_add(1);
        }
        injected
    }

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
    /// Host-coupled follow-ups: `merge_compaction_summary` is absorbed (slice
    /// 25a §E — `result.summary_prompt` is recorded into the
    /// [`pending_compaction_summary`](Self::take_pending_compaction_summary)
    /// slot and the host folds it into `session.system_prompt` post-`run`,
    /// behavior-equivalent to merging mid-`run` since the executor's system
    /// prompt is a static snapshot) **and** `reinject_compaction_attachments`
    /// is absorbed (slice 25b §E — fires right after the transcript replace
    /// via [`Self::reinject_compaction_attachments`] with `None` budget; dedup
    /// + push only — auto-compact isn't at a hard ceiling). Still deferred
    /// (see "Known gaps in compaction" in the module docs):
    /// `post_compact_cleanup` (25c), working-set `external_pins` /
    /// `external_working_set_paths`, and `CompactionEnhancements` (PreCompact
    /// hooks / session-memory-first).
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
                    // Slice 25c §E: signal the host to run
                    // `post_compact_cleanup` post-`run` — a non-merge
                    // compaction just staled the transcript (tool-result
                    // content cleared to placeholders), so the working set
                    // + `last_system_prompt_hash` should be reset for the
                    // next turn. Micro-compact produces no `summary_prompt`,
                    // so this is the closure (NOT the merge path).
                    self.signal_post_compact_cleanup();
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
                // `self.session.messages = result.messages`). Slice 25a §E:
                // `summary_prompt` is now recorded for the host to merge
                // post-`run` (the static `config.system` snapshot can't fold
                // it mid-run — the merge only matters for the next turn's
                // snapshot). Slice 25b §E: `reinject_compaction_attachments`
                // now fires DURING `run` (right here, after the transcript
                // replace) — `None` budget (auto-compact isn't at a hard
                // ceiling; dedup + push only). Slice 25c §E: this is the *merge*
                // path, so the cleanup signal is intentionally NOT set here
                // (full→summary XOR: `post_compact_cleanup` fires only on the
                // non-merge arms — pre-request micro / recovery micro /
                // hard-trim). `emit_session_updated` remains deferred until the
                // host-side post-`run` closure (fires alongside the merge).
                self.record_compaction_summary(result.summary_prompt.clone());
                history.clear();
                for m in result.messages {
                    history.push(m);
                }
                self.reinject_compaction_attachments(history, None).await;
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
    /// Absorbed (mirroring `run_compaction`): `merge_compaction_summary` +
    /// `emit_session_updated` (slice 25a §E — `result.summary_prompt` is
    /// recorded into the
    /// [`pending_compaction_summary`](Self::take_pending_compaction_summary)
    /// slot; the host folds the summary into `session.system_prompt` and
    /// emits a UI refresh post-`run`, behavior-equivalent to merging mid-`run`
    /// since the executor's system prompt is a static snapshot) **and**
    /// `reinject_compaction_attachments` (slice 25b §E — fires right after
    /// the transcript replace with `Some(target_budget)`; dedup + budget
    /// trial + push). Still deferred (same gaps as the compaction slice):
    /// `post_compact_cleanup` (25c), `CompactionEnhancements`.
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
                    // Slice 25c §E: signal the host to run
                    // `post_compact_cleanup` post-`run` — recovery micro
                    // changed the transcript (no `summary_prompt` produced),
                    // so this is the cleanup closure, NOT the merge path.
                    // Mirrors production's recovery-micro → `post_compact_cleanup`
                    // (the `#[allow(dead_code)]` `Engine::recover_context_overflow`
                    // micro arm at mod.rs:1850).
                    self.signal_post_compact_cleanup();
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
                // `self.session.messages = result.messages`). Slice 25a §E:
                // `summary_prompt` is now recorded for the host to merge
                // post-`run` (the static `config.system` snapshot can't fold
                // it mid-run — the merge only matters for the next turn's
                // snapshot). Slice 25b §E: `reinject_compaction_attachments`
                // now fires DURING `run` (right here) — `Some(target_budget)`
                // (at the hard ceiling, mirror production's
                // `Some(target_budget)`; dedup + budget trial + push). Slice
                // 25c §E: this is the *merge* path, so the cleanup signal is
                // intentionally NOT set here (full→summary XOR:
                // `post_compact_cleanup` fires only on the non-merge arms —
                // pre-request micro / recovery micro / hard-trim).
                // `emit_session_updated` remains deferred until the host-side
                // post-`run` closure (fires alongside the merge).
                if !result.messages.is_empty() || messages.is_empty() {
                    history.clear();
                    for m in result.messages {
                        history.push(m);
                    }
                }
                // summary_prompt recorded for post-`run` merge — slice 25a §E.
                self.record_compaction_summary(result.summary_prompt.clone());
                self.reinject_compaction_attachments(history, Some(target_budget))
                    .await;
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
            let before_trim = msgs.len();
            while msgs.len() > MIN_RECENT_MESSAGES_TO_KEEP
                && estimate_input_tokens_conservative(&msgs, system) > target_budget
            {
                msgs.remove(0);
            }
            // Capture before the `for` loop consumes `msgs`.
            let trimmed = before_trim > msgs.len();
            history.clear();
            for m in msgs {
                history.push(m);
            }
            // Slice 25c §E: hard-trim is a non-merge compaction — it changes
            // the transcript (oldest messages removed) without producing a
            // `summary_prompt`, so signal the host to run
            // `post_compact_cleanup` post-`run` (force-rebuild working set +
            // reset `last_system_prompt_hash`). Only fires when the trim
            // actually removed messages (bounded by
            // `MIN_RECENT_MESSAGES_TO_KEEP`); a no-op trim (already at the
            // keep-recent floor) leaves the transcript untouched. If Phase
            // 2 full compaction also recorded a summary here (rare:
            // compaction succeeded but didn't free enough budget), both
            // signals fire — the host does merge THEN cleanup (the
            // production `partial→both` analog).
            if trimmed {
                self.signal_post_compact_cleanup();
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
        // Per-input approval override (slice 20 §E): when a host dispatcher is
        // attached, consult its `approval_requirement_for` first — a `Some`
        // answer downgrades/upgrades the gate per input (mirrors production's
        // `registry.approval_requirement_for(..)` at turn_loop.rs:1700). `None`
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
        // NOTE: the steer stale-drain (`drain_stale_steers`) is a host-side
        // concern — production runs it in `handle_send_message` BEFORE
        // `handle_deepseek_turn`. Calling it inside `run_inner` would discard
        // steers the host queued for THIS turn before calling `run`. It is a
        // `pub` method the host calls before `run` at the wire-in step.
        loop {
            // Checkpoint A — loop-top cancel gate (mirrors
            // `turn_loop.rs:294-298`). First thing every iteration: if the
            // turn was cancelled, surface `Interrupted` and bail. This also
            // bounds all `continue` loops (capacity `RetryStep`, reactive
            // `RecoveredContextOverflow`, subagent resume) — a cancel that
            // landed during recovery is caught here before the next step.
            if self.is_cancelled() {
                self.emit_status("Request cancelled".to_string()).await;
                callback.on_complete(&StopReason::Interrupted).await;
                return Ok(StopReason::Interrupted);
            }
            // (1) per-step pre-request seam — ✅ steer drain (queued user
            // inputs injected before the request snapshot); ✅ compaction
            // (micro-compact + LLM-summary auto-compact, runs after steer and
            // before the LSP flush so a fresh diagnostic message survives
            // compaction); ✅ capacity preflight (hard token-budget gate +
            // emergency recovery, runs after compaction and before the LSP
            // flush so the estimate reflects the just-compacted transcript);
            // ✅ LSP flush (drain pending diagnostics into a synthetic user
            // message); cycle (checkpoint-restart) is a post-turn Session-level
            // concern (lives in `handle_send_message` after the turn returns), so
            // it lands at the wire-in step, not in this in-loop seam.
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
            // ✅ mid-stream steer buffer (`reduce_stream` `try_recv`s into
            // `pending_steers` during streaming; flushed post-stream / post-tool
            // below). `on_llm_start` fires once per step; retries and recovery
            // are transparent to the Callback. Subagent handoff / thinking-only
            // handling land here later.
            //
            // `early_tasks` is per-step: the inline reducer populates it during
            // streaming (spawning read-only tools at `ContentBlockStop`), and
            // the tool loop below consumes it (reusing / aborting the speculatively-
            // started tasks). On a `continue` (capacity `RetryStep` / reactive
            // `RecoveredContextOverflow`) the stream either never opened or died
            // before any content ⇒ no `ContentBlockStop` ⇒ the map is empty, so
            // dropping it leaks nothing.
            let mut early_tasks: HashMap<String, EarlyToolTask> = HashMap::new();
            // `pending_steers` is per-step: the inline reducer buffers steers
            // that arrive during streaming (mirrors `turn_loop.rs:683` declaring
            // `pending_steers` before the stream loop). Flushed post-stream /
            // post-tool below. On a `continue` (capacity `RetryStep` / reactive
            // `RecoveredContextOverflow`) the stream either never opened or died
            // before any content ⇒ no `try_recv` ran in `reduce_stream` ⇒ the
            // vec is empty, so dropping it leaks nothing.
            let mut pending_steers: Vec<String> = Vec::new();
            let (content, _stop_reason) = match self
                .stream_with_transparent_retry(
                    &client,
                    request,
                    &mut stream_retry_attempts,
                    &mut context_recovery_attempts,
                    history,
                    system.as_ref(),
                    &mut early_tasks,
                    &mut pending_steers,
                )
                .await
            {
                Ok(StreamRoundOutcome::Content { content, stop_reason, usage }) => {
                    // Add this stream's usage to the per-turn total — the
                    // end-of-stream handoff the retired `turn_loop.rs:1193`
                    // (`turn.add_usage(&usage)`) used to do inline; the host
                    // now reads it back via `take_usage` after `run` returns.
                    self.accumulate_usage(&usage);
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
                Ok(StreamRoundOutcome::Interrupted) => {
                    // The turn was cancelled during the stream phase
                    // (Checkpoint B/C/D inside `stream_with_transparent_retry`).
                    // Surface `Interrupted` — mirrors production's
                    // `TurnOutcomeStatus::Interrupted`.
                    self.emit_status("Request cancelled".to_string()).await;
                    callback.on_complete(&StopReason::Interrupted).await;
                    return Ok(StopReason::Interrupted);
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
                // Mid-stream steer flush (mirrors `turn_loop.rs:1297-1307`):
                // if steers arrived during streaming, flush them and resume the
                // turn on a fresh step BEFORE checking for sub-agent
                // completions — production checks `pending_steers` first.
                let flushed = self.flush_pending_steers(&mut pending_steers, history);
                if flushed > 0 {
                    step += 1;
                    continue;
                }
                // Sub-agent completion handoff (seam 2 post-stream resume).
                // Mirrors `handle_deepseek_turn`'s non-blocking completion drain
                // (`turn_loop.rs:1317-1397` + the late drain at `1501-1532`):
                // when the model finishes a step with no tool calls, surface any
                // child completions that arrived (queued during inference or
                // between turns) as `<codesmith:runtime_event
                // kind="subagent_completion">` user messages and resume the turn
                // instead of ending it — fulfilling the sentinel contract the
                // model was promised in `prompts/base.md`. The executor has no
                // thinking-only / goal-continuation / REPL branches, so this single
                // drain covers both production drain sites. The **blocking hold**
                // for still-running children (`should_hold_turn_for_subagents` +
                // a `biased select!` over cancel / completion `recv().await` /
                // steer `recv().await`) is absorbed ✅ — it fires when the
                // non-blocking drain found nothing but children are still
                // running (needs `subagent_api::running_count` + the
                // `subagent` receiver; absent either, the hold is skipped and
                // the turn ends on `NoToolCalls`).
                if let Some(probe) = &self.subagent {
                    let mut completions: Vec<SubAgentCompletion> = Vec::new();
                    // Non-blocking drain (`try_recv`). `tokio::sync::Mutex` —
                    // the lock is held only for the synchronous `try_recv`,
                    // never across the `history.push` `await` below (matches
                    // the steer drain pattern).
                    {
                        let mut rx = probe.lock().await;
                        while let Ok(c) = rx.try_recv() {
                            completions.push(c);
                        }
                    }
                    // Blocking hold (mirrors `turn_loop.rs:1321-1397`): when
                    // the non-blocking drain found nothing but children are
                    // still running, block on a `biased select!` over cancel
                    // / completion `recv().await` / steer `recv().await` until
                    // a child completes, the turn is cancelled, or the user
                    // steers. Needs `subagent_api` (for `running_count`) and
                    // the subagent receiver; absent `subagent_api`, skip the
                    // hold and fall through to `NoToolCalls`.
                    if completions.is_empty()
                        && let Some(api) = &self.subagent_api
                    {
                        let running = api.running_count().await;
                        if should_hold_turn_for_subagents(0, running) {
                            self.emit_status(format!(
                                "Waiting on {running} sub-agent(s) to complete..."
                            ))
                            .await;
                            // Clone the Arc so the lock guard borrows a
                            // local, leaving `self.steer` freely accessible in
                            // the steer arm (mirrors `let cancel_token =
                            // self.cancel_token.clone()` in the approval
                            // race). The lock is uncontended (single
                            // consumer) and held across `recv().await` only
                            // inside the `select!` below.
                            let sub_arc = Arc::clone(probe);
                            let mut sub_guard = sub_arc.lock().await;
                            let cancel_token = self.cancel_token.clone();
                            // Checkpoint E — sub-agent blocking-hold cancel
                            // race (mirrors `turn_loop.rs:1330-1339`).
                            // `biased` so cancel wins if both are ready. The
                            // cancel arm `return`s, the steer arm `continue`s
                            // — only the completion arm falls through to the
                            // `try_recv` drain + inject+resume path below.
                            tokio::select! {
                                biased;
                                _ = async {
                                    match &cancel_token {
                                        Some(token) => token.cancelled().await,
                                        None => std::future::pending::<()>().await,
                                    }
                                } => {
                                    self.emit_status(
                                        "Request cancelled while waiting for sub-agents"
                                            .to_string(),
                                    )
                                    .await;
                                    callback
                                        .on_complete(&StopReason::Interrupted)
                                        .await;
                                    return Ok(StopReason::Interrupted);
                                }
                                Some(c) = sub_guard.recv() => {
                                    completions.push(c);
                                }
                                Some(steer) = async {
                                    match &self.steer {
                                        Some(rx) => rx.lock().await.recv().await,
                                        None => {
                                            std::future::pending::<Option<String>>().await
                                        }
                                    }
                                } => {
                                    // Steer arm: inject the steered text as a
                                    // user message and resume the turn on a
                                    // fresh step (mirrors
                                    // `turn_loop.rs:1352-1368` + the
                                    // post-stream resume at `1297-1307`).
                                    // Closes the "steer post-stream resume"
                                    // gap — steers that arrive during the
                                    // hold are now surfaced immediately
                                    // rather than waiting for the next step's
                                    // pre-request drain.
                                    let trimmed = steer.trim().to_string();
                                    if !trimmed.is_empty() {
                                        let status = format!(
                                            "Steer input accepted: {}",
                                            summarize_text(&trimmed, 120)
                                        );
                                        // Same observe + enrich as the
                                        // pre-request drain — a steer arriving
                                        // during the sub-agent hold is recorded
                                        // against the working set and wrapped in
                                        // `<turn_meta>` when the probe is present
                                        // (production); plain text otherwise.
                                        self.push_steer_message(trimmed, history);
                                        self.emit_status(status).await;
                                    }
                                    step += 1;
                                    continue;
                                }
                            }
                            // Only reached if the completion arm won (cancel
                            // `return`ed, steer `continue`d). The `recv()`
                            // future was consumed, releasing the borrow on
                            // `sub_guard` — drain any completions batched
                            // behind the first (mirrors
                            // `turn_loop.rs:1342-1345`).
                            while let Ok(extra) = sub_guard.try_recv() {
                                completions.push(extra);
                            }
                        }
                    }
                    if !completions.is_empty() {
                        let count = completions.len();
                        for c in completions {
                            history.push(subagent_completion_runtime_message(&c.payload));
                            // `ContextPatch` apply (tighten-only
                            // `auto_approve`/`trust_mode`) is deferred — it
                            // mutates `Session` state not reachable through
                            // `ChatHistory`, and production hardcodes
                            // `context_patch: None` today (same gap class
                            // as compaction's working-set / cycle-state
                            // reinject).
                        }
                        self.emit_status(format!(
                            "Resuming turn with {count} sub-agent completion(s)"
                        ))
                        .await;
                        // A subagent resume is a new step (production's
                        // `turn.next_step()`), unlike the capacity/reactive
                        // `continue`s which retry the *same* step — so the
                        // `max_steps` bound still covers a chain of
                        // completions.
                        step += 1;
                        continue;
                    }
                }
                callback.on_complete(&StopReason::NoToolCalls).await;
                return Ok(StopReason::NoToolCalls);
            }

            // Execute each tool sequentially and feed the result back as a
            // `role:"user"` `ToolResult` block (Anthropic/OpenAI-compat shape).
            //
            // (3) per-tool seam — ✅ loop-guard; ✅ approval; ✅ early-tool-start
            // (reuse a speculatively-started task spawned at `ContentBlockStop`
            // during streaming if the args still match; otherwise abort + run
            // fresh); ✅ LSP post-edit collect. `loop_guard_halt` is per-step: a
            // halt short-circuits the tool loop and the whole turn at the (4)
            // seam below. Parallel dispatch lands here later.
            let mut loop_guard_halt: Option<String> = None;
            for (id, name, input) in tool_uses {
                callback.on_tool_start(&id, &name, &input).await;
                // loop-guard: block the 3rd identical (name+args) call this turn.
                let (result, blocked) = match loop_guard.record_attempt(&name, &input) {
                    AttemptDecision::Block(message) => {
                        // Abort any speculatively-started task — the call
                        // won't execute (Drop aborts the `JoinHandle`).
                        early_tasks.remove(&id);
                        (Ok(block_tool_result(message)), true)
                    }
                    AttemptDecision::Proceed => {
                        let result = match tools.get(&name) {
                            // approval gate: a tool that requires approval is
                            // gated behind the decision channel; denied ⇒ the
                            // tool never runs and a `permission_denied` error
                            // is fed back so the model can react (turn
                            // continues). Order: loop-guard first (matches
                            // production), then approval, then early-start reuse.
                            Some(tool) => {
                                match self
                                    .request_approval(&id, &name, &input, tool, &intent_summary)
                                    .await
                                {
                                    Ok(()) => {
                                        // Early-tool-start: reuse a
                                        // speculatively-started task (spawned
                                        // at `ContentBlockStop` during
                                        // streaming) if one exists for this
                                        // id and the model didn't revise the
                                        // args after the block closed;
                                        // otherwise run the tool fresh (mirrors
                                        // `turn_loop`'s
                                        // `early_task.filter(|t| t.name == name && t.input == input)`).
                                        match early_tasks.remove(&id) {
                                            Some(mut early)
                                                if early.name == name
                                                    && early.input == input =>
                                            {
                                                // Take the handle out so `Drop`
                                                // on the `EarlyToolTask`
                                                // doesn't abort it (the field
                                                // becomes `None`).
                                                let handle = early
                                                    .handle
                                                    .take()
                                                    .expect("handle present until consumed");
                                                match handle.await {
                                                    Ok(result) => result,
                                                    Err(join_err) => {
                                                        Err(ToolError::execution_failed(
                                                            format!(
                                                                "Early tool execution task failed: {join_err}"
                                                            ),
                                                        ))
                                                    }
                                                }
                                            }
                                            Some(_revised) => {
                                                // Args revised after the block
                                                // closed — can't reuse; the
                                                // dropped `EarlyToolTask`
                                                // (Drop aborts) cleans up the
                                                // orphaned speculative task.
                                                tool.run(input.clone()).await
                                            }
                                            None => tool.run(input.clone()).await,
                                        }
                                    }
                                    Err(denial) => {
                                        // Approval denied — abort any
                                        // speculative task (defensive:
                                        // early-start-safe tools don't
                                        // require approval, so this path has
                                        // none, but `Drop` is cheap).
                                        early_tasks.remove(&id);
                                        Err(ToolError::permission_denied(denial))
                                    }
                                }
                            }
                            None => {
                                // No tool registered — abort any speculative
                                // task (`reduce_stream` only spawns for
                                // registered tools, so this is defensive).
                                early_tasks.remove(&id);
                                Err(ToolError::NotAvailable {
                                    message: format!("no tool named '{name}'"),
                                })
                            }
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
                // production `output.success && tool_was_executed`); ✅ read_file
                // observe (records the compacted/sanitized output into
                // `recent_read_files` for post-compaction reinject); approval /
                // early-tool-start / parallel land here later.
                if !blocked {
                    if let Ok(r) = &result {
                        if r.success {
                            self.collect_lsp_diagnostics(&name, &input).await;
                            self.record_read_file_result(&name, &input, r);
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

            // Abort any speculatively-started tasks that weren't consumed by
            // the tool loop (e.g. a tool block completed during streaming but
            // didn't survive into the parsed `tool_uses`, or an args-mismatch
            // path left an orphaned task). `Drop` on each `EarlyToolTask`
            // aborts the spawned task. In normal operation every spawned task
            // is consumed or aborted within the loop, so this is defensive.
            early_tasks.clear();

            // Mid-stream steer flush (mirrors `turn_loop.rs:2632-2637`): push
            // any steers that arrived during streaming into the transcript
            // before the next step. Steers that arrive during the last step
            // before `MaxSteps` would otherwise be discarded by the next turn's
            // stale drain.
            self.flush_pending_steers(&mut pending_steers, history);

            // (4) per-step post-tool seam — ✅ cancel-token (Checkpoint G:
            // post-loop final gate — cancel takes priority over loop-guard
            // halt, mirroring `turn_loop.rs:2665-2671` where cancel is
            // checked before `turn_error`); ✅ loop-guard halt; capacity
            // post-tool checkpoint (opt-in `CapacityController` Gate A +
            // error-escalation) still to come; cycle (checkpoint-restart) is a
            // post-turn concern deferred to the wire-in step. The hard
            // token-budget preflight (Gate B) is absorbed at seam (1).
            if self.is_cancelled() {
                self.emit_status("Request cancelled".to_string()).await;
                callback.on_complete(&StopReason::Interrupted).await;
                return Ok(StopReason::Interrupted);
            }
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
    use crate::host_services::SubAgentApi;
    use crate::lsp_config::LspConfig;
    use crate::lsp_diagnostics::{Diagnostic, DiagnosticBlock, Severity};
    use crate::session::Session;
    use crate::session_history::SessionChatHistory;
    use crate::subagent::SubAgentResult;
    use crate::tool_state::plan::{
        new_shared_plan_state, PlanItemArg, SharedPlanState, StepStatus, UpdatePlanArgs,
    };
    use crate::tool_state::todo::{new_shared_todo_list, SharedTodoList, TodoStatus};
    use crate::tools::registry::ToolRegistry;
    use crate::tools::spec::{ToolContext, ToolSpec};
    use codesmith_agent::llm_client::{LlmClient, StreamEventBox};
    use codesmith_agent::models::{
        ContentBlockStart, Delta, MessageDelta, MessageResponse, StreamEvent, SystemBlock, Usage,
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

    /// A `ToolSpec` standing in for `read_file`: returns a configurable content
    /// + success flag so the §E read_file observe seam (keyed on tool name
    /// `read_file` + `output.success`) can be exercised. The `path` input field
    /// flows through to `record_read_file_result` (the observe dedup key).
    struct ReadFileSpec {
        content: String,
        success: bool,
    }

    impl ReadFileSpec {
        fn new(content: &str) -> Self {
            Self {
                content: content.to_string(),
                success: true,
            }
        }
        fn failing(content: &str) -> Self {
            Self {
                content: content.to_string(),
                success: false,
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolSpec for ReadFileSpec {
        fn name(&self) -> &str {
            "read_file"
        }
        fn description(&self) -> &str {
            "Reads a file at `path`; used to drive the read_file observe seam."
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
            _input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                content: self.content.clone(),
                success: self.success,
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
        /// §E cancel-token test hook: when set, `create_message_stream`
        /// cancels this token as a side-effect when the stream opens. Taken
        /// (fired once) so only the first stream call triggers it. This lets
        /// cancel-checkpoint tests (C/D) fire deterministically — the token
        /// is cancelled by the time `reduce_stream` runs, but the stream
        /// still opened (so Checkpoint B's stream-open race doesn't win).
        cancel_on_stream: Mutex<Option<CancellationToken>>,
        /// §E mid-stream-steer test hook: when set, `create_message_stream`
        /// pushes this steer text to the channel as a side-effect when the
        /// stream opens (inside the async block, after the pre-request
        /// `drain_steers` has already run). The first `try_recv` in
        /// `reduce_stream` catches it — simulating a steer arriving *during*
        /// streaming. Taken (fired once) so only the first stream call triggers
        /// it. Uses `try_send` (sync, non-blocking) so no `.await` is needed in
        /// the async block.
        steer_on_stream: Mutex<Option<(mpsc::Sender<String>, String)>>,
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
                cancel_on_stream: Mutex::new(None),
                steer_on_stream: Mutex::new(None),
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

        /// §E cancel-token test hook: cancel `token` as a side-effect when the
        /// next `create_message_stream` opens. Fired once (taken). Lets
        /// cancel-checkpoint tests prove the token is cancelled by the time
        /// `reduce_stream` runs — Checkpoint C (Empty arm) or D (Complete arm)
        /// catches it — without Checkpoint B winning the stream-open race.
        fn with_cancel_on_stream(self, token: CancellationToken) -> Self {
            *self.cancel_on_stream.lock().unwrap() = Some(token);
            self
        }

        /// §E mid-stream-steer test hook: push `text` to `tx` as a side-effect
        /// when the next `create_message_stream` opens (inside the async block,
        /// after the pre-request `drain_steers` has already drained the channel).
        /// Fired once (taken). Lets mid-stream-steer tests prove the steer is
        /// buffered by `reduce_stream`'s `try_recv` and flushed post-stream /
        /// post-tool — not caught by the pre-request drain.
        fn with_steer_on_stream(self, tx: mpsc::Sender<String>, text: String) -> Self {
            *self.steer_on_stream.lock().unwrap() = Some((tx, text));
            self
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
            // §E cancel-token test hook: take the token (if set) here so the
            // lock doesn't cross an await, but cancel it INSIDE the async
            // block. The cancel future (Checkpoint B) is polled first (biased)
            // and found pending — the token isn't cancelled yet. Then this
            // async block runs, cancels the token, and returns the stream.
            // Checkpoint B doesn't win the race; the cancel is caught at
            // Checkpoint C (Empty arm) or D (Complete arm) in `reduce_stream`'s
            // outcome — which is what these tests prove.
            let cancel_token = self.cancel_on_stream.lock().unwrap().take();
            // §E mid-stream-steer test hook: take the pair here so the lock
            // doesn't cross an await, but push the steer INSIDE the async block
            // (after the pre-request `drain_steers` has already run, so the
            // steer is not caught by it — the first `try_recv` in
            // `reduce_stream` catches it instead).
            let steer_pair = self.steer_on_stream.lock().unwrap().take();
            Box::pin(async move {
                if let Some(token) = cancel_token {
                    token.cancel();
                }
                if let Some((tx, text)) = steer_pair {
                    let _ = tx.try_send(text);
                }
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

    /// A `SubAgentApi` test double with a configurable sequence of
    /// `running_count` values (all other methods are no-ops). Each call to
    /// `running_count` pops the front of the sequence; when empty it returns 0.
    /// This lets a test say "one running child on the first poll (hold fires),
    /// then zero on the next (hold skipped)" — e.g. `FakeSubAgentApi::new(vec![1])`
    /// fires the hold once then stops, simulating a child completing.
    struct FakeSubAgentApi {
        counts: std::sync::Mutex<VecDeque<usize>>,
    }

    impl FakeSubAgentApi {
        fn new(counts: Vec<usize>) -> Arc<Self> {
            Arc::new(Self {
                counts: std::sync::Mutex::new(counts.into_iter().collect()),
            })
        }
    }

    #[async_trait::async_trait]
    impl SubAgentApi for FakeSubAgentApi {
        async fn running_count(&self) -> usize {
            self.counts.lock().unwrap().pop_front().unwrap_or(0)
        }
        async fn list(&self) -> Vec<SubAgentResult> {
            Vec::new()
        }
        async fn cleanup(&self, _max_age: std::time::Duration) {}
        async fn live_running_snapshots(&self) -> Vec<SubAgentResult> {
            Vec::new()
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

    /// A `MessageStart` carrying `input_tokens` — mirrors production, where the
    /// stream's first event seeds per-stream usage (`reduce_stream`'s
    /// `MessageStart` arm does `usage = message.usage;`, a REPLACE). Other
    /// `MessageResponse` fields are canned; only `usage` flows into
    /// `reduce_stream`. (slice 21 §E usage-tracking tests.)
    fn message_start_with_usage(input_tokens: u32) -> StreamEvent {
        StreamEvent::MessageStart {
            message: MessageResponse {
                id: "usage-start".to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content: Vec::new(),
                model: "mock-v0".to_string(),
                stop_reason: None,
                stop_sequence: None,
                container: None,
                usage: Usage {
                    input_tokens,
                    output_tokens: 0,
                    ..Usage::default()
                },
            },
        }
    }

    /// Like `finish` but the trailing `MessageDelta` carries cumulative usage
    /// — the per-stream usage the provider sends at stream end (`reduce_stream`'s
    /// `MessageDelta` arm REPLACE: the whole `Usage` is replaced, latest wins).
    /// The `input_tokens` is cumulative (re-sent by the provider alongside
    /// `output_tokens`), so a stream that opened with `MessageStart(input)`
    /// still reports that input after the delta replaces it. (slice 21 §E.)
    fn finish_with_usage(stop: &str, input_tokens: u32, output_tokens: u32) -> Vec<StreamEvent> {
        vec![
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(stop.to_string()),
                    stop_sequence: None,
                },
                usage: Some(Usage {
                    input_tokens,
                    output_tokens,
                    ..Usage::default()
                }),
            },
            StreamEvent::MessageStop,
        ]
    }

    /// A clean single-stream round carrying usage: `MessageStart(input)` + a
    /// text block + `MessageDelta(usage input,output)` + `MessageStop`. Composes
    /// [`message_start_with_usage`] + [`text_block`] + [`finish_with_usage`].
    /// The delta repeats `input` (cumulative) so the round reports
    /// `{input_tokens: input, output_tokens: output}` after reduction.
    /// (slice 21 §E usage-tracking tests.)
    fn usage_round(input: u32, output: u32) -> Vec<StreamEvent> {
        let mut call = vec![message_start_with_usage(input)];
        call.extend(text_block(0, "answer"));
        call.extend(finish_with_usage("end_turn", input, output));
        call
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
    // receiver is `Option<Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>>` —
    // interior-mutable because `AgentExecutor::run` is `&self` while
    // `try_recv`/`recv` takes `&mut self`. The pre-request drain is absorbed
    // (seam 1, non-blocking `try_recv`); the post-stream resume / blocking
    // `recv().await` during the sub-agent hold are absorbed in the blocking-hold
    // slice (the `biased select!` steer arm).

    /// Create a steer channel pair: the sender for tests to enqueue steers, and
    /// the interior-mutable receiver the executor expects. Uses
    /// `tokio::sync::Mutex` (matching `approval_channel`) so the guard may
    /// cross the blocking `recv().await` in the sub-agent blocking hold's
    /// `biased select!` steer arm.
    fn steer_channel() -> (
        mpsc::Sender<String>,
        Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>,
    ) {
        let (tx, rx) = mpsc::channel::<String>(64);
        (tx, Arc::new(tokio::sync::Mutex::new(rx)))
    }

    /// Create an approval channel pair: the sender for tests to push
    /// `ApprovalDecision`s (matched by wire tool id), and the interior-mutable
    /// receiver the executor expects. Uses `tokio::sync::Mutex` because the
    /// approval await blocks on `recv().await` — the guard must cross an
    /// `await`.
    fn approval_channel() -> (
        mpsc::Sender<ApprovalDecision>,
        Arc<tokio::sync::Mutex<mpsc::Receiver<ApprovalDecision>>>,
    ) {
        let (tx, rx) = mpsc::channel::<ApprovalDecision>(64);
        (tx, Arc::new(tokio::sync::Mutex::new(rx)))
    }

    /// Create a sub-agent completion channel pair: the sender for tests to push
    /// [`SubAgentCompletion`]s, and the interior-mutable receiver the executor
    /// expects. Uses `tokio::sync::Mutex` (matching `steer_channel` /
    /// `approval_channel`) so the guard may cross the blocking `recv().await`
    /// in the blocking hold's `biased select!` completion arm. The
    /// non-blocking `try_recv` drain holds the lock only synchronously
    /// (uncontended single consumer).
    fn subagent_channel() -> (
        mpsc::UnboundedSender<SubAgentCompletion>,
        Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<SubAgentCompletion>>>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel::<SubAgentCompletion>();
        (tx, Arc::new(tokio::sync::Mutex::new(rx)))
    }

    /// Build a `SubAgentCompletion` with a sentinel payload mirroring the
    /// `emit_parent_completion` wire shape (human summary line 1, sentinel
    /// line 2). `context_patch` is `None` — matching production today.
    fn completion(summary: &str) -> SubAgentCompletion {
        SubAgentCompletion {
            agent_id: "test-agent".to_string(),
            payload: format!(
                "{summary}\n<codesmith:subagent.done>{{\"agent_id\":\"test-agent\"}}</codesmith:subagent.done>"
            ),
            context_patch: None,
        }
    }

    /// True if any block of any message is a `Text` block containing the
    /// sub-agent completion sentinel (`<codesmith:runtime_event kind="subagent_completion">`).
    fn has_subagent_completion_msg(messages: &[Message]) -> bool {
        messages.iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text
                    .contains("kind=\"subagent_completion\""))
            })
        })
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

    /// Build a `ReadFileSpec` tool registry → framework `ToolSet`. The spec is
    /// configured with the given content + success flag so the observe seam can
    /// test both happy and failed paths.
    fn read_file_tools(spec: ReadFileSpec) -> Arc<ToolSet> {
        let mut registry = ToolRegistry::new(ToolContext::new(PathBuf::from("/tmp/ws")));
        registry.register(Arc::new(spec));
        Arc::new(registry.to_framework_tool_set())
    }

    /// Round 1: the model calls `read_file` (id `call_1`) with a `path` input.
    fn read_file_call(path: &str) -> Vec<StreamEvent> {
        let input = format!(r#"{{"path":"{path}"}}"#);
        let mut call = text_block(0, "reading the file now");
        call.extend(tool_use_block(1, "call_1", "read_file", &input));
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
            None,
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
            None,
            None,
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
            None,
            None,
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
            None,
            None,
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
            None,
            None,
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

    // === compaction summary_prompt slot (slice 25a §E) ====================

    /// Flatten a [`SystemPrompt`] to its concatenated text so the
    /// summary-prompt-slot tests can assert on the LLM summary content without
    /// caring whether `compact_messages_safe` produced `Text` or `Blocks`.
    fn system_prompt_text(sp: &SystemPrompt) -> String {
        match sp {
            SystemPrompt::Text(t) => t.clone(),
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// Slice 25a §E: a `run` that triggers Phase-2 auto-compaction records
    /// `result.summary_prompt` into the `pending_compaction_summary` slot (the
    /// host reads it via [`HostAgentExecutor::take_pending_compaction_summary`]
    /// post-`run`). Mirrors `auto_compact_summarizes_when_over_threshold` but
    /// asserts the seam hand-off (not just `compaction_calls()`).
    #[tokio::test]
    async fn run_compaction_records_summary_prompt() {
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
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "continue".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 1);
        // Slice 25a §E: the summary_prompt is recorded for the host to merge
        // post-`run` (no longer discarded — the "Known gaps" closure).
        let summary = executor
            .take_pending_compaction_summary()
            .expect("summary_prompt recorded by run_compaction");
        assert!(
            system_prompt_text(&summary).contains("Conversation summary."),
            "recorded summary reflects the LLM compaction summary"
        );
        // One-shot drain (mirrors `take_usage`): a second read yields `None`.
        assert!(executor.take_pending_compaction_summary().is_none());
    }

    /// Slice 25a §E: a `run` that triggers `recover_context_overflow`
    /// (reactive context-length recovery) records the summary_prompt too — the
    /// same slot covers both `run_compaction` and `recover` paths. The slot
    /// being non-`None` is exactly the hand-off the host's post-`run` merge
    /// reads (`mod.rs`, after `take_usage`); the fold itself lives on `Engine`
    /// (`merge_compaction_summary`), so this test asserts the seam, not the fold.
    #[tokio::test]
    async fn compaction_summary_flows_to_host_merge_post_run() {
        let mut sess = fresh_session();
        // 10 text messages (~750 tokens) ≪ the 3072 Ollama/llama2 budget, so
        // the preflight (seam 1) proceeds; the provider "rejects" with a
        // context-length error, exercising the reactive seam-2 path.
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
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run should succeed after reactive recovery");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 1);
        // The recover path recorded the summary_prompt — the host's post-`run`
        // `take_pending_compaction_summary` + `merge_compaction_summary` wiring
        // will fold it into `session.system_prompt`.
        let summary = executor
            .take_pending_compaction_summary()
            .expect("summary_prompt recorded by recover_context_overflow");
        assert!(
            system_prompt_text(&summary).contains("SUMMARY"),
            "recorded summary reflects the LLM compaction summary"
        );
    }

    /// Slice 25a §E: a turn may compact multiple times; the slot accumulates
    /// (folds each via `crate::compaction::merge_system_prompts`, mirroring
    /// production's `merge_compaction_summary` folding each into
    /// `session.compaction_summary_prompt`) rather than last-wins. Coaxing two
    /// real compactions in one `run` is fragile, so this drives
    /// `record_compaction_summary` twice directly to guard the fold.
    #[tokio::test]
    async fn multiple_compactions_accumulate_summary() {
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
            None,
            None,
            None,
        );
        // Two `summary_prompt`s as `compact_messages_safe` produces them
        // (`SystemPrompt::Blocks` with one summary block each).
        let summary_a = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "first compaction summary".to_string(),
            cache_control: None,
        }]);
        let summary_b = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "second compaction summary".to_string(),
            cache_control: None,
        }]);
        executor.record_compaction_summary(Some(summary_a));
        executor.record_compaction_summary(Some(summary_b));
        let merged = executor
            .take_pending_compaction_summary()
            .expect("accumulated merge present");
        let text = system_prompt_text(&merged);
        // Both summaries survive the in-slot fold (not last-wins).
        assert!(
            text.contains("first compaction summary"),
            "first summary preserved by accumulation: {text}"
        );
        assert!(
            text.contains("second compaction summary"),
            "second summary folded in by accumulation: {text}"
        );
    }

    /// Slice 25a §E: a clean run (no compaction) leaves the slot `None`, so the
    /// host's post-`run` `take_pending_compaction_summary` is a no-op (the
    /// `if let Some(summary)` branch in `mod.rs` is skipped — no spurious
    /// `merge_compaction_summary` / `emit_session_updated`).
    #[tokio::test]
    async fn no_compaction_yields_none_summary() {
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
            // High threshold ⇒ no Phase-2 auto-compact; micro-compact produces
            // no `summary_prompt` (only the LLM-summary path does).
            Some(CompactionProbe::new(
                compaction_config_high_threshold(),
                PathBuf::from("/tmp/codesmith-test"),
            )),
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
        assert_eq!(mock.compaction_calls(), 0);
        assert!(executor.take_pending_compaction_summary().is_none());
    }

    // === post-compact cleanup signal (slice 25c §E) =====================

    /// Slice 25c §E: a clean run (no compaction) leaves the cleanup slot
    /// `false`, so the host's post-`run` `take_pending_post_compact_cleanup`
    /// is a no-op (the `if needs_cleanup` branch in `mod.rs` is skipped — no
    /// spurious `post_compact_cleanup` / `emit_session_updated`). Mirrors
    /// `no_compaction_yields_none_summary` (25a) for the cleanup slot.
    #[tokio::test]
    async fn cleanup_signal_none_on_clean_run() {
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
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 0);
        // No compaction ⇒ cleanup slot never set.
        assert!(
            !executor.take_pending_post_compact_cleanup(),
            "clean run must not signal post-compact cleanup"
        );
    }

    /// Slice 25c §E: the pre-request Phase-1 micro-compact (inside
    /// `run_compaction`, triggered before the stream request when the
    /// transcript holds a large tool result) clears the tool result in place —
    /// no `summary_prompt`, so it signals the cleanup slot (the non-merge XOR).
    /// Mirrors `micro_compact_clears_old_tool_results` but asserts the signal.
    #[tokio::test]
    async fn cleanup_signal_on_pre_request_micro() {
        let mut sess = fresh_session();
        seed_large_file_read(&mut sess);
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
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        // High threshold ⇒ auto-compaction (Phase-2) won't fire; only the
        // Phase-1 micro-compact clears the tool result (no LLM call).
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
                compaction_config_high_threshold(),
                PathBuf::from("/tmp/codesmith-test"),
            )),
            None,
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "what did the file say".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Micro-compact is a no-API pass.
        assert_eq!(mock.compaction_calls(), 0);
        // The non-merge micro set the cleanup signal.
        assert!(
            executor.take_pending_post_compact_cleanup(),
            "pre-request micro-compact must signal post-compact cleanup"
        );
    }

    /// Slice 25c §E: the capacity-recovery Phase-1 micro-compact (inside
    /// `recover_context_overflow`, the best-effort in-place tool-result clear
    /// before forced LLM compaction) signals the cleanup slot when it clears
    /// enough to avoid the LLM call. Mirrors
    /// `capacity_micro_compact_clears_tool_results_in_recovery` but asserts the
    /// signal.
    #[tokio::test]
    async fn cleanup_signal_on_recovery_micro() {
        let mut sess = fresh_session();
        // A >32 KB `file_read` tool result pushes the transcript over the 3072
        // budget (Ollama / "llama2"). No CompactionProbe ⇒ run_compaction's
        // Phase-1 micro is skipped; only the capacity recovery's best-effort
        // micro runs (and clears enough — no LLM compaction).
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
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "what did the file say".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Recovery micro cleared the tool result before forced compaction was
        // needed — no LLM call.
        assert_eq!(mock.compaction_calls(), 0);
        // The non-merge recovery micro set the cleanup signal.
        assert!(
            executor.take_pending_post_compact_cleanup(),
            "recovery micro-compact must signal post-compact cleanup"
        );
    }

    /// Slice 25c §E: when capacity recovery's Phase-2 LLM compaction fails,
    /// Phase-3 hard-trim removes oldest messages (bounded by
    /// `MIN_RECENT_MESSAGES_TO_KEEP`) — a non-merge compaction that changes the
    /// transcript without a `summary_prompt`, so it signals the cleanup slot.
    /// Mirrors `capacity_over_budget_recovers_via_hard_trim` but asserts the
    /// signal.
    #[tokio::test]
    async fn cleanup_signal_on_hard_trim() {
        let mut sess = fresh_session();
        // 40 text messages × ~200 chars > 3072 budget (Ollama / "llama2").
        seed_text_messages(&mut sess, 40);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        // Compaction fails → Phase-3 hard trim is the fallback.
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
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Compaction was attempted (1 call) but failed; hard trim saved the turn.
        assert_eq!(mock.compaction_calls(), 1);
        assert!(history.len() < 42, "history len = {}", history.len());
        // The non-merge hard-trim set the cleanup signal.
        assert!(
            executor.take_pending_post_compact_cleanup(),
            "hard-trim must signal post-compact cleanup"
        );
    }

    /// Slice 25c §E: a successful Phase-2 LLM full-compaction records a
    /// `summary_prompt` (the 25a merge path) and does NOT set the cleanup slot
    /// — the `full→merge`, `micro/partial→cleanup` XOR. Guards against both
    /// signals firing on a plain full-compact (the host would run merge THEN a
    /// spurious cleanup that wipes the just-merged `last_system_prompt_hash`).
    /// Mirrors `capacity_over_budget_recovers_via_compaction` but asserts the
    /// XOR.
    #[tokio::test]
    async fn full_compact_does_not_signal_cleanup() {
        let mut sess = fresh_session();
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
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "hello".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 1);
        assert!(history.len() < 42);
        // XOR: full-compact recorded a summary_prompt (merge path)…
        assert!(
            executor.take_pending_compaction_summary().is_some(),
            "full-compact records a summary_prompt for the 25a merge"
        );
        // …and did NOT set the cleanup slot (full→merge, not cleanup).
        assert!(
            !executor.take_pending_post_compact_cleanup(),
            "full-compact must not signal post-compact cleanup (full→merge XOR)"
        );
    }

    /// Slice 25c §E: `take_pending_post_compact_cleanup` is a one-shot drain
    /// (the `std::mem::replace(&mut *guard, false)` clears the slot on read),
    /// so the host's post-`run` closure doesn't see a stale `true` from a
    /// previous harvest. Mirrors the 25a `take_pending_compaction_summary`
    /// one-shot semantics.
    #[tokio::test]
    async fn take_pending_post_compact_cleanup_is_one_shot() {
        let mut sess = fresh_session();
        seed_large_file_read(&mut sess);
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
                compaction_config_high_threshold(),
                PathBuf::from("/tmp/codesmith-test"),
            )),
            None,
            None,
            None,
            None,
        );
        let reason = executor
            .run(&mut history, "what did the file say".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // First take drains the slot (set by the pre-request micro).
        assert!(
            executor.take_pending_post_compact_cleanup(),
            "first take drains the signal set by the micro-compact"
        );
        // Second take is false — the slot was cleared on read.
        assert!(
            !executor.take_pending_post_compact_cleanup(),
            "second take is false — one-shot drain cleared the slot"
        );
    }

    // === compaction reinject (slice 25b §E) ==============================

    /// Build a [`ReinjectProbe`] whose `plan_state` + `todos` are populated
    /// with recognizable content (so reinject produces non-empty plan + todo
    /// candidates) and whose `recent_read_files` shares the session's
    /// `Arc<VecDeque<RecentReadFile>>`. Returns the `Arc`s too — tests assert
    /// on the probe and, for the dedup test, pre-compute the identical plan
    /// candidate from the same snapshot. `recent_read_files` is left as-is;
    /// callers populate it via [`Session::record_read_file_result`] before
    /// invoking this if they want that candidate.
    async fn populated_reinject_probe(
        sess: &Session,
    ) -> (SharedPlanState, SharedTodoList, ReinjectProbe) {
        let plan_state = new_shared_plan_state();
        plan_state
            .lock()
            .await
            .update(UpdatePlanArgs {
                explanation: Some("plan-explanation".to_string()),
                plan: vec![PlanItemArg {
                    step: "plan-step-one".to_string(),
                    status: StepStatus::InProgress,
                }],
            });
        let todos = new_shared_todo_list();
        todos
            .lock()
            .await
            .add("todo-item-content".to_string(), TodoStatus::Pending);
        let probe = ReinjectProbe::new(
            Arc::clone(&plan_state),
            Arc::clone(&todos),
            Arc::clone(&sess.recent_read_files),
            sess.model.clone(),
        );
        (plan_state, todos, probe)
    }

    /// Slice 25b §E: a `run` that triggers Phase-2 auto-compaction re-inserts
    /// the plan / todos / read_files attachment messages into the compacted
    /// transcript DURING `run` (via the [`ReinjectProbe`]), so the model keeps
    /// its working set. Mirrors `run_compaction_records_summary_prompt` (25a)
    /// but asserts the reinject seam — the compacted-out attachments resurface
    /// as `<system-reminder>` user messages in the post-run transcript.
    #[tokio::test]
    async fn reinject_pushes_plan_todo_readfile_candidates_after_compact() {
        let mut sess = fresh_session();
        seed_text_messages(&mut sess, 12);
        // Populate the host state the probe reaches during `run`.
        sess.record_read_file_result(
            &serde_json::json!({"path": "read_file_path.rs"}),
            "file contents here",
        );
        let (_, _, probe) = populated_reinject_probe(&sess).await;
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
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        let reason = executor
            .run(&mut history, "continue".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.compaction_calls(), 1, "Phase-2 auto-compaction fired");
        // Slice 25b §E: the compacted-out attachments resurface as
        // `<system-reminder>` user messages in the live transcript.
        let transcript: String = history
            .messages()
            .iter()
            .flat_map(|m| {
                m.content.iter().filter_map(|b| match b {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            transcript.contains("plan-step-one"),
            "plan candidate re-injected after compaction"
        );
        assert!(
            transcript.contains("Active todos resumed"),
            "todo candidate re-injected after compaction"
        );
        assert!(
            transcript.contains("read_file_path.rs"),
            "read_files candidate re-injected after compaction"
        );
    }

    /// Slice 25b §E: reinject dedups against the live transcript — a candidate
    /// already present (byte-stable equality, matching slice 24's host-side
    /// dedup) is not re-pushed. Exercises the dedup gate directly via the same
    /// private method `run_compaction`'s Ok arm calls.
    #[tokio::test]
    async fn reinject_dedup_skips_already_present() {
        let mut sess = fresh_session();
        let (plan_state, _todos, probe) = populated_reinject_probe(&sess).await;
        // Pre-compute the exact plan candidate the method will build (same
        // snapshot ⇒ same summary ⇒ same `<system-reminder>` message) and
        // pre-push it so dedup must skip it on re-inject.
        let snapshot = plan_state.lock().await.snapshot();
        let plan_summary =
            crate::compaction::attachment_reinject::format_plan_reinject_summary(&snapshot)
                .expect("non-empty plan ⇒ summary");
        let plan_candidate =
            crate::compaction::attachment_reinject::reinject_plan_attachment(&plan_summary)
                .expect("non-empty summary ⇒ candidate");
        let mut history = SessionChatHistory::new(&mut sess);
        history.push(plan_candidate.clone());
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![]));
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
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        let pushed = executor
            .reinject_compaction_attachments(&mut history, None)
            .await;
        // The plan candidate was deduped (already present); only the todo
        // candidate is pushed (read_files is empty here).
        assert_eq!(pushed, 1, "plan candidate skipped by dedup; only todo pushed");
        let plan_count = history
            .messages()
            .iter()
            .filter(|m| *m == &plan_candidate)
            .count();
        assert_eq!(plan_count, 1, "pre-pushed plan candidate not duplicated");
    }

    /// Slice 25b §E: on the context-overflow recovery path (the
    /// `recover_context_overflow` Ok arm), reinject budget-trials each
    /// candidate against `history.messages()` + the static `config.system`
    /// snapshot and skips over-budget ones. A tiny `target_budget` rejects
    /// every candidate (the candidate text alone exceeds 1 token). Exercises
    /// the budget gate directly via the same private method (the `Some` budget
    /// path).
    #[tokio::test]
    async fn reinject_budget_skips_oversized() {
        let mut sess = fresh_session();
        let (_, _, probe) = populated_reinject_probe(&sess).await;
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![]));
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
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        let before = history.messages().len();
        let pushed = executor
            .reinject_compaction_attachments(&mut history, Some(1))
            .await;
        assert_eq!(pushed, 0, "tiny budget rejects all over-budget candidates");
        assert_eq!(
            history.messages().len(),
            before,
            "no over-budget candidate was pushed"
        );
    }

    /// Slice 25b §E: with a [`TurnMetaProbe`] wired, each pushed reinject
    /// candidate is enriched with a leading `<turn_meta>` `ContentBlock::Text`
    /// (the slice-24 `[turn_meta, system-reminder]` shape), so byte-stable
    /// equality still holds for the next turn's dedup. Exercises the enrich
    /// gate directly.
    #[tokio::test]
    async fn reinject_enriches_with_turn_meta() {
        let mut sess = fresh_session();
        let (_, _, probe) = populated_reinject_probe(&sess).await;
        let turn_meta = turn_meta_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![]));
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
            None,
            None,
            None,
        )
        .with_reinject(Some(probe))
        .with_turn_meta(Some(turn_meta));
        let pushed = executor
            .reinject_compaction_attachments(&mut history, None)
            .await;
        assert!(pushed > 0, "plan + todo candidates pushed");
        // Every pushed candidate (the trailing user messages) starts with the
        // `<turn_meta>` block.
        let reinjected = history
            .messages()
            .iter()
            .rev()
            .take(pushed)
            .collect::<Vec<_>>();
        assert_eq!(
            reinjected.len(),
            pushed,
            "trailing messages match the pushed count"
        );
        for msg in &reinjected {
            match msg.content.first() {
                Some(ContentBlock::Text { text, .. }) => {
                    assert!(
                        text.contains("<turn_meta>"),
                        "reinject candidate enriched with leading <turn_meta>"
                    );
                }
                other => panic!("expected leading <turn_meta> Text block, got {other:?}"),
            }
        }
    }

    /// Slice 25b §E: absent [`ReinjectProbe`] (`.with_reinject(None)`) ⇒ reinject
    /// is a no-op (early return, 0 pushed). Embeds / tests that don't opt in
    /// are unaffected — matches the absent-probe precedent for the other
    /// probes. Exercises the same private method both full-compact Ok arms call.
    #[tokio::test]
    async fn reinject_no_probe_is_noop() {
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![]));
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
            None,
            None,
            None,
        )
        .with_reinject(None);
        let before = history.messages().len();
        let pushed = executor
            .reinject_compaction_attachments(&mut history, None)
            .await;
        assert_eq!(pushed, 0, "no probe ⇒ reinject is a no-op");
        assert_eq!(history.messages().len(), before);
    }

    // === read_file observe site (slice 25b §E follow-on) ==================

    /// Build a [`ReinjectProbe`] sharing the session's `recent_read_files`
    /// `Arc` (so tests can assert on the live queue after `run`), carrying the
    /// session's model for `compact_tool_result_for_context`.
    fn read_file_reinject_probe(sess: &Session) -> ReinjectProbe {
        ReinjectProbe::new(
            new_shared_plan_state(),
            new_shared_todo_list(),
            Arc::clone(&sess.recent_read_files),
            sess.model.clone(),
        )
    }

    /// Snapshot the `recent_read_files` queue as a `Vec<RecentReadFile>` clone.
    fn recent_read_files_snapshot(sess: &Session) -> Vec<crate::session::RecentReadFile> {
        sess.recent_read_files
            .lock()
            .expect("recent_read_files poisoned")
            .iter()
            .cloned()
            .collect()
    }

    #[tokio::test]
    async fn read_file_observe_populates_recent_read_files() {
        let mut sess = fresh_session();
        let probe = read_file_reinject_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![
            read_file_call("src/lib.rs"),
            end_call(),
        ]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            read_file_tools(ReadFileSpec::new("pub fn library() {}")),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        let reason = executor
            .run(&mut history, "read the file".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        let entries = recent_read_files_snapshot(&sess);
        assert_eq!(entries.len(), 1, "one read_file result observed");
        assert_eq!(entries[0].path, "src/lib.rs");
        assert!(
            entries[0].output_preview.contains("pub fn library()"),
            "preview retains the (sanitized) file content: {:?}",
            entries[0].output_preview
        );
    }

    #[tokio::test]
    async fn read_file_observe_skips_non_read_file_tools() {
        let mut sess = fresh_session();
        let probe = read_file_reinject_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        // Use the echo tool (name "echo") — not "read_file".
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());
        let mut echo_round = text_block(0, "echoing");
        echo_round.extend(tool_use_block(1, "call_1", "echo", r#"{"text":"hi"}"#));
        echo_round.extend(finish("tool_use"));
        let mock = Arc::new(MockLlm::new(vec![echo_round, end_call()]));
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
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        executor
            .run(&mut history, "echo something".to_string())
            .await
            .expect("run");
        let entries = recent_read_files_snapshot(&sess);
        assert!(entries.is_empty(), "non-read_file tools are not observed");
    }

    #[tokio::test]
    async fn read_file_observe_skips_failed_read_file() {
        let mut sess = fresh_session();
        let probe = read_file_reinject_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![
            read_file_call("missing.rs"),
            end_call(),
        ]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            read_file_tools(ReadFileSpec::failing("Error: file not found")),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        executor
            .run(&mut history, "read the file".to_string())
            .await
            .expect("run");
        let entries = recent_read_files_snapshot(&sess);
        assert!(
            entries.is_empty(),
            "a failed read_file (success: false) must not be observed"
        );
    }

    #[tokio::test]
    async fn read_file_observe_dedup_by_path_keeps_latest() {
        let mut sess = fresh_session();
        let probe = read_file_reinject_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        // Two read_file calls to the same path; the second content wins.
        let mock = Arc::new(MockLlm::new(vec![
            {
                let mut c = read_file_call("src/lib.rs");
                // second tool_use in the same round
                c.extend(tool_use_block(2, "call_2", "read_file", r#"{"path":"src/lib.rs"}"#));
                c.extend(finish("tool_use"));
                c
            },
            end_call(),
        ]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            read_file_tools(ReadFileSpec::new("fn second_read() {}")),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        executor
            .run(&mut history, "read the file twice".to_string())
            .await
            .expect("run");
        let entries = recent_read_files_snapshot(&sess);
        assert_eq!(entries.len(), 1, "dedup by path keeps one entry");
        assert!(
            entries[0].output_preview.contains("fn second_read()"),
            "latest content retained: {:?}",
            entries[0].output_preview
        );
    }

    #[tokio::test]
    async fn read_file_observe_strips_hidden_unicode() {
        // Security: the observe feeds `compact_tool_result_for_context`, which
        // runs `partially_sanitize_unicode` (HackerOne #3086545). A zero-width
        // char (U+200B) injected into file content must not survive into the
        // retained preview — otherwise it would be re-injected post-compaction.
        let mut sess = fresh_session();
        let probe = read_file_reinject_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![
            read_file_call("evil.txt"),
            end_call(),
        ]));
        // Content with a zero-width space injected mid-token.
        let poisoned = format!("clean_start\u{200B}secret_end");
        let executor = HostAgentExecutor::new(
            mock.clone(),
            read_file_tools(ReadFileSpec::new(&poisoned)),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_reinject(Some(probe));
        executor
            .run(&mut history, "read the file".to_string())
            .await
            .expect("run");
        let entries = recent_read_files_snapshot(&sess);
        assert_eq!(entries.len(), 1);
        assert!(
            !entries[0].output_preview.contains('\u{200B}'),
            "zero-width char must be stripped from the retained preview: {:?}",
            entries[0].output_preview
        );
        assert!(
            entries[0].output_preview.contains("clean_start"),
            "non-hidden content retained: {:?}",
            entries[0].output_preview
        );
    }

    #[tokio::test]
    async fn read_file_observe_none_reinject_is_noop() {
        // No `ReinjectProbe` ⇒ the observe site must be a silent no-op (no
        // panic, no mutation) — mirrors `reinject_no_probe_is_noop` for the
        // observe (write) side.
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let mock = Arc::new(MockLlm::new(vec![
            read_file_call("src/lib.rs"),
            end_call(),
        ]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            read_file_tools(ReadFileSpec::new("pub fn library() {}")),
            callback,
            AgentExecutorConfig::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_reinject(None);
        executor
            .run(&mut history, "read the file".to_string())
            .await
            .expect("run");
        let entries = recent_read_files_snapshot(&sess);
        assert!(
            entries.is_empty(),
            "no reinject probe ⇒ observe is a no-op, recent_read_files untouched"
        );
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
            None,
            None,
            None,
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
                StreamDelta::ToolCallStarted { id, name, .. } => {
                    format!("ToolCallStarted({id}, {name})")
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

    // === §E slice 29 — ToolCallStarted stream-time + bridge dedup ==========
    //
    // `reduce_stream` fires `StreamDelta::ToolCallStarted` at `ContentBlockStop`
    // for a tool block (carrying the wire id + finalized input). The
    // `CallbackBridge` forwards it as `Event::ToolCallStarted` with the real
    // wire id and marks the id announced; the execute-time `on_tool_start`
    // sees the announcement and skips re-emitting (dedup). These four tests
    // prove: (1) the delta fires at `ContentBlockStop` with the right wire
    // id/name/input, (2) it flows end-to-end through the bridge with the real
    // wire id (not a synthesized `bridge-{n}`), (3) exactly one
    // `Event::ToolCallStarted` per call (deduped), (4) it fires even for an
    // unregistered tool (the UI sees "calling X" before the execute-time
    // lookup fails).

    #[tokio::test]
    async fn stream_emits_tool_call_started_at_content_block_stop() {
        // `reduce_stream` announces a tool call at `ContentBlockStop` (after
        // `finalize_tool_input` parses the `InputJsonDelta` fragments), before
        // the tool actually executes. `DeltaRecorder` captures stream deltas
        // only — no execute-time path — so this isolates the stream-time seam.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let recorder = Arc::new(DeltaRecorder::new());
        let callback: Arc<dyn Callback> = recorder.clone();

        // text block(0) + tool block(1, wire id "toolu_1") + finish(tool_use);
        // call 2 ends the turn.
        let mut call1 = text_block(0, "let me echo");
        call1.extend(tool_use_block(1, "toolu_1", "echo", r#"{"text":"hi"}"#));
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
            None,
            None,
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

        // Exactly one ToolCallStarted delta, carrying the wire id + finalized
        // input (parsed from the InputJsonDelta fragment).
        let started: Vec<_> = recorder
            .deltas()
            .iter()
            .filter_map(|d| match d {
                StreamDelta::ToolCallStarted { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 1, "one ToolCallStarted: {started:?}");
        assert_eq!(started[0].0, "toolu_1", "wire id passthrough");
        assert_eq!(started[0].1, "echo");
        assert_eq!(started[0].2, serde_json::json!({"text":"hi"}));
    }

    #[tokio::test]
    async fn tool_call_started_flows_through_callback_bridge_with_wire_id() {
        // End-to-end: executor → CallbackBridge → Event::ToolCallStarted on the
        // Event channel carrying the REAL wire id (not a synthesized
        // `bridge-{n}` — that synthesis was retired in slice 29).
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(CallbackBridge::new(
            Some(tx),
            None,
            test_template(),
        ));

        let mut call1 = text_block(0, "calling");
        call1.extend(tool_use_block(1, "toolu_42", "echo", r#"{"text":"yo"}"#));
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
            None,
            None,
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

        let events = drain(&mut rx);
        let started = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallStarted { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .expect("Event::ToolCallStarted emitted");
        // Real wire id, NOT a synthesized `bridge-{n}`.
        assert_eq!(started.0, "toolu_42", "real wire id (not bridge-{{n}})");
        assert!(!started.0.starts_with("bridge-"), "no synthesized id");
        assert_eq!(started.1, "echo");
        assert_eq!(started.2, serde_json::json!({"text":"yo"}));

        // The matching complete carries the same wire id.
        let complete = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallComplete { id, .. } => Some(id.clone()),
                _ => None,
            })
            .expect("Event::ToolCallComplete emitted");
        assert_eq!(started.0, complete, "start/end wire ids correlate");
    }

    #[tokio::test]
    async fn tool_call_started_not_duplicated_at_execute_time() {
        // Dedup: the stream-time `on_stream_delta(ToolCallStarted)` announces +
        // marks the id; the execute-time `on_tool_start` sees the announcement
        // and skips re-emitting. Exactly ONE `Event::ToolCallStarted` per call.
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(CallbackBridge::new(
            Some(tx),
            None,
            test_template(),
        ));

        let mut call1 = text_block(0, "calling");
        call1.extend(tool_use_block(1, "toolu_99", "echo", r#"{"text":"x"}"#));
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
            None,
            None,
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

        // Exactly one ToolCallStarted — the stream-time emission, with the
        // execute-time `on_tool_start` deduped against it.
        let events = drain(&mut rx);
        let started_count = events
            .iter()
            .filter(|e| matches!(e, Event::ToolCallStarted { .. }))
            .count();
        assert_eq!(
            started_count,
            1,
            "exactly one ToolCallStarted (deduped): {events:?}"
        );
        let started = events
            .iter()
            .find_map(|e| match e {
                Event::ToolCallStarted { id, .. } => Some(id.clone()),
                _ => None,
            })
            .expect("ToolCallStarted emitted");
        assert_eq!(started, "toolu_99");
    }

    #[tokio::test]
    async fn tool_call_started_emitted_even_for_unregistered_tool() {
        // The stream-time emission fires for ALL tool blocks at
        // `ContentBlockStop`, regardless of registration — the UI sees
        // "calling ghost" before the execute-time lookup fails (the lookup
        // happens later, in the tool loop).
        let tools = Arc::new(ToolSet::new()); // no tools registered

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let recorder = Arc::new(DeltaRecorder::new());
        let callback: Arc<dyn Callback> = recorder.clone();

        let mut call1 = text_block(0, "calling ghost");
        call1.extend(tool_use_block(1, "toolu_7", "ghost", r#"{"x":1}"#));
        call1.extend(finish("tool_use"));
        let mut call2 = text_block(0, "ok");
        call2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

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
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The unregistered tool still surfaced a ToolCallStarted delta.
        let started: Vec<_> = recorder
            .deltas()
            .iter()
            .filter_map(|d| match d {
                StreamDelta::ToolCallStarted { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 1, "ghost tool still announced: {started:?}");
        assert_eq!(started[0].0, "toolu_7");
        assert_eq!(started[0].1, "ghost");
        assert_eq!(started[0].2, serde_json::json!({"x":1}));
    }

    // === early-tool-start (seam 2 speculative dispatch) ====================

    /// A read-only `ToolSpec` that signals a [`tokio::sync::Notify`] when its
    /// `execute` runs, so a test can prove the tool was dispatched *during*
    /// streaming (before the executor's tool loop) — the hallmark of
    /// early-tool-start. `Notify::notify_one` stores a permit if no waiter is
    /// registered yet, so the signal survives a scheduling gap between the
    /// early dispatch and the test's `notified().await`.
    struct SignalingSpec {
        notify: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl ToolSpec for SignalingSpec {
        fn name(&self) -> &str {
            "signal"
        }
        fn description(&self) -> &str {
            "Signals a Notify when executed."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            self.notify.notify_one();
            Ok(ToolResult {
                content: "signaled".to_string(),
                success: true,
                metadata: None,
            })
        }
    }

    /// A read-only `ToolSpec` that counts how many times `execute` runs via a
    /// shared [`AtomicU32`]. If early-start reuses the speculatively-started
    /// task, the count stays 1 (not 2 — the early run is reused, not re-run at
    /// execute time).
    struct CountingSpec {
        count: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait::async_trait]
    impl ToolSpec for CountingSpec {
        fn name(&self) -> &str {
            "count"
        }
        fn description(&self) -> &str {
            "Counts how many times it runs."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolResult {
                content: "counted".to_string(),
                success: true,
                metadata: None,
            })
        }
    }

    /// A read-only `ToolSpec` that cancels a `CancellationToken` on execute —
    /// proves a tool can trigger a mid-turn cancel that Checkpoint G (the
    /// post-tool-loop gate) catches, surfacing `StopReason::Interrupted`
    /// instead of continuing to the next step. Read-only so it skips the
    /// approval gate (the cancel comes from `execute`, not the approval wait).
    struct CancelOnCallSpec {
        token: CancellationToken,
    }

    #[async_trait::async_trait]
    impl ToolSpec for CancelOnCallSpec {
        fn name(&self) -> &str {
            "cancel_on_call"
        }
        fn description(&self) -> &str {
            "Cancels the turn token when executed."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            self.token.cancel();
            Ok(ToolResult {
                content: "cancelled".to_string(),
                success: true,
                metadata: None,
            })
        }
    }

    #[test]
    fn early_start_safe_allows_readonly() {
        assert!(early_start_safe(&[ToolCapability::ReadOnly]));
        // Network / Sandboxable don't disqualify a read-only tool.
        assert!(early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::Network,
        ]));
        assert!(early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::Sandboxable,
        ]));
    }

    #[test]
    fn early_start_safe_disqualifies_non_readonly() {
        // No ReadOnly at all ⇒ not safe.
        assert!(!early_start_safe(&[]), "empty caps");
        assert!(!early_start_safe(&[ToolCapability::Network]), "network only");
        // ReadOnly + a disqualifier ⇒ not safe.
        assert!(!early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::WritesFiles,
        ]));
        assert!(!early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::ExecutesCode,
        ]));
        assert!(!early_start_safe(&[
            ToolCapability::ReadOnly,
            ToolCapability::RequiresApproval,
        ]));
    }

    /// The read-only tool is dispatched **during** streaming (at
    /// `ContentBlockStop`), before the executor reaches the tool loop. Proven
    /// by running the executor on a spawned task and awaiting the tool's
    /// `Notify` signal — which fires from inside the spawned early task, so it
    /// can only arrive before the executor returns.
    #[tokio::test]
    async fn early_start_dispatches_readonly_tool_during_stream() {
        let tmp = tempdir().expect("tempdir");
        let notify = Arc::new(tokio::sync::Notify::new());
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(SignalingSpec {
            notify: notify.clone(),
        }));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Call 1: text + tool_use(signal). Call 2: text-only → NoToolCalls.
        let mut call1 = text_block(0, "let me signal");
        call1.extend(tool_use_block(1, "s1", "signal", r#"{"x":1}"#));
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
            None,
            None,
            None,
        );

        // Run on a spawned task so the test can observe the tool's signal
        // mid-run. The `run` future borrows the executor + session, so move
        // them into the spawned block — the block owns them and is `'static`.
        let handle = tokio::spawn(async move {
            let mut history = SessionChatHistory::new(&mut sess);
            executor
                .run(&mut history, "signal please".to_string())
                .await
        });

        // The early dispatch fires `notify_one` from inside the spawned early
        // task (during streaming). If early-start were absent, the tool would
        // only run at execute time — but the executor hasn't returned yet
        // (it's still inside `run`), so no execute-time call has happened
        // either. The only way this `notified` resolves is the early dispatch.
        notify.notified().await;

        let reason = handle
            .await
            .expect("executor task panicked")
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
    }

    /// The speculatively-started task's result is **reused** — `execute` runs
    /// once (in the early task), not twice (early + execute-time). If the
    /// reuse path were broken, `execute` would be called again at execute
    /// time → count == 2.
    #[tokio::test]
    async fn early_start_reuses_result_without_re_running() {
        let tmp = tempdir().expect("tempdir");
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(CountingSpec {
            count: count.clone(),
        }));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call1 = text_block(0, "go");
        call1.extend(tool_use_block(1, "c1", "count", r#"{}"#));
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
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "count".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "tool should run once (early-started + reused, not re-run)"
        );
    }

    /// An early-started read-only tool whose task panics surfaces as a
    /// `ToolError::execution_failed` (the `JoinHandle` errors). The tool is
    /// still recorded in the transcript (as an error result) and the turn ends
    /// cleanly (`NoToolCalls` on the follow-up). The panic is contained in the
    /// spawned task — `tokio::spawn` catches it, the `JoinHandle` returns
    /// `Err(JoinError)`.
    struct PanickingSpec;

    #[async_trait::async_trait]
    impl ToolSpec for PanickingSpec {
        fn name(&self) -> &str {
            "boom"
        }
        fn description(&self) -> &str {
            "Panics in execute."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            panic!("boom from early task");
        }
    }

    #[tokio::test]
    async fn early_start_join_error_surfaces_execution_failed() {
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(PanickingSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call1 = text_block(0, "go");
        call1.extend(tool_use_block(1, "b1", "boom", r#"{}"#));
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
            None,
            None,
            None,
        );

        // Suppress the panic message on stderr (the panic is caught by the
        // JoinHandle — the test itself doesn't panic).
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let reason = executor
            .run(&mut history, "boom".to_string())
            .await
            .expect("run");
        std::panic::set_hook(prev_hook);

        assert_eq!(reason, StopReason::NoToolCalls);
        // The tool result is an error carrying the join-failure message.
        let last_tool_result = &sess.messages[2].content[0];
        let ContentBlock::ToolResult {
            content,
            is_error,
            ..
        } = last_tool_result
        else {
            panic!("expected ToolResult, got {last_tool_result:?}");
        };
        assert_eq!(*is_error, Some(true), "should be an error result");
        assert!(
            content.contains("Early tool execution task failed"),
            "should mention the join failure: {content}"
        );
    }

    /// A non-read-only tool (WritesFiles) is **not** early-dispatched. It runs
    /// once at execute time (not during streaming). Proven by the call count
    /// staying at 1 (if it were early-dispatched + re-run, it'd be 2 — but
    /// early-start-safe correctly excludes it, so it's just 1 execute-time
    /// run). The `early_start_safe_disqualifies_non_readonly` unit test covers
    /// the gate; this integration test confirms the executor respects it.
    #[tokio::test]
    async fn early_start_skips_non_readonly_tool() {
        let tmp = tempdir().expect("tempdir");
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(CountingWriteSpec {
            count: count.clone(),
        }));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut call1 = text_block(0, "write");
        call1.extend(tool_use_block(1, "w1", "write_file", r#"{"path":"/tmp/x"}"#));
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
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "write".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Not early-dispatched (WritesFiles) ⇒ runs exactly once at execute
        // time. If early-start wrongly dispatched it, the count would still
        // be 1 (reused) — so this test alone doesn't prove "not dispatched",
        // but combined with the unit test for the gate, it confirms the
        // executor doesn't double-execute non-read-only tools.
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "write tool runs once at execute time"
        );
    }

    /// A `ToolSpec` that declares `WritesFiles` (so `early_start_safe` is
    /// false) and counts `execute` calls — used to confirm the executor
    /// doesn't early-dispatch non-read-only tools.
    struct CountingWriteSpec {
        count: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait::async_trait]
    impl ToolSpec for CountingWriteSpec {
        fn name(&self) -> &str {
            "write_file"
        }
        fn description(&self) -> &str {
            "Writes a file (counts calls)."
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
            _input: serde_json::Value,
            _context: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolResult {
                content: "wrote".to_string(),
                success: true,
                metadata: None,
            })
        }
    }

    // === subagent post-stream completion drain =================================

    #[tokio::test]
    async fn subagent_none_is_noop() {
        // No subagent receiver ⇒ the turn ends on the first no-tool-call round,
        // no extra messages injected.
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mut ok = text_block(0, "all done");
        ok.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![ok]));

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
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // [user(seed), assistant(text)] — no completion injected.
        assert_eq!(history.len(), 2);
        assert!(!has_subagent_completion_msg(sess.messages.as_slice()));
        assert_eq!(mock.requests().len(), 1, "one stream round, no resume");
    }

    #[tokio::test]
    async fn subagent_empty_queue_returns_no_tool_calls() {
        // A present-but-empty completion queue ⇒ NoToolCalls (the blocking hold
        // for running children is deferred; with no queued completion and no
        // running-count probe, the turn ends).
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (_tx_sub, rx_sub) = subagent_channel();

        let mut ok = text_block(0, "all done");
        ok.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![ok]));

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
            Some(rx_sub),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(history.len(), 2);
        assert!(!has_subagent_completion_msg(sess.messages.as_slice()));
        assert_eq!(mock.requests().len(), 1, "no resume when queue empty");
    }

    #[tokio::test]
    async fn subagent_drain_injects_queued_completions_and_resumes() {
        // Two completions queued before run. Round 1: model returns no tool
        // calls ⇒ drain finds 2 completions ⇒ inject 2 sentinel user messages
        // ⇒ resume. Round 2: model returns no tool calls ⇒ drain empty ⇒ end.
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(CallbackBridge::new(
            Some(tx.clone()),
            None,
            test_template(),
        ));

        let (tx_sub, rx_sub) = subagent_channel();
        tx_sub.send(completion("child-a finished")).unwrap();
        tx_sub.send(completion("child-b finished")).unwrap();

        let call1 = {
            let mut c = text_block(0, "let me wait for children");
            c.extend(finish("end_turn"));
            c
        };
        let call2 = {
            let mut c = text_block(0, "resuming, all done");
            c.extend(finish("end_turn"));
            c
        };
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

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
            Some(rx_sub),
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Two stream rounds — the turn resumed after the first drain.
        assert_eq!(mock.requests().len(), 2, "drain resumed the turn");

        // The injected sentinels reached the transcript. Layout:
        // [user(seed), assistant(text), user(sentinel-a), user(sentinel-b),
        //  assistant(text)]
        assert!(history.len() >= 5, "expected ≥5 messages, got {}", history.len());
        // Both sentinel messages are present in the session transcript.
        let sentinel_msgs: Vec<&Message> = sess
            .messages
            .iter()
            .filter(|m| {
                m.content.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text, .. } if text
                        .contains("kind=\"subagent_completion\""))
                })
            })
            .collect();
        assert_eq!(sentinel_msgs.len(), 2, "expected 2 sentinel messages");
        // The second stream request saw the sentinels (they were pushed before
        // the resume, so the request snapshot includes them).
        let reqs = mock.requests();
        let second_req_has_sentinels = reqs[1]
            .iter()
            .filter(|m| {
                m.content.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text, .. } if text
                        .contains("kind=\"subagent_completion\""))
                })
            })
            .count();
        assert_eq!(
            second_req_has_sentinels, 2,
            "second request must include both sentinels"
        );

        // Status surfaced the resume count.
        let msgs = statuses(&drain(&mut rx));
        assert!(
            msgs.iter().any(|m| m.contains("Resuming turn with 2 sub-agent completion(s)")),
            "expected resume status, got {msgs:?}"
        );
    }

    #[tokio::test]
    async fn subagent_picks_up_completion_queued_between_runs() {
        // Cross-run persistence: the `Arc<Mutex<Receiver>>` lives on the
        // executor struct, so a completion queued between runs is surfaced on
        // the next run's post-stream drain — a per-run local receiver could
        // not do this.
        let tools = Arc::new(ToolSet::new());
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_sub, rx_sub) = subagent_channel();

        // run1: one text round, no completion queued ⇒ clean NoToolCalls.
        let mut ok = text_block(0, "first turn");
        ok.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![ok, {
            let mut c = text_block(0, "second turn after child finished");
            c.extend(finish("end_turn"));
            c
        }]));

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
            Some(rx_sub),
            None,
            None,
        );

        // run1: no completion queued → NoToolCalls, no sentinel.
        let mut sess1 = fresh_session();
        let mut history1 = SessionChatHistory::new(&mut sess1);
        let reason1 = executor
            .run(&mut history1, "turn one".to_string())
            .await
            .expect("run1");
        assert_eq!(reason1, StopReason::NoToolCalls);
        assert!(!has_subagent_completion_msg(sess1.messages.as_slice()));
        assert_eq!(mock.requests().len(), 1, "run1: one round, no resume");

        // Between runs: queue a completion on the SAME receiver.
        tx_sub.send(completion("child finished between turns")).unwrap();

        // run2: SAME executor (+ new Session). Post-stream drain surfaces the
        // queued completion → resume → second round ends.
        let mut sess2 = fresh_session();
        let mut history2 = SessionChatHistory::new(&mut sess2);
        let reason2 = executor
            .run(&mut history2, "turn two".to_string())
            .await
            .expect("run2");
        assert_eq!(reason2, StopReason::NoToolCalls);
        // The completion was drained on run2 (2 stream rounds in run2 alone:
        // round1 resumes, round2 ends). Total rounds across both runs = 3.
        assert_eq!(mock.requests().len(), 3, "run2 resumed after draining");
        assert!(
            has_subagent_completion_msg(sess2.messages.as_slice()),
            "run2 transcript must contain the completion drained from the shared receiver"
        );
        // The run2 resume request (the 3rd overall, the 2nd of run2) saw the sentinel.
        let reqs = mock.requests();
        assert!(
            reqs[2].iter().any(|m| {
                m.content.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text, .. } if text
                        .contains("kind=\"subagent_completion\""))
                })
            }),
            "run2's second request must include the sentinel"
        );
    }

    // === §E subagent blocking hold =========================================

    /// When the model finishes a step with no tool calls, no queued
    /// completions, but children still running (`running_count > 0`), the
    /// executor blocks on a `biased select!` until a child completes. The
    /// completion is injected as a sentinel and the turn resumes — proving the
    /// hold fires and the completion arm works.
    #[tokio::test]
    async fn subagent_hold_waits_for_running_children_then_resumes() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx_event, mut rx_event) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (tx_sub, rx_sub) = subagent_channel();
        let api = FakeSubAgentApi::new(vec![1]); // 1 running → hold fires, then 0

        // Round 1: no tool calls → post-stream hold. Round 2: no tool calls →
        // running_count is now 0 → no hold → NoToolCalls.
        let mut call1 = text_block(0, "working on it");
        call1.extend(finish("end_turn"));
        let mut call2 = text_block(0, "all done");
        call2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

        // Push a completion AFTER the hold starts blocking (50ms delay ensures
        // the non-blocking drain ran first and found nothing).
        let tx_clone = tx_sub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = tx_clone.send(completion("child finished"));
        });

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx_event),
            None,
            None,
            None,
            None,
            None,
            Some(rx_sub),
            None,
            Some(api),
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // The hold fired and the completion was injected.
        assert!(has_subagent_completion_msg(sess.messages.as_slice()));
        // Two stream rounds: round 1 + resume.
        assert_eq!(mock.requests().len(), 2, "hold resumed the turn once");
        // The "Waiting on 1 sub-agent(s)" status proves the hold fired (the
        // non-blocking drain alone would not emit this).
        let events = drain(&mut rx_event);
        assert!(
            events.iter().any(|e| matches!(e, Event::Status { message, .. } if message
                .contains("Waiting on 1 sub-agent(s)"))),
            "hold must emit the waiting status: {events:?}"
        );
    }

    /// A cancel token that fires during the hold breaks out via Checkpoint E
    /// (the hold's own `biased select!` cancel arm) and returns `Interrupted`.
    /// Asserting `requests().len() == 1` proves the stream completed and the
    /// cancel landed during the hold (not at Checkpoint A before the stream).
    #[tokio::test]
    async fn subagent_hold_cancel_returns_interrupted() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (_tx_sub, rx_sub) = subagent_channel();
        let api = FakeSubAgentApi::new(vec![1]);
        let token = CancellationToken::new();

        let mut call1 = text_block(0, "working on it");
        call1.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1]));

        // Cancel during the hold (after the stream completes).
        let token_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            token_clone.cancel();
        });

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
            Some(rx_sub),
            Some(token),
            Some(api),
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::Interrupted);
        // One stream call proves the cancel landed after the stream (during
        // the hold), not at Checkpoint A (which would be 0 streams).
        assert_eq!(mock.requests().len(), 1);
        assert!(!has_subagent_completion_msg(sess.messages.as_slice()));
    }

    /// A steer that arrives during the hold fires the steer arm: the steered
    /// text is injected as a user message and the turn resumes on a fresh step
    /// (closes the "steer post-stream resume" gap). Round 2: no running children
    /// → `NoToolCalls`.
    #[tokio::test]
    async fn subagent_hold_steer_arm_resumes_with_steered_text() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (tx_steer, rx_steer) = steer_channel();
        let (_tx_sub, rx_sub) = subagent_channel();
        let api = FakeSubAgentApi::new(vec![1]);

        let mut call1 = text_block(0, "working on it");
        call1.extend(finish("end_turn"));
        let mut call2 = text_block(0, "redirected");
        call2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

        // Push a steer during the hold.
        let tx_clone = tx_steer.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = tx_clone.send("please use Python".to_string()).await;
        });

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
            Some(rx_sub),
            None,
            Some(api),
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // The steer text was injected as a user message.
        assert!(
            sess.messages.iter().any(|m| {
                m.content.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text, .. } if text.contains("please use Python"))
                })
            }),
            "steer text must appear in the transcript"
        );
        // Two stream rounds (round 1 + resume after steer).
        assert_eq!(mock.requests().len(), 2);
        // The steer text reached the model on the resume request.
        assert!(
            mock.requests()[1].iter().any(|m| {
                m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { text, .. } if text.contains("please use Python")))
            }),
            "steer text must be in the resume request"
        );
    }

    /// No `subagent_api` ⇒ the hold is skipped even with a present receiver
    /// and an empty queue (no `running_count` to check). The turn ends on the
    /// first no-tool-call round.
    #[tokio::test]
    async fn subagent_hold_no_subagent_api_skips_hold() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (_tx_sub, rx_sub) = subagent_channel();

        let mut call1 = text_block(0, "all done");
        call1.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1]));

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
            Some(rx_sub),
            None,
            None, // no subagent_api
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.requests().len(), 1, "no resume without the hold");
        assert!(!has_subagent_completion_msg(sess.messages.as_slice()));
    }

    /// `running_count == 0` ⇒ `should_hold_turn_for_subagents` returns false ⇒
    /// the hold is skipped. The turn ends on `NoToolCalls`.
    #[tokio::test]
    async fn subagent_hold_no_running_children_skips_hold() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (_tx_sub, rx_sub) = subagent_channel();
        let api = FakeSubAgentApi::new(vec![0]); // no running children

        let mut call1 = text_block(0, "all done");
        call1.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1]));

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
            Some(rx_sub),
            None,
            Some(api),
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.requests().len(), 1, "no hold when no children running");
        assert!(!has_subagent_completion_msg(sess.messages.as_slice()));
    }

    /// Multiple completions batched behind the first are drained by the
    /// `try_recv` loop after `recv()` returns (mirrors `turn_loop.rs:1342-1345`).
    /// All three are injected as sentinel messages.
    #[tokio::test]
    async fn subagent_hold_drains_batched_completions() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);
        let (tx_sub, rx_sub) = subagent_channel();
        let api = FakeSubAgentApi::new(vec![1]);

        let mut call1 = text_block(0, "working on it");
        call1.extend(finish("end_turn"));
        let mut call2 = text_block(0, "all done");
        call2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

        // Push 3 completions during the hold (batched behind each other).
        let tx_clone = tx_sub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = tx_clone.send(completion("child 1 done"));
            let _ = tx_clone.send(completion("child 2 done"));
            let _ = tx_clone.send(completion("child 3 done"));
        });

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
            Some(rx_sub),
            None,
            Some(api),
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        // Count sentinel messages — all 3 must be injected.
        let sentinel_count = sess
            .messages
            .iter()
            .filter(|m| {
                m.content.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text, .. } if text
                        .contains("kind=\"subagent_completion\""))
                })
            })
            .count();
        assert_eq!(
            sentinel_count, 3,
            "all 3 batched completions must be drained and injected"
        );
        assert_eq!(mock.requests().len(), 2, "hold resumed the turn once");
    }

    // === §E cancel-token ===================================================

    /// `cancel_token = None` is a no-op — the turn runs normally and returns
    /// `NoToolCalls`. Proves the opt-in nature: existing callers that don't
    /// supply a token are unaffected (every `is_cancelled()` returns `false`).
    #[tokio::test]
    async fn cancel_none_is_noop() {
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
            None,
            None,
            None,
            None,
            None,
            None, // cancel_token = None
            None,
        );

        let reason = executor
            .run(&mut history, "hi".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(history.len(), 2, "[user, assistant]");
    }

    /// A pre-cancelled token short-circuits at Checkpoint A (loop-top gate)
    /// before any stream call. The turn returns `Interrupted` (not `Error`),
    /// and the mock records zero `create_message_stream` calls.
    #[tokio::test]
    async fn cancel_pre_cancelled_returns_interrupted() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let token = CancellationToken::new();
        token.cancel();

        let mut call = text_block(0, "hello");
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
            None,
            Some(token),
            None,
        );

        let reason = executor
            .run(&mut history, "hi".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::Interrupted);
        assert_eq!(
            mock.requests().len(),
            0,
            "no stream call — caught at loop-top (Checkpoint A)"
        );
        assert_eq!(history.len(), 1, "only the seed user message");
    }

    /// A tool that cancels the token during `execute` is caught by Checkpoint G
    /// (post-tool-loop gate) — the turn returns `Interrupted` instead of
    /// continuing to the next step. The second mock round is never consumed.
    #[tokio::test]
    async fn cancel_between_steps_returns_interrupted() {
        let tmp = tempdir().expect("tempdir");
        let token = CancellationToken::new();
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(CancelOnCallSpec {
            token: token.clone(),
        }));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Round 1: text + tool_use(cancel_on_call) → cancels the token.
        let mut call1 = text_block(0, "cancelling now");
        call1.extend(tool_use_block(1, "c1", "cancel_on_call", "{}"));
        call1.extend(finish("tool_use"));
        // Round 2: text-only → never reached (Checkpoint G catches the cancel).
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

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
            None,
            Some(token),
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::Interrupted);
        assert_eq!(
            mock.requests().len(),
            1,
            "second round never consumed — caught at Checkpoint G"
        );
    }

    /// When the token is cancelled as a side-effect of `create_message_stream`
    /// (stream opened, then died empty), Checkpoint C (transparent-retry
    /// `!cancelled` guard) aborts the retry — the turn returns `Interrupted`
    /// instead of burning the retry budget.
    #[tokio::test]
    async fn cancel_short_circuits_transparent_retry() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let token = CancellationToken::new();
        let mock = Arc::new(
            MockLlm::with_rounds(vec![MockRound::StreamErr("connection reset".into())])
                .with_cancel_on_stream(token.clone()),
        );
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
            None,
            Some(token),
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::Interrupted);
        assert_eq!(
            mock.requests().len(),
            1,
            "no retry — Checkpoint C aborted the retry loop"
        );
    }

    /// When the token is cancelled as a side-effect of `create_message_stream`
    /// but the stream completes cleanly, Checkpoint D (post-stream gate)
    /// discards the content and returns `Interrupted` — the assistant turn is
    /// NOT committed to the transcript.
    #[tokio::test]
    async fn cancel_after_clean_stream_returns_interrupted() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let token = CancellationToken::new();
        let mut call = text_block(0, "clean content");
        call.extend(finish("end_turn"));
        let mock = Arc::new(
            MockLlm::with_rounds(vec![MockRound::Events(call)])
                .with_cancel_on_stream(token.clone()),
        );
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
            None,
            Some(token),
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::Interrupted);
        assert_eq!(mock.requests().len(), 1, "one stream call");
        assert_eq!(
            history.len(),
            1,
            "content discarded — only the seed user message remains"
        );
    }

    /// A cancel that lands while the approval gate is blocking breaks out of
    /// the `recv().await` via the `select!` race — the tool records an error
    /// result, and Checkpoint G catches the cancel so the turn returns
    /// `Interrupted` (not a next-step continuation).
    #[tokio::test]
    async fn cancel_during_approval_returns_interrupted() {
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(WriteSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (_tx_approval, rx_approval) = approval_channel();
        // No decision pushed — the gate blocks until the cancel fires.

        let token = CancellationToken::new();
        // Round 1: text + tool_use(write_file) → requires approval (blocks).
        let mut call1 = text_block(0, "writing the file");
        call1.extend(tool_use_block(1, "call_1", "write_file", r#"{"path":"/tmp/x"}"#));
        call1.extend(finish("tool_use"));
        // Round 2: never reached.
        let mut call2 = text_block(0, "done");
        call2.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

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
            None,
            Some(token.clone()),
            None,
        );

        // Background task: cancel the token after a short delay so the
        // approval gate is definitely blocking when it fires.
        let bg_token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            bg_token.cancel();
        });

        // Wrap in a timeout so the test fails fast if the cancel race doesn't
        // fire (instead of hanging on the approval recv).
        let reason = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            executor.run(&mut history, "please write".to_string()),
        )
        .await
        .expect("must not hang — cancel should break the approval wait")
        .expect("run");
        assert_eq!(reason, StopReason::Interrupted);
        assert_eq!(mock.requests().len(), 1, "second round never consumed");
    }

    /// `drain_stale_steers` discards steers queued before the turn — they do
    /// NOT appear in the LLM request or the transcript. Mirrors production's
    /// `while self.rx_steer.try_recv().is_ok() {}` at the start of
    /// `handle_send_message` (`engine/mod.rs:1013-1014`).
    #[tokio::test]
    async fn steer_stale_drain_discards_previous_turn_steers() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();
        tx_steer
            .send("stale steer 1".to_string())
            .await
            .unwrap();
        tx_steer
            .send("stale steer 2".to_string())
            .await
            .unwrap();

        let mut call = text_block(0, "acknowledged");
        call.extend(finish("end_turn"));
        let mock = Arc::new(MockLlm::new(vec![call]));

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
            None,
            None,
            None,
        );

        // Drain stale steers BEFORE run — mirrors the host calling this
        // before the turn starts (production: handle_send_message start).
        executor.drain_stale_steers().await;

        let reason = executor
            .run(&mut history, "fresh start".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(mock.requests().len(), 1);
        // The stale steers must NOT appear in the request or transcript.
        let reqs = mock.requests();
        assert!(
            !reqs[0].iter().any(|m| {
                m.content.iter().any(|b| matches!(b,
                    ContentBlock::Text { text, .. } if text.contains("stale steer")))
            }),
            "stale steers must be discarded, not injected"
        );
        assert_eq!(
            history.len(),
            2,
            "[user(seed), assistant] — no steer messages"
        );
    }

    // === usage tracking (slice 21 §E) ======================================
    //
    // Restores the token-counter that the §E wire-in cutover (slice 20) dropped:
    // `reduce_stream` now seeds per-stream usage on `MessageStart` and replaces
    // it on each `MessageDelta` (latest cumulative wins), and `run_inner`
    // accumulates across steps into the executor's `usage` field, which the
    // host harvests via `take_usage`. These tests pin the three semantics:
    //   - within a stream: REPLACE (MessageStart seeds, MessageDelta overwrites)
    //   - across steps:    ADD (each step's final usage sums into the turn total)
    //   - Empty → retry:   DROP (a stream that dies before content contributes
    //     no usage — usage threads through `Content`, not `Empty`).

    #[tokio::test]
    async fn usage_captures_message_start_and_delta_within_a_stream() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // One stream: MessageStart(input=100) + text + MessageDelta(cumulative
        // input=100, output=50). After reduction the delta's usage replaces the
        // MessageStart seed, so the round reports {input:100, output:50}.
        let mock = Arc::new(MockLlm::new(vec![usage_round(100, 50)]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None, None, None, None, None, None, None, None, None,
        );
        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        let usage = executor.take_usage();
        assert_eq!(usage.input_tokens, 100, "MessageStart input captured");
        assert_eq!(usage.output_tokens, 50, "MessageDelta output captured");
    }

    #[tokio::test]
    async fn usage_accumulates_across_multiple_steps() {
        // A tool-call roundtrip spans two streams, so usage accrues across
        // steps (ADD), not just within one (REPLACE).
        let mut registry = ToolRegistry::new(ToolContext::new(PathBuf::from("/tmp/ws")));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // Stream 1: text + tool_use(echo); MessageStart(100) / Delta(100, 50).
        let mut call1 = vec![message_start_with_usage(100)];
        call1.extend(text_block(0, "calling echo"));
        call1.extend(tool_use_block(1, "call_1", "echo", r#"{"text":"hi"}"#));
        call1.extend(finish_with_usage("tool_use", 100, 50));
        // Stream 2: text + end; MessageStart(120) / Delta(120, 30).
        let call2 = usage_round(120, 30);
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None, None, None, None, None, None, None, None, None,
        );
        let reason = executor
            .run(&mut history, "echo hi".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Across steps: 100+120 input, 50+30 output.
        let usage = executor.take_usage();
        assert_eq!(usage.input_tokens, 220, "100 + 120 across two steps");
        assert_eq!(usage.output_tokens, 80, "50 + 30 across two steps");
    }

    #[tokio::test]
    async fn usage_replaces_within_stream_keeps_latest_delta() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        // One stream with two MessageDelta usage events (provider sends a
        // partial cumulative 30, then the final 70). Within a stream the latest
        // delta wins (REPLACE), so output=70 — not 30+70=100.
        let mut call = vec![message_start_with_usage(0)];
        call.extend(text_block(0, "answer"));
        call.push(StreamEvent::MessageDelta {
            delta: MessageDelta {
                stop_reason: None,
                stop_sequence: None,
            },
            usage: Some(Usage {
                input_tokens: 0,
                output_tokens: 30,
                ..Usage::default()
            }),
        });
        call.push(StreamEvent::MessageDelta {
            delta: MessageDelta {
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
            },
            usage: Some(Usage {
                input_tokens: 0,
                output_tokens: 70,
                ..Usage::default()
            }),
        });
        call.push(StreamEvent::MessageStop);
        let mock = Arc::new(MockLlm::new(vec![call]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None, None, None, None, None, None, None, None, None,
        );
        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        let usage = executor.take_usage();
        assert_eq!(
            usage.output_tokens, 70,
            "within-stream REPLACE keeps the latest cumulative delta, not 30+70"
        );
    }

    #[tokio::test]
    async fn usage_none_on_clean_stream_is_zero() {
        // Regression guard: the legacy event shape (no MessageStart, usage:None
        // on the MessageDelta — what every existing test double emits) must
        // leave the turn usage at zero. No behavior change for these shapes.
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mock = Arc::new(MockLlm::new(vec![end_call()]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None, None, None, None, None, None, None, None, None,
        );
        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        let usage = executor.take_usage();
        assert_eq!(usage.input_tokens, 0, "no MessageStart ⇒ no input");
        assert_eq!(usage.output_tokens, 0, "usage:None ⇒ no output");
    }

    #[tokio::test]
    async fn usage_empty_retry_drops_failed_attempt_usage() {
        // A stream that yields only MessageStart(input=100) then dies mid-flight
        // produces an Empty outcome (MessageStart doesn't flip
        // any_content_received, so no content ⇒ Empty ⇒ transparent retry).
        // Empty carries no usage, so the failed attempt's input=100 is dropped —
        // usage threads through Content, not Empty. The retry's clean round
        // (input=200, output=60) is what the host harvests.
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let mock = Arc::new(MockLlm::with_rounds(vec![
            MockRound::EventsThenErr(
                vec![message_start_with_usage(100)],
                "connection reset".to_string(),
            ),
            MockRound::Events(usage_round(200, 60)),
        ]));
        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            None, None, None, None, None, None, None, None, None,
        );
        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run recovers via transparent retry");
        assert_eq!(reason, StopReason::NoToolCalls);
        // The failed attempt was issued, then retried: two stream calls.
        assert_eq!(mock.requests().len(), 2, "failed attempt + one retry");

        let usage = executor.take_usage();
        assert_eq!(
            usage.input_tokens, 200,
            "failed attempt's MessageStart(100) dropped; only the clean round's 200 kept"
        );
        assert_eq!(usage.output_tokens, 60);
    }

    // === turn_meta enrichment (slice 22 §E) =================================

    /// Build a `TurnMetaProbe` from a session, mirroring the production wire-in
    /// at `engine/mod.rs` (`Arc::clone` of the working set + snapshot of the
    /// model-routing fields). `skills_dir` is a nonexistent path so no
    /// conditional-skills block is emitted — these tests assert on `<turn_meta>`
    /// presence + working-set summary, not skill matching.
    fn turn_meta_probe(sess: &Session) -> TurnMetaProbe {
        TurnMetaProbe::new(
            Arc::clone(&sess.working_set),
            sess.workspace.clone(),
            PathBuf::from("/nonexistent-codesmith-skills-test"),
            sess.model.clone(),
            sess.auto_model,
            sess.reasoning_effort.clone(),
            sess.reasoning_effort_auto,
        )
    }

    #[tokio::test]
    async fn turn_meta_probe_enrich_wraps_text_and_observe_increments_turn() {
        let sess = fresh_session();
        let probe = turn_meta_probe(&sess);

        // Fresh session ⇒ working set at turn 0, no entries.
        assert_eq!(sess.working_set.lock().expect("poisoned").turn, 0);

        // observe_user_message records the turn (mirrors production's steer
        // observe) — increments `turn` even though "hello there" carries no
        // path tokens.
        probe.observe_user_message("hello there");
        assert_eq!(
            sess.working_set.lock().expect("poisoned").turn,
            1,
            "observe must increment the shared working set's turn"
        );

        // enrich wraps the text in a 2-block user message: `<turn_meta>` block
        // first, then the raw text.
        let msg = probe.enrich_user_text_message("steer body".to_string());
        assert_eq!(msg.role.as_str(), "user");
        assert_eq!(msg.content.len(), 2, "enriched message has turn_meta + text");
        match (&msg.content[0], &msg.content[1]) {
            (ContentBlock::Text { text: meta, .. }, ContentBlock::Text { text: body, .. }) => {
                assert!(meta.contains("<turn_meta>"), "first block is turn_meta: {meta}");
                assert!(meta.contains("</turn_meta>"));
                assert!(meta.contains("Current local date"));
                assert_eq!(body, "steer body", "second block is the raw text");
            }
            other => panic!("expected [turn_meta Text, body Text], got {other:?}"),
        }
    }

    #[tokio::test]
    async fn steer_drain_enriches_with_turn_meta_and_observes() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        // Build the probe BEFORE the `&mut sess` borrow held by the history
        // (production takes the `Arc::clone` before `SessionChatHistory::new`).
        let probe = turn_meta_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();
        tx_steer.send("remember this".to_string()).await.unwrap();

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
            None,
            None,
            None,
        )
        .with_turn_meta(Some(probe));

        let reason = executor
            .run(&mut history, "start".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Layout: [user(seed "start" plain), user(steer enriched), assistant].
        assert_eq!(history.len(), 3);
        // The seed message stays plain (single Text block) — only steer /
        // LSP-flush pushes are enriched, not the executor's own seed push.
        assert_eq!(sess.messages[0].role.as_str(), "user");
        assert_eq!(
            sess.messages[0].content.len(),
            1,
            "seed push is NOT enriched (only steer/LSP pushes are)"
        );
        match &sess.messages[0].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "start"),
            other => panic!("expected plain seed Text, got {other:?}"),
        }
        // The steer message is enriched: `<turn_meta>` block + raw steer text.
        assert_eq!(sess.messages[1].role.as_str(), "user");
        assert_eq!(sess.messages[1].content.len(), 2, "steer is enriched (2 blocks)");
        match (&sess.messages[1].content[0], &sess.messages[1].content[1]) {
            (ContentBlock::Text { text: meta, .. }, ContentBlock::Text { text: body, .. }) => {
                assert!(meta.contains("<turn_meta>"), "steer wrapped: {meta}");
                assert_eq!(body, "remember this", "raw steer text is the second block");
            }
            other => panic!("expected [turn_meta, steer] Text blocks, got {other:?}"),
        }

        // The steer was observed against the shared working set (turn >= 1).
        assert!(
            sess.working_set.lock().expect("poisoned").turn >= 1,
            "steer observe must increment the working set's turn"
        );

        // The model saw the `<turn_meta>` block in its (only) request.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 1);
        let saw_turn_meta = reqs[0].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text.contains("<turn_meta>"))
            })
        });
        assert!(saw_turn_meta, "request must include the steer's <turn_meta>: {reqs:?}");
    }

    #[tokio::test]
    async fn lsp_flush_enriches_with_turn_meta() {
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EditSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let fake = FakeLsp::returning(error_diag_block("foo.rs", 12, 8, "missing semicolon"));
        let probe = LspProbe::new(fake.clone(), tmp.path().to_path_buf());

        let mut sess = fresh_session();
        let turn_meta = turn_meta_probe(&sess);
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
            None,
            None,
            None,
        )
        .with_turn_meta(Some(turn_meta));

        let reason = executor
            .run(&mut history, "edit foo.rs".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The diagnostics flush message is enriched: `<turn_meta>` then
        // `<diagnostics`. Find it by content (robust to layout shifts).
        let diag_msg = sess
            .messages
            .iter()
            .find(|m| {
                m.content.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text, .. } if text.contains("<diagnostics"))
                })
            })
            .expect("diagnostics message present");
        assert_eq!(diag_msg.role.as_str(), "user");
        assert_eq!(
            diag_msg.content.len(),
            2,
            "diagnostics flush is enriched (turn_meta + diagnostics)"
        );
        match (&diag_msg.content[0], &diag_msg.content[1]) {
            (ContentBlock::Text { text: meta, .. }, ContentBlock::Text { text: diag, .. }) => {
                assert!(meta.contains("<turn_meta>"), "diagnostics wrapped: {meta}");
                assert!(diag.contains("<diagnostics"));
                assert!(diag.contains("missing semicolon"));
            }
            other => panic!("expected [turn_meta, diagnostics] Text blocks, got {other:?}"),
        }

        // LSP flush is enrich-only — no observe (working set turn stays 0).
        assert_eq!(
            sess.working_set.lock().expect("poisoned").turn,
            0,
            "LSP flush must NOT observe the working set"
        );
    }

    #[tokio::test]
    async fn subagent_sentinel_stays_plain_under_turn_meta() {
        // Regression guard: even with a TurnMetaProbe wired in, the subagent
        // completion sentinel push is NOT enriched (matches production — the
        // sentinel is a runtime-event marker, not user intent).
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let turn_meta = turn_meta_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_sub, rx_sub) = subagent_channel();
        tx_sub.send(completion("child-a finished")).unwrap();

        let call1 = {
            let mut c = text_block(0, "let me wait for children");
            c.extend(finish("end_turn"));
            c
        };
        let call2 = {
            let mut c = text_block(0, "resuming, all done");
            c.extend(finish("end_turn"));
            c
        };
        let mock = Arc::new(MockLlm::new(vec![call1, call2]));

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
            Some(rx_sub),
            None,
            None,
        )
        .with_turn_meta(Some(turn_meta));

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Two stream rounds — the turn resumed after draining the sentinel.
        assert_eq!(mock.requests().len(), 2, "sentinel drain resumed the turn");

        // The sentinel message is present and PLAIN: a single Text block with
        // the runtime-event payload, NO `<turn_meta>` block anywhere.
        let sentinel_msgs: Vec<&Message> = sess
            .messages
            .iter()
            .filter(|m| {
                m.content.iter().any(|b| {
                    matches!(b, ContentBlock::Text { text, .. } if text
                        .contains("kind=\"subagent_completion\""))
                })
            })
            .collect();
        assert_eq!(sentinel_msgs.len(), 1, "expected 1 sentinel message");
        let sentinel = sentinel_msgs[0];
        assert_eq!(sentinel.role.as_str(), "user");
        assert_eq!(
            sentinel.content.len(),
            1,
            "sentinel is plain (single Text block), not enriched"
        );
        for b in &sentinel.content {
            if let ContentBlock::Text { text, .. } = b {
                assert!(
                    !text.contains("<turn_meta>"),
                    "sentinel must NOT carry a <turn_meta> block: {text}"
                );
            }
        }
    }

    #[tokio::test]
    async fn turn_meta_reflects_working_set_summary() {
        let sess = fresh_session();
        // Seed a path into the working set (mirrors production observing an
        // earlier user message) so the `<turn_meta>` summary is non-empty.
        sess.working_set
            .lock()
            .expect("poisoned")
            .observe_user_message("please inspect src/lib.rs", &sess.workspace);
        let probe = turn_meta_probe(&sess);

        // The seeded path surfaces in the working-set summary inside `<turn_meta>`.
        let msg = probe.enrich_user_text_message("now fix the bug".to_string());
        assert_eq!(msg.role.as_str(), "user");
        assert_eq!(msg.content.len(), 2);
        match &msg.content[0] {
            ContentBlock::Text { text: meta, .. } => {
                assert!(meta.contains("<turn_meta>"));
                assert!(
                    meta.contains("Repo Working Set"),
                    "<turn_meta> must carry the working-set summary: {meta}"
                );
                assert!(
                    meta.contains("src/lib.rs"),
                    "<turn_meta> must name the seeded path: {meta}"
                );
            }
            other => panic!("expected turn_meta Text block, got {other:?}"),
        }
        match &msg.content[1] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "now fix the bug"),
            other => panic!("expected raw text block, got {other:?}"),
        }
    }

    // === mid-stream steer buffer drain (slice 23 §E) =========================

    /// A steer arriving *during* streaming (after the pre-request drain ran)
    /// is buffered by `reduce_stream`'s `try_recv` and flushed post-stream when
    /// the model returns no tool calls — the turn resumes so the model sees the
    /// steer on the next request. Without the mid-stream buffer, this steer
    /// would be discarded by the next turn's stale drain.
    #[tokio::test]
    async fn mid_stream_steer_buffered_and_flushed_on_no_tool_calls() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();

        // Round 1: text + end_turn (no tool calls → post-stream flush triggers).
        // Round 2: text + end_turn (the resume after flush).
        let mut round1 = text_block(0, "partial answer");
        round1.extend(finish("end_turn"));
        let mut round2 = text_block(0, "final answer");
        round2.extend(finish("end_turn"));
        let mock = Arc::new(
            MockLlm::new(vec![round1, round2])
                .with_steer_on_stream(tx_steer, "mid-stream steer".to_string()),
        );

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
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "start".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Transcript: [user("start"), assistant("partial"), user(steer), assistant("final")].
        assert_eq!(history.len(), 4);
        assert_eq!(sess.messages[2].role.as_str(), "user");
        match &sess.messages[2].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "mid-stream steer"),
            other => panic!("expected steer Text, got {other:?}"),
        }

        // 2 stream calls (round 1 + resume). Request 2 must contain the steer.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2);
        let saw_steer = reqs[1].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text == "mid-stream steer")
            })
        });
        assert!(saw_steer, "request 2 must contain the mid-stream steer: {reqs:?}");
    }

    /// A steer arriving during streaming with tool calls is flushed after tool
    /// execution (the post-tool flush site) — so the model sees it on the next
    /// step's request.
    #[tokio::test]
    async fn mid_stream_steer_buffered_and_flushed_after_tool_execution() {
        let tmp = tempdir().expect("tempdir");
        let mut registry = ToolRegistry::new(ToolContext::new(tmp.path().to_path_buf()));
        registry.register(Arc::new(EchoSpec));
        let tools = Arc::new(registry.to_framework_tool_set());

        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();

        // Round 1: text + tool_use(echo) + tool_use stop. Round 2: text + end_turn.
        let mut round1 = text_block(0, "running echo");
        round1.extend(tool_use_block(1, "t1", "echo", r#"{"text":"world"}"#));
        round1.extend(finish("tool_use"));
        let mut round2 = text_block(0, "done");
        round2.extend(finish("end_turn"));
        let mock = Arc::new(
            MockLlm::new(vec![round1, round2])
                .with_steer_on_stream(tx_steer, "mid-stream steer".to_string()),
        );

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
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "start".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Transcript: [user("start"), assistant, user(tool_result), user(steer), assistant("done")].
        assert_eq!(history.len(), 5);
        // The steer is pushed AFTER the tool result (post-tool flush).
        assert_eq!(sess.messages[3].role.as_str(), "user");
        match &sess.messages[3].content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "mid-stream steer"),
            other => panic!("expected steer Text, got {other:?}"),
        }

        // Request 2 must contain the steer.
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2);
        let saw_steer = reqs[1].iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::Text { text, .. } if text == "mid-stream steer")
            })
        });
        assert!(saw_steer, "request 2 must contain the mid-stream steer: {reqs:?}");
    }

    /// A mid-stream-buffered steer emits "Steer input queued:" (distinct from
    /// the pre-request drain's "Steer input accepted:") — the status signals
    /// to the user that the steer will be processed after the current stream.
    #[tokio::test]
    async fn mid_stream_steer_emits_queued_status() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let (tx, mut rx) = mpsc::channel(256);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();

        let mut round1 = text_block(0, "partial");
        round1.extend(finish("end_turn"));
        let mut round2 = text_block(0, "final");
        round2.extend(finish("end_turn"));
        let mock = Arc::new(
            MockLlm::new(vec![round1, round2])
                .with_steer_on_stream(tx_steer, "queued steer".to_string()),
        );

        let executor = HostAgentExecutor::new(
            mock.clone(),
            tools,
            callback,
            AgentExecutorConfig::default(),
            Some(tx),
            None,
            Some(rx_steer),
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

        let all_statuses = statuses(&drain(&mut rx));
        let queued: Vec<_> = all_statuses
            .iter()
            .filter(|s| s.contains("Steer input queued"))
            .cloned()
            .collect();
        assert_eq!(
            queued.len(),
            1,
            "one 'queued' status for the mid-stream steer: {queued:?}"
        );
        assert!(
            queued[0].contains("queued steer"),
            "status previews the steer text: {queued:?}"
        );
        // No "accepted" status — the steer was never pre-request-drained.
        let accepted: Vec<_> = all_statuses
            .iter()
            .filter(|s| s.contains("Steer input accepted"))
            .cloned()
            .collect();
        assert!(
            accepted.is_empty(),
            "mid-stream steer must emit 'queued' not 'accepted': {accepted:?}"
        );
    }

    /// A mid-stream-buffered steer is enriched with `<turn_meta>` and observed
    /// against the shared working set when a `TurnMetaProbe` is present —
    /// matching production's `observe_user_message` + `enrich` for steer pushes.
    #[tokio::test]
    async fn mid_stream_steer_enriched_with_turn_meta() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        // Build the probe BEFORE the `&mut sess` borrow held by the history.
        let probe = turn_meta_probe(&sess);
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();

        let mut round1 = text_block(0, "partial");
        round1.extend(finish("end_turn"));
        let mut round2 = text_block(0, "final");
        round2.extend(finish("end_turn"));
        let mock = Arc::new(
            MockLlm::new(vec![round1, round2])
                .with_steer_on_stream(tx_steer, "mid-stream steer".to_string()),
        );

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
            None,
            None,
            None,
        )
        .with_turn_meta(Some(probe));

        let reason = executor
            .run(&mut history, "start".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // The mid-stream steer message (position 2) is enriched: 2 blocks.
        assert_eq!(sess.messages[2].role.as_str(), "user");
        assert_eq!(
            sess.messages[2].content.len(),
            2,
            "mid-stream steer is enriched (2 blocks)"
        );
        match (&sess.messages[2].content[0], &sess.messages[2].content[1]) {
            (ContentBlock::Text { text: meta, .. }, ContentBlock::Text { text: body, .. }) => {
                assert!(meta.contains("<turn_meta>"), "steer wrapped: {meta}");
                assert_eq!(body, "mid-stream steer");
            }
            other => panic!("expected [turn_meta, steer] Text blocks, got {other:?}"),
        }

        // The steer was observed against the shared working set (turn >= 1).
        assert!(
            sess.working_set.lock().expect("poisoned").turn >= 1,
            "mid-stream steer observe must increment the working set's turn"
        );
    }

    /// Empty / whitespace-only steers arriving during streaming are skipped
    /// (not buffered, no extra user message).
    #[tokio::test]
    async fn mid_stream_steer_empty_skipped() {
        let tools = Arc::new(ToolSet::new());
        let mut sess = fresh_session();
        let mut history = SessionChatHistory::new(&mut sess);
        let callback: Arc<dyn Callback> = Arc::new(codesmith_agent::callback::NoopCallback);

        let (tx_steer, rx_steer) = steer_channel();

        let mut round1 = text_block(0, "answer");
        round1.extend(finish("end_turn"));
        let mock = Arc::new(
            MockLlm::new(vec![round1])
                .with_steer_on_stream(tx_steer, "   ".to_string()),
        );

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
            None,
            None,
            None,
        );

        let reason = executor
            .run(&mut history, "go".to_string())
            .await
            .expect("run");
        assert_eq!(reason, StopReason::NoToolCalls);

        // Transcript: [user("go"), assistant("answer")] — no steer message.
        assert_eq!(
            history.len(),
            2,
            "empty steer must not produce an extra user message"
        );
    }
}
