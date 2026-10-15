# CodeSmith


[简体中文 README](README.zh-CN.md)

````bash
➜  CodeSmith git:(main) ✗ codesmith
CodeSmith 0.9.0 (4fe017cfcc60) —  GLM5.3 · AGENT mode · preset middle
workspace /Users/camile/Work/Rust/CodeSmith · /help lists commands (Tab completes, Shift+Tab cycles the mode) · ? shows the keyboard shortcuts · Ctrl+C cancels a turn
● idle │ middle │ 🧭 0% (48/200k) │ 🚥agent │ 🤖0 │ GLM5.3 │ 🧠high │ v0.9.0 (4fe017cfcc60)
❯ What the project?
CodeSmith is a terminal coding agent for open-weight models — DeepSeek, Moonshot, OpenAI
-compatible gateways, vLLM, Ollama,
 and similar. The idea: a model answers a question; an agent finishes a task
. CodeSmith is the harness in between.

Key traits:

- **Written
 Constitution** — ranks competing authorities (user intent outranks stale instructions, verification
 outranks confidence).
- **Approval gates
 + OS sandboxing** — tool calls that
 write are gated; execution is sandboxed.
- **
Rollback snapshots** — every turn leaves one, so edits are revert
ible.
- **Sub-agents** — up to 20 run
 concurrently for
 parallel work.
- **Provider
-neutral** — `rig-core` is one swappable implementation behind its own `LlmClient`/`Tool` traits;
 not tied to one vendor.

It
's a Rust
 workspace (~20 crates split into
 a framework group and an implementation group), a fork of CodeWhale,
 itself derived from deepseek-tui. Not affiliated
 with DeepSeek Inc. MIT licensed.

Quick
start:

```bash
codesmith auth set --provider deepseek   # or CODE
SMITH_API_KEY
codesmith doctor
 # verify setup
codesmith                                # interactive REPL
```

Docs live
in `docs/`
— `GUIDE.md` to start, `HARNESS.md` for how the agent machinery
works, `ARCHITECTURE.md` for crate boundaries
. Note the install channels (npm/Home
brew/Docker) aren't live yet; build
from source with `cargo install --path crates/cli --locked`.
● idle │ middle │ 🧭 8% (15k/200k) │ 🚥agent │ 🤖0 │ claude-sonnet-4-5 │ 🧠high │ v0.9.0 (4fe017cfcc60)
````

## Install

> [!WARNING]
> **Install channels are not ready yet.** npm / Homebrew / Scoop / crates.io / Docker publishing is still in progress — none of those commands (China mirrors included) work today. Until then, [build from source](docs/INSTALL.md#7-build-from-source):

```bash
git clone https://github.com/camilesing/CodeSmith.git && cd CodeSmith
cargo install --path crates/cli --locked   # provides `codesmith`
```

## What Is It?

A model answers a question; an agent finishes a task. CodeSmith is the harness in between: a written Constitution ranks competing authorities (user intent outranks stale instructions, verification outranks confidence), tools are approval-gated and OS-sandboxed, every turn leaves a rollback snapshot, and up to 20 sub-agents run concurrently. How it all works: [docs/HARNESS.md](docs/HARNESS.md).

## Quickstart

```bash
codesmith auth set --provider deepseek   # or export CODESMITH_API_KEY
codesmith doctor                         # verify setup & connectivity
codesmith                                # interactive REPL
```

Modes — **Plan** (read-only) / **Agent** (default, gated) / **YOLO** (auto-approve): [docs/PRESETS.md](docs/PRESETS.md) · other providers: [docs/PROVIDERS.md](docs/PROVIDERS.md)

## One Key, Four Tiers

Don't read a hundred config keys to get the tool you want — pick a tier. `preset` is one config.toml key that baselines tool surface, thinking depth, memory persistence, sub-agent cap, and a set of resource switches (code index, LSP diagnostics, snapshots). Explicit keys always win; deviations show as `diy`:

```bash
codesmith --preset simple    # Pi-style minimal: 9 tools, no index/LSP/memory
codesmith --preset all       # everything stable on, 20 sub-agents
codesmith preset show        # the tier matrix + your effective preset
```

Four progressive tiers ship built-in — `simple` / `middle` *(factory default)* / `all` / `experiment` — plus the `plan` workflow preset. Yours live in `~/.codesmith/presets/` and per-project `.codesmith/presets/` — commit the project ones, that's the sharing story. Memory is a dial like everything else: `goldfish` (no cross-session memory) / `notebook` (only what you explicitly save) / `elephant` (auto memory with budget + decay). Full schema and semantics: [docs/PRESETS.md](docs/PRESETS.md).

## Documentation

Get started: [user guide](docs/GUIDE.md) · [presets & approvals](docs/PRESETS.md) · [skills](docs/SKILLS.md) · [memory](docs/MEMORY.md) · [localization](docs/LOCALIZATION.md) · [full command catalog](docs/CLI.md)

Configure: [configuration](docs/CONFIGURATION.md) · [providers](docs/PROVIDERS.md) · [install](docs/INSTALL.md) · [Docker](docs/DOCKER.md)

Internals & extension: [harness](docs/HARNESS.md) · [architecture](docs/ARCHITECTURE.md) · [design internals](docs/DESIGN_INTERNALS.md) · [sandbox](docs/SANDBOX.md) · [sub-agents](docs/SUBAGENTS.md) · [MCP](docs/MCP.md) · [hooks](docs/HOOKS.md) · [script mods](docs/MODS.md) · [runtime API & Zed ACP](docs/RUNTIME_API.md) · [ops runbook](docs/OPERATIONS_RUNBOOK.md) · [release process](docs/RELEASE_RUNBOOK.md) · [changelog](CHANGELOG.md)

## Project

CodeSmith is a fork of [CodeWhale](https://github.com/Hmbown/CodeWhale) (derived from deepseek-tui) — thanks to Hunter Bown and upstream contributors.

Questions & bugs: [open an issue](https://github.com/camilesing/CodeSmith/issues/new/choose) · contributing: [CONTRIBUTING.md](CONTRIBUTING.md) · security: [SECURITY.md](SECURITY.md) (no public issues)

> *Not affiliated with DeepSeek Inc.* · License: [MIT](LICENSE)
