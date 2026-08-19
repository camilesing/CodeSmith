# §F5 Post-Closure Housekeeping (follow-up plan)

> Follow-up to the §F5e final-review housekeeping (merged to `main` as
> `8b37d6fe` + `d5d4eb1d`). **§F5 (the dylib-loading extension-system
> phase) is fully complete** — post-merge investigation confirmed no
> `§F5f`/`§F6` design, plan, or ROADMAP progress block exists anywhere;
> §F5e is the first §F5 block with no "Still deferred / next focus"
> pointer (only explicit YAGNI out-of-scope items). The §F long-tail
> phases (§F3 EventBus, §F4 registerProvider, §F6 Renderers, §F7
> Shortcut+Flag, §F8 Embedding API) remain "on-demand" — none specced
> or planned, so starting one is a *new* brainstorming→spec→plan cycle,
> not "continuing the work."
>
> This plan closes the **3 remaining doc/spec/handoff loose ends** the
> post-closure investigation surfaced — same flavor as the 2 nits just
> merged (doc-drift + spec-hygiene + stale handoff note).

## Goal

3 behavior-preserving housekeeping tasks (docs/spec/handoff only — no `.rs` changes, no behavior change):

1. **Fix stale `docs/EXTENSIONS.md:91`** "Phase 2 (§F5, deferred)" line → reflect §F5 delivered (doc-drift, same flavor as §F5e T6 reconciled stale §F5c-stub comments).
2. **Commit 3 untracked spec design docs** (§F umbrella + §F5b + §F5c) → match the §F5d/§F5e tracked-spec convention (deferred spec-hygiene task per §F5e design §1 out-of-scope).
3. **Refresh deeply-stale `docs/superpowers/todo.md`** handoff note (slice-1-era, never updated through §F2/§F5b-e).

## Operation constraints (carried forward from §F5e)

- **不直接在 main 上动**: `git checkout -b chore/extensions-post-closure-housekeeping` (housekeeping → `chore/` prefix; branch off `main` at `8b37d6fe`).
- **plain cargo**（不要 `cargo +1.90.0`）— the stale `todo.md` line 20 is itself a `+1.90.0` artifact; the refresh removes it.
- **不回归 baseline**: 每步报告 REAL cargo test 计数. These are docs/spec-only changes → ext 75 / agent 98 / agent-runtime 1165+2 / tui 2867+2 should be **unchanged** (no `.rs` touched).
- **commit message 带 REAL 计数 + 设计出处** (§F5 post-closure housekeeping; cite the investigation findings).
- **finishing-a-development-branch skill 引导收尾**; merge to main **需我确认** (outward-facing + hard-to-reverse); prefer **ff-merge** 保 commit hash (§F5c/§F5d/§F5e 先例); 删 branch local + remote + prune.
- **行号会漂移——以内容为准** (grep for content, don't trust line numbers).

## Verified state (pre-check — confirmed by the post-closure investigation + this session's direct verification)

**§F5 fully complete** (no remaining §F5 work):
- ROADMAP: `### F5e` (line ~2676) is the tail §F5 subsection; no `### F5f`/`### F6` header anywhere. §F5e block has no "Still deferred" pointer (first §F5 block without one) — only "By-design gaps (显式 out-of-scope)" YAGNI items.
- §F5 design doc (`specs/2026-07-21-...-design.md` §9 line ~415): every §F5 scope item (libloading loader, `~/.codesmith/extensions/` discovery, `extension.toml` manifest, project-local trust prompt, `/extension install`/`uninstall` real impl, install-source abstractions Git/CratesIo/LocalPath/PrebuiltDylib) is delivered across §F5b/§F5c/§F5d/§F5e.
- Repo-wide grep for `§F5f`/`F5f`/`slice-5f`/`§F5g` → **0 matches**.

**ITEM 1 — EXTENSIONS.md:91 stale line** (confirmed, read lines 85-95):
```
- **Phase 1 (slice 1, static):** compiled-in extensions register an
  `ExtensionRegistration { factory, metadata }` via `inventory::submit!`.
  `discover_static()` collects every `ExtensionRegistration` linked into
  the binary. The in-tree `scratchpad` sample is the reference
  registration.
- **Phase 2 (§F5, deferred):** dylib loading from an install root +
  `extension.toml` manifest + trust prompt + the `ExtensionSource` /
  `ExtensionBuilder` / `ExtensionPlacer` trait impls. Slice 1 ships the
  **traits only** — no impls.
```
The Phase 2 item's "(§F5, deferred)" + "Slice 1 ships the traits only — no impls" is **false** post-§F5b/§F5c/§F5d/§F5e. §F5e T7 (commit `a2775198`) updated the EXTENSIONS.md intro (lines 22-56) + install row (line 81) but missed this Discovery-section line. Same doc-drift flavor as the §F5c-stub comments §F5e T6 fixed.

**ITEM 2 — 3 untracked spec design docs** (confirmed via `git ls-files`):
- Tracked specs (2): `2026-07-24-...-slice-5d-design.md`, `2026-07-27-...-slice-5e-design.md`.
- Untracked specs (3) — the spec-hygiene targets:
  - `docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md` (§F umbrella design)
  - `docs/superpowers/specs/2026-07-22-codesmith-extension-system-slice-5b-design.md` (§F5b)
  - `docs/superpowers/specs/2026-07-23-codesmith-extension-system-slice-5c-design.md` (§F5c)
- Cross-ref problem: the **tracked** §F5e spec references the untracked §F5c spec by path (`specs/2026-07-27-...-slice-5e-design.md:8` + `:174` → `docs/superpowers/specs/2026-07-23-...-slice-5c-design.md`); a fresh clone gets a dangling reference. Committing the 3 specs fixes this.
- §F5e design §1 "Out of scope (remain deferred)" explicitly lists "Committing §F5a/5b/5c untracked specs (separate spec-hygiene task)" — this IS that task. (§F5e spec's shorthand "§F5a" = the umbrella §F design doc.)
- **Plans remain untracked** per §F5d convention (working artifacts, intentionally not committed) — do NOT `git add` anything under `docs/superpowers/plans/`.

**ITEM 3 — `docs/superpowers/todo.md` deeply stale** (confirmed, read lines 1-60):
- Written after §F slice 1 (commit `961bf380` on `feat/pluggable-framework-core`); **never updated** through §F2a/b/c + §F5b/c/d/e + housekeeping.
- Line 1: "TODO — CodeSmith §F extension system (next-session handoff)" — frames itself as the standing handoff.
- Line 14: "Branch: `feat/pluggable-framework-core`" — long-gone branch; we're on `main`.
- Line 17: "Last slice done: §F slice 1" — actually §F2a/b/c + §F5b/c/d/e + 2 housekeeping rounds all done.
- Line 20: `cargo +1.90.0 build --workspace` — **violates the current "plain cargo" constraint**.
- Line 20 baselines: ext 8 / agent 93 / agent-runtime 1152 / tui 2853 — all WAY behind (now 75 / 98 / 1165 / 2867).
- Line 43: "## Next slice: §F2" — §F2 is DONE (§F2a/b/c delivered the full event set + HandlerOutcome + per-variant subscription + catch_unwind + round-trip + live reload wiring).
- Line 54: lists §F5 as future ("dylib + install/uninstall + ... (§F5)") — §F5 is DONE.
- Not a reliable next-slice pointer (per the investigation).

---

## Task 1: Fix `docs/EXTENSIONS.md:91` stale "Phase 2 (§F5, deferred)" line

**Read first** to confirm exact current text (line numbers drift — grep for `Phase 2 (§F5, deferred)`):
```bash
grep -n "Phase 2 (§F5, deferred)" docs/EXTENSIONS.md
```

**Edit** — replace the stale Phase 2 item (old_string spans the 5 lines from `- **Phase 2 (§F5, deferred):**` through `**traits only** — no impls.`):

old:
```
- **Phase 2 (§F5, deferred):** dylib loading from an install root +
  `extension.toml` manifest + trust prompt + the `ExtensionSource` /
  `ExtensionBuilder` / `ExtensionPlacer` trait impls. Slice 1 ships the
  **traits only** — no impls.
```

new (match the Phase 1 item's voice — ~5 lines; verify surrounding context first, refine wording to fit the doc):
```
- **Phase 2 (§F5, delivered):** dylib loading from an install root +
  `extension.toml` manifest + trust prompt + the `ExtensionSource` /
  `ExtensionBuilder` / `ExtensionPlacer` trait impls (Git / LocalPath /
  CratesIo / PrebuiltDylib — §F5c wired Git/LocalPath, §F5e wired
  CratesIo/Prebuilt). Host tools/commands wired live per-turn by §F5d;
  `/extension install`/`uninstall`/`reload` real (two-phase `Library`
  drop safe at the turn boundary).
```

**Verify:**
```bash
grep -n "Phase 2 (§F5" docs/EXTENSIONS.md   # expect "(§F5, delivered)" — no "deferred"
grep -rn "traits only.*no impls" docs/EXTENSIONS.md  # expect 0 (stale phrase gone)
cargo build --workspace 2>&1 | tail -3      # expect green (docs-only, no .rs change)
```

**Commit:**
```
docs(extensions): EXTENSIONS.md:91 Phase 2 (§F5) deferred→delivered (doc-drift)

The Discovery-section "Phase 2 (§F5, deferred) ... Slice 1 ships the traits only — no impls" line was stale: §F5b/§F5c/§F5d/§F5e delivered dylib loading + extension.toml + trust prompt + all 4 ExtensionSource/ExtensionBuilder/ExtensionPlacer impls (Git/LocalPath via §F5c, CratesIo/Prebuilt via §F5e) + live per-turn host wiring (§F5d) + real install/uninstall/reload. §F5e T7 (a2775198) updated the EXTENSIONS.md intro + install row but missed this line. Same doc-drift flavor as §F5e T6's §F5c-stub-comment reconciliation.

Docs-only — no .rs change. ext 75 passed; 0 failed; 1 ignored (unchanged). Design: §F5 post-closure housekeeping nit (EXTENSIONS.md doc-drift, §F5e T7 miss).
```

---

## Task 2: Commit 3 untracked spec design docs (spec-hygiene)

**Stage** (exactly these 3 spec files — do NOT stage anything under `plans/`):
```bash
git add docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md \
        docs/superpowers/specs/2026-07-22-codesmith-extension-system-slice-5b-design.md \
        docs/superpowers/specs/2026-07-23-codesmith-extension-system-slice-5c-design.md
git status   # confirm ONLY these 3 staged; plans/ + .zcode/ still untracked
```

**Verify no content drift** (these are existing untracked files — confirm they weren't modified):
```bash
git diff --cached --stat   # expect "3 files changed, N insertions(+)" where N = their full line counts (newly tracked)
# Pre-check the line counts so you know what to expect:
wc -l docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md \
       docs/superpowers/specs/2026-07-22-codesmith-extension-system-slice-5b-design.md \
       docs/superpowers/specs/2026-07-23-codesmith-extension-system-slice-5c-design.md
```

**Commit:**
```
docs(spec): commit §F umbrella + §F5b/§F5c design specs (spec-hygiene)

§F5d + §F5e design specs are tracked; the §F umbrella design (2026-07-21) + §F5b (2026-07-22) + §F5c (2026-07-23) design specs were left untracked — deferred as the "Committing §F5a/5b/5c untracked specs (separate spec-hygiene task)" item in §F5e design §1 "Out of scope (remain deferred)". Committing them now so the tracked §F5e spec's cross-references resolve in a fresh clone (§F5e design:8 + :174 cite the §F5c spec by path; it was a dangling ref before this commit).

Plans remain untracked per §F5d convention (working artifacts). Pure `git add` of existing untracked files — no content changes. ext 75 passed; 0 failed; 1 ignored (unchanged — docs-only). Design: §F5e design §1 out-of-scope spec-hygiene task.
```

---

## Task 3: Refresh deeply-stale `docs/superpowers/todo.md` handoff note

**Read first** to confirm the current stale content (it's ~60 lines; the staleness is documented in "Verified state" above).

**Decision**: refresh to a **minimal accurate handoff** (recommended) — the user values handoff files (keeps asking for follow-up plans to files). Alternative: delete it (it's untracked; the plan files in `plans/` serve as handoffs) — but refresh is preferred since a standing high-level pointer is useful for new sessions. **Pick refresh unless the user says otherwise.**

**Refresh** — replace the entire file content with a minimal accurate handoff (verify the commit hash + baselines against the current `main` HEAD at execution time; the values below are accurate as of `8b37d6fe` but main may have advanced):

```markdown
# TODO — CodeSmith §F extension system (handoff)

> Current as of `main` HEAD `<verify-and-fill-commit-hash>` (§F5
> post-closure housekeeping merged). **§F5 (the dylib-loading
> extension-system phase) is fully complete.** §F2 (events / handler
> chains / per-variant subscription / catch_unwind / live reload)
> delivered via §F2a/b/c. Next phases are **on-demand** (no spec/plan
> yet) — start any with brainstorming skill → spec → plan →
> subagent-driven-development (same flow as §F5b-e).

## Current state

- **Last slices done**: §F5e (CratesIo + Prebuilt `INSTALL` source impls,
  closing the §F5c-deferred work) + 2 rounds of §F5e final-review
  housekeeping (UnimplementedSource dead-code removal + unused-import
  fix; then this post-closure round).
- **§F5 delivered**: `libloading` loader + `~/.codesmith/extensions/`
  discovery + `extension.toml` manifest + project-local trust prompt
  (FirstLoad) + real `/extension install`/`uninstall`/`reload` +
  install-source abstractions (Git / LocalPath / CratesIo / PrebuiltDylib)
  + two-phase `Library` drop safe at the turn boundary + host
  ext tools/commands wired live per-turn.
- **Working tree**: clean except `.zcode/` (local tooling) +
  `docs/superpowers/plans/` (untracked working artifacts per §F5d
  convention).
- **Build/test baseline** (all green, **plain cargo** — no toolchain
  override; `cargo +1.90.0` is no longer used):
  - `cargo build --workspace` (142 tui warnings = pre-existing baseline,
    not new)
  - `cargo test -p codesmith-extensions --lib`: 75 pass + 1 ignored
  - `cargo test -p codesmith-agent --lib`: 98 pass
  - `cargo test -p codesmith-agent-runtime --lib`: 1165 pass + 2 ignored
    (streamable_http flaky — pre-existing; isolate-rerun if fires)
  - `cargo test -p codesmith-tui --bin codesmith-tui`: 2867 pass + 2
    ignored (runtime_api flaky — pre-existing)

## Next options (on-demand — needs brainstorming→spec→plan before code)

- **§F3**: EventBus real impl (currently `crates/extensions/src/bus.rs`
  `subscribe`/`publish` return `ExtensionError::Unimplemented(
  "EventBus.subscribe (§F3)")`).
- **§F4**: registerProvider.
- **§F6**: Renderers. **§F7**: Shortcut + Flag. **§F8**: Embedding API.
- (Hot-load permanently out — spec §2.4 "never".)

## Pointers

- §F umbrella design: `docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md`
- §F5 slice specs: `docs/superpowers/specs/2026-07-2[2-7]-...-slice-5[b-e]-design.md`
- Recent plans (untracked, working artifacts): `docs/superpowers/plans/`
- ROADMAP §F section: `ROADMAP.md` (§F5 progress block at ~line 2676).
```

**Verify:**
```bash
grep -c "+1.90.0" docs/superpowers/todo.md   # expect 0 (stale toolchain ref gone)
grep -c "Next slice: §F2" docs/superpowers/todo.md   # expect 0 (stale pointer gone)
grep -c "961bf380\|pluggable-framework-core" docs/superpowers/todo.md   # expect 0 (stale branch/commit refs gone)
cargo build --workspace 2>&1 | tail -3       # expect green (docs-only)
```

**Commit:**
```
docs(todo): refresh stale handoff note to §F5-complete state

docs/superpowers/todo.md was a §F-slice-1-era handoff note (commit 961bf380, branch feat/pluggable-framework-core) never updated through §F2a/b/c + §F5b/c/d/e + housekeeping: it named §F2 as "Next slice" (done), listed §F5 as future (done), used cargo +1.90.0 (no longer used), and showed slice-1 test counts (ext 8/agent 93/agent-runtime 1152/tui 2853 — now 75/98/1165/2867). Refreshed to a minimal accurate handoff: §F5 complete, §F2 done, next = §F3-§F8 on-demand (§F3 EventBus subscribe/publish still return Unimplemented in bus.rs), current baselines, plain cargo, pointers to specs/plans/ROADMAP.

Docs-only — no .rs change. ext 75 passed; 0 failed; 1 ignored (unchanged). Design: §F5 post-closure housekeeping nit (stale handoff note).
```

---

## Self-Review (run after all 3 tasks)

**Completeness:**
- EXTENSIONS.md:91 now says "delivered" not "deferred"? `grep -n "Phase 2 (§F5" docs/EXTENSIONS.md` → "(§F5, delivered)".
- 3 specs tracked + plans still untracked? `git ls-files docs/superpowers/specs/ | wc -l` → 5 (was 2); `git ls-files docs/superpowers/plans/ | wc -l` → 0 (unchanged — plans stay untracked).
- todo.md no stale refs? `grep -c "+1.90.0\|Next slice: §F2\|961bf380\|pluggable-framework-core" docs/superpowers/todo.md` → 0.

**Regression gate (4-suite, report REAL counts — honest reporting; these are docs-only changes so all should be UNCHANGED):**
```bash
cargo build --workspace 2>&1 | tail -3
cargo test -p codesmith-extensions --lib 2>&1 | tail -3        # expect 75 passed; 0 failed; 1 ignored
cargo test -p codesmith-agent --lib 2>&1 | tail -3             # expect 98 passed (unchanged)
cargo test -p codesmith-agent-runtime --lib 2>&1 | tail -3    # expect 1165 passed; 0 failed; 2 ignored (streamable_http flaky — isolate-rerun if fires)
cargo test -p codesmith-tui --bin codesmith-tui 2>&1 | tail -5 # expect 2867 passed; 0 failed; 2 ignored (runtime_api flaky — pre-existing)
```
If any count differs, STOP + investigate — docs-only changes should not change behavior. (ext may shift by ±0 since no `.rs` touched; if ext count changes, something is wrong.)

**Discipline:**
- Only `docs/` files touched (EXTENSIONS.md + 3 specs + todo.md)? `git diff main..HEAD --stat` → exactly these 5 files (3 specs newly tracked show as "new file" in the diff).
- No scope creep (no reformatting of surrounding EXTENSIONS.md content, no "while I'm here" edits)?

---

## Execution Handoff

These 3 tasks are small + behavior-preserving (docs/spec-only) — no need for subagent dispatch; execute directly in the session (TDD red→green doesn't apply to doc edits / git-add / handoff refresh; just edit → verify → commit). If you prefer the subagent-driven pattern, dispatch one implementer per task (sonnet), but it's overkill for ~30-line edits.

**Suggested new-session prompt (user → new session):**

> 继续做 §F5 闭合后的 housekeeping。背景：§F5（dylib-loading 扩展系统阶段）已全部完成——`§F5f`/`§F6` 的设计/计划/ROADMAP 条目都不存在，§F5e 是闭合切片（无 "Still deferred" 指针，只有显式 YAGNI out-of-scope 项）；§F3-§F8 是 on-demand 阶段（无 spec/plan，启动需先 brainstorming→spec→plan）。post-closure 调查发现 3 个 housekeeping loose ends。计划文件在 `docs/superpowers/plans/2026-07-27-extensions-post-closure-housekeeping.md`——读取它并执行 Task 1（修 `docs/EXTENSIONS.md:91` 的 stale "Phase 2 (§F5, deferred)" 行→delivered）+ Task 2（commit 3 个 untracked spec：§F umbrella + §F5b + §F5c，匹配 §F5d/§F5e tracked-spec 约定；plans 保持 untracked）+ Task 3（refresh 深度 stale 的 `docs/superpowers/todo.md` handoff note，slice-1-era 从未更新过），然后跑 Self-Review 的 4-suite 验证门（docs-only，应全部 unchanged），最后用 `finishing-a-development-branch` skill 收尾（ff-merge to main 需我确认；prefer ff-merge 保 commit hash；删 branch local+remote+prune）。操作约束：不直接动 main（`git checkout -b chore/extensions-post-closure-housekeeping`）；plain cargo（不要 `cargo +1.90.0`）；不回归 baseline——每步报告 REAL cargo test 计数；commit message 带 REAL 计数 + 设计出处（§F5 post-closure housekeeping）。行号会漂移——以内容为准（grep 内容，不要信行号）。

**Expected final state:** `main` ff-merged with 3 housekeeping commits (EXTENSIONS.md:91 delivered + 3 specs tracked + todo.md refreshed); `chore/extensions-post-closure-housekeeping` deleted local+remote+pruned; `cargo build --workspace` green; `grep "Phase 2 (§F5, deferred)"` → 0; `git ls-files docs/superpowers/specs/ | wc -l` → 5; todo.md has no `+1.90.0`/`Next slice: §F2`/`961bf380` refs.

---

## What comes AFTER this housekeeping (NOT in scope — for the user to decide)

Once the above is merged, §F5 + its docs/specs/handoff are fully reconciled. The **next real work** is starting a new on-demand phase (§F3 EventBus is the most natural next — it's the only one with concrete `Unimplemented` markers in `crates/extensions/src/bus.rs:6,16-17,30-41`). But starting §F3 (or any of §F4/§F6/§F7/§F8) requires the **brainstorming skill first** (BLOCKING, per the project's work mode) → spec → writing-plans → subagent-driven-development. Do NOT jump straight to coding a new phase — the brainstorming→spec→plan flow is mandatory and was the pattern for §F5b/§F5c/§F5d/§F5e.
