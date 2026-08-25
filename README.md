# CodeSmith

> Terminal coding agent for DeepSeek V4. It runs from the `codesmith` command, streams reasoning blocks, edits local workspaces with approval gates, and includes an auto mode that chooses both model and thinking level per turn.

[简体中文 README](README.zh-CN.md)


## Install

`codesmith` installs as a matched pair of self-contained Rust release binaries:
the `codesmith` dispatcher command and the sibling `codesmith-tui` runtime it
launches for interactive sessions. npm, Homebrew, and Docker install both for
you; Cargo and manual installs must put both binaries in the same directory
(normally a directory on your `PATH`). The npm package is only an
installer/wrapper for those release binaries; the agent does not run on Node.

```bash
# 1. npm — easiest if you already use Node. The package downloads the
#    matching prebuilt Rust binaries from GitHub Releases.
npm install -g codesmith

# 2. Cargo — no Node needed. Requires Rust 1.88+ (the crates use the
#    2024 edition; older toolchains fail with "feature `edition2024` is
#    required"). Run `rustup update` first, or use a non-Cargo path below.
cargo install codesmith-cli --locked   # `codesmith` (entry point)
cargo install codesmith-tui     --locked   # `codesmith-tui` (TUI binary)

# 3. Homebrew — macOS package manager.
#    The tap/formula name is legacy; it installs codesmith and codesmith-tui.
brew tap Hmbown/deepseek-tui
brew install codesmith

# 4. Direct download — platform archive from GitHub Releases.
#    https://github.com/Hmbown/CodeSmith/releases
#    Archives include both codesmith and codesmith-tui plus an install script.
#    Individual binaries are also attached for scripts; keep the pair together.

# 5. Docker — prebuilt release image.
docker volume create codesmith-home
docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/hmbown/codesmith:latest
```

> In mainland China, speed up the npm path with
> `--registry=https://registry.npmmirror.com`, or use the
> [Cargo mirror](#china--mirror-friendly-installation) below.
>
> Download safety: official release binaries live under
> `https://github.com/Hmbown/CodeSmith/releases`. For manual downloads,
> verify the SHA-256 manifest and avoid look-alike repositories or search-result
> mirrors. See [download safety and checksums](docs/INSTALL.md#2-download-safety-and-checksums).

Already installed? Use the updater that matches the install path:

```bash
codesmith update                         # release-binary updater
npm install -g codesmith@latest      # npm wrapper
brew update && brew upgrade codesmith
cargo install codesmith-cli --locked --force
cargo install codesmith-tui     --locked --force
```

> codesmith update now supports --proxy, update through a proxy
> eg: codesmith update --proxy https://localhost:7897

[![CI](https://github.com/Hmbown/CodeSmith/actions/workflows/ci.yml/badge.svg)](https://github.com/Hmbown/CodeSmith/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/codesmith)](https://www.npmjs.com/package/codesmith)
[![crates.io](https://img.shields.io/crates/v/codesmith-cli?label=crates.io)](https://crates.io/crates/codesmith-cli)
[DeepWiki project index](https://deepwiki.com/Hmbown/CodeSmith)

![codesmith screenshot](assets/screenshot.png)

---

## What Is It?

A model answers a question. An agent finishes a task. The difference is
the harness — a system of rules, evidence, and feedback that keeps the
model oriented instead of drifting.

CodeSmith is that harness, built around DeepSeek V4 and guided by three ideas:

| Principle | How it works |
|---|---|
| **Start with trust** | Every turn begins with "A" — possibility before certainty, craft before convenience |
| **Clear jurisdiction** | A written Constitution with nine tiers of authority. User intent outranks stale instructions. Verification outranks confidence. |
| **Recursive improvement** | V4 helped write the harness. As the harness improves, V4 becomes more effective — and helps improve the harness further. Each turn starts stronger. |

It's open source, terminal-native, and packaged as a matched `codesmith` /
`codesmith-tui` Rust binary pair.

## How the Harness Works

Agentic models deal with conflicting information at scale: user intent,
project rules, system defaults, tool output, and stale memory all compete
for authority in a single turn. LLM-as-a-judge needs jurisdiction — which
source wins when they disagree?

CodeSmith answers this with a **Constitution** (`prompts/base.md`). It's a
formal hierarchy of law — Article VII ranks nine sources from the
Constitution's own articles down to prior-session handoffs. The user's
current message outranks stale project instructions. Live tool output
outranks assumptions. Verification outranks confidence. The model inherits
a clear chain of authority every turn and never has to guess which
directive to follow.

Seven articles sit above the hierarchy, defining the model's identity,
duties, and agency: a verification mandate (Article V — every action leaves
evidence, never declare success on faith), a coordination legacy (Article
VI — leave the workspace legible for the next intelligence), and a
primacy-of-truth clause (Article II — no lower rule may override it).

DeepSeek V4's prefix caching makes this practical. The Constitution is long
and detailed, but once cached it costs roughly 100× less per turn than a
cold read. The model references it recursively — peeking, scanning, and
querying through RLM sessions — revisiting information on demand rather
than relying on a single memorized pass. It performs more like an
open-book test than a closed one.

Because the authority structure is explicit, failure isn't hidden. Non-zero
exit codes, type errors from rust-analyzer arriving between turns, sandbox
denials — these are fed back as correction vectors. The model uses its own
drift to self-correct.

Three modes control the action space. Plan is read-only. Agent gates
destructive operations behind approval. YOLO auto-approves in trusted
workspaces. OS-level sandboxing is enforced per platform: macOS Seatbelt,
Linux Landlock + seccomp (plus optional bubblewrap), and a Windows Job
Object v1. See [docs/SANDBOX.md](docs/SANDBOX.md).

Fin — a cheap Flash call with thinking off — handles model auto-routing per
turn. `--model auto` is the default.

Every turn records a side-git snapshot outside your repo's `.git`.
`/restore` and `revert_turn` roll back the workspace.

Sub-agents run concurrently (up to 20). `agent_open` returns immediately;
results arrive inline as completion sentinels with a summary. Full
transcripts stay behind bounded handles through `agent_eval`. See
[docs/SUBAGENTS.md](docs/SUBAGENTS.md).

The rest of the surface: LSP diagnostics after every edit (rust-analyzer,
pyright, typescript-language-server, gopls, clangd, jdtls,
vue-language-server), RLM sessions for batched analysis, MCP protocol,
HTTP/SSE runtime API, persistent task queue, ACP adapter for Zed,
SWE-bench export, and live cost tracking with cache hit/miss breakdowns.

---

## The Harness

`codesmith` (dispatcher CLI) → `codesmith-tui` (companion binary) → ratatui interface ↔ async engine ↔ OpenAI-compatible streaming client. Tool calls route through a typed registry (shell, file ops, git, web, sub-agents, MCP, RLM) and results stream back into the transcript. The engine manages session state, turn tracking, the durable task queue, and an LSP subsystem that feeds post-edit diagnostics into the model's context before the next reasoning step.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full walkthrough.

### Sub-agents: Concurrent Background Execution

CodeSmith can dispatch multiple sub-agents that run in parallel — like a concurrent task queue:

- **Non-blocking launch.** `agent_open` returns immediately. The child gets its own fresh context and tool registry and runs independently. The parent keeps working.
- **Background execution.** Sub-agents execute concurrently (default cap: 10, configurable to 20). The engine manages the pool — no polling loop needed.
- **Completion notification.** When a sub-agent finishes, the runtime injects a `<codesmith:subagent.done>` sentinel into the parent's transcript. The human-readable summary — including the child's findings, changed files, and any risks — sits on the line immediately before the sentinel. The parent model reads that summary and integrates findings without an extra tool call.
- **Bounded result retrieval.** The full child transcript lives behind a `transcript_handle` accessible through `agent_eval`. When the summary isn't enough, the parent calls `handle_read` for slices, line ranges, or JSONPath projections — keeping the parent context lean without losing access to the details.

See [docs/SUBAGENTS.md](docs/SUBAGENTS.md) for the full sub-agent reference.

---

## Quickstart

```bash
npm install -g codesmith
codesmith --version
codesmith --model auto
```

Prebuilt binary pairs and platform archives are published for **Linux x64**, **Linux ARM64** (v0.8.8+), **macOS x64**, **macOS ARM64**, and **Windows x64**. For other targets (musl, riscv64, FreeBSD, etc.), see [Install from source](#install-from-source) or [docs/INSTALL.md](docs/INSTALL.md).

On first launch you'll be prompted for your [DeepSeek API key](https://platform.deepseek.com/api_keys). The key is saved to `~/.codesmith/config.toml` (legacy `~/.codesmith/config.toml` also supported) so it works from any directory without OS credential prompts.

You can also set it ahead of time:

```bash
codesmith auth set --provider deepseek   # saves to ~/.codesmith/config.toml
codesmith auth status                    # shows the active credential source

export CODESMITH_API_KEY="YOUR_KEY"      # env var alternative; use ~/.zshenv for non-interactive shells
codesmith

codesmith doctor                         # verify setup
```

If `codesmith doctor` says the rejected key came from `CODESMITH_API_KEY` (or
its `DEEPSEEK_API_KEY` legacy alias), remove
the stale export from your shell startup file, open a fresh shell, or run
`codesmith auth set --provider deepseek`. Use `codesmith auth status` to see the
config, keyring, and env-var source state without printing the key. Saved config
keys take precedence over the keyring and environment and are easier to rotate.

> To rotate or remove a saved key: `codesmith auth clear --provider deepseek`.

### Tencent Cloud / CNB Remote-First Path

For an always-on workspace you can control from a phone, use the Tencent-native
path: CNB mirror/source, Tencent Lighthouse HK, a Feishu/Lark long-connection
bridge, and optional EdgeOne for a deliberate public HTTPS edge. The runtime API
stays bound to localhost; EdgeOne is not used to expose `/v1/*`.

Start with [docs/TENCENT_CLOUD_REMOTE_FIRST.md](docs/TENCENT_CLOUD_REMOTE_FIRST.md),
then use [docs/TENCENT_LIGHTHOUSE_HK.md](docs/TENCENT_LIGHTHOUSE_HK.md) for the
server runbook.

### Auto Mode

Use `codesmith --model auto` or `/model auto` when you want codesmith to decide how much model and reasoning power a turn needs.

Auto mode controls two settings together:

- Model: `deepseek-v4-flash` or `deepseek-v4-pro`
- Thinking: `off`, `high`, or `max`

Before the real turn is sent, the app makes a small `deepseek-v4-flash` routing call with thinking off. That router looks at the latest request and recent context, then selects a concrete model and thinking level for the real request. Short/simple turns can stay on Flash with thinking off; coding, debugging, release work, architecture, security review, or ambiguous multi-step tasks can move up to Pro and/or higher thinking.

`auto` is local to codesmith. The upstream API never receives `model: "auto"`; it receives the concrete model and thinking setting chosen for that turn. The TUI shows the selected route, and cost tracking is charged against the model that actually ran. If the router call fails or returns an invalid answer, the app falls back to a local heuristic. Sub-agents inherit auto mode unless you assign them an explicit model.

Use a fixed model or fixed thinking level when you want repeatable benchmarking, a strict cost ceiling, or a specific provider/model mapping.

### Linux ARM64 (Raspberry Pi, Asahi, Graviton, HarmonyOS PC)

`npm i -g codesmith` works on glibc-based ARM64 Linux from v0.8.8 onward. You can also download prebuilt binaries from the [Releases page](https://github.com/Hmbown/CodeSmith/releases) and place them side by side on your `PATH`.

### China / Mirror-friendly Installation

If GitHub or npm downloads are slow from mainland China, use a Cargo registry mirror:

```toml
# ~/.cargo/config.toml
[source.crates-io]
replace-with = "tuna"

[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"
```

Then install both binaries (the dispatcher delegates to the TUI at runtime):

```bash
cargo install codesmith-cli --locked   # provides `codesmith`
cargo install codesmith-tui     --locked   # provides `codesmith-tui`
codesmith --version
```

Prebuilt binaries can also be downloaded from [GitHub Releases](https://github.com/Hmbown/CodeSmith/releases). Use `CODESMITH_RELEASE_BASE_URL` for mirrored release assets.

### Windows (Scoop)

[Scoop](https://scoop.sh) is a Windows package manager. The `codesmith` package is listed
in Scoop's main bucket, but that manifest updates independently and can lag the
GitHub/npm/Cargo release. Run `scoop update` first, then verify the installed
version with `codesmith --version`:

```bash
scoop update
scoop install codesmith
codesmith --version
```

Use npm or direct GitHub release downloads when you need the newest release
before Scoop's manifest catches up.


<details id="install-from-source">
<summary>Install from source</summary>

Works on any Tier-1 Rust target — including musl, riscv64, FreeBSD, and older ARM64 distros.

```bash
# Linux build deps (Debian/Ubuntu/RHEL):
#   sudo apt-get install -y build-essential pkg-config libdbus-1-dev
#   sudo dnf install -y gcc make pkgconf-pkg-config dbus-devel

git clone https://github.com/Hmbown/CodeSmith.git
cd CodeSmith

cargo install --path crates/cli --locked   # requires Rust 1.88+; provides `codesmith`
cargo install --path crates/tui --locked   # provides `codesmith-tui`
```

Both binaries are required. Cross-compilation and platform-specific notes: [docs/INSTALL.md](docs/INSTALL.md).

</details>

### Other API Providers

For the full shipped provider registry, including model IDs, auth variables,
base URLs, and capability boundaries, see [docs/PROVIDERS.md](docs/PROVIDERS.md).

```bash
# NVIDIA NIM
codesmith auth set --provider nvidia-nim --api-key "YOUR_NVIDIA_API_KEY"
codesmith --provider nvidia-nim

# AtlasCloud
codesmith auth set --provider atlascloud --api-key "YOUR_ATLASCLOUD_API_KEY"
codesmith --provider atlascloud

# Wanjie Ark
codesmith auth set --provider wanjie-ark --api-key "YOUR_WANJIE_API_KEY"
codesmith --provider wanjie-ark --model deepseek-reasoner

# OpenRouter
codesmith auth set --provider openrouter --api-key "YOUR_OPENROUTER_API_KEY"
codesmith --provider openrouter --model deepseek/deepseek-v4-pro
codesmith --provider openrouter --model arcee-ai/trinity-large-thinking
codesmith --provider openrouter --model qwen/qwen3.7-max

# Xiaomi MiMo
codesmith auth set --provider xiaomi-mimo --api-key "YOUR_XIAOMI_KEY"
codesmith --provider xiaomi-mimo --model mimo-v2.5-pro

# Novita
codesmith auth set --provider novita --api-key "YOUR_NOVITA_API_KEY"
codesmith --provider novita --model deepseek/deepseek-v4-pro

# Fireworks
codesmith auth set --provider fireworks --api-key "YOUR_FIREWORKS_API_KEY"
codesmith --provider fireworks --model deepseek-v4-pro

# SiliconFlow
codesmith auth set --provider siliconflow --api-key "YOUR_SILICONFLOW_API_KEY"
codesmith --provider siliconflow --model deepseek-ai/DeepSeek-V4-Pro

# Generic OpenAI-compatible endpoint
codesmith auth set --provider openai --api-key "YOUR_OPENAI_COMPATIBLE_API_KEY"
OPENAI_BASE_URL="https://openai-compatible.example/v4" codesmith --provider openai --model glm-5

# Custom DeepSeek-compatible endpoint
CODESMITH_BASE_URL="https://your-provider.example/v1" \
  CODESMITH_MODEL="deepseek-ai/DeepSeek-V4-Pro" \
  codesmith --provider deepseek

# Self-hosted SGLang
SGLANG_BASE_URL="http://localhost:30000/v1" codesmith --provider sglang --model deepseek-v4-flash

# Self-hosted vLLM
VLLM_BASE_URL="http://localhost:8000/v1" codesmith --provider vllm --model deepseek-v4-flash
# Trusted LAN vLLM over HTTP
VLLM_BASE_URL="http://192.168.0.110:8000/v1" codesmith --provider vllm --model deepseek-v4-flash

# Self-hosted Ollama
ollama pull codesmith-coder:1.3b
codesmith --provider ollama --model codesmith-coder:1.3b
```

Inside the TUI, `/provider` opens the provider picker and `/model` opens the
local model/thinking picker. `/provider openrouter` and `/model <id>` switch
directly, while `/models` explicitly fetches and lists live API models when the
active provider supports model listing.

---

## Release Notes

Release-specific changes live in [CHANGELOG.md](CHANGELOG.md). This README
stays focused on current install paths, core workflows, provider setup, runtime
interfaces, and extension points.

---

## Usage

```bash
codesmith                                         # interactive TUI
codesmith "explain this function"                 # one-shot prompt
codesmith exec --auto --output-format stream-json "fix this bug"  # NDJSON backend stream
codesmith exec --resume <SESSION_ID> "follow up"  # continue a non-interactive session
codesmith --model deepseek-v4-flash "summarize"   # model override
codesmith --model auto "fix this bug"             # auto-select model + thinking
codesmith --yolo                                  # auto-approve tools
codesmith auth set --provider deepseek            # save API key
codesmith doctor                                  # check setup & connectivity
codesmith doctor --json                           # machine-readable diagnostics
codesmith setup --status                          # read-only setup status
codesmith setup --tools --plugins                 # scaffold tool/plugin dirs
codesmith models                                  # list live API models
codesmith sessions                                # list saved sessions
codesmith resume --last                           # resume the most recent session in this workspace
codesmith resume <SESSION_ID>                     # resume a specific session by UUID
codesmith fork <SESSION_ID>                       # fork a saved session into a sibling path
codesmith serve --http                            # HTTP/SSE API server
codesmith serve --mobile                          # LAN mobile control page; token-gated by default
codesmith serve --acp                             # ACP stdio adapter for Zed/custom agents
codesmith run pr <N>                              # fetch PR and pre-seed review prompt
codesmith mcp list                                # list configured MCP servers
codesmith mcp validate                            # validate MCP config/connectivity
codesmith mcp-server                              # run dispatcher MCP stdio server
codesmith update                                  # check for and apply binary updates
```

### Branching Conversations

Saved sessions are intentionally branchable. `codesmith fork <SESSION_ID>` copies
an existing saved session into a new sibling session, records the parent session
id in metadata, and opens that fork so you can explore an alternate direction
without polluting the original path. The session picker and `codesmith sessions`
mark forked sessions with their parent id.

Inside the TUI, Esc-Esc backtrack can rewind the active transcript to a prior
user prompt and put that prompt back in the composer for editing. `/restore`
and `revert_turn` are separate workspace rollback tools: they restore files
from side-git snapshots but do not rewrite conversation history.

Docker images are published to GHCR for release builds:

```bash
docker volume create codesmith-home

docker run --rm -it \
  -e CODESMITH_API_KEY="$CODESMITH_API_KEY" \
  -v codesmith-home:/home/codesmith/.codesmith \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/hmbown/codesmith:latest
```

See [docs/DOCKER.md](docs/DOCKER.md) for pinned tags, local image builds,
volume ownership notes, and non-interactive pipeline usage.

### Zed / ACP

CodeSmith can run as a custom Agent Client Protocol server for editors that
spawn local ACP agents over stdio. In Zed, add a custom agent server:

```json
{
  "agent_servers": {
    "CodeSmith": {
      "type": "custom",
      "command": "codesmith",
      "args": ["serve", "--acp"],
      "env": {}
    }
  }
}
```

The first ACP slice supports new sessions and prompt responses through your
existing CodeSmith config/API key. Tool-backed editing and checkpoint replay are
not exposed through ACP yet.

Community-maintained adapter: [acp-codesmith-adapter](https://github.com/rockeverm3m/acp-codesmith-adapter)
bridges `codesmith exec --auto` to `cc-connect` for users who need tool-backed
ACP workflows outside the built-in Zed slice.

### Keyboard Shortcuts

| Key | Action |
|---|---|
| `Tab` | Complete `/` or `@` entries; while running, queue draft as follow-up; otherwise cycle mode |
| `Shift+Tab` | Cycle reasoning-effort: off → high → max |
| `F1` | Searchable help overlay |
| `Esc` | Back / dismiss |
| `Ctrl+K` | Command palette |
| `Ctrl+R` | Resume an earlier session |
| `Alt+R` | Search prompt history and recover cleared drafts |
| `Ctrl+S` | Stash current draft (`/stash list`, `/stash pop` to recover) |
| `@path` | Attach file/directory context in composer |
| `↑` (at composer start) | Select attachment row for removal |

Full shortcut catalog: [docs/KEYBINDINGS.md](docs/KEYBINDINGS.md).

---

## Modes

| Mode | Behavior |
| --- | --- |
| **Plan** 🔍 | Read-only investigation — model explores and proposes a plan before making changes; multi-step investigations use `checklist_write` |
| **Agent** 🤖 | Default interactive mode — multi-step tool use with approval gates; substantial work is tracked with `checklist_write` |
| **YOLO** ⚡ | Auto-approve all tools in a trusted workspace; multi-step work still keeps a visible checklist |

---

## Configuration

User config: `~/.codesmith/config.toml` . Project overlay: `<workspace>/.codesmith/config.toml` (denied: `api_key`, `base_url`, `provider`, `mcp_config_path`). [config.example.toml](config.example.toml) has every option.

Custom DeepSeek-compatible endpoints usually do not need a new provider. Keep
`provider = "deepseek"` and set `[providers.deepseek].base_url` / `model`, or
use `provider = "openai"` for generic OpenAI-compatible gateways. Keep
`provider`, `api_key`, and `base_url` in user config or environment variables;
project overlays cannot set them.

Key environment variables:

| Variable | Purpose |
|---|---|
| `CODESMITH_API_KEY` | API key (legacy alias `DEEPSEEK_API_KEY`) |
| `CODESMITH_BASE_URL` | API base URL |
| `CODESMITH_HTTP_HEADERS` | Optional custom model request headers, e.g. `X-Model-Provider-Id=your-model-provider` |
| `CODESMITH_MODEL` | Default model |
| `CODESMITH_PROVIDER` | `deepseek` (default), `nvidia-nim`, `openai`, `atlascloud`, `wanjie-ark`, `volcengine`, `openrouter`, `xiaomi-mimo`, `novita`, `fireworks`, `siliconflow`, `moonshot`, `sglang`, `vllm`, `ollama` |
| `CODESMITH_PROFILE` | Config profile name |
| `CODESMITH_MEMORY` | Set to `on` to enable user memory |
| `NVIDIA_API_KEY` / `OPENAI_API_KEY` / `ATLASCLOUD_API_KEY` / `WANJIE_ARK_API_KEY` / `VOLCENGINE_API_KEY` / `OPENROUTER_API_KEY` / `XIAOMI_MIMO_API_KEY` / `XIAOMI_API_KEY` / `MIMO_API_KEY` / `NOVITA_API_KEY` / `FIREWORKS_API_KEY` / `SILICONFLOW_API_KEY` / `MOONSHOT_API_KEY` / `KIMI_API_KEY` / `SGLANG_API_KEY` / `VLLM_API_KEY` / `OLLAMA_API_KEY` | Provider auth |
| `OPENAI_BASE_URL` / `OPENAI_MODEL` | Generic OpenAI-compatible endpoint and model ID |
| `ATLASCLOUD_BASE_URL` / `ATLASCLOUD_MODEL` | AtlasCloud endpoint and model override |
| `WANJIE_ARK_BASE_URL` / `WANJIE_ARK_MODEL` | Wanjie Ark endpoint and model override |
| `OPENROUTER_BASE_URL` | OpenRouter endpoint override |
| `XIAOMI_MIMO_BASE_URL` / `MIMO_BASE_URL` / `XIAOMI_MIMO_MODEL` / `MIMO_MODEL` | Xiaomi MiMo endpoint and model override |
| `NOVITA_BASE_URL` | Novita endpoint override |
| `FIREWORKS_BASE_URL` | Fireworks endpoint override |
| `SILICONFLOW_BASE_URL` / `SILICONFLOW_MODEL` | SiliconFlow endpoint and model override |
| `SGLANG_BASE_URL` | Self-hosted SGLang endpoint |
| `SGLANG_MODEL` | Self-hosted SGLang model ID |
| `VLLM_BASE_URL` | Self-hosted vLLM endpoint |
| `VLLM_MODEL` | Self-hosted vLLM model ID |
| `OLLAMA_BASE_URL` | Self-hosted Ollama endpoint |
| `OLLAMA_MODEL` | Self-hosted Ollama model tag |
| `NO_ANIMATIONS=1` | Force accessibility mode at startup |
| `SSL_CERT_FILE` | Custom CA bundle for corporate proxies |

Set `locale` in `settings.toml`, use `/config locale zh-Hans`, or rely on `LC_ALL`/`LANG` to choose UI chrome and the fallback language sent to V4 models. The latest user message still wins for natural-language reasoning and replies, so Chinese user turns stay Chinese even on an English system locale. See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) and [docs/MCP.md](docs/MCP.md).


---

## Publishing Your Own Skill

codesmith discovers skills from workspace directories (`.agents/skills` → `skills` → `.opencode/skills` → `.claude/skills` → `.cursor/skills` → `.codesmith/skills`) and global directories (`~/.agents/skills` → `~/.claude/skills` → `~/.codesmith/skills` → `~/.codesmith/skills`). Each skill is a directory with a `SKILL.md` file:

```text
~/.agents/skills/my-skill/
└── SKILL.md
```

Frontmatter required:

```markdown
---
name: my-skill
description: Use this when CodeSmith should follow my custom workflow.
---

# My Skill
Instructions for the agent go here.
```

Commands: `/skills` (list), `/skill <name>` (activate), `/skill new` (scaffold), `/skill install github:<owner>/<repo>` (community), `/skill update` / `uninstall` / `trust`. Community installs from GitHub require no backend service. Installed skills appear in the model-visible session context; the agent can auto-select relevant skills via the `load_skill` tool when your task matches their descriptions.

This section is the short version — the full reference (frontmatter fields, conditional `paths` activation, trust model, troubleshooting) lives in [docs/SKILLS.md](docs/SKILLS.md).

First launch also installs bundled system skills for common workflows:
`skill-creator`, `delegate`, `v4-best-practices`, `plugin-creator`,
`skill-installer`, `mcp-builder`, `documents`, `presentations`,
`spreadsheets`, `pdf`, and `feishu`. These live under
`~/.codesmith/skills` (or legacy `~/.codesmith/skills`) and are versioned so new bundles are added on upgrade
without recreating skills the user deliberately deleted.

---

## Documentation

| Doc | Topic |
|---|---|
| [GUIDE.md](docs/GUIDE.md) | First-run user guide |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | Codebase internals |
| [DESIGN_INTERNALS.md](docs/DESIGN_INTERNALS.md) | Deep dive: framework core, provider seam, guardrails, extensions |
| [CONFIGURATION.md](docs/CONFIGURATION.md) | Full config reference |
| [PROVIDERS.md](docs/PROVIDERS.md) | Provider IDs, auth, model defaults, and capability metadata |
| [MODES.md](docs/MODES.md) | Plan / Agent / YOLO modes |
| [SKILLS.md](docs/SKILLS.md) | Skills: authoring, discovery, and community installs |
| [MCP.md](docs/MCP.md) | Model Context Protocol integration |
| [HOOKS.md](docs/HOOKS.md) | Lifecycle hooks: events, conditions, and I/O contracts |
| [RUNTIME_API.md](docs/RUNTIME_API.md) | HTTP/SSE API server and mobile control page |
| [INSTALL.md](docs/INSTALL.md) | Platform-specific install guide |
| [DOCKER.md](docs/DOCKER.md) | GHCR image, volumes, and Docker usage |
| [MEMORY.md](docs/MEMORY.md) | User memory feature guide |
| [SUBAGENTS.md](docs/SUBAGENTS.md) | Sub-agent role taxonomy and lifecycle |
| [KEYBINDINGS.md](docs/KEYBINDINGS.md) | Full shortcut catalog |
| [RELEASE_RUNBOOK.md](docs/RELEASE_RUNBOOK.md) | Release process |
| [LOCALIZATION.md](docs/LOCALIZATION.md) | UI locale matrix & switching |
| [OPERATIONS_RUNBOOK.md](docs/OPERATIONS_RUNBOOK.md) | Ops & recovery |

Full Changelog: [CHANGELOG.md](CHANGELOG.md).

---

## Thanks

Thank the [CodeWhale](https://github.com/Hmbown/CodeWhale)，as this project is based on it.


## Help & Support

- **Usage questions** — check the [documentation](#documentation) first; if you're still stuck, [open an issue](https://github.com/Hmbown/CodeSmith/issues/new/choose).
- **Bugs & feature requests** — use the [issue templates](https://github.com/Hmbown/CodeSmith/issues/new/choose).
- **Security vulnerabilities** — do **not** open a public issue; follow [SECURITY.md](SECURITY.md) instead.
- Full routing guide: [SUPPORT.md](SUPPORT.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Pull requests welcome — check the [open issues](https://github.com/Hmbown/CodeSmith/issues) for good first contributions.

Support: [Buy me a coffee](https://www.buymeacoffee.com/hmbown).

> [!Note]
> *Not affiliated with DeepSeek Inc.*

## License

[MIT](LICENSE)

## Star History

[![Star History Chart](https://api.star-history.com/chart?repos=Hmbown/CodeSmith&type=date&legend=top-left)](https://www.star-history.com/?repos=Hmbown%2FCodeSmith&type=date&logscale=&legend=top-left)
