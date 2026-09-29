# Presets and Approvals

codesmith has two related concepts:

- **TUI mode**: what kind of visible interaction you're in (Plan/Agent/YOLO).
- **Approval mode**: how aggressively the UI asks before executing tools.

On top of both sits the **preset layer**: one key (`preset = "middle"`, one
command `/preset <name>`) that bundles every dial below — tools, thinking,
memory, approvals, sub-agents, model, and a set of resource switches (code
index, LSP diagnostics, snapshots, background checks) — into a shareable
TOML file. Four progressive tiers ship built-in; the factory default is
`middle`.

Model selection is separate. `--model auto` and `/model auto` route each turn to
a concrete model and thinking level; they are not TUI modes and are not part of
the `Tab` cycle.

## Configuration Presets (`/preset <name>)

A *preset* is a bundle of dials in a single TOML file. Preset values are
**baselines, not overrides**: a value is only applied to a key you left
unset in `config.toml`, so `preset = "simple"` plus `[lsp] enabled = true`
keeps LSP on. When any explicit key differs from the selected tier, the
effective preset is reported as **`diy`** (a derived state — you cannot
select it) so the label never lies about what is running.

```bash
codesmith --preset simple    # Pi-style minimal: 9 tools, no index/LSP/memory
codesmith --preset all       # everything stable on
/preset list                 # see every preset visible to this workspace
/preset plan                 # switch mid-session (live dials; restart notes for the rest)
/preset export my-setup      # snapshot current dials to a shareable file
/preset off                  # drop the preset layer, keep current dials
codesmith-tui preset show    # tier matrix + your effective preset
```

Built-in tiers, progressive from lightest to heaviest:

| Tier | Thinking | Tools | Memory | Sub-agents | Resource switches |
|---|---|---|---|---|---|
| `simple` | medium | core file + shell only (`tools.include`) | goldfish (none) | off | index/LSP/snapshots/memory/update check/audit off |
| `middle` *(default)* | inherits | inherits | inherits | 10 | high-value low-cost set on (index, LSP, snapshots, memory, strong-brain auto router); experimental seams off |
| `all` | inherits | full surface | notebook (memory on, KOD off) | 20 | middle + LSP warnings; preview flags still off |
| `experiment` | inherits | full surface | elephant + Knowledge On Demand | 20 | everything on (vision, agent teams, coordinator, context manager, capacity controller, strict tool mode) |
| `plan` | inherits | read-only + plan tooling | notebook (explicit only) | inherits | inherits |

Governed switch matrix (`codesmith-tui preset show` prints this with your
effective values):

| key | simple | middle | all | experiment |
|---|---|---|---|---|
| `[index].enabled` | off | on | on | on |
| `[lsp].enabled` | off | on | on | on |
| `[lsp].include_warnings` | off | off | on | on |
| `[snapshots].enabled` | off | on | on | on |
| `[memory].enabled` | off | on | on | on |
| `[memory].kod_enabled` | off | off | off | on |
| `[context].project_pack` | off | on | on | on |
| `[context].enabled` | off | off | off | on |
| `[capacity].enabled` | off | off | off | on |
| `[auto].cost_saving` | off | off | off | off |
| `[update].check_for_updates` | off | on | on | on |
| `[network].audit` | off | on | on | on |
| `strict_tool_mode` | off | off | off | on |
| features: `subagents` / `web_search` / `mcp` | off | on | on | on |
| features: `vision_model` / `knowledge_on_demand` / `agent_teams` / `coordinator_mode` | off | off | off | on |

Never governed by any tier: safety keys (`yolo`, `approval_policy`,
`sandbox_mode`), privacy opt-ins (`telemetry`), user content (prompts,
`personality`, `instructions`), provider credentials, and tool overrides.
Safety-relevant keys stay yours.

Preset files live in two scanned directories, later layers overriding
built-ins by name (legacy `modes/` directories are still scanned, with a
one-time migration warning):

1. `~/.codesmith/presets/*.toml` — your presets, everywhere
2. `<workspace>/.codesmith/presets/*.toml` — project presets (commit these)

A preset file's full schema (every field optional):

```toml
name = "review"
description = "Read-only code review posture"
app_mode = "agent"              # agent | yolo | plan | coordinator
reasoning_effort = "high"       # off | low | medium | high | max | auto
approval_policy = "never"       # suggest | auto | never
sandbox_mode = "read-only"      # read-only | workspace-write | danger-full-access
memory_level = "notebook"       # goldfish | notebook | elephant
max_subagents = 2
model = "deepseek-v4-pro"
provider = "deepseek"           # startup-only; needs a restart to change

# Resource switch baselines (fill-if-unset, same vocabulary as config.toml)
index_enabled = false
lsp_enabled = false
lsp_include_warnings = false
snapshots_enabled = false
memory_enabled = false
memory_kod_enabled = false
context_enabled = false
context_project_pack = false
capacity_enabled = false
auto_cost_saving = false
update_check = false
network_audit = false
strict_tool_mode = false

[tools]
include = ["read_file", "grep_files", "list_dir"]  # allowlist when set
exclude = ["exec_shell"]                            # trimmed after include

[features]
subagents = false
web_search = false
```

**Memory dials** (`memory_level`) map onto the existing multi-layer memory
system ([docs/MEMORY.md](MEMORY.md)): `goldfish` disables cross-session
memory, `notebook` keeps only what you explicitly save (`# note`,
`/remember`), `elephant` turns on Knowledge On Demand with budget and decay.

**Hot vs. restart.** App mode, thinking, approvals, tool allow/denylists,
sub-agent cap, and model switch on the next turn. The resource switches
(index, LSP, snapshots, context/capacity seams), provider, feature flags,
and memory injection are read at engine startup — switching to a preset
that sets them prints what will apply after restart.

**Precedence for the active preset:** `--preset name` (CLI, or
`CODESMITH_PRESET`) > `preset = "name"` in config.toml (the legacy `mode`
key still works as a deprecated alias) > the last preset picked in the TUI
(persisted in settings.toml) > the factory default `middle`. Unsetting is
`/preset off`.

**Deprecated names.** The pre-rename spellings still work and map with a
warning: `minimal` → `simple`, `balanced` → `middle`, `maximal` → `all`;
`--mode` and `/mode` are aliases of `--preset` / `/preset`.

## TUI Modes

Press `Tab` to complete composer menus, queue a draft as a next-turn follow-up
while a turn is running, or cycle through the visible modes when the composer is
otherwise idle: **Plan → Agent → YOLO → Plan**.
Press `Shift+Tab` to cycle reasoning effort.
Run `/preset` to open the mode picker, or switch directly with
`/preset agent`, `/preset plan`, `/preset yolo`, `/preset 1`, `/preset 2`,
or `/preset 3`.

- **Plan**: design-first prompting. Read-only investigation tools stay available; shell and patch execution stay off. Use this when you want to think out loud and produce a plan to hand to a human (yourself later, or a reviewer).
- **Agent**: multi-step tool use. Shell execution (`exec_shell`, `task_shell_start`, `task_shell_wait`) requires `allow_shell = true` in config; approval prompts gate each call. File writes are allowed without a prompt.
- **YOLO**: enables shell + trust mode and auto-approves all tools. Use only in trusted repos.

All action-capable modes have access to persistent RLM sessions through `rlm_open`, `rlm_eval`, `rlm_configure`, and `rlm_close`. Inside an RLM Python REPL, `sub_query_batch` fans out 1-16 cheap parallel child calls pinned to `deepseek-v4-flash`. The model reaches for it when work is too large or repetitive for the parent transcript.

The fast light-tier path (`deepseek-v4-flash` on DeepSeek endpoints) with
thinking off is called Fin in the product language. Fin is a seam for quick
tool work, summaries, and cheap child calls; it does not change approval
behavior. Model routing itself is no longer Fin's job — the router runs on the
strongest tier (see Auto Model Routing below).

`/goal` sets a session objective with an optional token budget and keeps that
objective visible as Work context. It does not change the active TUI mode,
approval mode, or model route. This remains distinct from `--model auto`, which
only controls model and thinking selection.

## Auto Model Routing

Use `codesmith --model auto` or `/model auto` when you want codesmith to decide how much model and reasoning power a turn needs.

Auto mode controls two settings together:

- Model tier: `light` (fast/cheap) or `heavy` (strongest) — resolved to a concrete model ID for your provider
- Thinking: `off`, `high`, or `max`

Before the real turn is sent, the app makes a small routing call **on the provider's strongest tier** with thinking off. Routing is the highest-leverage decision of the turn — a misroute wastes the whole request — so the strongest available brain makes it, on a deliberately tiny input (a few lines of recent context). The router looks at the latest request, then selects a tier and thinking level for the real request. Short/simple turns stay on the light tier with thinking off; coding, debugging, release work, architecture, security review, or ambiguous multi-step tasks move up to the heavy tier and/or higher thinking.

Tier answers resolve per provider: on DeepSeek endpoints `light`/`heavy` map to `deepseek-v4-flash`/`deepseek-v4-pro`; on OpenRouter they map to that provider's flash/pro pair; on pass-through providers (OpenAI-compatible gateways, Ollama, custom endpoints) they fall back to your configured model unless you pin them explicitly:

```toml
[auto]
# heavy_model = "your-strongest-model"
# light_model = "your-cheapest-model"
# router_model = "override the classifier brain itself"
```

`auto` is local to codesmith. The upstream API never receives `model: "auto"`; it receives the concrete model and thinking setting chosen for that turn. The TUI shows the selected route, and cost tracking is charged against the model that actually ran. If the router call fails or returns an invalid answer, the app falls back to a free local heuristic (which also short-circuits obvious cases so trivial turns never pay for routing). Sub-agent assignment routing uses the same strong-brain classifier and tier vocabulary.

`[auto] cost_saving = true` flips the whole router to the original cheap-first design: the classifier runs on the configured `[utility_model]` (cheap brain) and ambiguous requests resolve to the light tier. The factory presets ship with cost-saving off (quality-first); users who had explicitly set `auto.cost_saving` keep their value.

Use a fixed model or fixed thinking level when you want repeatable benchmarking, a strict cost ceiling, or a specific provider/model mapping.

## Compatibility Notes

- Older settings files with `default_mode = "normal"` still load as `agent`; saving rewrites the normalized value.

## Escape Key Behavior

`Esc` is a cancel stack, not a mode switch.

- Close slash menus or transient UI first.
- Cancel the active request if a turn is running.
- Discard a queued draft if the composer is empty.
- Clear the current input if text is present.
- Otherwise it is a no-op.

## Approval Mode

You can override approval behavior at runtime:

```text
/config
# edit the approval_mode row to: suggest | auto | never
```

Legacy note: `/set approval_mode ...` was retired in favor of `/config`.

- `suggest` (default): uses the per-mode rules above.
- `auto`: auto-approves all tools (similar to YOLO approval behavior, but without forcing YOLO mode).
- `never`: blocks any tool that isn't considered safe/read-only.

## Small-Screen Status Behavior

When terminal height is constrained, the status area compacts first so header/chat/composer/footer remain visible:

- Loading and queued status rows are budgeted by available height.
- Queued previews collapse to compact summaries when full previews do not fit.
- `/queue` workflows remain available; compact status only affects rendering density.

## Workspace Boundary and Trust Mode

By default, file tools are restricted to the `--workspace` directory. Enable trust mode to allow file access outside the workspace:

```text
/trust
```

YOLO mode enables trust mode automatically.

## MCP Behavior

MCP tools are exposed as `mcp__<server>__<tool>` (double underscore; the old single-underscore `mcp_<server>_<tool>` spelling is still accepted as a legacy alias) and use the same approval flow as built-in tools. Read-only MCP helpers may auto-run in suggestive approval modes; MCP tools with possible side effects require approval.

See `MCP.md`.

## Related CLI Flags

Run `codesmith --help` for the canonical list. Common flags:

- `-p, --prompt <TEXT>`: one-shot prompt mode (prints and exits)
- `codesmith exec --auto --output-format stream-json <PROMPT>`: run the tool-backed non-interactive agent and emit one JSON object per line for harnesses and backend wrappers
- `codesmith exec --resume <ID|PREFIX> <PROMPT>` / `--session-id <ID|PREFIX>`: continue a saved session non-interactively
- `codesmith exec --continue <PROMPT>`: continue the most recent saved session for this workspace non-interactively
- `codesmith swebench run --instance-id <ID> --issue-file <PATH>`: run the tool-backed agent on one SWE-bench task and write/update a prediction JSONL row
- `codesmith fork <ID|PREFIX>` / `codesmith fork --last`: copy a saved session into a new sibling session; forked sessions retain additive parent-session metadata and show that lineage in session listings
- `--model <MODEL>`: when using the `codesmith` facade, forward a model override to the TUI
- `--workspace <DIR>`: workspace root for file tools
- `--yolo`: start in YOLO mode
- `-r, --resume <ID|PREFIX|latest>`: resume a saved session
- `-c, --continue`: resume the most recent session in this workspace
- `--max-subagents <N>`: clamp to `1..=20`
- `--mouse-capture` / `--no-mouse-capture`: opt in or out of internal mouse scrolling, transcript selection, right-click context actions, and transcript scrollbar dragging. Mouse capture is enabled by default on non-Windows terminals and on Windows Terminal/ConEmu/Cmder so drag selection copies only transcript text, removes visual wrap-column line breaks from paragraphs, and stays scoped to the transcript pane; hold Shift while dragging or use `--no-mouse-capture` for raw terminal selection. It defaults off on legacy Windows console (CMD without `WT_SESSION` / `ConEmuPID`) and inside JetBrains JediTerm — PyCharm/IDEA/CLion/etc. — where the terminal advertises mouse support but forwards SGR mouse events as raw text (#878, #898). Use `--mouse-capture` to opt in anywhere it's defaulted off. Raw terminal selection may cross the right sidebar and include visual wraps because the terminal, not the TUI, owns the selection.
- `--profile <NAME>`: select config profile
- `--preset <NAME>`: select a configuration preset (simple | middle | all | experiment | plan | custom); see [Configuration Presets](#configuration-presets-preset-name). The pre-rename `--mode` spelling still works.
- `--config <PATH>`: config file path
- `-v, --verbose`: verbose logging

## Branching and Rollback

CodeSmith has three related but intentionally separate recovery paths:

- `codesmith fork <ID>` creates a new saved session from an existing saved
  conversation and records the source session id. This is the safe way to
  explore a different answer path without overwriting the original session.
- Esc-Esc backtrack rewinds the live transcript to a previous user prompt and
  restores that prompt into the composer for editing.
- `/restore` and the `revert_turn` tool restore workspace files from side-git
  snapshots. They do not rewrite conversation history.

A Pi-style in-file tree browser is a larger UI/data-model project. v0.8.40
ships the bounded fork/backtrack primitives and explicit lineage metadata.
