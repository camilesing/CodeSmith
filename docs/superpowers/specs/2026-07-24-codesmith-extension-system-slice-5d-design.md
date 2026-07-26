# §F5d — Extension tool/command host wiring + true unload Design

- **Date:** 2026-07-24
- **Branch:** `feat/ext-wire-unload` (NOT yet created — created at the writing-plans→TDD step, after spec review)
- **Predecessor:** §F5c (dylib INSTALL 侧), commits `98b3a12f`→`2eba6e9c` (T1 `install_source.rs` → T7 docs). §F5c merged to `main` + pushed to `origin/main` (`2eba6e9c`, 0 ahead/0 behind); feature branch deleted on both local + remote; stale remote-tracking ref pruned.
- **Spec:** this file (APPROVED 2026-07-24 — see STATE banner below)
- **Plan (to be written):** `docs/superpowers/plans/2026-07-24-codesmith-extension-system-slice-5d.md`
- **Authoritative scope source:** this session's code verification on `main` HEAD `2eba6e9c` + `ROADMAP.md` §F5c "By-design gaps" (lines ~2633-2637: `CratesIo`/`Prebuilt`, 真卸载 `clear_tools`/`clear_commands` + `Library` drop, tui-level install e2e, JSON multi-cdylib) + `docs/EXTENSIONS.md`.

> **STATE — approved 2026-07-24.** The user approved the re-presented design (brainstorming
> step 5) with one revision: **§4b — exclude dylib tools from subagent inheritance** (a
> subagent's name-based tool subset never includes extension tools, even under
> `inherit_full_registry=true`, so subagents never hold dylib `Arc`s across turns → the
> two-phase `Library` drain is unconditionally safe). §2 code findings were **re-verified
> against main HEAD `2eba6e9c` — all line numbers exact, zero drift**. This draft is now the
> **spec** (self-reviewed inline; pending the user's spec-file review). Next: user reviews →
> `writing-plans` → `git checkout -b feat/ext-wire-unload` → TDD execute. **No plan, no
> branch, no code exists yet.** Per project convention, spec/plan files stay untracked
> (code + ROADMAP/EXTENSIONS are authoritative). **§F5d follow-up (2026-07-26):** this spec
> is now an EXCEPTION — tracked + reconciled (the slice shipped as PR #4; §4b/§7/§10's
> provenance-marker suggestion marked STALE/SUPERSEDED by the structural resolution; see
> those sections + ROADMAP §F5d §4b reconciliation note).

---

## 0. Origin — why this slice (B1 真卸载) expanded to "combined wire + unload"

The user picked **B1 (真卸载)** from the §F5c deferred items. Code exploration then
discovered that **extension-contributed tools AND commands are not wired into the host
production tool path** — the `ToolDefinition`→`ToolSpec` bridge (`ExtensionToolSpecAdapter`,
`crates/agent-runtime/src/tools/extension.rs`) exists + is unit-tested but is **never
constructed in production**; `runner.bound_tools()` is read in production only by
`/extension status` (display). So the Q1 "bounded retention" gap (tools/commands stay bound
until process restart) is currently **display-only** (tools aren't callable → no stale-tool-
in-loop problem; no external `Arc` holder → the vtable UB risk is effectively absent).

This reframed B1: doing pure unload now is low-risk but low-value. To make 真卸载
**meaningful**, the host tool wiring must exist first — at which point dropping a `Library`
while a turn holds an `Arc<dyn ToolDefinition>` is a genuine UB concern the design must
handle. The user chose **combined wire + unload** in one slice.

### User decisions (via Q&A in the prior session — re-confirm with the user)

1. **Scope:** Combined — wire extension tools+commands into the host per-turn tool set
   (make them callable) AND add safe unload. (Chosen over: unload-only / wire-only /
   reconsider-direction.)
2. **Unload timing (tools/commands/handlers):** **Reload-deferred** — `/extension
   uninstall` removes files + state + marks; the next `/extension reload` clears the
   extension's live registries. Matches the existing "handlers clear on next `/extension
   reload`" convention.
3. **`Library` drop (the only UB-risky part):** **Two-phase engine-side drop** — UI-thread
   reload moves orphaned `Library` handles to a runner `pending_drop` slot; the **engine**
   drains + drops them at the turn boundary (op-loop top) where no in-flight dylib `Arc`
   exists on the main thread (verified §2/§4b). Subagents are excluded from holding dylib
   `Arc`s (§4b) so the drain is safe even with background subagents active. (Chosen over:
   idle-guard reload / engine-thread reload-op / defer Library-drop.)

> The user's "reload-deferred = UB-safe by construction" premise was **corrected**: reload
> runs on the **UI thread** (`commands::execute`, `ui.rs:5901`) **concurrent** with the
> engine thread (turn processing via `Op::SendMessage`, `ui.rs:4776`); `/extension reload`
> calls `reload_extension_runtime` directly (`extension_commands.rs:188`) with no engine
> sync. So reload is **NOT guaranteed between turns**. `clear_tools`/`clear_commands`/
> `clear_handlers` remain safe concurrent (in-flight `Arc`s survive via refcount; `Library`
> kept → vtable valid); **only `clear_libraries` is UB** concurrent with an in-flight dylib
> turn. The two-phase engine-side drop is what makes `Library` drop UB-safe.

---

## 1. Goal & scope

**In scope:**
- (a) Wire extension-contributed **tools + commands** into the host per-turn tool set so
  they're actually callable (today both are dead production code).
- (b) Safe **unload** — clear tools/commands/handlers at `/extension reload` (mirroring
  `clear_handlers`) + drop orphan `Library` handles via a two-phase engine-side drain, so
  an uninstalled extension's live state is gone after reload with **no UB**.

**Out of scope (remain §F5c deferred):** `crate:`/`prebuilt:` source impls;
`--message-format=json` multi-cdylib; tui-level install e2e; per-extension provenance
tracking (unnecessary — see §5).

## 2. Code-verified findings (VERIFIED on main HEAD `2eba6e9c` — all line numbers exact, zero drift; re-confirmed 2026-07-24)

- **`ExtensionRunner` registries** (`crates/extensions/src/runner.rs`): `tools:
  Mutex<HashMap<String, Arc<dyn ToolDefinition>>>` (keyed by **name**, no ext-id
  provenance), `commands: Mutex<HashMap<...CommandDefinition>>` (same), `handlers:
  Mutex<Vec<RegisteredHandler>>`, `libraries: Mutex<Vec<Library>>` (flat Vec, **no ext-id
  association**; pushed by `load_dylib` ~:181; reload does NOT clear — Q1 comment ~:105-112
  frames keeping `Library` alive as correctness-preserving for in-flight vtables).
  `clear_handlers` exists (~:156); **no `clear_tools`/`clear_commands` anywhere** (grep-
  confirmed).
- **`bind_core` flush** (`runner.rs:195`): drains `pending_*` into the live HashMaps
  (name-keyed; `HashMap::insert` = last-wins, despite the "first-wins" doc comment —
  conflict-suffixing deferred to §F2).
- **Bridge type** `ExtensionToolSpecAdapter` (`crates/agent-runtime/src/tools/extension.rs:27`):
  wraps `tool: Arc<dyn ToolDefinition>` + `ctx: Arc<dyn ExtensionContext>`, impls host
  `ToolSpec`. **`ExtensionToolSpecAdapter::new` is constructed ONLY in tests** (grep-confirmed)
  → dead in production.
- **`bound_tools()`** (`runner.rs:307`) production call sites: only `/extension status`
  (`extension_commands.rs:170`, display) + test logging (`loader.rs:117`, `installer.rs:230`).
- **Production tool-build path:** `self.host.build_turn_dispatcher(req)`
  (`crates/agent-runtime/src/engine/mod.rs:1196`) → `plan.tool_registry`. NOT the
  test-helper `build_turn_tool_registry_builder` (`engine.rs:798`, labeled "(test helper)").
- **`build_turn_dispatcher`** (`crates/tui/src/core/engine/runtime_traits.rs:236`): per-turn
  `HostServices` method; calls `build_turn_tool_registry_builder_for` (:269) → builds
  registry (:339) → configures plugins (:439) → finalizes `tool_registry: … as
  Arc<dyn ToolDispatcher>` (:468). Runs per turn from `handle_send_message` (`mod.rs:1196`).
  `EngineHost` holds `extension_runner: Option<Arc<ExtensionRunner>>` (`engine.rs:127/171`).
- **Commands also not wired:** `runner.try_dispatch_command(name)` (`runner.rs:299`) is
  not called in the tui `execute()` command-dispatch path (grep-confirmed).
- **Threading:** slash commands run via `commands::execute(input, app)` on the **UI thread**
  (`ui.rs:5901`); engine runs on its own thread (`Op::SendMessage`, `ui.rs:4776`);
  `/extension reload` calls `reload_extension_runtime` directly (`extension_commands.rs:188`).
- **`ExtensionCommandContext: ExtensionContext`** (`crates/agent/src/extension.rs:375`) —
  the context stored at `bind_core` can be upcast to `Arc<dyn ExtensionContext>` for the
  adapter (Rust 1.86+ trait-upcasting, already used elsewhere).
- **Engine op-loop** (`crates/agent-runtime/src/engine/mod.rs:517`):
  `while let Some(op) = self.rx_op.recv().await { match op { Op::SendMessage {..} => …,
  Op::Shutdown => … } }`. Sequential, single-threaded. Executor constructed fresh per turn
  (`host_executor.rs:1850` "constructed fresh each turn") → between ops the previous turn's
  executor + `ToolSet` are dropped → **no in-flight dylib `Arc`** at the op-loop top.
- **`reload_extension_runtime`** (`engine.rs:484`): `clear_handlers()` → `invalidate()` →
  re-discover (`discover_static` + `discover_dylib`) → reconcile `state.is_enabled` →
  re-`load`/`load_dylib` → re-`bind_core`. The bounded-retention gap = reload does
  `clear_handlers` but NOT `clear_tools`/`clear_commands`/`clear_libraries`.

### Safe/unsafe table (reload concurrent with an in-flight turn)

| Op at reload | Concurrent w/ in-flight turn? | Safe? |
|---|---|---|
| `clear_tools`/`clear_commands`/`clear_handlers` | yes, possibly | **Safe** — per-turn `build_turn_dispatcher`/`emit` snapshot under short lock; in-flight `Arc`s survive via refcount; `Library` stays alive → vtable valid |
| `clear_libraries` | yes, possibly | **❌ UB** — dropping a `Library` mid-turn while an in-flight `Arc<dyn ToolDefinition>`/`Arc<dyn Handler>` from that dylib exists → dangling vtable → use-after-free |

## 3. Wiring half — make contributions live

**Insertion point:** `EngineHost::build_turn_dispatcher`
(`crates/tui/src/core/engine/runtime_traits.rs:236`).
- **Tools:** after the registry is assembled + plugins configured, if
  `Some(runner) = &self.extension_runner`, read `runner.bound_tools()` and for each
  `(name, Arc<dyn ToolDefinition>)` construct
  `ExtensionToolSpecAdapter::new(tool, ctx)` (`tools/extension.rs:34`) and
  `registry.register(Arc::new(adapter) as Arc<dyn ToolSpec>)`. `ctx` = runner's bound
  context (new accessor — see below). Per-turn rebuild → clearing `runner.tools[id]`
  before the next turn's `build_turn_dispatcher` is sufficient (no persistent host holder).
- **Commands:** wire `runner.try_dispatch_command(name)` (`runner.rs:299`) into the tui
  `execute()` command-dispatch path (same seam `extension_commands::try_dispatch` uses).
- **New runner accessor:** `pub fn bound_context(&self) -> Option<Arc<dyn
  ExtensionContext>>` — upcasts the stored `Arc<dyn ExtensionCommandContext>`
  (`ExtensionCommandContext: ExtensionContext`, `agent/src/extension.rs:375`; Rust 1.86+
  trait-upcasting).
- **UB note:** per-turn `Arc` clones; refcount keeps in-flight ones alive during the turn;
  `Library` stays alive until the two-phase drop (§4). Safe.
- **Scope of wiring (§4b):** ext tools are registered into the **main turn** `ToolRegistry`
  only — they are **not** added to any subagent's tool-subset basis (see §4b: subagents must
  never inherit dylib `Arc`s across turns).

## 4. Unload half

### 4a. tools/commands/handlers (reload-deferred, safe-concurrent)
- **New runner methods** mirroring `clear_handlers` (`runner.rs:156`): `clear_tools(&self)`
  + `clear_commands(&self)` — `.clear()` the respective `Mutex<HashMap>`.
- **`reload_extension_runtime`** (`engine.rs:484`): after `clear_handlers()`, call
  `clear_tools()` + `clear_commands()` **before** re-discover/re-load/re-`bind_core`.
  Re-load re-populates for present extensions (static always; dylib iff on disk +
  trust-gated). After reload, registries exactly reflect on-disk state → uninstalled
  extension's tools/commands/handlers gone.

### 4b. `Library` (two-phase engine-side drop, UB-safe)
- **New runner field:** `pending_drop: Mutex<Vec<Library>>`.
- **New runner methods:** `drain_libraries_to_pending(&self)` — move the live `libraries`
  Vec into `pending_drop`; `drop_pending(&self)` — drop `pending_drop` contents.
- **`reload_extension_runtime`:** **before** re-load, call
  `drain_libraries_to_pending()`. Re-load re-opens + re-pushes present dylibs into the live
  `libraries`. After reload: live `libraries` = present (re-opened); `pending_drop` = old
  handles (orphans whose dylib is gone + superseded-by-reopen). *(Current reload already
  re-dlopens every reload and accumulates — this fixes that leak.)*
- **Engine drains at turn boundary:** at the op-loop top
  (`engine/mod.rs:517`, before `match op`): `if let Some(r) = &self.extension_runner {
  r.drop_pending(); }`. Safe because between ops the previous turn's executor + `ToolSet`
  are dropped → **no in-flight dylib `Arc`**.
- **Subagent cross-turn hold — RESOLVED by exclusion (user decision 2026-07-24):** background
  subagents (`Op::SpawnSubAgent`, `engine/mod.rs:570`; `SubAgentManager::spawn_background`,
  `runtime_traits.rs:209`) are detached tasks that run across turn boundaries, so the naive
  "no detached task holds a dylib `Arc`" invariant is **false by construction**. A
  subagent's tool set is a **name-based subset** of the parent `ToolRegistry`
  (`child_subset_basis: Option<Vec<String>>`, `subagent/mod.rs:607`; built in
  `SubAgentToolRegistry::new`, `mod.rs:4643`; `inherit_full_registry` default **false** =
  curated subset, `engine_config.rs:271`; **true** = `None` → full parent surface,
  `subagent/mod.rs:600`). Post-wiring, `inherit_full_registry=true` would let a mid-turn
  subagent hold a dylib `Arc` while the engine drains an orphaned `Library` → UB.
  **Resolution:** wire ext tools into the **main turn** `ToolRegistry` only; **never add
  their names to a subagent's tool-subset basis**, even under `inherit_full_registry=true`.
  Subagents then never hold dylib `Arc`s → the two-phase drain needs **no runtime
  subagent-check guard** (structurally safe: exclusion is the precondition, not a guard).
  **⚠ STALE — SUPERSEDED (§F5d follow-up reconciliation, 2026-07-26):** the
  provenance-marker / force-subset-when-dylib-loaded suggestion in this paragraph was
  **NOT implemented**. §4b was resolved more simply + structurally: `SubAgentRuntime`
  (`subagent/mod.rs:548-608`) has **NO `extension_runner` field** +
  `SubAgentToolRegistry::new` (`subagent/mod.rs:6111`) rebuilds its **OWN fresh built-in
  `ToolRegistry`** (`ToolRegistryBuilder::new().with_full_agent_surface(...)` `:6160`,
  no parent `Arc` clone) → ext tools (added only in `EngineHost::build_turn_dispatcher`) never reach
  a subagent's effective set, **regardless of `inherit_full_registry`**. **NO provenance
  marker / force-subset / runtime subagent-check guard was added.** The original "Plan T4
  ... adds a provenance marker ... Trade-off accepted: `inherit_full_registry=true`
  subagents cannot call extension tools" framing is moot — the structural rebuild means no
  subagent (any `inherit_full_registry` setting) ever sees ext tools, with no trade-off.
  Authoritative record: ROADMAP §F5d progress block §4b reconciliation note; locked by
  regression test `subagent_ext_tool_excluded_from_effective_set`
  (`subagent/tests.rs:941`, expected Green under both inherit settings).
- **Main-thread drain safety (verified):** the drain at the op-loop top is safe on the engine
  thread itself — `handle_send_message` constructs the executor as a local
  (`HostAgentExecutor::new(...).with_extension_runner(...)`, `mod.rs:1286-1328`) dropped at
  function return; "constructed fresh each turn" (`host_executor.rs:1850`). So between ops
  no main-thread in-flight dylib `Arc` exists.

## 5. Provenance — none needed

Clear-all-then-re-load-present makes per-extension ownership tracking unnecessary: reload
re-derives the live set from disk, so an uninstalled extension simply isn't re-loaded.
(Selective "remove only extension X's entries" is harder + unneeded.)

## 6. `/extension uninstall` command

Files + state removal unchanged (`extension_commands.rs:298`). Live-registry cleanup +
`Library` drop happen on the next `/extension reload`. Update the warning message: drop the
"bounded retention / remain bound until process restart" caveat; say "live bindings
(tools/commands/Library) clear on next `/extension reload`."

## 7. Testing (TDD Red→Green; UB can't be directly unit-tested)

- **Wiring (Red→Green):** an extension's registered tool is callable through the host
  `ToolSpec` path — reuses the §F5b `fixture_echo` tool. Red (not wired) → Green (wired via
  `build_turn_dispatcher`). Same for a contributed command resolving via dispatch.
- **`clear_*`:** after `clear_tools`+`clear_commands` + re-load-present, the uninstalled
  extension's tool/command absent from `bound_tools()`/`bound_commands()`.
- **Two-phase:** `drain_libraries_to_pending` moves handles to `pending_drop`;
  `drop_pending` empties it; invariant test that `drop_pending` is only invoked at the engine
  turn-boundary drain (no `Library` lingers in `pending_drop` across a turn). The actual
  use-after-free can't be unit-tested — covered by the invariant + a documented Miri note
  (dylib+Miri is unreliable; the invariant is the proof).
- **Concurrency invariant:** an in-flight `Arc<dyn ToolDefinition>` survives `clear_tools`
  (refcount) — asserts the no-UB-under-concurrent-reload property.
- **Subagent exclusion (§4b):** a registered ext tool does **not** appear in a spawned
  subagent's effective tool set — both under default `inherit_full_registry=false` (curated
  subset excludes it) AND under `=true`. **⚠ STALE — SUPERSEDED (§F5d follow-up
  reconciliation, 2026-07-26):** the original "(the provenance/force-subset guard skips
  it)" mechanism was **NOT implemented**; the real `=true` exclusion is structural —
  `SubAgentRuntime` has no `extension_runner` field + `SubAgentToolRegistry::new` rebuilds
  its own fresh built-in `ToolRegistry` (no parent `Arc` clone) → ext tools never reach a
  subagent's effective set regardless of `inherit_full_registry`; NO provenance marker/guard
  added (see §4b STALE note + ROADMAP §F5d §4b reconciliation note; locked by regression
  test `subagent_ext_tool_excluded_from_effective_set`, `subagent/tests.rs:941`). Proves
  subagents never hold dylib `Arc`s, so the two-phase drain is UB-safe even with background
  subagents active.
- **4-suite honesty:** ext grows; agent/agent-runtime unchanged unless touched; tui
  reported as "N pass/26 pre-existing runtime_api fail/2 ignored" (never "green", never
  attributed to new work).

## 8. Docs

- **EXTENSIONS.md:** intro — tool/command wiring now live (corrects the stale "agent loop
  sees extension tools as normal `ToolSpec`s" claim, now actually true); In-TUI Manager —
  update the uninstall row (drop bounded-retention caveat); Sandbox Stance — reload-deferred
  unload + two-phase `Library` drop + ext-tools-main-turn-only (not inherited by subagents → drain UB-safe).
- **ROADMAP.md:** §F5c progress block "next focus" mark this done + a new `### F5d`
  subsection (Status + still-deferred) + a §F5d progress block (reconciliations: wiring-point
  discovery, two-phase rationale, no-provenance simplification, test counts, by-design gaps).
- **Spec/plan:** untracked work products in `docs/superpowers/specs/` + `plans/` (per project
  convention — not committed; code + ROADMAP/EXTENSIONS are authoritative).

## 9. Branch + commit protocol

`git checkout -b feat/ext-wire-unload` **before any implementation code** (spec/plan writing
first, untracked, on `main` is fine). Per-task TDD Red→Green→commit; commit messages carry
real test counts + API-reconciliation notes (e.g., the `build_turn_tool_registry_builder`
"test helper" surprise → real point is `build_turn_dispatcher`; no-provenance
simplification; two-phase drop rationale). Plain cargo; the 26 tui `runtime_api` fails are
pre-existing/environmental.

## 10. Task decomposition (for the plan stage)

- **T1** Wire tools in `build_turn_dispatcher` + `bound_context()` accessor.
- **T2** Wire commands into the tui `execute()` dispatch (`try_dispatch_command`).
- **T3** `clear_tools`/`clear_commands` + `reload_extension_runtime` ordering.
- **T4** Two-phase: `pending_drop` field + `drain_libraries_to_pending`/`drop_pending` +
  engine op-loop-top turn-boundary drain. **+ §4b subagent exclusion:** verify the default
  subagent subset excludes dylib tools. **⚠ STALE — SUPERSEDED (§F5d follow-up
  reconciliation, 2026-07-26):** the original "for `inherit_full_registry=true` add a
  provenance marker on host-registry entries (or force-subset-when-dylib-loaded) so dylib
  tools are skipped" was **NOT implemented**; §4b was resolved structurally
  (`SubAgentRuntime` has no `extension_runner` field + `SubAgentToolRegistry::new` rebuilds
  its own fresh built-in `ToolRegistry`, no parent `Arc` clone → ext tools never reach a
  subagent's effective set regardless of `inherit_full_registry`; NO provenance marker/guard
  added). See §4b STALE note + ROADMAP §F5d §4b reconciliation note; locked by regression
  test `subagent_ext_tool_excluded_from_effective_set` (`subagent/tests.rs:941`).
- **T5** `/extension uninstall` message + EXTENSIONS.md + ROADMAP.md docs.

---

## Status — next steps (2026-07-24)

1. ✅ Context recovered from files (git: `main` = `origin/main` = `2eba6e9c`, 0 ahead/0
   behind; §F5c T1-T7 present; working tree clean except untracked `.zcode/` +
   `docs/superpowers/`).
2. ✅ §2 code findings **re-verified against main HEAD `2eba6e9c` — all line numbers exact,
   zero drift** (wiring gap, `build_turn_dispatcher` insertion point @ runtime_traits.rs:236,
   reload-on-UI-thread @ extension_commands.rs:188, op-loop @ mod.rs:517, executor-fresh-
   per-turn @ host_executor.rs:1850, safe/unsafe table).
3. ✅ Design **re-presented + user-approved** (brainstorming step 5) with the §4b revision
   (exclude dylib tools from subagent inheritance).
4. ✅ Spec written + self-reviewed inline (this file).
5. ⏭ Next: **user reviews this spec file** → invoke `writing-plans` skill → `git checkout
   -b feat/ext-wire-unload` → TDD execute (Red→Green→commit per task; commit messages carry
   real test counts + API-reconciliation notes). Plain cargo; the 26 tui `runtime_api` fails
   are pre-existing/environmental.
