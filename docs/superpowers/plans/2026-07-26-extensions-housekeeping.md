# §F5e Housekeeping Plan — `UnimplementedSource` dead-code + `unused import: Path`

**Date:** 2026-07-26
**Predecessor:** §F5e (CratesIo + Prebuilt INSTALL source impls) — ff-merged to main as `a2775198` on 2026-07-26.
**Branch:** `feat/extensions-housekeeping` (create from `main`; do NOT work on main)
**Scope:** two small pre-existing housekeeping items surfaced by the §F5e final holistic review. Both pre-date §F5e (the slice's only `installer.rs` touch was a comment-only doc-drift fix; `UnimplementedSource` has been dead since §F5c).

## Goal

Clean up two pre-existing extensions-crate nits so the tree builds warning-free + carries no dead §F1 stub:
1. Delete `UnimplementedSource` (the §F1 placeholder source kind — fully dead, 0 use-sites anywhere in the repo).
2. Fix the `unused import: Path` warning at `installer.rs:9` (`Path` is used only inside `#[cfg(test)] mod e2e_tests`; move the import into the test module).

## Operation constraints (carry forward from §F5e session)

- **Don't work on `main`:** `git checkout -b feat/extensions-housekeeping` from `main`.
- **Plain `cargo`** (NEVER `cargo +1.90.0`).
- **Don't regress baseline:** report REAL `cargo test` counts per task (ext/agent/agent-runtime/tui). Both tasks are extensions-crate-only + behavior-preserving, so all 4 suites should be unchanged: ext `75 passed; 0 failed; 1 ignored` / agent `98` / agent-runtime `1165 passed; 0 failed; 2 ignored` / tui `2867 passed; 0 failed; 2 ignored`.
- **Commit messages:** REAL test counts + design provenance (cite "§F5e final-review housekeeping").
- **Finishing:** use the `finishing-a-development-branch` skill for wrap-up; merge to main needs user confirmation; prefer ff-merge (preserve commit hashes, §F5c/§F5d/§F5e precedent); delete branch local + remote + prune.

## Verified state (as of `a2775198` on main)

**`UnimplementedSource` — 3 refs, all definitional, 0 use-sites:**
```
crates/extensions/src/install_source.rs:41   pub struct UnimplementedSource;
crates/extensions/src/install_source.rs:42   impl ExtensionSource for UnimplementedSource {
crates/extensions/src/lib.rs:53             (inside pub use install_source::{…, UnimplementedSource, …})
```
(grep `UnimplementedSource` across `--include="*.rs"` → exactly these 3 lines; no `::new()`/`Box::new(UnimplementedSource)`/type-annotation use-sites anywhere.)

**`unused import: Path` — confirmed warning:**
```
$ cargo build -p codesmith-extensions
warning: unused import: `Path`
 --> crates/extensions/src/installer.rs:9:17
  |
9 | use std::path::{Path, PathBuf};
  |                 ^^^^
warning: `codesmith-extensions` (lib) generated 1 warning
```
`Path` bare-uses in `installer.rs` are at lines 145, 146, 167, 178 — ALL inside `#[cfg(test)] mod e2e_tests` (the `FakeSource`/`FakeBuilder` trait-impl test fixtures from §F5c). The `e2e_tests` module currently gets `Path` via `use super::*;` (line 130), which re-exports the top-level `use std::path::{Path, PathBuf}` (line 9). Top-level non-test code only uses `PathBuf`, hence the warning under `cargo build` (non-test).

---

## Task 1: Delete `UnimplementedSource` (dead §F1 stub)

**Files:**
- `crates/extensions/src/install_source.rs` — delete the `pub struct UnimplementedSource;` + its `impl ExtensionSource for UnimplementedSource { … }` block (lines ~41-48).
- `crates/extensions/src/lib.rs` — remove `UnimplementedSource` from the `pub use install_source::{…}` re-export (line ~53).

**Steps:**

1. Read `crates/extensions/src/install_source.rs` around lines 38-55 to see the exact `UnimplementedSource` struct + impl block (including the closing `}` + surrounding blank lines/comments). Read `crates/extensions/src/lib.rs` around line 53 to see the re-export glob.

2. Delete the `UnimplementedSource` struct + its `impl ExtensionSource` block from `install_source.rs`. Also delete any doc-comment immediately above the struct (e.g. a `/// §F1 placeholder for unimplemented source kinds` line — if present, remove it too; if the struct has no doc-comment, just remove the two-line struct+impl).

3. Remove `UnimplementedSource` from the `pub use install_source::{…}` re-export in `lib.rs` (line ~53). The re-export is a multi-line glob; remove just the `UnimplementedSource,` token (keep the other entries + trailing comma/comma-placement tidy).

4. Verify:
   ```bash
   cargo build -p codesmith-extensions 2>&1 | tail -3          # expect: no errors
   cargo test -p codesmith-extensions --lib 2>&1 | tail -3      # expect: 75 passed; 0 failed; 1 ignored (unchanged — dead code removal)
   grep -rn "UnimplementedSource" --include="*.rs" .            # expect: 0 hits
   cargo build --workspace 2>&1 | tail -3                       # expect: no errors (tui/agent/agent-runtime don't use UnimplementedSource — verified 0 refs)
   ```

5. Commit:
   ```bash
   git add crates/extensions/src/install_source.rs crates/extensions/src/lib.rs
   git commit -m "chore(extensions): remove dead UnimplementedSource (§F1 stub, 0 use-sites)

   UnimplementedSource was the §F1 placeholder for unimplemented source kinds. §F5c's R4 reconciliation established the actual stub was the tui-layer install_precheck (NOT this type), so UnimplementedSource has had 0 use-sites since §F5c. §F5e wired real CratesIoSource/PrebuiltDylibSource impls + removed the tui stub (T6), making this type doubly redundant. Final-review housekeeping (§F5e holistic review noted it as pre-existing dead code).

   grep UnimplementedSource across *.rs: 0 hits post-removal. ext 75 passed; 0 failed; 1 ignored (unchanged — dead code). Design: §F5e final-review housekeeping nit #1."
   ```

---

## Task 2: Fix `unused import: Path` warning in `installer.rs`

**File:**
- `crates/extensions/src/installer.rs` — line 9 (top-level import) + line ~130 (add import inside `e2e_tests`).

**Steps:**

1. Read `crates/extensions/src/installer.rs` lines 1-15 (top imports) + lines 128-180 (the `#[cfg(test)] mod e2e_tests` block: its imports at 130-135 + the `Path` use-sites at 145/146/167/178).

2. Change the top-level import at line 9 from:
   ```rust
   use std::path::{Path, PathBuf};
   ```
   to:
   ```rust
   use std::path::PathBuf;
   ```
   (drop `Path` — non-test code only uses `PathBuf`.)

3. Inside the `#[cfg(test)] mod e2e_tests` block, add `use std::path::Path;` to the import group at lines 130-135. Place it alphabetically/logically among the existing `use` statements (e.g. after `use crate::install_source::*;` at line 131, or grouped with `use std::sync::Arc;` at line 134 — either is fine; match the file's existing ordering). The result should look like:
   ```rust
   #[cfg(test)]
   mod e2e_tests {
       use super::*;
       use crate::install_source::*;
       use async_trait::async_trait;
       use codesmith_agent::extension::*;
       use std::path::Path;          // ← NEW (Path used by FakeSource/FakeBuilder sigs below)
       use std::sync::Arc;
       use tokio_util::sync::CancellationToken;
       …
   ```

4. Verify the warning is gone + tests still pass:
   ```bash
   cargo build -p codesmith-extensions 2>&1 | grep -i "warning\|error" | head   # expect: 0 hits (warning gone; no new errors)
   cargo test -p codesmith-extensions --lib 2>&1 | tail -3                      # expect: 75 passed; 0 failed; 1 ignored (unchanged — pure import move)
   ```
   If `cargo test` complains `cannot find type Path` in e2e_tests, the `use std::path::Path;` addition didn't land in the right scope — re-check it's inside the `mod e2e_tests { … }` block (after the `use super::*;` line, before the test fns).

5. Commit:
   ```bash
   git add crates/extensions/src/installer.rs
   git commit -m "chore(extensions): fix unused import Path warning (move into e2e_tests module)

   installer.rs:9 had 'use std::path::{Path, PathBuf}' but Path is only used inside #[cfg(test)] mod e2e_tests (FakeSource/FakeBuilder trait sigs at lines 145/146/167/178, brought in via 'use super::*'). Non-test 'cargo build' flagged 'unused import: Path'. Fix: top-level now 'use std::path::PathBuf' (PathBuf only); e2e_tests module gains its own 'use std::path::Path'. Final-review housekeeping (§F5e holistic review noted the warning as pre-existing).

   'cargo build -p codesmith-extensions' now 0 warnings. ext 75 passed; 0 failed; 1 ignored (unchanged — pure import move). Design: §F5e final-review housekeeping nit #2."
   ```

---

## Self-Review (run after both tasks)

**Completeness:**
- `UnimplementedSource` fully gone (struct + impl + re-export)? `grep -rn "UnimplementedSource" --include="*.rs" .` → 0 hits.
- `unused import: Path` warning gone? `cargo build -p codesmith-extensions 2>&1 | grep -ic warning` → 0.
- No new errors/warnings anywhere? `cargo build --workspace 2>&1 | grep -i "warning\|error"` → 0 hits attributable to these changes.

**Regression gate (4-suite, report REAL counts — honest reporting):**
```bash
cargo build --workspace 2>&1 | tail -3
cargo test -p codesmith-extensions --lib 2>&1 | tail -3        # expect 75 passed; 0 failed; 1 ignored
cargo test -p codesmith-agent --lib 2>&1 | tail -3             # expect 98 passed (unchanged)
cargo test -p codesmith-agent-runtime --lib 2>&1 | tail -3    # expect 1165 passed; 0 failed; 2 ignored (streamable_http flaky — isolate-rerun if fires)
cargo test -p codesmith-tui --bin codesmith-tui 2>&1 | tail -5 # expect 2867 passed; 0 failed; 2 ignored (runtime_api flaky — pre-existing if fires)
```
All 4 should be unchanged from the §F5e merged baseline (these are behavior-preserving housekeeping edits to the extensions crate only). If any count differs, STOP + investigate — these edits should not change behavior.

**Discipline:**
- Only the 3 files touched (`install_source.rs`, `lib.rs`, `installer.rs`)? `git diff main..HEAD --stat` → exactly these 3.
- No scope creep (no reformatting of surrounding code, no "while I'm here" edits)?

## Execution Handoff

**New session:** read this file + execute Task 1 → Task 2 → Self-Review → `finishing-a-development-branch` skill (ff-merge to main needs user confirmation; delete branch local+remote+prune).

These two tasks are small + behavior-preserving — no need for subagent dispatch; execute directly in the session (TDD red→green doesn't apply to dead-code/import moves; just edit → verify → commit). If you prefer the subagent-driven pattern, dispatch one implementer per task (sonnet), but it's overkill for ~5-line edits.

**Suggested new-session prompt (user → new session):**
> 继续做 §F5e final-review 留下的两个 housekeeping 任务。计划文件在 `docs/superpowers/plans/2026-07-26-extensions-housekeeping.md`——读取它并执行 Task 1（删 `UnimplementedSource` 死代码）+ Task 2（修 `installer.rs` 的 `unused import: Path` 警告），然后跑 Self-Review 的 4-suite 验证门，最后用 `finishing-a-development-branch` skill 收尾（ff-merge to main 需我确认；prefer ff-merge 保 commit hash；删 branch local+remote+prune）。操作约束：不直接动 main（`git checkout -b feat/extensions-housekeeping`）；plain cargo（不要 `cargo +1.90.0`）；不回归 baseline——每步报告 REAL cargo test 计数；commit message 带 REAL 计数 + 设计出处（§F5e final-review housekeeping）。

**Expected final state:** main ff-merged with 2 housekeeping commits; `feat/extensions-housekeeping` deleted local+remote+pruned; `cargo build --workspace` warning-free; `grep UnimplementedSource` → 0.
