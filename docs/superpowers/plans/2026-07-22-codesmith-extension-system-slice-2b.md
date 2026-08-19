# §F2b Host Seam Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task = Red→impl→Green→commit, matching §F2a granularity/style.

**Goal:** Wire the §F2a extension contract into the host: (1) honor `EmitOutcome` (Block/Cancel/Transform) at the 7 existing host_executor seams; (2) emit the 22 not-yet-wired events (`ToolExecutionUpdate` deferred to §F2c); (3) add a full e2e round-trip test; (4)+(5) make `/extension reload` re-discover live on the shared runner Arc.

**Architecture:** §F2b is **host-side-only — it does NOT change the §F2a contract** (`ExtensionEvent` 23 variants, `HandlerOutcome`, `EmitOutcome`, `on_variant`, `catch_unwind` all stay stable). At each seam, inspect `out.outcome` for the capability that seam supports: **Block** = `ToolCall`; **Cancel** = `SessionBefore*`; **Transform** = `Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult`; out-of-place outcomes → `Continue`. New emits bind `let out = runner.emit(...).await` (or `let _ =` for observe-only) so the added `#[must_use]` stays clean. Reload re-runs discover→reconcile→load→bind_core on the **shared Arc** (the only way the App-level handler can update the Engine's field, since App can't reach `&mut Engine`).

**Tech Stack:** Rust 1.90.0; crates `codesmith-agent` (contract, read-only), `codesmith-extensions` (runtime), `codesmith-agent-runtime` (host_executor + engine), `codesmith-tui` (App/commands/reload).

## Confirmed scope decisions
- **22/23 events wired.** `ToolExecutionUpdate` has no host seam (`Callback` lacks `on_tool_progress`); **deferred to §F2c** (needs a `Callback` trait extension). EXTENSIONS.md notes the deferral.
- **`SessionStart`/`SessionShutdown` wired** (observe-only) — closes the §F1 gap (only 4 of the "6 live §F1" events were actually emitted; these 2 were declared but never fired).
- **Path correction:** host_executor is at `crates/agent-runtime/src/engine/host_executor.rs` (not `crates/agent/src/`).

## Baseline (must not regress at slice end)
`codesmith-extensions --lib` 14 · `codesmith-agent --lib` 97 · `codesmith-agent-runtime --lib` 1152+2 · `codesmith-tui --bin codesmith-tui` 2853+2 · grep `.emit(codesmith_agent::extension::ExtensionEvent` =7 (host_executor) · `.emit(&...` =0.

## File Structure (modified)
- `crates/extensions/src/runner.rs` — `#[must_use] EmitOutcome` (T1); `clear_handlers` (T7).
- `crates/agent-runtime/src/engine/host_executor.rs` — T1 (7 seams), T2 (run_inner agent/provider), T3 (tool-exec+compaction), T4 (e2e test).
- `crates/agent-runtime/src/engine/mod.rs` — T5 (AgentSettled/SessionStart/SessionShutdown).
- `crates/tui/src/core/engine.rs` — T7 (extract `reload_extension_runtime` from `build_extension_runtime:357`).
- `crates/tui/src/tui/app.rs` / `ui.rs:561` — T7 (live reload reach).
- `crates/tui/src/commands/extension_commands.rs:123` — T7 (reload re-discovers).
- `crates/tui/src/{core/engine/handle.rs, mcp_server.rs, tui/ui.rs, runtime_threads.rs, core/engine/tool_setup.rs}` — T6.
- `docs/ROADMAP.md`, `docs/ARCHITECTURE.md`, `docs/EXTENSIONS.md` — T8.

## Variant → capability → host-seam map (authored by §F2b; spec had no such table)

| Variant | Capability | Host seam (task) |
|---|---|---|
| `ProjectTrust` | observe | tui trust resolution (T6) |
| `SessionStart` | observe | engine session creation (T5) |
| `ResourcesDiscover` | observe | tui MCP list_resources (T6) |
| `Input` | **transform** (text) | tui submit_user_input (T6) |
| `BeforeAgentStart` | **transform** (system_prompt, inject_message) | host_executor run_inner top (T2) |
| `AgentStart` | observe | host_executor run_inner top (T2) |
| `TurnStart` | observe | host_executor run_inner (existing, T1 binds) |
| `BeforeProviderHeaders` | observe | host_executor pre-request (T2) |
| `BeforeProviderRequest` | **transform** (messages) | host_executor pre-stream (T2) |
| `AfterProviderResponse` | observe | host_executor Content arm (T2) |
| `ToolExecutionStart` | observe | host_executor tool.run closure (T3) |
| `ToolCall` | **block** | host_executor per-tool dispatch (existing, T1 honors) |
| `ToolExecutionUpdate` | observe | **DEFERRED to §F2c** (no Callback::on_tool_progress) |
| `ToolResult` | **transform** (result) | host_executor post-tool (existing, T1 honors) |
| `ToolExecutionEnd` | observe | host_executor tool.run closure (T3) |
| `TurnEnd` | observe | host_executor run_inner returns (existing, T1 binds) |
| `AgentEnd` | observe | host_executor run_inner returns (T2) |
| `AgentSettled` | observe | engine post-run drain (T5) |
| `SessionBeforeSwitch` | **cancel** | tui switch_workspace (T6) |
| `SessionBeforeFork` | **cancel** | tui fork_at_user_message (T6) |
| `SessionShutdown` | observe | engine shutdown (T5) |
| `SessionBeforeCompact` | **cancel** | host_executor run_compaction (T3) |
| `SessionCompact` | observe | host_executor run_compaction (T3) |

---

## Task 1: Honor `EmitOutcome` at the 7 host_executor seams + `#[must_use]`

**Files:**
- Modify: `crates/extensions/src/runner.rs:63` (`#[must_use]` on `EmitOutcome`)
- Modify: `crates/agent-runtime/src/engine/host_executor.rs` seams 3736/3784/4268/4390/4478/4496/4591
- Test: `crates/agent-runtime/src/engine/host_executor.rs` (test module)

- [ ] **Step 1: Write the failing tests**

```rust
// f2b_block_at_toolcall_skips_dispatch — a handler returning Block on ToolCall
// must prevent tool.run; on_tool_end sees a blocked (permission_denied) result.
// f2b_transform_at_toolresult_rewrites_on_tool_end — a handler returning
// Transform(ToolResult{ result: Ok(ToolResult::success("x")) }) must make
// on_tool_end + the downstream outcomes[idx].result see "x".
```
Register a `RecHandler`-style handler whose `handle` returns `HandlerOutcome::Block{..}` on `ExtensionEventKind::ToolCall` (resp. `Transform(...)` on `ToolResult`). Reuse the `MockLlm`+`EchoSpec`+`CallbackBridge`+`with_extension_runner` scaffolding from `extension_runner_bound_emits_lifecycle_events_on_minimal_run` (host_executor.rs:15649). Capture `on_tool_end` args via a `RecordingHookHost`/callback spy; assert dispatch was skipped / result rewritten.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo +1.90.0 test -p codesmith-agent-runtime --lib f2b_`
Expected: FAIL (seams currently discard `EmitOutcome` → Block not honored, Transform not applied).

- [ ] **Step 3: Implement**

- `#[must_use]` on `EmitOutcome` (`runner.rs:63`, before `#[derive(Debug, Clone)]`).
- Observe-only seams (TurnStart 3736, TurnEnd 3784 + 4268): `let _ = runner.emit(...).await;`
- **Block, serial ToolCall (4496):** bind `let out = runner.emit(ToolCall{...}).await;`; if `matches!(out.outcome, HandlerOutcome::Block{..})`, take the `reason`, set `result = Err(ToolError::permission_denied(reason))`, `blocked = true` and skip the approval/`tool.run` branch (mirrors the `guard_result` path at 4511-4512).
- **Block, parallel ToolCall (4390):** in loop-1 (`on_tool_start`+emit, 4384-4399) record a `blocked_by_ext: HashSet<usize>` when `out.outcome` is `Block`; in loop-2 (`futs.push`, 4403-4461) push a blocked-result future for those indices (mirrors the loop-guard blocked path 4414-4419).
- **Transform, ToolResult parallel (4478) + serial (4591):** reorder — emit BEFORE `on_tool_end`; extract transformed result; propagate to `outcomes[idx].result` so Phase-4 sees it:

```rust
// parallel (4469-4487): emit → extract → on_tool_end → update outcomes
let out = runner.emit(ExtensionEvent::ToolResult(ToolResultEvent {
    id: plan.id.clone(), name: plan.name.clone(), result: outcome.result.clone(),
})).await;
let final_result = match out.event {
    ExtensionEvent::ToolResult(tr) => tr.result,
    _ => outcome.result.clone(),  // out-of-place outcome → Continue
};
callback.on_tool_end(&plan.name, &final_result).await;
outcomes[plan.index].as_mut().expect("outcome populated").result = final_result;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo +1.90.0 test -p codesmith-agent-runtime --lib`
Expected: PASS (new `f2b_*` + existing 5 runner tests + minimal-run e2e lifecycle unchanged).

- [ ] **Step 5: Commit**

```
git add crates/extensions/src/runner.rs crates/agent-runtime/src/engine/host_executor.rs
git commit -m "feat(framework): §F2b T1 honor EmitOutcome at 7 host_executor seams (#[must_use] EmitOutcome; Block at ToolCall skips dispatch; Transform at ToolResult reorders on_tool_end + propagates transformed result; observe-only let _=; +2 tests)"
```

---

## Task 2: host_executor agent-lifecycle + provider events (6)

**Files:** `crates/agent-runtime/src/engine/host_executor.rs` run_inner (~3706 top, ~3930-4003 provider block).

**Events/sites:**
- `BeforeAgentStart(AgentStartEvent{system_prompt:None, inject_message:None})` at run_inner top (~3718 after `base`); Transform applies `inject_message` (`history.push` user msg before the user-turn push 3727) + overrides `base` if `system_prompt` set.
- `AgentStart` right after (observe).
- `BeforeProviderHeaders` before `request` build (3931) (observe).
- `BeforeProviderRequest(BeforeProviderRequestEvent{messages: request.messages.clone()})` after `request` built (3949), before `stream_with_transparent_retry` (3985); Transform:
```rust
let out = runner.emit(ExtensionEvent::BeforeProviderRequest(
    BeforeProviderRequestEvent { messages: request.messages.clone() })).await;
if let ExtensionEvent::BeforeProviderRequest(e) = out.event { request.messages = e.messages; }
```
- `AfterProviderResponse(AfterProviderResponseEvent{...})` in `Content` arm after `accumulate_usage` (4003) (observe).
- `AgentEnd` before each `return Ok(...)` in run_inner (3790/3805/3853/4274 + normal end) (observe).

- [ ] **Step 1: Write failing tests** — `f2b_before_agent_start_transform_injects_message_and_system_prompt`; `f2b_before_provider_request_transform_rewrites_messages` (assert MockLlm receives rewritten `messages`).
- [ ] **Step 2: Run to verify fail** — `cargo +1.90.0 test -p codesmith-agent-runtime --lib f2b_before_`
- [ ] **Step 3: Implement** the 6 emit sites above (observe = `let _ =`; the 2 transforms inspect `out.event` + apply the actionable field; out-of-place → no-op).
- [ ] **Step 4: Run to verify pass** — `cargo +1.90.0 test -p codesmith-agent-runtime --lib`
- [ ] **Step 5: Commit** — `feat(framework): §F2b T2 wire agent-lifecycle + provider events in host_executor (AgentStart/BeforeAgentStart[transform]/BeforeProviderHeaders/BeforeProviderRequest[transform]/AfterProviderResponse/AgentEnd; +2 transform tests)`

---

## Task 3: host_executor tool-execution + compaction events (4)

**Files:** `crates/agent-runtime/src/engine/host_executor.rs` tool closures (~4413-4460 parallel, ~4556-4559 serial) + `run_compaction` (~3083-3098).

**Events/sites:**
- `ToolExecutionStart` + `ToolExecutionEnd` inside the `async move` closure (parallel) / around `tool.run` (serial) — capture `extension: Option<Arc<ExtensionRunner>>` into the closure; `let _ = runner.emit(ToolExecutionStart).await;` before `tool.run`, `let _ = runner.emit(ToolExecutionEnd).await;` after.
- `SessionBeforeCompact` inside `run_compaction` after the `should_compact` gate (3083), before `compact_messages_safe` (3087); Cancel → `return`:
```rust
let out = runner.emit(ExtensionEvent::SessionBeforeCompact).await;
if matches!(out.outcome, HandlerOutcome::Cancel { .. }) { return; }  // skip compaction
```
- `SessionCompact` after the summary is applied (Ok arm after 3098) (observe).

- [ ] **Step 1: Write failing tests** — `f2b_tool_execution_start_end_bracket_tool_run`; `f2b_session_before_compact_cancel_skips_compaction`; `f2b_session_compact_fires_after_summary` (force compaction w/ large transcript).
- [ ] **Step 2: Run to verify fail** — `cargo +1.90.0 test -p codesmith-agent-runtime --lib f2b_tool_execution f2b_session_before_compact f2b_session_compact`
- [ ] **Step 3: Implement** the 4 emit sites above.
- [ ] **Step 4: Run to verify pass** — `cargo +1.90.0 test -p codesmith-agent-runtime --lib`
- [ ] **Step 5: Commit** — `feat(framework): §F2b T3 wire tool-execution + compaction events (ToolExecutionStart/End bracket tool.run; SessionBeforeCompact[cancel] gates compaction; SessionCompact after summary; +3 tests)`

---

## Task 4: full e2e round-trip test (item 3)

**Files:** `crates/agent-runtime/src/engine/host_executor.rs:15649` (extend `extension_runner_bound_emits_lifecycle_events_on_minimal_run` + new `f2b_full_lifecycle_ordered_events`).

- [ ] **Step 1: Write failing test** — extend `RecHandler` match to cover new variant labels; assert the full ordered host lifecycle for a 2-call minimal run (call1=tool_use echo, call2=end_turn):
`[BeforeAgentStart, AgentStart, TurnStart, BeforeProviderHeaders, BeforeProviderRequest, AfterProviderResponse, ToolCall, ToolExecutionStart, ToolExecutionEnd, ToolResult, BeforeProviderHeaders, BeforeProviderRequest, AfterProviderResponse, TurnEnd, AgentEnd]` (15 events). Reuse `MockLlm`+`EchoSpec`+`CallbackBridge`+`with_extension_runner` scaffolding verbatim. (Compact events excluded — covered by T3's forced-compaction tests.)
- [ ] **Step 2: Run to verify fail** — `cargo +1.90.0 test -p codesmith-agent-runtime --lib f2b_full_lifecycle`
- [ ] **Step 3: Implement** the assertion (events are emitted by T1-T3; this task only writes/extends the test). If an event is missing/out-of-order, fix the emit site in T2/T3.
- [ ] **Step 4: Run to verify pass** — `cargo +1.90.0 test -p codesmith-agent-runtime --lib extension_runner_bound_emits f2b_full_lifecycle`
- [ ] **Step 5: Commit** — `feat(framework): §F2b T4 full e2e round-trip (extend minimal-run test to assert complete ordered host_executor lifecycle — 15 events across 2 calls)`

---

## Task 5: engine-level events — `AgentSettled` + `SessionStart` + `SessionShutdown`

**Files:** `crates/agent-runtime/src/engine/mod.rs` (post-run drain ~1425; session start/shutdown sites — located in Step 1).

**Events/sites:**
- `AgentSettled` at end of post-run drain block (~1425, after capacity-decision apply) (observe, `let _ =`).
- `SessionStart` at session/engine creation (observe).
- `SessionShutdown` at engine shutdown/drop path (observe).

- [ ] **Step 1: Locate** the session-creation + engine-shutdown sites in `engine/mod.rs`; write failing tests `f2b_agent_settled_fires_post_run`; `f2b_session_start_shutdown_bracket_session_lifecycle` (engine-level harness).
- [ ] **Step 2: Run to verify fail.**
- [ ] **Step 3: Implement** the 3 emit sites (all observe-only `let _ =`).
- [ ] **Step 4: Run to verify pass** — `cargo +1.90.0 test -p codesmith-agent-runtime --lib f2b_agent_settled f2b_session_start f2b_session_shutdown`
- [ ] **Step 5: Commit** — `feat(framework): §F2b T5 wire engine-level events (AgentSettled post-run; SessionStart/SessionShutdown — closes the §F1 unwired gap; +2 tests)`

---

## Task 6: tui-level events (5)

**Files:** `tui/core/engine/tool_setup.rs:44` (ProjectTrust), `tui/mcp_server.rs:237` (ResourcesDiscover), `tui/core/engine/handle.rs:86` (Input), `tui/tui/ui.rs:5619` (SessionBeforeSwitch), `tui/runtime_threads.rs:1305` (SessionBeforeFork).

**Events/sites (emit via `app.extension_runner`):**
- `ProjectTrust` at trust resolution; `ResourcesDiscover` at MCP `list_resources` (observe, `let _ =`).
- `Input(InputEvent{text})` at `submit_user_input`; Transform applies rewritten `text`.
- `SessionBeforeSwitch` at `switch_workspace` entry; Cancel → abort (return early).
- `SessionBeforeFork` at `fork_at_user_message` entry; Cancel → abort.

- [ ] **Step 1: Write failing tests** — `f2b_input_transform_rewrites_submitted_text`; `f2b_session_before_switch_cancel_aborts`; `f2b_session_before_fork_cancel_aborts` (per-site tui harness).
- [ ] **Step 2: Run to verify fail** — `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui f2b_`
- [ ] **Step 3: Implement** the 5 emit sites (observe = `let _ =`; Input Transform + 2 Cancels inspect `out`).
- [ ] **Step 4: Run to verify pass** — `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui f2b_`
- [ ] **Step 5: Commit** — `feat(framework): §F2b T6 wire tui-level events (ProjectTrust/ResourcesDiscover observe; Input[transform] rewrites submitted text; SessionBeforeSwitch/SessionBeforeFork[cancel] abort; +3 tests)`

---

## Task 7: live reload — App field + `/extension reload` re-discover (items 4+5)

**Files:** `crates/extensions/src/runner.rs` (new `clear_handlers`); `crates/tui/src/core/engine.rs:357` (extract `reload_extension_runtime`); `crates/tui/src/commands/extension_commands.rs:123`; `crates/tui/src/tui/app.rs` + `ui.rs:561`.

**Rationale:** App holds `extension_runner`(Arc)+`extension_state`+`workspace` but NOT `Engine`/`cancel_token`; `engine/mod.rs:1312` clones `self.extension_runner` at build. So swapping App's Arc can't update the Engine's field → **must re-populate the shared Arc** (all holders auto-update). `bind_core` appends to `handlers` (Vec) without clearing → need a clear.

- [ ] **Step 1: Verify cancel_token reachability** from the `/extension reload` handler; if App lacks it, store it (or a small `ExtensionReloadHandle` capturing cancel_token+generation_arc) on App at `build_engine` (`engine.rs:443`). Write failing test `f2b_extension_reload_re_discovers_handlers` (register an extension post-build, reload, assert new handler bound + `generation` bumps; existing handler replaced not duplicated).
- [ ] **Step 2: Run to verify fail.**
- [ ] **Step 3: Implement**
  - Add `pub fn clear_handlers(&self)` to `ExtensionRunner` (`runner.rs`) — `self.handlers.lock().unwrap().clear();` (runtime lifecycle method, NOT a contract change).
  - Extract `reload_extension_runtime(runner: &Arc<ExtensionRunner>, workspace, state: &ExtensionStateStore, cancel_token)` from `build_extension_runtime`: `runner.clear_handlers()` → `discover_static()` → reconcile against `state` → `load` each → `bind_core(fresh HostExtensionContext)`. `build_extension_runtime` calls it on a fresh runner.
  - Rewire `reload(app)` (`extension_commands.rs:123`) to call `reload_extension_runtime(app.extension_runner, &app.workspace, &app.extension_state, <cancel_token>)` instead of `runner.invalidate()`.
- [ ] **Step 4: Run to verify pass** — `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui f2b_extension_reload`
- [ ] **Step 5: Commit** — `feat(framework): §F2b T7 live reload (ExtensionRunner::clear_handlers; extract reload_extension_runtime; /extension reload re-discovers+re-loads+re-binds on shared runner Arc → App.extension_runner + Engine field live; +1 test)`

---

## Task 8: docs

**Files:** `docs/ROADMAP.md` (§F2b entry), `docs/ARCHITECTURE.md` (§F status row), `docs/EXTENSIONS.md` (host-seam mapping table per variant + outcome semantics + `#[must_use]` + `ToolExecutionUpdate`→§F2c deferral note).

- [ ] **Step 1: Implement** the doc updates (mirror §F2a T9 docs style).
- [ ] **Step 2: Commit** — `feat(framework): §F2b T8 docs (ROADMAP §F2b entry + ARCHITECTURE §F status + EXTENSIONS host-seam mapping/#[must_use]/ToolExecutionUpdate→§F2c deferral)`

---

## Task 9: verification gate (no commit)

- [ ] **Step 1: Full build**

```
cargo +1.90.0 build --workspace
```
Expected: green (142 tui warnings = slice-47 baseline, non-new).

- [ ] **Step 2: Four test suites**

```
cargo +1.90.0 test -p codesmith-extensions --lib          # ≥14
cargo +1.90.0 test -p codesmith-agent --lib               # ≥97
cargo +1.90.0 test -p codesmith-agent-runtime --lib       # ≥1152+2
cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui   # ≥2853+2
```
Expected: all green; no baseline regressions (new tests added on top).

- [ ] **Step 3: grep verifications**

```
grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs  # >7 (new emits added)
grep -c '\.emit(&codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs # 0
```

---

## Self-review (spec/brief coverage)
- **Item 1** (honor EmitOutcome at 7 seams) → **T1** ✓
- **Item 2** (wire ~17 new events) → T2(6)+T3(4)+T5(3)+T6(5) = 18 new + SessionStart/SessionShutdown (T5, §F1 gap) = **22 live** (ToolExecutionUpdate deferred, T8 notes) ✓
- **Item 3** (full e2e) → **T4** ✓
- **Item 4** (App field live) → **T7** ✓
- **Item 5** (reload re-discover) → **T7** ✓
- Variant capability honors: Block=ToolCall(T1), Cancel=SessionBefore*(T3 compact + T6 switch/fork), Transform=Input/BeforeAgentStart/BeforeProviderRequest/ToolResult(T1,T2,T6) ✓; out-of-place→Continue enforced via the `match _ => original` fallthrough ✓
- No contract change: `ExtensionEvent`/`HandlerOutcome`/`EmitOutcome`(only `#[must_use]`)/`on_variant`/`catch_unwind` untouched; `clear_handlers` is a runtime lifecycle method, not a contract change ✓
- Commit style mirrors §F2a (`feat(framework): §F2b T<n> …`, mixed CN/EN, file:line refs, `+test`) ✓
