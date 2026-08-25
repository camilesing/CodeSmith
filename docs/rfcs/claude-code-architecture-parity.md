# RFC: Claude Code Architecture Parity

**Issue:** TBD
**Status:** Decision record (2026-08 review) — Slice 1 mostly complete, Slices 2 & 5 implemented, Slices 3 & 4 accepted-as-is with revisit triggers, Slice 6 closed
**Date:** 2026-06-20 · review 2026-08

## Context

This RFC records the design background and follow-up work from comparing
CodeSmith with the architecture described in:

`/Users/camile/Work/TypeScript/claude-code-analysis/analysis/01-architecture-overview.md`

The reference document describes Claude Code as a local agent platform with six
major layers: CLI bootstrap, initialization/setup, command plane and TUI/REPL,
query/agent execution, tool/permission orchestration, and persistence/memory plus
extensions.

The goal of this RFC is not to rewrite CodeSmith in TypeScript, adopt React/Ink,
or copy the Claude Code file layout one-for-one. The goal is to use that six-layer
architecture as a reference model for hardening CodeSmith's own Rust platform
boundaries.

## Current CodeSmith posture

CodeSmith is already more than a chat CLI. The current Rust workspace has most of
the local-agent platform skeleton in place:

- A dispatcher CLI in `crates/cli/src/lib.rs`.
- A separate TUI/runtime binary in `crates/tui/src/main.rs`.
- A rich agent engine centered on `Engine`, `EngineConfig`, sessions, turn
  contexts, and `handle_codesmith_turn`.
- A broad tool surface managed through `ToolRegistry`, `ToolSpec`,
  `ToolContext`, and `ToolRegistryBuilder`.
- Trust, yolo, sandbox, exec policy, network policy, and approval-related
  concepts.
- Session/state, compaction, capacity management, snapshots, and KoD/memory
  features.
- MCP client/server surfaces, skills, plugins, subagents, teams, runtime HTTP,
  and ACP/server modes.

The main gaps are not that the capabilities are absent. The main gaps are that
some platform boundaries are not yet explicit enough, especially initialization
trust boundaries, headless engine API boundaries, tool scheduling policy, MCP
server parity, and remote/swarm scope.

## Reference architecture layers

The reference architecture can be summarized as:

1. **CLI bootstrap**
   - A thin CLI entrypoint handles fast-path commands such as version, prompt
     dump, remote-control, daemon/background/runner modes, and only loads the
     full main runtime when needed.
2. **Initialization and setup**
   - Runtime initialization is split into trust-before and trust-after phases.
   - Safe process/environment scaffolding happens before trust.
   - Project env, full telemetry, hooks, memory watchers, snapshots, and other
     project-sensitive work happen after trust.
3. **Command plane, TUI, and REPL**
   - Slash commands, menus, feature gates, and the interactive app state provide
     the control plane for user-visible runtime behavior.
4. **Query and agent execution core**
   - A query loop handles messages, system prompt construction, streaming,
     tool_use/tool_result, hooks, compaction, and cancellation.
   - A headless `QueryEngine`-style abstraction provides SDK/server-style
     execution outside the TUI.
5. **Tool and permission orchestration**
   - Tools have schemas, permissions, execution contexts, and concurrency
     metadata.
   - Tool calls are partitioned into safe concurrent batches and serial batches
     for side-effecting or workspace-mutating tools.
6. **Persistence, memory, and extensions**
   - Sessions, transcript storage, memory files, compaction, MCP, plugins,
     remote bridge, and swarm backends turn the CLI into a local agent platform.

## Gap table

| Layer | Reference expectation | Current CodeSmith implementation | Gap / risk | Priority | Related files |
| --- | --- | --- | --- | --- | --- |
| CLI bootstrap | Thin entrypoint with fast-path dispatch, then full runtime loading. | `crates/cli` dispatches direct commands and delegates agent/TUI commands to `codesmith-tui`. | The dispatcher shape exists, but fast paths such as prompt dump, remote-control, daemon/background/runner parity are not confirmed. The boundary is binary delegation rather than dynamic runtime import. | P2 | `crates/cli/src/lib.rs`, `crates/tui/src/main.rs` |
| Initialization / setup | Explicit pre-trust and post-trust phases for env, certs, telemetry, hooks, memory, and project setup. | `crates/tui/src/main.rs` performs process hardening, panic hook, signals, dotenv, config/logging, and command dispatch. `setup` initializes MCP/skills/tools/plugins and reports sandbox availability. | Trust exists as a runtime concept, but the initialization boundary is not documented as a strict trust-gated pipeline. Early env and telemetry behavior should be audited. | P0 | `crates/tui/src/main.rs`, `crates/tui/src/commands/config.rs` |
| Command plane / TUI / REPL | React/Ink REPL, slash/menu commands, broad app state bus. | Rust terminal UI with `AppMode`, interactive runtime, slash commands, onboarding, sessions, skills, trust, and config commands. | Conceptually present, but state/event boundaries need documentation against the reference model. No React/Ink parity is required. | P2 | `crates/tui/src/main.rs`, `crates/tui/src/tui/app.rs`, `crates/tui/src/commands/` |
| Query / agent core | `query.ts` loop plus `QueryEngine` for headless/SDK execution. | `Engine`, `EngineConfig`, `Session`, `TurnContext`, and `handle_codesmith_turn`; exec, stream-json, app-server/runtime API, and ACP surfaces exist. | The agent loop is strong, but there is no confirmed stable public `QueryEngine`-style headless API boundary reused by every frontend. | P2 | `crates/tui/src/core/engine.rs`, `crates/tui/src/core/engine/turn_loop.rs`, `crates/app-server/`, `crates/core/` |
| Tool / permission orchestration | Tool metadata, permission context, `runTools()`, and `partitionToolCalls()` for concurrent-safe versus serial execution. | `ToolRegistry`, `ToolSpec`, `ToolContext`, approval/trust/sandbox/network concepts, MCP tools, plugin overrides, subagent/team tools. | Tool surface is rich, but explicit concurrency-safety metadata and batch partitioning policy have not been confirmed. Workspace-mutating tools need a clear serial/parallel policy. | P1 | `crates/tui/src/tools/registry.rs`, `crates/tui/src/core/engine/turn_loop.rs` |
| Persistence / memory | `sessionStorage`, `SessionMemory`, memdir/`MEMORY.md`, memory prompt construction, compaction. | Session manager/state store, thread metadata, KoD memory, memory config, compaction, capacity recovery, snapshots. | Capabilities exist, but transcript, summary, KoD, memory files, and system-prompt injection should be mapped and documented. | P2 | `crates/tui/src/session_manager.rs`, `crates/state/src/lib.rs`, `crates/tui/src/compaction/`, `crates/tui/src/core/engine/turn_loop.rs` |
| MCP / plugins | MCP client and server, plugin surface, internal tools exposed through the platform where appropriate. | MCP pool/client integration, `serve --mcp`, separate `crates/mcp`, skills, plugins, tool overrides. | MCP server parity is unclear: the server may be lifecycle/management oriented rather than exposing the internal `ToolRegistry`. | P1 | `crates/mcp/src/lib.rs`, `crates/tui/src/mcp.rs`, `crates/tui/src/mcp_server.rs`, `crates/tui/src/tools/registry.rs` |
| Remote / bridge / swarm | Remote bridge, WebSocket orchestration, daemon/background runners, and swarm backend registry such as in-process/tmux/iTerm2. | HTTP/mobile runtime and ACP server exist; subagents, teams, coordinator mode, and send-message tools exist. | No confirmed Claude-style Remote/Bridge WebSocket orchestrator or tmux/iTerm2 swarm backend registry. This may be intentionally out of scope. | P1 | `crates/tui/src/main.rs`, `crates/app-server/`, `crates/tui/src/core/engine.rs`, `crates/tui/src/tools/registry.rs` |

## Design todos

> **2026-08 status annotations.** Each todo below records its current
> disposition after a code-vs-RFC review. Details in “Proposed slices”.

### P0: Define the trust-gated initialization boundary — mostly complete

Document and, if needed, refactor startup into explicit phases:

1. Process hardening, panic hook, crash dump, and signal cleanup.
2. Argument parsing and minimal safe config discovery.
3. Workspace trust decision.
4. Trusted project env, dotenv, certs, hooks, memory watchers, snapshots,
   telemetry attachment, and project config.
5. Runtime dispatch into interactive, exec, serve, or command modes.

The first-stage audit is recorded in
`docs/STARTUP_TRUST_BOUNDARY_AUDIT.md`. Runtime behavior changes should follow as
separate implementation slices after the pre-trust/post-trust classification is
reviewed.

### P1: Confirm or implement tool orchestration concurrency policy — implemented

Verified in place (2026-08): `ToolMetadata` carries `supports_parallel` /
`is_read_only` (`crates/agent-runtime/src/tool_dispatch.rs:27`),
`plan_tool_execution_batches` partitions parallel-safe vs serial batches
(`crates/agent-runtime/src/engine/dispatch.rs:290`), and
`execute_tool_with_lock` enforces a read/write lock so non-read-only tools
are serialized (`crates/agent-runtime/src/engine/tool_execution.rs:248`).
No further work.

The tool runtime should have an explicit policy for which tools can run in
parallel and which must run serially. At minimum, inspect and document behavior
for:

- File reads versus file writes.
- Shell commands.
- Git commands, especially mutating commands.
- Memory writes and task state mutation.
- MCP tools with unknown side effects.
- Plugin install/update and skills installation.
- Subagent/team messaging.

If the current scheduler does not already encode this, add metadata such as a
`ToolConcurrency` classification and partition tool calls before execution.

### P1: Decide MCP server parity — decision: accept the dual implementations

2026-08 decision: CodeSmith keeps both surfaces as-is —
`crates/tui/src/mcp_server.rs` exposes a curated ToolRegistry subset over
stdio with a coarse `require_approval` switch, and `crates/mcp` remains a
lifecycle/management server for the CLI. They are intentionally different
products; unifying their permission models is not justified by any current
consumer.

**Revisit trigger:** an external MCP consumer needs TUI-equivalent tools
*with* the full approval/trust/sandbox permission model over MCP. Until
then, do not expose shell/file-edit tools beyond the existing curated set.

Clarify whether CodeSmith's MCP server is intended to be:

1. A management/lifecycle server for configured MCP servers and resources.
2. An internal-tool server that exposes CodeSmith's own `ToolRegistry` tools to
   MCP clients.
3. Both, through separate modes or namespaces.

Claude Code parity requires a clear answer here. If internal tools should be
exposed, the MCP server should reuse the same registry, permission checks, and
execution contexts as the TUI/agent runtime.

### P1: Decide Remote/Bridge scope — decision: closed (out of scope)

2026-08 decision: HTTP/SSE runtime API (`serve --http`), ACP, and the
tmux/iTerm2 pane backends under `crates/tui/src/tools/team/backend/`
suffice. WebSocket bridges, daemons, and attach/detach session models are
out of scope. `docs/ARCHITECTURE.md` already records that no model-visible
swarm tool remains. Reopen only as an explicit product requirement with a
separate security review — not for parity's sake.

The reference architecture treats remote bridge and background runners as part of
the platform. CodeSmith should explicitly decide whether this is in scope.

Questions to settle:

- Should sessions support attach/detach from a daemon?
- Is mobile/HTTP runtime enough, or is a WebSocket bridge needed?
- Are background runner and remote-control modes product requirements?
- Should teams/subagents map to a swarm backend registry, or remain an internal
  runtime concept?

### P2: Extract or document a headless Agent Engine API — decision: accept two stacks short-term

2026-08 decision: the agent-runtime `Engine` (TUI + `exec`) and
`codesmith-core` `Runtime` + app-server (HTTP/stdio) remain two supported
stacks. `docs/ARCHITECTURE.md` documents the boundary honestly. A third
minimal adapter (ACP) stays minimal on purpose.

**Revisit trigger:** app-server or ACP needs engine-level features
(compaction, capacity management, teams, subagent fan-out) — at that point
the cost of a duplicated loop exceeds the cost of convergence, and a
dedicated unification RFC should be written. Until then, do not grow a
second full agent loop inside `codesmith-core`.

Define a stable boundary that can be reused by:

- TUI interactive mode.
- `exec` and stream-json modes.
- Runtime HTTP/app-server.
- ACP server.
- Integration tests and future SDK consumers.

The boundary should describe session creation, resume/fork, one turn execution,
streaming events, cancellation, compaction, tool approval, and final usage
reporting.

### P2: Document memory and session architecture — implemented

2026-08 status: `docs/MEMORY.md`, `docs/ARCHITECTURE.md`, and
`docs/STARTUP_TRUST_BOUNDARY_AUDIT.md` (all with `_cn` variants) cover the
memory/session/prompt-injection flow and the startup trust boundary.

Document how these pieces fit together:

- Transcript storage.
- Session metadata.
- Resume and fork behavior.
- Compaction summaries.
- KoD memory.
- `MEMORY.md` or equivalent memory files.
- Project/user/team memory layering.
- System-prompt injection.

## Proposed slices

### Slice 1: Initialization and trust boundary audit

**Status (2026-08): mostly complete.** Audit captured; early `.env` loading
removed, project-config overlay gated pre-trust, `SessionStart` hooks
deferred behind the trust prompt. The five-helper startup extraction was
deliberately deferred (see Plan 05 implementation notes in
`extra-findings-05-telemetry-scaffolding.md`) — the post-trust block is not
self-contained, so the extraction has no testable seam yet.

Status: first-stage audit captured in `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md`; the first implementation slice removes early implicit `.env` loading, gates project config overlay before trust, and defers `SessionStart` hooks while the workspace trust prompt is active.

Deliverables:

- A read-only audit of startup actions in `crates/cli` and `crates/tui`.
- A table classifying each action as pre-trust safe, post-trust only, constrained
  pre-trust, or uncertain.
- A follow-up implementation plan if actions need to move.

Stop rule: do not move initialization code until the safe/post-trust
classification is reviewed.

### Slice 2: Tool concurrency metadata and scheduler

**Status (2026-08): implemented** — see the P1 todo annotation above
(`engine/dispatch.rs` batch partitioning + RwLock serialization in
`engine/tool_execution.rs`).

Deliverables:

- Inspect current tool execution in the turn loop.
- Document current behavior.
- Add or confirm metadata for concurrency-safety.
- Add tests for ordering and serial execution of mutating tools if new behavior
  is introduced.

Stop rule: do not parallelize unknown-side-effect tools by default.

### Slice 3: MCP server parity decision

**Status (2026-08): decided — accept dual implementations.** See the P1
todo annotation above.

Deliverables:

- Document current `crates/mcp` and TUI MCP server responsibilities.
- Decide whether internal `ToolRegistry` tools should be exposed.
- If yes, design the registry adapter and permission model.

Stop rule: do not expose shell/file-edit tools over MCP without explicit trust
and permission behavior.

### Slice 4: Headless engine API cleanup

**Status (2026-08): decided — accept two stacks short-term.** See the P2
todo annotation above.

Deliverables:

- Identify the lowest-level reusable engine boundary.
- Make TUI, exec, app-server, and ACP responsibilities explicit.
- Decide whether a public SDK-style API is in scope.

Stop rule: avoid a second agent loop implementation.

### Slice 5: Memory/session architecture document

**Status (2026-08): complete.** See the P2 todo annotation above.

Deliverables:

- Diagram transcript, compaction, memory, and prompt injection flow.
- Map existing files/modules to each role.
- Identify duplicate or unclear memory paths.

Stop rule: document current behavior before changing persistence formats.

### Slice 6: Remote/bridge/swarm scope decision

**Status (2026-08): closed.** See the P1 Remote/Bridge annotation above.

Deliverables:

- Compare current HTTP/mobile/ACP, teams, and subagents with the reference remote
  and swarm model.
- Decide whether daemon, WebSocket bridge, attach/detach, tmux, or iTerm2
  backends are product goals.

Stop rule: do not add background daemons or remote bridge protocols without a
separate security review.

## Out of scope

This RFC does not require:

- Rewriting CodeSmith in TypeScript.
- Replacing the Rust TUI with React/Ink.
- Renaming Rust modules to match Claude Code file names.
- Implementing daemon, background runner, WebSocket bridge, tmux, or iTerm2
  swarm backends immediately.
- Exposing shell or file-edit tools over MCP without an explicit permission
  model.
- Treating Claude Code's implementation as the only acceptable architecture.

## Open questions — answered (2026-08 review)

- *Tracking issue / rename?* — Not pursued; this file is the record.
- *MCP server exposes internal tools, management, or both?* — Both, as two
  separate implementations; accepted (see Slice 3).
- *Remote/Bridge a product requirement?* — No; HTTP/mobile runtime and ACP
  are sufficient (see Slice 6).
- *Headless engine a public SDK API?* — Remains internal; two stacks
  accepted short-term with a defined revisit trigger (see Slice 4).
- *What runs before workspace trust?* — Classified and enforced in
  `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md` + the implemented Slice 1 items;
  residual startup-helper extraction deferred by design.
- *Team/subagent mapped to named swarm backends?* — No; the model-visible
  swarm tooling was removed and pane backends stay a `team/` implementation
  detail.

## References

- Reference analysis:
  - `/Users/camile/Work/TypeScript/claude-code-analysis/analysis/01-architecture-overview.md`
- Workspace layout:
  - `Cargo.toml`
- CLI bootstrap and delegation:
  - `crates/cli/src/lib.rs`
- TUI/runtime entrypoint:
  - `crates/tui/src/main.rs`
- Agent engine and turn loop:
  - `crates/tui/src/core/engine.rs`
  - `crates/tui/src/core/engine/turn_loop.rs`
- Tool registry and tool surface:
  - `crates/tui/src/tools/registry.rs`
- MCP surfaces:
  - `crates/mcp/src/lib.rs`
  - `crates/tui/src/mcp.rs`
  - `crates/tui/src/mcp_server.rs`
- TUI app state and modes:
  - `crates/tui/src/tui/app.rs`
- Session and state:
  - `crates/tui/src/session_manager.rs`
  - `crates/state/src/lib.rs`
- Existing architecture and related docs:
  - `docs/ARCHITECTURE.md`
  - `docs/TOOL_SURFACE.md`
  - `docs/LEGACY_RUST_AUDIT_0_7_6.md`
  - `docs/rfcs/2190-mcp-modularization.md`
  - `docs/rfcs/2189-persistence-sqlite.md`
  - `docs/rfcs/1364-hooks-lifecycle.md`
