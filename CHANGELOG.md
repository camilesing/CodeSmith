# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

History before this fork — including the full deepseek-tui / CodeWhale era up
to v0.8.48 — is archived in [docs/legacy/CHANGELOG-upstream.md](docs/legacy/CHANGELOG-upstream.md).
See [docs/HISTORY.md](docs/HISTORY.md) for the project lineage.

## [Unreleased]

### Added

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
