# CodeSmith

> Terminal coding agent for open-source and open-weight coding models — DeepSeek, Moonshot, OpenAI-compatible gateways, vLLM, Ollama, and more. Streams reasoning, edits workspaces behind approval gates, and auto mode picks model + thinking level per turn.

[简体中文 README](README.zh-CN.md)

![codesmith screenshot](assets/screenshot.png)

## Install

> [!WARNING]
> **Install channels are not ready yet.** npm / Homebrew / Scoop / crates.io / Docker publishing is still in progress — none of those commands (China mirrors included) work today. Until then, [build from source](docs/INSTALL.md#7-build-from-source):

```bash
git clone https://github.com/camilesing/CodeSmith.git && cd CodeSmith
cargo install --path crates/cli --locked   # provides `codesmith`
cargo install --path crates/tui --locked   # provides `codesmith-tui` (required companion)
```

## What Is It?

A model answers a question; an agent finishes a task. CodeSmith is the harness in between: a written Constitution ranks competing authorities (user intent outranks stale instructions, verification outranks confidence), tools are approval-gated and OS-sandboxed, every turn leaves a rollback snapshot, and up to 20 sub-agents run concurrently. How it all works: [docs/HARNESS.md](docs/HARNESS.md).

## Quickstart

```bash
codesmith auth set --provider deepseek   # or export CODESMITH_API_KEY
codesmith doctor                         # verify setup & connectivity
codesmith                                # interactive TUI
```

Modes — **Plan** (read-only) / **Agent** (default, gated) / **YOLO** (auto-approve): [docs/MODES.md](docs/MODES.md) · other providers: [docs/PROVIDERS.md](docs/PROVIDERS.md)

## Documentation

Get started: [user guide](docs/GUIDE.md) · [modes & approvals](docs/MODES.md) · [keybindings](docs/KEYBINDINGS.md) · [skills](docs/SKILLS.md) · [memory](docs/MEMORY.md) · [localization](docs/LOCALIZATION.md) · [full command catalog](docs/CLI.md)

Configure: [configuration](docs/CONFIGURATION.md) · [providers](docs/PROVIDERS.md) · [install](docs/INSTALL.md) · [Docker](docs/DOCKER.md)

Internals & extension: [harness](docs/HARNESS.md) · [architecture](docs/ARCHITECTURE.md) · [design internals](docs/DESIGN_INTERNALS.md) · [sandbox](docs/SANDBOX.md) · [sub-agents](docs/SUBAGENTS.md) · [MCP](docs/MCP.md) · [hooks](docs/HOOKS.md) · [runtime API & Zed ACP](docs/RUNTIME_API.md) · [ops runbook](docs/OPERATIONS_RUNBOOK.md) · [release process](docs/RELEASE_RUNBOOK.md) · [changelog](CHANGELOG.md)

## Project

CodeSmith is a fork of [CodeWhale](https://github.com/Hmbown/CodeWhale) (derived from deepseek-tui) — thanks to Hunter Bown and upstream contributors.

Questions & bugs: [open an issue](https://github.com/camilesing/CodeSmith/issues/new/choose) · contributing: [CONTRIBUTING.md](CONTRIBUTING.md) · security: [SECURITY.md](SECURITY.md) (no public issues)

> *Not affiliated with DeepSeek Inc.* · License: [MIT](LICENSE)
