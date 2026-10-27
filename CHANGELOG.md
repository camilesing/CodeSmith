# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

One section per release tag, describing what changed since the previous tag.
Aim for **400–1200 characters** of prose per section (headings, markup, and
notes not counted): 400 is the floor — a section too thin to be worth reading
is not worth writing — and 1200 the ceiling, past which the receipts (test
results, file lists, review threads) belong in the pull request that landed
the change.

History before this fork — including the full deepseek-tui / CodeWhale era up
to v0.8.48 — is archived in [docs/legacy/CHANGELOG-upstream.md](docs/legacy/CHANGELOG-upstream.md).

<!-- The compare links sit above the first heading on purpose: `/release-notes`
     splits this file on `## ` headings, so a trailing link block would be
     printed as part of v0.1.0's body. -->

[Unreleased]: https://github.com/camilesing/CodeSmith/compare/v0.11.0...HEAD
[0.11.0]: https://github.com/camilesing/CodeSmith/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/camilesing/CodeSmith/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/camilesing/CodeSmith/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/camilesing/CodeSmith/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/camilesing/CodeSmith/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/camilesing/CodeSmith/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/camilesing/CodeSmith/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/camilesing/CodeSmith/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/camilesing/CodeSmith/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/camilesing/CodeSmith/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/camilesing/CodeSmith/releases/tag/v0.1.0

## [Unreleased]

- **The plan-approval question sits on its own line** — `exit_plan_mode` prints
  the plan body and then asks `approve this plan?` on a row of its own, with the
  three keys on the row under it (`[y] approve and implement · [n] reject ·
  [r] revise`), so the question is no longer read as part of the option list.
  Keys and meanings are unchanged: `[r]` still takes your reason on the same
  line, and a bare `[r]` still asks for it on the next one.

## [0.11.0] - 2026-10-25

### Added

- Sandbox enforcement that fails closed (Landlock and seatbelt, escalation
  behind approval); MCP over Streamable HTTP; ACP as sub-agent provider and
  session host; an attachment store; `search_sessions`, `lsp_navigate`,
  `/feedback`, an inbox journal, and one web service for search and fetch.

- The policy seam (`compact-policy`, `large-output-route`, `working-set`;
  last-wins, `/mods show` names the winner), registration scopes, `on_event`,
  ephemeral session mods, and single-mod load/unload.

### Changed

- Plan approval is a three-way gate (approve, refuse-and-steer,
  `[r] <feedback>` revises); `[a]` covers a tool for the session; independent
  calls and sub-agent spawns fan out concurrently.

### Fixed

- Every approval decision reaches the session file; a truncated call replays
  as an object; poisoned mutexes recover; config defaults fail closed; tool I/O
  is bounded; framing-tag closers are defused.

### Security

- Manifests reject unsafe ids and entry paths at parse; `register_provider`
  rejects header injection; a guard that cannot bind fails the load rather than
  silently bypassing. Credentials are scrubbed from logs, argv, and dumps;
  audit writes take an fd-lock; `~/.codesmith` is created `0700`; the npm
  installer is https-only with checksums.

> **Alpha, up to and including v0.10.0.** Every release in this file through
> v0.10.0 is an alpha. Interfaces, config keys, session-file formats, tool
> behaviour, and packaging may change or break between releases with no
> deprecation notice and no compatibility path. Even a commit identifier is
> not a stable reference: work is rebased, squashed, and re-landed, so a SHA
> recorded today can be gone from the repository tomorrow — cite a version,
> not a hash. Nothing here is stable, and a version number below v0.11.0
> implies no compatibility promise.

## [0.10.0] - 2026-10-15

### Added

- The line-based REPL and a single `codesmith` binary: the ratatui TUI and
  `codesmith-tui` are gone, replaced by a Claude Code style key table and
  slash-command surface — mid-turn composer, `!` shell lines, `/rewind` file
  snapshots, `/permissions`, `/branch`, `/btw` — with sessions auto-saved at
  each turn boundary.

### Changed

- The approval prompt names the tool and its argument summary and states what
  each answer covers (`this command family`, `patches to the same paths`,
  `requests to the same host`, `this exact call`); a mistyped subcommand fails
  loud instead of opening a session; out-of-workspace paths ask rather than
  fail; plan mode opens read-only children.

### Fixed

- A truncated tool call no longer poisons every later request; a path approval
  runs the call that asked; collapsed tool lines pair each completion with its
  own arguments; the live status line no longer shares a row with streamed
  text. Security and bug sweeps across every crate: execpolicy precedence,
  sandbox roots, path confinement, secret redaction, stream back-fill.

## [0.9.0] - 2026-10-09

### Added

- The framework/implementation crate split with a dependency-direction guard,
  and the seams it enables: `register_provider`, deny-only tool guards, mod
  skills in the catalogue, named system-prompt sections, record/replay of LLM
  envelopes, an event-dispatch contract table, and a capability graph that
  governs tool selection for the session and for sub-agents.

- Session persistence: the per-step request envelope, the compaction fact
  ledger, and the todo/plan projections all survive a restart, and `ToolResult`
  carries a canonical value beside its rendered text.

### Changed

- Compaction keeps facts — a ledger, a reflection loop that turns failures into
  invariants, a deliverables watchdog, a skeleton-first statute — and the guard
  pipeline lands: docs-pair drift and the dead-code budget are now checked.

## [0.8.0] - 2026-10-04

### Changed

- CI becomes a real gate: `-D warnings` is enforced with rustfmt and the
  cross-target std in the pinned toolchain, the first clean pass surfaced
  failures that had been masked, and fixture-dylib resolution, Windows path
  semantics, MCP session teardown, web_run state sharing, and a freshly-booted
  `Instant` underflow were repaired (Windows AV quarantine where unresolvable).

- The CNB mirror sync workflow is removed; `AGENTS.md` is tracked instead of
  ignored, and the CI facts it carried are pruned.

### Added

- TUI readline parity: kill ring, undo, and `!` shell lines.

## [0.7.0] - 2026-10-03

### Added

- The Rhai mod layer: hooks, model-callable tools, slash commands, mod
  self-install, an activation gate, and hot reload — with `docs/MODS.md`
  rewritten tutorial-first around verified minimal snippets and split into the
  bilingual pair `MODS.md` / `MODS_cn.md`.

### Fixed

- Clippy warnings cleared across agent-runtime, the config env lock tolerates
  poisoning, and the MCP no_proxy loopback and nvidia-nim tests are aligned
  with the behavior they cover.

## [0.6.0] - 2026-10-03

### Added

- Static command analysis over a real bash AST (tree-sitter), so compound
  commands are decomposed before policy decides and a dangerous segment cannot
  hide behind a friendly prefix; tier-based routing on the strongest configured
  brain; an environment-gated rate-limit hold that keeps a quota-walled turn
  alive instead of failing it.

### Changed

- `max_steps` collapses into one constant (`DEFAULT_MAX_STEPS`, raised to 1024),
  GLM-1214 outbound messages are sanitized, and DeepSeek usage frames default
  their missing `prompt_cache_*` fields.

## [0.5.0] - 2026-09-27

### Added

- Layered configuration presets (simple / middle / all / experiment) that fill
  only unset keys and keep a `diy` deviation state — the `modes` → `presets`
  rename included — plus a result-claim verifier that replays a turn's
  test/build claims into four-element verdicts and an evolution jsonl log that
  `/verify` reads for the claim-match rate.

- An advisory doctor fallback that root-causes findings with a model, and
  offline MEMORY.md sleep learning that consolidates memory through a validated
  merge.

### Fixed

- Security hardening from a full-repo review: sandbox runs fail closed and
  report honestly, execpolicy resolves compound-command segments, path
  traversal and memory/skill trust bounds are enforced, locks tolerate
  poisoning, and writes are atomic.

## [0.4.0] - 2026-09-26

### Added

- Parse-gated editing: a pre-write syntax gate rejects a write that would break
  a file which parsed cleanly before the edit — reporting line and column and
  leaving the file untouched — and re-normalizes rustfmt-clean Rust afterwards
  so patch anchors stay stable. `Cargo.lock` is exempt and a missing rustfmt
  never blocks a write.

- A length-stop gate that refuses to repair or execute a tool call truncated by
  the output limit, and a stream termination proof: `MessageStop` is required
  before a round counts as complete, `stop_reason` is derived rather than
  assumed, and a repeated HTTP/2 protocol failure falls back to HTTP/1.1.

### Changed

- The three-zone prompt contract is wired into the engine's request path, and
  clippy violations blocking `-D warnings` builds are repaired.

## [0.3.0] - 2026-09-23

### Fixed

- Three review rounds over the v0.2.0 features: billed tokens commit at the
  right stream boundary, context-length errors recover mid-stream instead of
  ending the turn, transient 5xx responses retry, and spillover handles its
  Running, stale, and pid states.

- Persisted grants are re-checked on lookup with `never`-mode precedence,
  `edit_file` matchers are UTF-8 safe, and the merged grants store writes
  atomically; freshness dedup, refill accounting, and usage-anchor invalidation
  are corrected.

## [0.2.0] - 2026-09-23

### Added

- Named runtime modes as one-file dial bundles that hot-switch mid-session,
  a session-aware working directory, `edit_file` anchor mode, and persistent
  approval grants that survive a restart.

- Adaptive stream retry, shell-output spillover to a file when a result is too
  large, stale-read hints, and usage-anchored context estimates behind a
  rapid-refill guard.

### Fixed

- Sandbox, secret, and tool-path hardening from a full-repo review, with stream
  liveness, provenance, and network defaults tightened alongside.

## [0.1.0] - 2026-08-25

### Added

- First public CodeSmith release: a terminal coding agent for open-source and
  open-weight models — DeepSeek, Moonshot, OpenAI and Anthropic compatible
  gateways, NVIDIA NIM, OpenRouter, SiliconFlow, Fireworks, Novita, vLLM,
  SGLang, Ollama — with streaming chat that renders reasoning blocks, per-turn
  thinking levels, auto mode, approval-gated workspace editing, a sandboxed
  shell, YOLO mode, MCP clients, persistent sessions, sub-agent teams, and a
  local skill registry.

- Shipped as the matched `codesmith` dispatcher plus `codesmith-tui` runtime
  pair, with an npm installer, a Homebrew tap, a Docker image, and Nix
  packaging.