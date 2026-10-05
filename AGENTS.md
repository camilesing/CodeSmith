# CodeSmith agent guidance

CodeSmith is a terminal coding agent for open-weight models (DeepSeek,
Moonshot, OpenAI-compatible gateways, vLLM, Ollama) — a fork of CodeWhale,
itself derived from deepseek-tui. Packages are `codesmith-*`; user storage is
`~/.codesmith/`.

Keep this file durable. Derive changing release, provider, branch, and flake
state from the repository, tests, CI, and current issue tracker rather than from
instructions or memory. The nearest scoped `AGENTS.md` adds path-specific rules.
Repo map and layer boundaries: `docs/ARCHITECTURE.md` (boundary note at the
top), `CONTRIBUTING.md` ("Project Structure"), `docs/HARNESS.md`; script mods
(Rhai) are documented in `docs/MODS.md`. Docs are maintained in bilingual pairs
(`X.md` + `X_cn.md`) — update both or neither.

## The ponytail method

From [dietrichgebert/ponytail](https://github.com/dietrichgebert/ponytail) —
"the laziest senior dev in the room." *He says nothing. He writes one line. It
works.* The best code is the code you never wrote.

Before writing code, walk the decision ladder in order and stop at the first
rung that answers:

1. **Does this need to exist?** → Skip it.
2. **Already in this codebase?** → Reuse it.
3. **Stdlib does it?** → Use it.
4. **Native platform feature?** → Use it.
5. **Installed dependency?** → Use it.
6. **One line?** → One line.
7. **Only then:** the minimum that works.

The ladder runs *after* understanding the problem. Lazy about solutions, never
about reading the code first — a short diff written without reading the call
sites is not ponytail, it is a guess.

**Never cut, at any rung:** trust-boundary validation, data-loss handling,
security, accessibility. Brevity is not a reason to drop a guard.

Rung 2 is the one this repository keeps failing — "one turn loop, one base
prompt" is rung 2 with a name. Two corollaries earned here:

- **An abstraction must delete caller code.** If adopting it is pure
  obligation — required methods, no default bodies that do work — it gets
  built, adopted once, and abandoned.
- **Migrate the last consumer, or do not start.** Framework, one caller,
  ticket the rest, silence the warning: that ships two systems and a comment
  that is no longer true. If the migration will not fit, narrow the slice —
  never the adoption. The standing `#[allow(dead_code)]` count is the running
  receipt.

## Working rules

- Inspect status and existing consumers before editing. Preserve unrelated,
  dirty, and untracked work.
- Before adding a module named `model_*`, `*_config`, `provider_*`, or
  anything that "bridges", "mirrors", or "stages" an existing thing, grep
  for the existing thing and edit it. A new layer must name the predecessor
  it replaces in the module doc; otherwise edit the original.
- Prefer the simplest implementation that preserves observable contracts. A
  rewrite is acceptable when justified by product intent and observed behavior,
  not as a shortcut around understanding existing code.
- Search for behavior and symbols before reviving work from an old branch. If a
  lane is obsolete, preserve its intent and evidence rather than merging stale
  code mechanically.
- A small coherent change may be committed directly to `main` when that checkout
  is current, clean, and owns the affected files. Default to the checkout that
  already exists: when several agents share it, partition by file, stage only
  the paths your slice touched, and retry a commit that fails on `index.lock`.
  A fresh worktree is for conflicting, dirty, stale, or independent lanes,
  not for parallel agents on the same lane. Local commit
  permission never implies push, merge, tag, release, or deploy permission.
- When the task is local-only, stay fully offline: no browsing, GitHub or remote
  Git operations, downloads, dependency installation, provider calls, or
  source/diff transmission. Record the missing external receipt and keep working
  locally.
- Public name is **CodeSmith** (upstream: Codewhale). Compatibility
  identifiers such as `codesmith`, `codesmith-tui`, protocol names, and
  storage keys (`~/.codesmith/`) change only through an explicit migration.
- Keep providers and models first-class and provider-neutral. `rig-core` is
  one swappable implementation behind `crates/agent`'s own `LlmClient` /
  `Tool` traits, not the framework vocabulary.
- Never rewrite published history, retag a release, force-push a shared ref, or
  publish without explicit authorization. Preserve human contributor credit.
- **Model-visible means logged.** Anything that reaches a model request must be
  reconstructable from the session log, and a new model-visible input needs a
  session event. Live presentation and the persisted record must agree; when they
  disagree the record is right.
- **Misconfiguration fails loud**, at load when it is self-contained, otherwise
  at the earliest point it can be resolved. Never silently skip a missing
  referent.
- **Write down what a design does not do**, beside the behaviour it owns — a
  short known-limitations note in the owning module. A stated limit stops the
  next reader from assuming a capability that was never built.
- **Agents do not comment on issues or PRs** (founder, 2026-09-22). Spend the
  time on code: evidence goes in the commit message and PR body, claims go in
  Linear. Do not reply to review bots or post status, "superseded", or
  "for the record" notes. The one exception is closing or superseding a human
  contributor's PR or issue: one sentence saying why, with the link. The PR and
  issue review workflows are disabled; re-enable one only by founder decision.
- **A user feature lands with its docs.** A new command, preset, `[features]`
  flag, or provider updates `docs/CLI.md` and the owning guide (bilingual
  pair) in the same change.
- **Write `close`/`fix`/`resolve #N` only when you mean it.** GitHub closes the
  issue on merge even inside "does not close #N"; use `Refs #N` otherwise.
- Keep new enforcement dry-run unless explicitly approved.

## Landing other people's work

An external contributor's branch goes stale because *we* land things, not
because they did anything wrong. Treat their time as more expensive than ours.

**The goal is the contributor's PR merging as itself.** Review it, help it
rebase, or fix it on their branch — that is the default path. Closing their PR
and re-landing the work as our own commit (`auto-close-harvested`) is the
fallback for a branch that truly cannot merge in reasonable time; done
casually it reads as taking the work even when credit is preserved.

- **Never make a contributor rebase around our churn.** If their PR conflicts
  only because main moved, a maintainer resolves it.
- Landing mechanics — direct merge vs. harvest, the contribution gate — are
  documented in `CONTRIBUTING.md` ("How Your Contribution Lands"). Follow
  them instead of improvising.
- **Preserve credit in the mechanical sense, not just the polite one.** Commit
  authorship and `Co-authored-by` trailers must use the contributor's own
  GitHub-linked address — GitHub reads commit metadata for the contribution
  graph, not prose credit or mailmap-style indirection. A harvested landing
  carries `Harvested from PR #N by @handle` so `auto-close-harvested.yml`
  closes their PR with credit.

## Merging under a gate

- **A gate is its artifact.** When a rail says a PR merges only on a passing
  acceptance record, the record must literally say PASS at merge time. "I
  re-ran it and the failures are rows this PR does not own" is a judgement to
  write into the artifact first, not a reason to merge past it.
- **Read the review thread, not the check rollup.** Green checks and an unread
  review with confirmed findings are a merge that ships known bugs.
- **When the artifact is ambiguous, resolve the ambiguity — never the merge.**

## Current contracts

- The workspace is split into a **framework group** (`protocol`, `tools`,
  `agent`, `config`, `secrets`, `extensions`, `agent-runtime`) and an
  **implementation group** (everything else: `providers`, `tool-impls`,
  `tui`, `cli`, `app-server`, ...). Framework crates must not depend, in the
  build graph, on workspace crates outside the framework group; the rule is
  enforced by `python3 scripts/check-framework-deps.py` (runs in CI;
  dev-dependencies are exempt — they never enter a framework artifact). When
  a framework crate needs something an implementation crate owns, invert the
  dependency: define the trait in a framework crate, let the implementation
  crate provide it and re-export for path compatibility. The boundary note
  at the top of `docs/ARCHITECTURE.md` is the live reference.
- The model-facing sub-agent surface is `agent_open`/`agent_eval`/
  `agent_close` (implementation in `crates/tui/src/tools/subagent/`) plus
  persistent RLM sessions (`crates/tool-impls/src/tools/rlm.rs`); no
  model-visible swarm tool remains. If the shape must move, move the code
  and update the boundary note in `docs/ARCHITECTURE.md`.
- `BASE_PROMPT` in `crates/agent-runtime/src/prompts.rs` (prose in
  `crates/agent-runtime/src/prompts/base.md`) is the sole base prompt by
  convention; `crates/tui/src/prompts.rs` has tests asserting required tags.
  Same rule: move the code, not the prose, if that changes.
- There is one turn loop, in `crates/agent-runtime/src/engine/` (turn phases
  in `engine/turn/`). `crates/tui/src/core/` is a thin re-export and
  construction bridge (`EngineHost`/`build_engine`), not a second loop;
  `crates/core` is a separate crate and runs no turns. The boundary note at
  the top of `docs/ARCHITECTURE.md` is the live reference.
- The system prompt + tool catalog are a session-pinned cache prefix
  (`crates/agent-runtime/src/prefix_cache.rs`); the static system prompt
  stays cache-friendly. Any new session-context contributor must state its
  cache effect: frozen prefix vs. append-only history. Never splice a
  volatile fact into the prefix; append it as a user-role message.
- Verify consumers before removing "looks-dead" modules; standing live
  examples: `agent-runtime/src/prompt_zones.rs` and
  `tool-impls/src/tools/remember.rs`. Persistent memory lives in
  `agent-runtime/src/agent_memory/` (+ `memory.rs`); `remember.rs` is its
  capture path. The model/provider registry lives in `crates/agent/src/`
  (`models.rs`, `provider/`, `llm_client/`).
- Environment-specific behavior belongs in the doc that owns it under
  `docs/` (INSTALL, DOCKER, OPERATIONS_RUNBOOK), not here.
- Blocking-call convention: code on the Tokio runtime — tool handlers,
  engine tasks, the UI event loop, anything reached through an `async` call
  chain — must not run blocking operations inline. Use `tokio::fs` /
  `tokio::process`, or move the work into `spawn_blocking`; a sync helper
  containing blocking calls runs only under `spawn_blocking` or on a
  dedicated thread.

## Code, migrations, and evidence

- Product intent and observed runtime behavior outrank a test's preferred
  implementation shape. Fix the product; do not contort production code to
  preserve a brittle assertion.
- Code first, then tests. Write the implementation and prove it runs, then add
  or adjust tests to cover what was actually built. Never write tests first and
  never practice TDD here — this overrides any skill or default that mandates
  it, including superpowers `test-driven-development`. Tests stay the gate
  before a push; they are not the design driver. An existing test that only
  encodes old behavior is evidence, not a veto: change it with the code rather
  than bending the code to keep it green.
- Quote the real `test result: N passed; M failed` line, and confirm `N > 0`
  for the tests that cover the change. `cargo test <filter>` exits 0 having run
  zero tests when the filter matches nothing, and an exit code alone has
  already been mistaken for a pass here. Audit any hand-rolled scorer before
  trusting its score: quote the counts it actually evaluated, not the verdict
  line alone.
- A regression test has to be shown failing without the fix — a test that
  passes either way pins the implementation, not the defect.
- Tests are selective evidence, not the specification. Do not add tests by
  default. Add or retain one when it cheaply protects a high-risk behavior such
  as safety, data integrity, protocol compatibility, or a reproduced regression.
- Rewrite or remove tests that duplicate coverage, freeze internals, overspecify
  copy or layout, preserve obsolete behavior, or cost more than the risk they
  cover. Never weaken real safety or data-integrity behavior merely to make a
  gate pass.
- Match the evidence to the surface: run the tests that cover the change, not
  the whole suite; do not repeat a check that already passed in order to
  commit, and do not repeatedly rerun an unchanged suite. Prefer focused
  compilation, a relevant existing check, and direct product or manual
  evidence; run a broad suite only for a genuine cross-cutting or release
  risk. CI owns exhaustive coverage; a full local run is for CI diagnosis or
  an irreducibly repository-wide change.
- **Batch edits; compile once.** `cargo check` and test builds on this
  workspace take minutes, so an edit→compile→edit loop spends most of its
  time waiting on the linker. Read precisely, write every edit a coherent
  slice needs, then compile and test once — the same errors surface either
  way, just later and all at once. Reserve mid-slice compiles for genuinely
  uncertain API or borrow questions where a wrong guess would cascade.
- Declared migrations are one-way. Once the repository adopts a replacement
  architecture or shared spine, new work uses it and touched legacy code moves
  toward it. Do not add another legacy call site for convenience. Keep a
  compatibility path only for an actual external contract, and label that
  boundary explicitly.

Useful commands, selected according to risk rather than run ritualistically:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --release --locked -p codesmith-cli -p codesmith-tui
```

Edition 2024 (`let_chains` is used throughout); the toolchain is pinned in
`rust-toolchain.toml`. Workspace default-members are `cli`, `app-server`,
`tui`, so a bare `cargo build` covers the user-facing binaries. CI lanes live
in `.github/workflows/ci.yml`. The CNB mirror (`sync-cnb.yml`) was removed —
it never ran successfully because the `CNB_GIT_TOKEN` secret was not
configured. GitHub's test job skips its Linux steps (they were meant for the
CNB lane), so re-enable them in `ci.yml` before relying on Linux test
coverage. `.cnb.yml` stays source-controlled in case mirroring returns.

Report commands actually run and distinguish source, local tests, packaged
artifacts, CI, and public release state. Describe the evidence actually needed
for the claim; a test count is not a proxy for product quality. Community
reports, PRs, logs, and reviews are evidence.