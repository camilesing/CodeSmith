# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

History before this fork — including the full deepseek-tui / CodeWhale era up
to v0.8.48 — is archived in [docs/legacy/CHANGELOG-upstream.md](docs/legacy/CHANGELOG-upstream.md).
See [docs/HISTORY.md](docs/HISTORY.md) for the project lineage.

## [Unreleased]

### Added

- **Three-zone prompt contract wired into the engine request path
  (#2264 Phase 2)**: the `prompt_zones` types are now load-bearing instead
  of scaffolding. `Session.messages` is an `AppendLog` — `push` is the
  only everyday mutation the type expresses (`insert` / `remove` /
  `truncate` / `clear` / index writes do not compile); sanctioned
  wholesale replacements (compaction, overflow recovery, `/edit` rollback,
  session restore, cycle reseeds, front trims) all funnel through
  `AppendLog::rebuild` with a named `RebuildReason`, recorded in an audit
  record and surfaced as `Event::TranscriptRebuilt`. The framework
  `ChatHistory` trait gains an audited `replace_all` hook (default:
  clear + push loop) that `SessionChatHistory` overrides to land mid-run
  compaction/recovery replacements in the same record atomically. The
  per-step `MessageRequest` is assembled through `ThreeZoneRequest`
  (`messages` = log slice + scratch tail only) — byte-identical to the
  legacy direct construction, pinned by the
  `three_zone_assembly_sends_verbatim_log_snapshot` regression test.
  `PrefixStabilityManager` now consumes the same `FrozenPrefix` the
  request path freezes per step, unifying the two parallel fingerprint
  implementations; tool identity hashes the full sorted JSON of every
  tool definition instead of names only, so a tool description or schema
  edit is now detected as prefix drift. `TurnScratch` is wired as the
  engine's per-turn staging area (working-set paths + the composed user
  message, committed to the log before the request loop, cleared at the
  turn boundary; the request-time scratch is empty in production).
  `/cache zones` reports live zone state including the bounded rebuild
  audit ("why did my cache reset") instead of confessing it is not wired.
- **Parse-gated file editing (P0-1)**: `write_file`, `edit_file`,
  `apply_patch`, and `fim_edit` now syntax-check writes to `.rs` / `.toml` /
  `.json` files *before* anything touches disk. The gate is regression-only —
  a write is rejected only when the file parsed cleanly before the edit and
  the new content does not, so already-broken files (and new files) stay
  editable. Rejections report the offending line and column and leave the
  file unmodified; `Cargo.lock` is exempt. Rust files that were
  rustfmt-clean before the edit are re-normalized with `rustfmt` after the
  gate passes (disclosed in the tool result) to keep subsequent patch
  anchors stable — a missing rustfmt never blocks a write. Rust parsing uses
  `syn` behind the new `parse-gate` cargo feature of `codesmith-agent-runtime`
  (enabled by `codesmith-tui` and `codesmith-tool-impls`; builds without it
  compile a pass-through gate). Configure with `[edit] parse_gate`
  (bool, default `true`).
- **Stream termination proof (P0-3)**: the rig adapter's stream mapper no
  longer forges `MessageDelta`/`MessageStop` when the provider's SSE
  connection dies silently. rig's `Final` payload is treated as the
  termination proof (rig-core 0.39 collapses the wire `finish_reason` /
  `[DONE]` evidence into it); a stream that ends without one is surfaced as
  a retryable "interrupted connection" error, so zero-content rounds
  transparently re-send and partial rounds surface the content they
  received. The engine adds a matching defense at its own seam: any stream
  that ends without `MessageStop` is treated as interrupted, never as a
  clean completion. The terminal `stop_reason` is derived instead of
  hard-coded `end_turn`: billed output reaching the requested `max_tokens`
  cap reports `max_tokens` (arming the P0-2 truncation gate on real
  traffic), tool rounds report `tool_use`. Transparent stream retries now
  also drive the structured retry banner (`retry_status`: attempt,
  countdown, reason — rendered in the TUI footer). All four rig-backed
  provider factories inject a reqwest backend that falls back to HTTP/1.1
  after the first HTTP/2 protocol failure (sticky, one replay).

## [0.1.0] - 2026-08-25

### Added

- Initial public release of **CodeSmith**, a terminal coding agent for
  open-source and open-weight coding models (DeepSeek, Moonshot, OpenAI,
  Anthropic, NVIDIA NIM, OpenRouter, SiliconFlow, Fireworks, Novita, vLLM,
  SGLang, Ollama, and any OpenAI-compatible gateway).
- Matched binary pair: `codesmith` dispatcher CLI + `codesmith-tui` runtime.
- Streaming chat with reasoning-block rendering and per-turn thinking levels.
- Auto mode that selects model and thinking effort per turn.
- Approval-gated workspace editing, sandboxed shell execution, and YOLO mode.
- MCP client support, persistent task/session state, sub-agent teams, and a
  local skill registry.
- npm installer package, Homebrew tap, Docker image, and Nix packaging.

[Unreleased]: https://github.com/camilesing/CodeSmith/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/camilesing/CodeSmith/releases/tag/v0.1.0
