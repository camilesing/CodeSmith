# §F5 Slice 1 — ProjectTrust{FirstLoad} trust-prompt emit site Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task = Red→impl→Green→commit, matching §F2a/§F2b/§F2c granularity/style.

**Goal:** Land the first sub-slice of §F5: the `ProjectTrust { reason: TrustReason::FirstLoad }` emit at the onboarding trust-accept site — the once-per-session signal that extension handlers can observe when the user accepts the workspace trust prompt. This closes the §F2c T3 `FirstLoad → §F5` deferral. The full §F5 dylib loader (`libloading`/`abi_stable`, `extension.toml`, project-local discovery trust gate, `/extension install`/`uninstall`) remains §F5 续作 / §F3+; this slice emits the `FirstLoad` *event* only (no dylib machinery).

**Architecture:** §F5 slice 1 is **one host-side emit site + one additive dispatch test**. It does NOT change the `ExtensionEvent` enum (23 variants stable), `TrustReason` (`FirstLoad` exists since §F2a, `crates/agent/src/extension.rs:156`), `HandlerOutcome`, `EmitOutcome`, `on_variant`, `catch_unwind`, or the §F2c per-turn `Trusted`/`Untrusted` emits. The single load-bearing code change is the `FirstLoad` emit inside the `TrustDirectory` `y/Y/1` accept arm of `run_tui`'s key loop (`tui/ui.rs`), after `app.trust_mode = true` (`:2868`) and before `fire_session_start_hook_if_ready(app)` (`:2879`), mirroring the §F2c T3 `if let Some(runner) = &<host>.extension_runner { let _ = runner.emit(...).await; }` pattern verbatim (`runtime_traits.rs:258-268` / `:170-180`). `FirstLoad` semantics = **prompt-site only**: the emit fires solely on the onboarding `TrustDirectory` acceptance; `/trust on`, YOLO entry, and persisted-trust workspace startup do NOT emit `FirstLoad` (those surface per-turn as `Trusted`/`Untrusted` via §F2c T3). No dedup guard (the onboarding prompt accepts once per session). The tui-level e2e (asserting the emit fires through `run_tui`) is deferred per the §F2b `SessionBeforeSwitch` precedent (TaskManager-class scaffolding); the new behavior is guarded instead by a runner-level dispatch test (`f5_project_trust_first_load_dispatches`) additive to §F2c T3 (which exercised `Trusted`→`Untrusted` only).

**Tech Stack:** Rust 1.90.0; crates `codesmith-tui` (`ui.rs` emit site + `host_executor.rs` test block), `docs/EXTENSIONS.md` + `ROADMAP.md` (T2). `codesmith-extensions`, `codesmith-agent`, `codesmith-agent-runtime` are read-only w.r.t. the contract (no enum/trait change) — only a +1 test in `agent-runtime`.

## Design decisions (load-bearing — finalized in the prior brainstorm; do not re-explore intent/requirements/design)
- **`FirstLoad` = prompt-site only.** The emit fires exclusively at the onboarding `TrustDirectory` accept arm (`tui/ui.rs` `KeyCode::Char('y'|'Y'|'1')` arm, `:2863`). `/trust on` (`config.rs:828`), YOLO init (`app.rs:1939`), YOLO entry (`app.rs:2159`), and persisted-trust workspace startup do NOT fire `FirstLoad` — those set `trust_mode = true` outside the onboarding prompt and surface per-turn as `Trusted`/`Untrusted` (§F2c T3 in `build_turn_dispatcher`/`spawn_subagent`). No dedup guard: the onboarding prompt accepts once per session by construction.
- **Test strategy = runner-level dispatch test + deferred tui e2e.** `f5_project_trust_first_load_dispatches` (mirrors §F2c `f2c_project_trust_dispatches_reason` at `host_executor.rs:16965-17052`, reuses the `ProjectTrustRecorderExt` fixture at `:16995`) asserts `ProjectTrust { FirstLoad }` dispatches to a bound handler and the `FirstLoad` reason round-trips. It is **additive** to §F2c T3 (which tested `Trusted`→`Untrusted` only). It is a regression/characterization guard (green on write — the runner's `ProjectTrust` dispatch is proven by §F2a/§F2c); the genuinely new behavior (the `ui.rs` emit) is covered by the deferred tui e2e per the §F2b `SessionBeforeSwitch` precedent (`EngineHost` + `run_tui`/`TrustDirectory` scaffolding).
- **Approach 1 = inline emit, verbatim mirror of §F2c T3.** No contract change (`FirstLoad` exists since §F2a; this slice only fires it). The emit block is `if let Some(runner) = &app.extension_runner { let _ = runner.emit(ExtensionEvent::ProjectTrust { reason: TrustReason::FirstLoad }).await; }` — the same shape as `runtime_traits.rs:258-268`/`:170-180`, with `app` in place of `self` (`EngineHost`) and `FirstLoad` in place of the per-turn `Trusted`/`Untrusted`.
- **Reachable at emit time.** `spawn_engine` (`ui.rs:556`) + `app.extension_runner = engine_handle.extension_runner.clone()` (`:561`) run before the event loop, so at `TrustDirectory` accept time `app.extension_runner` is `Some`. The `if let Some` guard handles the (impossible-in-prod) `None` case defensively.

## Baseline (must not regress at slice end — post-§F2c, commit 161a5327)
`codesmith-extensions --lib` 15 · `codesmith-agent --lib` 98 · `codesmith-agent-runtime --lib` 1162+2 (1164 total; see flaky-test note below) · `codesmith-tui --bin codesmith-tui` 2855+2 · grep `.emit(codesmith_agent::extension::ExtensionEvent` across `host_executor.rs` = 16 · `.emit(&...` = 0 · `TrustReason::FirstLoad` in `crates/tui` = 0.

> **Pre-existing flaky test (NOT a regression — do not fix):** `mcp::tests::streamable_http_stale_session_reconnects_and_retries_tool_call` (`crates/agent-runtime/src/mcp.rs:5489`) fails intermittently under parallel load (mock-server race) but passes consistently in isolation (verified 3/3). §F5 does not touch `mcp.rs`. At slice end, if the `agent-runtime` run shows 1 failure with this test name, re-run it in isolation to confirm green before treating the gate as met; the expected green state is 1163 passed + 2 ignored.

## File Structure (modified)
- `crates/tui/src/tui/ui.rs` — T1 (`FirstLoad` emit in the `TrustDirectory` accept arm, before `fire_session_start_hook_if_ready`).
- `crates/agent-runtime/src/engine/host_executor.rs` — T1 (`f5_project_trust_first_load_dispatches` dispatch test in the §F2c test block, after `f2c_project_trust_dispatches_reason`).
- `docs/EXTENSIONS.md` — T2 (intro slice-status block + host-seam `ProjectTrust` row + Sandbox Stance clarification).
- `ROADMAP.md` — T2 (§F2c "下一聚焦工作" §F5 bullet update + new §F5 progress block + `### F5` subsection).

---

## Task 1: wire `ProjectTrust { FirstLoad }` at the onboarding trust-accept site + `f5` dispatch test

**Files:**
- Modify: `crates/tui/src/tui/ui.rs:2879` (insert emit block immediately before `fire_session_start_hook_if_ready(app);`)
- Modify: `crates/agent-runtime/src/engine/host_executor.rs:17052` (insert test immediately after `f2c_project_trust_dispatches_reason`'s closing `}`, before the test-mod closing `}` at `:17053`)

- [ ] **Step 1: add the `f5` dispatch test (regression guard; mirrors §F2c T3, reuses `ProjectTrustRecorderExt`).** In `crates/agent-runtime/src/engine/host_executor.rs`, immediately after the closing `}` of `f2c_project_trust_dispatches_reason` (`:17052`) and before the test-module closing `}` (`:17053`), add:

```rust

/// §F5 — `ProjectTrust { reason: FirstLoad }` must dispatch and carry its
/// `TrustReason` payload through. The onboarding trust-accept site
/// (`tui/ui.rs` `TrustDirectory` y/Y/1 arm) fires `FirstLoad` once per session,
/// distinct from the per-turn `Trusted`/`Untrusted` emits of §F2c T3. This test
/// is additive to `f2c_project_trust_dispatches_reason` (which exercised
/// `Trusted`→`Untrusted` only); it reuses the §F2c recorder fixture. The
/// host-wire e2e (the `ui.rs` emit firing through `run_tui`) is deferred per
/// the §F2b `SessionBeforeSwitch` precedent — it needs an `EngineHost` +
/// `run_tui`/`TrustDirectory` fixture (TaskManager-class scaffolding); the
/// emit mirrors the tested §F2c per-turn pattern.
#[tokio::test]
async fn f5_project_trust_first_load_dispatches() {
    let runner = Arc::new(ExtensionRunner::new());
    let seen: Arc<Mutex<Vec<TrustReason>>> = Arc::new(Mutex::new(Vec::new()));
    runner
        .load(&ProjectTrustRecorderExt { seen: seen.clone() })
        .await
        .unwrap();
    runner.bind_core(Arc::new(HostExtensionContext::new(
        PathBuf::from("/tmp/codesmith-test"),
        ExtensionMode::Tui,
        Arc::new(Mutex::new(true)),
        Arc::new(Mutex::new(CancellationToken::new())),
        runner.generation_arc(),
    )));

    let _ = runner
        .emit(ExtensionEvent::ProjectTrust {
            reason: TrustReason::FirstLoad,
        })
        .await;

    let recorded = seen.lock().unwrap();
    assert_eq!(recorded.len(), 1, "FirstLoad emit must dispatch");
    assert_eq!(
        recorded[0], TrustReason::FirstLoad,
        "the emit must carry FirstLoad"
    );
}
```

- [ ] **Step 2: run the new test — expect PASS (regression guard, green on write like §F2c T3's dispatch test; the runner's `ProjectTrust` dispatch is proven by §F2a/§F2c).** Run: `cargo +1.90.0 test -p codesmith-agent-runtime --lib f5_project_trust_first_load_dispatches`. Expected: `test result: ok. 1 passed; 0 failed; 0 ignored`.

- [ ] **Step 3: wire the `FirstLoad` emit at the onboarding trust-accept site.** In `crates/tui/src/tui/ui.rs`, in the `KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('1') if app.onboarding == OnboardingState::TrustDirectory =>` arm (`:2863`), inside the `match onboarding::mark_trusted(&app.workspace) { Ok(_) => { … } }` body, insert the emit block **immediately before** `fire_session_start_hook_if_ready(app);` (`:2879`) — i.e. after the `if app.onboarding_workspace_trust_gate { … } else { … }` block (`:2873-2878`). Match the surrounding 32-space indentation:

```rust
                                // §F5 — `ProjectTrust { FirstLoad }`: the onboarding
                                // trust-accept is the once-per-session site, distinct
                                // from the per-turn `Trusted`/`Untrusted` emits in
                                // `build_turn_dispatcher`/`spawn_subagent` (§F2c T3).
                                // `/trust on`, YOLO entry, and persisted-trust startup
                                // set `trust_mode = true` outside this prompt and
                                // surface per-turn as `Trusted`/`Untrusted`, not
                                // `FirstLoad`. Observe-only (`let _ =`). Mirrors the
                                // §F2c T3 emit pattern verbatim (`app` in place of
                                // `self`, `FirstLoad` in place of per-turn reason).
                                if let Some(runner) = &app.extension_runner {
                                    let _ = runner
                                        .emit(codesmith_agent::extension::ExtensionEvent::ProjectTrust {
                                            reason: codesmith_agent::extension::TrustReason::FirstLoad,
                                        })
                                        .await;
                                }
                                fire_session_start_hook_if_ready(app);
```

  > **Borrow note:** `&app.extension_runner` (a shared field reborrow of `&mut App`) is held across `.await` only inside the `if let` block; the shared borrow ends at the block's `}`, so the subsequent `fire_session_start_hook_if_ready(app)` (which takes `&mut App`) is unobstructed (NLL). If the borrow checker objects, clone the `Arc` (cheap) — `if let Some(runner) = app.extension_runner.clone() { … }` — same semantics, no borrow across `.await`.

- [ ] **Step 4: build + run the four suites (T1-level verification).**
  - `cargo +1.90.0 build --workspace` — green.
  - `cargo +1.90.0 test -p codesmith-extensions --lib` — 15 (unchanged).
  - `cargo +1.90.0 test -p codesmith-agent --lib` — 98 (unchanged).
  - `cargo +1.90.0 test -p codesmith-agent-runtime --lib` — 1163+2 (was 1162+2; +1 `f5`). If the only failure is `streamable_http_stale_session_reconnects_and_retries_tool_call`, re-run it in isolation to confirm green (see Baseline flaky-test note); do not touch `mcp.rs`.
  - `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` — 2855+2 (unchanged; the new emit is in `run_tui`'s onboarding arm, not exercised by bin tests; no `App`/field regression).
  - `grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs` — 16 (unchanged; the new emit is in `tui`, not `host_executor`).
  - `grep -rn 'TrustReason::FirstLoad' crates/tui/src` — 1 (the new emit).

- [ ] **Step 5: commit.**
```bash
git add crates/tui/src/tui/ui.rs crates/agent-runtime/src/engine/host_executor.rs
git commit -m "feat(framework): §F5 T1 ProjectTrust{FirstLoad} trust-prompt emit site (onboarding TrustDirectory y/Y/1 arm fires ProjectTrust{reason: TrustReason::FirstLoad} after app.trust_mode=true before fire_session_start_hook_if_ready; prompt-site only — /trust on/YOLO/persisted-trust startup surface per-turn as Trusted/Untrusted via §F2c T3 not FirstLoad; no dedup guard—onboarding accepts once per session; verbatim mirror of §F2c T3 if-let-Some(runner)=&<host>.extension_runner {let _=runner.emit(...).await} pattern with app in place of self; +1 test f5_project_trust_first_load_dispatches reusing ProjectTrustRecorderExt—additive to §F2c T3 (Trusted→Untrusted only); tui e2e deferred per §F2b SessionBeforeSwitch precedent; ext 15, agent 98, agent-runtime 1162+2→1163+2, tui 2855+2; host_executor .emit=16 unchanged, TrustReason::FirstLoad in tui=1)"
```

## Task 2: docs (EXTENSIONS host-seam + intro + Sandbox Stance; ROADMAP §F5 progress block + `### F5` subsection + §F2c next-focus update)

**Files:**
- Modify: `docs/EXTENSIONS.md:17-32` (intro slice-status), `:245` (host-seam `ProjectTrust` row), `:257-264` (Sandbox Stance)
- Modify: `ROADMAP.md:2549` (§F2c "下一聚焦工作" §F5 bullet), insert §F5 progress block before `---` at `:2552`, append `### F5` subsection after `:2968` (end of file)

- [ ] **Step 1: EXTENSIONS.md intro slice-status block (`:17-32`).** After the §F2c sentence ("…`ProjectTrust` per-turn wire) is done."), add a §F5 sentence before the "`ToolExecutionUpdate` (needs a streaming `Tool` contract…)" sentence:

```
§F5 slice 1 (`ProjectTrust { FirstLoad }` emit at the onboarding trust-accept site — the once-per-session signal extension handlers observe when the user accepts the workspace trust prompt) is done. The full §F5 dylib loader (`libloading`/`abi_stable`, `extension.toml` manifests, project-local discovery trust gate, `/extension install`/`uninstall`) remains §F5 续作 / §F3+; this slice emits the `FirstLoad` *event* only (no dylib machinery).
```

- [ ] **Step 2: EXTENSIONS.md host-seam `ProjectTrust` row (`:245`).** Replace the row:

old:
```
| `ProjectTrust` | `HostServices::build_turn_dispatcher` (+ `spawn_subagent`) after `build_tool_context_for` | observe | per-turn `Trusted`/`Untrusted` from `session.trust_mode`; `FirstLoad` → §F5 trust prompt |
```
new:
```
| `ProjectTrust` | `HostServices::build_turn_dispatcher` (+ `spawn_subagent`) after `build_tool_context_for` (per-turn `Trusted`/`Untrusted`); onboarding trust-accept `tui/ui.rs` `TrustDirectory` y/Y/1 arm after `app.trust_mode = true` (`FirstLoad`) | observe | per-turn `Trusted`/`Untrusted` from `session.trust_mode`; `FirstLoad` once per onboarding trust acceptance (`TrustReason::FirstLoad`) — distinct from the runtime `trust_mode` toggle (`/trust on`), YOLO entry, and persisted-trust startup, which surface per-turn as `Trusted`/`Untrusted`, not `FirstLoad` |
```

- [ ] **Step 3: EXTENSIONS.md Sandbox Stance (`:257-264`).** After the existing "Project local dylib install (phase 2, §F5) will require a trust prompt before the first load." sentence, add:

```
The `ProjectTrust { FirstLoad }` event (§F5 slice 1) now fires at onboarding trust acceptance — it is an *observe-only signal* extension handlers can subscribe to, distinct from (and not delivering) the phase-2 dylib loader that *consumes* project-local trust. The dylib loader, `extension.toml`, and project-local discovery trust gate remain §F5 续作 / §F3+.
```

- [ ] **Step 4: ROADMAP §F2c "下一聚焦工作" §F5 bullet (`:2549`).** Replace:

old:
```
- §F5：trust prompt 站点（`ProjectTrust{FirstLoad}` 真正 emit 处）。
```
new:
```
- §F5 slice 1 已落地（见下 §F5 进度块）：`ProjectTrust{FirstLoad}` onboarding trust-accept emit site。剩余 §F5：dylib loader（`libloading`/`abi_stable`）+ `extension.toml` manifest + 项目本地发现 trust gate + `/extension install`/`uninstall` 真实现（phase 2）。
```

- [ ] **Step 5: ROADMAP §F5 progress block.** Insert a new `**进度（…§F5…）**` block immediately before the `---` at `:2552` (after the §F2c "下一聚焦工作" block, `:2550`), mirroring the §F2c progress-block structure (`:2520-2551`):

```
**进度（2026-07-22 §F5 slice 1 ProjectTrust{FirstLoad} trust-prompt emit——§F5 首个子切片：onboarding TrustDirectory 接受站点 emit `ProjectTrust{reason: TrustReason::FirstLoad}` once-per-session，`feat/pluggable-framework-core`）：**

接 §F2c（per-turn `ProjectTrust` wire）。§F2c T3 把 `FirstLoad` 标记为 "→ §F5 trust-prompt site"；本切片落地该 emit。§F5 全量 dylib 机器（loader/manifest/install/项目本地发现 trust gate）仍是 §F5 续作 / §F3+；本切片只 fire 已存在的 `FirstLoad` 变体（§F2a 落地），无 contract 变更。plan：`docs/superpowers/plans/2026-07-22-codesmith-extension-system-slice-5.md`。

**关键设计决策：**
- **`FirstLoad` = prompt-site only**：仅在 onboarding `TrustDirectory` 接受（`tui/ui.rs` `y/Y/1` 臂）emit 一次；`/trust on`、YOLO entry、已持久化信任的 workspace 启动均不 emit（runtime toggle / 前次会话决定，由 per-turn `Trusted`/`Untrusted` 反映）。无 dedup guard（prompt 每会话只接受一次）。
- **Approach 1 = inline emit，逐字镜像 §F2c T3**：`if let Some(runner) = &app.extension_runner { let _ = runner.emit(ProjectTrust{reason: TrustReason::FirstLoad}).await; }`，在 `app.trust_mode = true` 之后、`fire_session_start_hook_if_ready(app)` 之前。`app` in place of `self`（§F2c T3 用 `EngineHost`），`FirstLoad` in place of per-turn `Trusted`/`Untrusted`。`app.extension_runner` 由 `spawn_engine`（`ui.rs:556`）+ `:561` clone 在事件循环前设置，故 trust 接受时已 `Some`。
- **测试 = runner-level dispatch test + defer tui e2e**：`f5_project_trust_first_load_dispatches`（reuse `ProjectTrustRecorderExt`，additive 于 §F2c T3 的 `Trusted`→`Untrusted`）；tui e2e deferred per §F2b `SessionBeforeSwitch` precedent（`run_tui`/`TrustDirectory` fixture scaffolding 比例失衡）。

**落地步骤：**
1. T1 `crates/tui/src/tui/ui.rs`：`TrustDirectory` 接受臂 insert `FirstLoad` emit before `fire_session_start_hook_if_ready`；`crates/agent-runtime/src/engine/host_executor.rs` test block 加 `f5_project_trust_first_load_dispatches`（reuse §F2c `ProjectTrustRecorderExt`）。
2. T2 `docs/EXTENSIONS.md`：intro slice-status + host-seam `ProjectTrust` row + Sandbox Stance 澄清（`FirstLoad` emit ≠ 全量 phase-2 dylib）；`ROADMAP.md`：§F2c "下一聚焦工作" §F5 bullet 更新 + §F5 进度块 + `### F5` 子节（scoped 到 FirstLoad emit，全量 dylib 记为 §F5 续作 / §F3+）。

**测试/验证：** `cargo +1.90.0 build --workspace` 全绿；`codesmith-extensions --lib` 15（不变）；`codesmith-agent --lib` 98（不变）；`codesmith-agent-runtime --lib` 1162+2 → 1163+2（+1 `f5_project_trust_first_load_dispatches`；pre-existing flaky `streamable_http_stale_session_reconnects_and_retries_tool_call` 隔离重跑绿，不触 `mcp.rs`）；`codesmith-tui --bin codesmith-tui` 2855+2（不变）；grep `.emit(codesmith_agent::extension::ExtensionEvent` 跨 `host_executor.rs` = 16（不变——新 emit 在 `tui` 非 `host_executor`）；`TrustReason::FirstLoad` 跨 `crates/tui` = 1。

**By-design gaps（显式 out-of-scope）：**
- §F5 全量 dylib 机器：`libloading`/`abi_stable` loader + `extension.toml` manifest + 项目本地发现 trust gate + `/extension install`/`uninstall` 真实现（phase 2）——本切片只 emit `FirstLoad` 事件，不 consume trust。
- tui-level e2e（`run_tui` 触发 `FirstLoad` emit）：§F2b `SessionBeforeSwitch` precedent（`EngineHost` + `run_tui`/`TrustDirectory` fixture scaffolding）；emit 镜像已测的 §F2c 模式。

**下一聚焦工作：**
- §F5 续作 / §F3+：dylib loader / `extension.toml` manifests / install/uninstall / `registerProvider` / renderers / shortcuts / flags / `EventBus` impl（按需）。
- 残项：P2 doc drift（推迟 slice 54）+ §E4 两 follow-up（按需）——均 on-demand / 非阻塞。
```

- [ ] **Step 6: ROADMAP `### F5` subsection.** Append after the `### F2c` subsection's last line (`:2968`, end of file), mirroring `### F2c` structure (`:2937-2968`):

```

### F5 — Slice 5 (ProjectTrust{FirstLoad} trust-prompt emit site)

- `ProjectTrust { FirstLoad }` onboarding trust-accept emit: the `TrustDirectory`
  `y/Y/1` accept arm in `tui/ui.rs` (`run_tui`) fires
  `ProjectTrust { reason: TrustReason::FirstLoad }` after `app.trust_mode = true`
  and before `fire_session_start_hook_if_ready`. Once-per-session (the prompt
  accepts once); observe-only (`let _ =`). Verbatim mirror of the §F2c T3
  per-turn emit pattern (`app.extension_runner` in place of
  `EngineHost.extension_runner`, `FirstLoad` in place of `Trusted`/`Untrusted`).
  `app.extension_runner` is `Some` at accept time (`spawn_engine` + clone run
  before the event loop).
- `FirstLoad` = prompt-site only: `/trust on`, YOLO entry, and persisted-trust
  workspace startup do NOT emit `FirstLoad` — those set `trust_mode = true`
  outside the onboarding prompt and surface per-turn as `Trusted`/`Untrusted`
  (§F2c T3 in `build_turn_dispatcher`/`spawn_subagent`). No dedup guard.
- Test: `f5_project_trust_first_load_dispatches` (runner-level dispatch,
  reuses the §F2c `ProjectTrustRecorderExt` fixture; additive to §F2c T3's
  `Trusted`→`Untrusted`). tui e2e deferred per the §F2b `SessionBeforeSwitch`
  precedent (`run_tui`/`TrustDirectory` fixture scaffolding).

**Status (slice 5 §F5 slice 1):** done. `FirstLoad` emit wired at the
onboarding trust-accept site. Still deferred (§F5 续作 / §F3+): the full dylib
loader (`libloading`/`abi_stable`), `extension.toml` manifests, project-local
discovery trust gate, and `/extension install`/`uninstall` real impl (phase 2) —
this slice emits the `FirstLoad` *event* only (no dylib machinery; no contract
change — `FirstLoad` exists since §F2a). Remaining §F3–§F8 unchanged.
```

- [ ] **Step 7: verify no regressions (docs-only change).** `cargo +1.90.0 build --workspace` green (no code change). Optionally re-run `cargo +1.90.0 test -p codesmith-agent-runtime --lib f5_project_trust_first_load_dispatches` to confirm the T1 test still passes.

- [ ] **Step 8: commit.**
```bash
git add docs/EXTENSIONS.md ROADMAP.md
git commit -m "docs(framework): §F5 T2 (EXTENSIONS intro slice-status + host-seam ProjectTrust row: FirstLoad emit site = onboarding TrustDirectory y/Y/1 arm after app.trust_mode=true + effect notes prompt-site-only distinction from /trust on/YOLO/persisted-trust; Sandbox Stance clarifies FirstLoad emit is observe-only signal ≠ full phase-2 dylib loader; ROADMAP §F2c next-focus §F5 bullet marks FirstLoad done + new §F5 progress block + ### F5 subsection scoped to FirstLoad emit, full dylib noted §F5 续作/§F3+; no code change—T2 doc-only, all 4 suites green at T1 commit unchanged)"
```

## Verification gate (slice end — not committed)
- [ ] `cargo +1.90.0 build --workspace` green.
- [ ] `cargo +1.90.0 test -p codesmith-extensions --lib` = 15.
- [ ] `cargo +1.90.0 test -p codesmith-agent --lib` = 98.
- [ ] `cargo +1.90.0 test -p codesmith-agent-runtime --lib` = 1163+2 (was 1162+2; +1 `f5`). If the only failure is `streamable_http_stale_session_reconnects_and_retries_tool_call`, re-run in isolation to confirm green (pre-existing flake, not touched).
- [ ] `cargo +1.90.0 test -p codesmith-tui --bin codesmith-tui` = 2855+2.
- [ ] `grep -c '\.emit(codesmith_agent::extension::ExtensionEvent' crates/agent-runtime/src/engine/host_executor.rs` = 16; `\.emit(&...` = 0.
- [ ] `grep -rn 'TrustReason::FirstLoad' crates/tui/src` = 1 (the new emit).
- [ ] `grep -rn 'ProjectTrust' crates/tui/src/tui/ui.rs` shows the new `FirstLoad` emit.

## Out of scope (explicitly deferred, documented in T2)
- §F5 full dylib machinery: `libloading`/`abi_stable` loader + `extension.toml` manifest + project-local discovery trust gate + `/extension install`/`uninstall` real impl (phase 2). This slice emits the `FirstLoad` *event* only (no dylib machinery; no contract change — `FirstLoad` exists since §F2a).
- tui-level e2e (the `ui.rs` emit firing through `run_tui`): §F2b `SessionBeforeSwitch` precedent (`run_tui`/`TrustDirectory` fixture scaffolding); the emit mirrors the tested §F2c per-turn pattern.
- `ToolExecutionUpdate` genuine emit (awaits a streaming `Tool` contract — a larger, separate slice).
- `ResourcesDiscover` / `SessionBeforeFork` (corrected rationale, §F2c).
