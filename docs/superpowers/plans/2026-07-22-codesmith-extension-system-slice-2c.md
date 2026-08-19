# §F2c Extension System Close-out (Reframed) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task = Red→impl→Green→commit, matching §F2a/§F2b granularity/style.

**Goal:** Close out the §F2 by-design gaps that §F2b deferred here. After a reachability investigation, the original §F2c framing ("fire `ToolExecutionUpdate` at `reduce_stream` `ContentBlockStop`; 3 tui seams structurally unreachable for the stated reasons") was found to rest on three misreads. This slice reframes accordingly: (1) land the `on_tool_progress` `Callback` API surface + correct the `ToolExecutionUpdate` rationale (event stays 22/23 unwired — `Tool::run` is one-shot); (2) make the extension context's cancel `signal()` reflect the engine's live token (Layer 2) + pass the shared token on reload; (3) wire `ProjectTrust` per-turn from `build_turn_dispatcher`/`spawn_subagent` (`FirstLoad` → §F5); (4) correct the `ResourcesDiscover`/`SessionBeforeFork` deferral rationale. `ResourcesDiscover` + `SessionBeforeFork` stay deferred (with corrected rationale).

**Architecture:** §F2c is mostly **host-side wiring + one `HostExtensionContext` storage change**. It does NOT change the `ExtensionEvent` enum (23 variants stable), `HandlerOutcome`, `EmitOutcome`, `on_variant`, or `catch_unwind`. The only contract-adjacent change is `HostExtensionContext`'s internal `signal` storage (`CancellationToken` snapshot → `Arc<Mutex<CancellationToken>>` shared form) — the `ExtensionContext::signal() -> CancellationToken` **trait signature is unchanged** (still returns a snapshot; now it snapshots the *current* inner token at call time). `on_tool_progress` is a defaulted trait method (additive, no break). `ProjectTrust` is a new observe-only emit from the async `HostServices` layer (no sync cascade — `build_tool_context_for` stays sync; its prod callers are async).

**Tech Stack:** Rust 1.90.0; crates `codesmith-agent` (`Callback` trait, `extension` payloads — read-only/additive), `codesmith-extensions` (`HostExtensionContext`), `codesmith-agent-runtime` (test ctx callers + bridge), `codesmith-tui` (engine build/populate/reload, `EngineHost`, `runtime_traits`, App/ui).

## Reachability findings (load-bearing — these correct the §F2b deferral notes)
- **`ToolExecutionUpdate` — premise was wrong.** `reduce_stream` (`agent-runtime/src/engine/host_executor.rs:2110`) reduces the **provider** response stream; its `ContentBlockStop` (2284) fires for model-emitted content blocks (text/thinking/tool_use), NOT tool-execution progress. `Tool::run` (`agent/src/tools/mod.rs:71`) is a **one-shot** future returning `Result<ToolResult,ToolError>` — there is no content-block stream during execution, no `ContentBlockStop`-equivalent. The only tool bracket is the Phase-3 site (4581/4614 parallel, 4727/4809 serial) that already fires `ToolExecutionStart`/`End`. **Root cause = "no streaming `Tool` contract," not "no `Callback::on_tool_progress` hook."** Genuine streaming progress awaits a streaming `Tool` variant (a larger, separate slice).
- **`reload` cancel_token — fixable but deeper; no consumer yet.** The `Engine` is unreachable from `/extension reload` (only `&mut App`; App has no engine/token field). Fresh token minted at `extension_commands.rs:137`. `Engine` has `shared_cancel_token: Arc<StdMutex<CancellationToken>>` (`mod.rs:157`), reset per turn (`mod.rs:1037`) + on cancel (`:554`). BUT `HostExtensionContext` held a **snapshot** `CancellationToken` (`state.rs:24`), so `signal()` (`:56`) returned a stale build-time token. Layer 2: store the shared `Arc` so `signal()` reads the *current* inner. Verified **zero** `.signal()` consumers today → forward-looking infra.
- **`ProjectTrust` — "sync can't await" was a red herring.** `build_tool_context_for` (`tool_setup.rs:31`) is sync, BUT its **production** callers (`build_turn_dispatcher` `runtime_traits.rs:222`, `spawn_subagent` `:152`) are **async** → emit from those callers (no sync cascade). Real blocker: `EngineHost` (`engine.rs:143`) has no `extension_runner` field. `TrustReason` derivable from `session.trust_mode: bool` (Trusted/Untrusted); `FirstLoad` needs the App-level trust *decision* (`app.rs` onboarding gate) → **§F5 trust-prompt site**.
- **`ResourcesDiscover` — "separate process" over-stated.** An in-process site exists: `McpPool::list_resources` (`agent-runtime/src/mcp.rs:2751`) called from the `list_mcp_resources` pseudo-tool dispatch (`:3019`), whose caller `HostAgentExecutor` holds the runner `Arc`. BUT that's the tool seam (already `ToolCall`/`ToolResult`-bracketed) → firing `ResourcesDiscover` there **conflates with tool execution** and `DiscoverReason::{Startup,Manual,Reload}` has no clean mapping. No dedicated Startup/Manual/Reload discover seam with the runner `Arc`; the `tui/mcp_server.rs:237` stdio site (what §F2b picked) IS genuinely out-of-process.
- **`SessionBeforeFork` — "no `RuntimeThreadManager` ctor" is false.** `fork_at_user_message` (`runtime_threads.rs:1304`) IS dead code (true; `#[allow(dead_code)]`, zero non-test callers). BUT tui **does** construct a `RuntimeThreadManager` via `TaskManager::start` (`ui.rs:507`→`task_manager.rs:465`). The live in-TUI backtrack path is `apply_backtrack` (`ui.rs:6922`), where `app.extension_runner` IS in scope — BUT `apply_backtrack` is an in-place **rewind** (`truncate_history_to`/`api_messages.truncate`), NOT a **fork** (new-thread creation). Genuine fork primitives are dead (`fork_at_user_message`) or HTTP-only (`fork_thread`, runtime-api, no `App.extension_runner`). Spec *could* redefine the event to cover rewind — flagged to spec owner, not done here.

## Confirmed scope decisions (you selected ①+②+③+④)
- **① `ToolExecutionUpdate`:** land `on_tool_progress` `Callback` hook (default no-op) + `CallbackSet` fan-out + tests. **Event stays 22/23 unwired** (honest — no streaming Tool). Correct the deferral rationale. No host_executor fire, no `CallbackBridge` arm (no host `Event` variant).
- **② reload shared token:** Layer 2 — `HostExtensionContext` holds the shared `Arc`; `signal()` lock+clone current inner; reload passes the engine's shared token; App carries the shared-Arc field. No handler consumes `signal()` yet — forward-looking.
- **③ `ProjectTrust`:** per-turn wire from `build_turn_dispatcher` (+ `spawn_subagent`); `EngineHost.extension_runner` field; `Trusted`/`Untrusted` from `session.trust_mode`; `FirstLoad` → §F5. Host-wire e2e test deferred (§F2b `SessionBeforeSwitch` precedent — `EngineHost`+`TurnDispatchRequest` scaffolding).
- **④ rationale corrections:** `ResourcesDiscover` + `SessionBeforeFork` stay deferred with corrected rationale; `ProjectTrust` + `ToolExecutionUpdate` rows updated to reflect ③/①.

## Baseline (must not regress at slice end — post-§F2b)
`codesmith-extensions --lib` 14 · `codesmith-agent --lib` 97 · `codesmith-agent-runtime --lib` 1161+2 · `codesmith-tui --bin codesmith-tui` 2855+2 · grep `.emit(codesmith_agent::extension::ExtensionEvent` = 16 (host_executor) · `.emit(&...` = 0.

## File Structure (modified)
- `crates/agent/src/callback/mod.rs` — T1 (`on_tool_progress` trait method + `CallbackSet` fan-out + tests).
- `crates/agent-runtime/src/callback_bridge.rs` — T1 (bridged-gaps note; no new arm).
- `crates/extensions/src/state.rs` — T2 (`HostExtensionContext.signal` → shared-Arc + `#[cfg(test)]`).
- `crates/tui/src/core/engine.rs` — T2 (`build/populate/reload_extension_runtime` signatures; `build_engine:474`), T3 (`EngineHost.extension_runner` field + Default; `build_engine` set).
- `crates/tui/src/commands/extension_commands.rs` — T2 (`reload` passes shared token).
- `crates/tui/src/tui/app.rs` — T2 (`extension_shared_cancel_token` field + init).
- `crates/tui/src/tui/ui.rs` — T2 (`app.extension_shared_cancel_token = Some(engine_handle.cancel_token.clone())` at 561).
- `crates/tui/src/core/engine/runtime_traits.rs` — T3 (`build_turn_dispatcher` + `spawn_subagent` emit).
- `crates/tui/src/core/engine/tests.rs`, `crates/agent-runtime/src/engine/host_executor.rs` (×10), `crates/agent-runtime/src/tools/extension.rs` — T2 (13 test `HostExtensionContext::new` callers updated).
- `crates/agent-runtime/src/engine/host_executor.rs` (test block) — T3 (`ProjectTrust` dispatch test).
- `docs/EXTENSIONS.md`, `ROADMAP.md` — T4.

---

## Task 1: `on_tool_progress` Callback hook + `ToolExecutionUpdate` rationale correction

- [ ] `crates/agent/src/callback/mod.rs` — add `on_tool_progress` to the `Callback` trait (default `noop()`), after `on_tool_end` (177, before `on_step` 180). Signature mirrors `on_tool_start` (159-167): `(&'a self, id: &'a str, name: &'a str, message: &'a str) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>` with `let _ = (id, name, message); noop()`.
- [ ] Same file — add the forwarding impl to `impl Callback for CallbackSet` (mirror `on_tool_start` at 266-278: clone `self.callbacks`, `Box::pin(async move { for cb in &cbs { cb.on_tool_progress(id, name, message).await; } })`).
- [ ] `crates/agent-runtime/src/callback_bridge.rs` — add a one-line entry to the "Bridged vs. documented gaps" table (294-298) noting `on_tool_progress` is unbridged (no host `Event` variant for streaming tool progress; pending a streaming `Tool` contract). No new arm — `CallbackBridge` inherits the default no-op.
- [ ] Tests — `crates/agent/src/callback/mod.rs` `mod tests`: extend `noop_callback_defaults_are_callable` (331) to call `cb.on_tool_progress("t1","echo","step 1").await;`; extend `callback_set_fans_out` (355) `Counter` to override `on_tool_progress` + assert it fires for each member (mirror the atomic-counter pattern).
- [ ] `cargo +1.90.0 test -p codesmith-agent --lib` green (≥97). `cargo +1.90.0 build --workspace` green.
- [ ] Commit: `feat(framework): §F2c T1 on_tool_progress Callback hook (default no-op; API surface for ToolExecutionUpdate; no host_executor fire — Tool::run is one-shot; CallbackSet fan-out + noop/fans_out tests; bridge gap-table note; event stays 22/23 unwired, awaits streaming Tool contract)`.

## Task 2: reload shared `cancel_token` — Layer 2 (`HostExtensionContext` holds the shared-Arc)

- [ ] `crates/extensions/src/state.rs` — field `signal: CancellationToken` (24) → `signal: Arc<Mutex<CancellationToken>>`; `new(... signal: Arc<Mutex<CancellationToken>> ...)` (34-42); `signal(&self)` (56-58) → `self.signal.lock().expect("signal mutex poisoned").clone()`. `ExtensionContext::signal() -> CancellationToken` trait signature unchanged.
- [ ] `crates/extensions/src/state.rs` — add `#[cfg(test)] mod tests` with `host_extension_context_signal_reflects_engine_reset`: build ctx with `Arc::new(Mutex::new(CancellationToken::new()))`; `signal()` → token A; swap `*arc.lock().unwrap() = CancellationToken::new()` (mimic `reset_cancel_token`); `signal()` → token B; `A.cancel()` does not cancel B (proves Layer 2 vs stale snapshot).
- [ ] `crates/tui/src/core/engine.rs` — `build_extension_runtime` (357-366), `populate_extension_runtime` (372-425), `reload_extension_runtime` (436-445): param `cancel_token: CancellationToken` → `shared_cancel_token: Arc<StdMutex<CancellationToken>>`; pass to `HostExtensionContext::new` (417-423); update doc comments (414-415, 433-435 — now shares the engine's live token).
- [ ] Same file — `build_engine` call at 474: `build_extension_runtime(&config.workspace, cancel_token.clone())` → `build_extension_runtime(&config.workspace, shared_cancel_token.clone())`.
- [ ] `crates/tui/src/tui/app.rs` — add field after 1487: `pub extension_shared_cancel_token: Option<std::sync::Arc<std::sync::Mutex<tokio_util::sync::CancellationToken>>>,` (+ doc); init `extension_shared_cancel_token: None,` at 2039.
- [ ] `crates/tui/src/tui/ui.rs` — at 561 add `app.extension_shared_cancel_token = Some(engine_handle.cancel_token.clone());` after the `extension_runner` line.
- [ ] `crates/tui/src/commands/extension_commands.rs` — `reload` (123-144): add guard `let Some(shared) = app.extension_shared_cancel_token.clone() else { return CommandResult::error("Extension runner not bound."); };`; pass `shared` to `reload_extension_runtime` (replacing `CancellationToken::new()` at 137); fix the stale "fresh token" comment (131-132).
- [ ] Update the 13 test `HostExtensionContext::new` callers to pass `Arc::new(std::sync::Mutex::new(CancellationToken::new()))` where they pass `CancellationToken::new()`: `tui/src/core/engine/tests.rs:4121,:4179`; `agent-runtime/src/engine/host_executor.rs:15900,16029,16195,16257,16364,16482,16597,16716,16804,16910`; `agent-runtime/src/tools/extension.rs:120`.
- [ ] `cargo +1.90.0 test -p codesmith-extensions --lib` (14 → 15), `-p codesmith-agent-runtime --lib` (≥1161+2), `-p codesmith-tui --bin codesmith-tui` (≥2855+2). Build green.
- [ ] Commit: `feat(framework): §F2c T2 reload shares engine's live cancel_token (HostExtensionContext.signal: CancellationToken snapshot → Arc<Mutex<CancellationToken>> shared form; signal() lock+clone current inner so per-turn reset_cancel_token is visible at call time; build/populate/reload_extension_runtime take shared-Arc; build_engine:474 passes shared_cancel_token; App.extension_shared_cancel_token field + ui.rs:561 set; reload passes shared token not fresh; 13 test ctx callers updated; +1 test signal_reflects_engine_reset; ExtensionContext::signal() trait sig unchanged; no .signal() consumer yet — forward-looking infra)`.

## Task 3: `ProjectTrust` per-turn wire (partial; `FirstLoad` → §F5)

- [ ] `crates/tui/src/core/engine.rs` `EngineHost` (143-167) — add `pub extension_runner: Option<std::sync::Arc<codesmith_extensions::ExtensionRunner>>,` (+ doc). Update `Default for EngineHost` (169+) → `extension_runner: None,`.
- [ ] Same file `build_engine` — after `let extension_runner = build_extension_runtime(...)` (474) and before `Engine::new_runtime` (674), set `host.extension_runner = Some(extension_runner.clone());` on the owned `mut host` (before wrap at 671).
- [ ] `crates/tui/src/core/engine/runtime_traits.rs` — `build_turn_dispatcher` (222-): after `let tool_context = build_tool_context_for(...)` (239), emit `ProjectTrust { reason: if session.trust_mode { TrustReason::Trusted } else { TrustReason::Untrusted } }` guarded by `if let Some(runner) = &self.extension_runner { let _ = runner.emit(...).await; }` (observe-only `let _ =`). Doc: per-turn; `FirstLoad` not derivable here → §F5; handler wanting once-per-session dedups on first `Trusted`/`Untrusted`.
- [ ] Same file `spawn_subagent` (152-) — after `build_tool_context_for(...)` (166), emit the same using `req.session.trust_mode` (symmetric; distinct context — no double-fire with the main turn).
- [ ] Test — `crates/agent-runtime/src/engine/host_executor.rs` test block (mirror `f2b_*`): register a handler recording `ProjectTrust` + its `TrustReason`; `runner.emit(ProjectTrust{Trusted})`; assert handler saw `Trusted`. (Variant dispatch + reason round-trip; `kind()` round-trip already covered by `f2a`.) Name it `f2c_project_trust_dispatches_reason`.
- [ ] Note in T4 doc: host-wire e2e (`build_turn_dispatcher` emits) deferred per §F2b `SessionBeforeSwitch` precedent (`EngineHost`+`TurnDispatchRequest` scaffolding).
- [ ] `cargo +1.90.0 test -p codesmith-agent-runtime --lib` (≥1161+3), `-p codesmith-tui --bin codesmith-tui` (≥2855+2). Build green. `grep -rn 'ProjectTrust' crates/tui/src/core/engine/runtime_traits.rs` shows the emit.
- [ ] Commit: `feat(framework): §F2c T3 ProjectTrust per-turn wire (EngineHost.extension_runner field + Default None; build_engine sets it at 474; build_turn_dispatcher + spawn_subagent emit ProjectTrust{Trusted/Untrusted from session.trust_mode} after build_tool_context_for — async callers, no sync cascade; FirstLoad → §5 trust prompt; +1 test f2c_project_trust_dispatches_reason; host-wire e2e deferred per §F2b SessionBeforeSwitch precedent)`.

## Task 4: Correct deferral rationale (`ResourcesDiscover`/`SessionBeforeFork`/`ProjectTrust`/`ToolExecutionUpdate`)

- [ ] `docs/EXTENSIONS.md` host-seam table (243-246):
  - `ProjectTrust` (243) — emit site "`HostServices::build_turn_dispatcher` (+ `spawn_subagent`) after `build_tool_context_for`"; effect "per-turn `Trusted`/`Untrusted` from `session.trust_mode`; `FirstLoad` → §F5 trust prompt".
  - `ResourcesDiscover` (244) — corrected rationale (only in-process site = `list_mcp_resources` tool seam, conflates; no dedicated discover seam; `tui/mcp_server.rs` stdio is separate process; "separate process" framing over-stated).
  - `SessionBeforeFork` (245) — corrected rationale (live backtrack `apply_backtrack` is rewind not fork; genuine fork primitives dead/HTTP-only; tui DOES construct `RuntimeThreadManager` via `TaskManager::start` — "no ctor" was wrong; spec-redefine-to-rewind flagged, not done).
  - `ToolExecutionUpdate` (246) — corrected rationale (no streaming `Tool` contract — `Tool::run` one-shot; `on_tool_progress` hook landed T1; emit awaits streaming `Tool` variant; "no hook" was the surface symptom).
- [ ] `ROADMAP.md` — rewrite §F2c gaps block (2509-2513) + progress bullets (2493/2496/2497/2516) to reflect reframed scope; add "进度（§F2c）" block + "### F2c" subsection mirroring §F2b structure.
- [ ] Commit: `feat(framework): §F2c T4 docs (EXTENSIONS host-seam table: ProjectTrust wired per-turn + ResourcesDiscover/SessionBeforeFork/ToolExecutionUpdate rationale corrected to real blockers; ROADMAP §F2c progress block + subsection)`.

## Verification gate (slice end — T9-equivalent, not committed)
- [ ] `cargo +1.90.0 build --workspace` green.
- [ ] `cargo +1.90.0 test -p codesmith-extensions --lib` = 15.
- [ ] `cargo +1.90.0 test -p codesmith-agent --lib` ≥ 97.
- [ ] `cargo +1.90.0 test -p codesmith-agent-runtime --lib` ≥ 1161+3.
- [ ] `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` ≥ 2855+2.
- [ ] `grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs` = 16; `\.emit(&...` = 0.
- [ ] `grep -rn 'ProjectTrust' crates/tui/src/core/engine/runtime_traits.rs` shows the new emit.

## Out of scope (explicitly deferred, documented in T4)
- `ResourcesDiscover` (no clean in-process discover seam; conflates with tool seam).
- `SessionBeforeFork` (rewind ≠ fork; spec redefinition flagged, not done).
- `SessionBeforeSwitch` e2e test (§F2b precedent; TaskManager scaffolding).
- `ToolExecutionUpdate` genuine emit (awaits streaming `Tool` contract — a larger, separate slice).
- `ExtensionContext::signal()` return-type → shared-Arc (deeper contract change; Layer 2 makes call-time current-token sufficient).
