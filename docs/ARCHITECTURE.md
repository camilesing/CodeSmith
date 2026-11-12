# codesmith Architecture

This document provides an overview of the codesmith architecture for developers and contributors.
For the deep design narrative (provider seam, framework-core agent executor, guardrail
absorption, extension system), see [DESIGN_INTERNALS.md](DESIGN_INTERNALS.md).
For the general-agent event-stream concept map (Source / Thread / Eval / Policy /
Ticket / Mandate / Subscription / Assignment / Executor / Channel, each pinned to
the code that owns it here), see [AGENT_CONCEPTS.md](AGENT_CONCEPTS.md).

Current boundary note (v0.12.0):
- **Framework / implementation layer split.** The workspace is divided into a
  framework group — `protocol`, `tools`, `agent`, `config`, `secrets`,
  `extensions`, `session` (the session data plane: transcript zones,
  journal, saved-session store), `sandbox` (the execution-policy plane:
  command safety, execpolicy, OS sandboxes, network policy), `context` (the
  session context plane: project docs, working set, workspace trust,
  persistent memory), `mcp-client` (the MCP client pool: stdio/HTTP/SSE/WS
  transports, tool discovery, OAuth), `prompts` (system-prompt assembly:
  the base constitution, mode deltas, personalities, the skills catalogue,
  prefix-cache discipline), `agent-runtime`
  (the kernel: the turn loop composed through
  the `TurnExecutorRegistry` behind the `AgentExecutor` seam, engine,
  compaction) — and an implementation
  group: everything else (`providers`,
  `tool-impls`, `cli`, `app-server`, `mcp`, `state`, `execpolicy`,
  `hooks`, `index`, `core`, `release`, test fixtures). Framework
  crates must not depend, in the build graph, on any workspace crate outside
  the group; implementation crates depend freely on the framework. The rule
  is enforced physically by `scripts/check-framework-deps.py` (cargo
  metadata scan; runs in CI) — dev-dependencies are exempt because they
  never enter a framework artifact. New capabilities follow the seam triple:
  definition in a framework crate, provider in an implementation crate,
  consumer anywhere. Standing example: `IndexServiceApi` + its value types
  are defined in `codesmith-tools::index_api`, implemented and re-exported
  by `codesmith-index`, consumed by the kernel's tool context
  (`agent-runtime/src/tools/spec.rs`) — the same shape as `LlmClient`
  (defined in `codesmith-agent`, implemented in `codesmith-providers`)
  and as the unified web service (`WebService` defined in
  `agent-runtime/src/web_service.rs`, `HttpWebService` implemented in
  `codesmith-tool-impls::web_service`, consumed by the `web_search` /
  `fetch_url` / `web.run` tools through `ToolContext`).
  The generated capability graph (`docs/CAPABILITY_GRAPH.md`, from
`scripts/capability-graph.py`, CI-checked for freshness) lists every
seam's triple — a new capability lands all three roles in the same
change, and every `ExtensionEventKind` declares its dispatch mode
(`dispatch_mode()`, pinned by a contract test). The generated event matrix
(`docs/EVENT_MATRIX.md`, from `scripts/event-matrix.py`, CI-checked) lists
each kind's dispatch mode, the files that construct it for dispatch, and its
in-tree subscribers; `scripts/verify-md-links.py` (CI) keeps relative links
in the top-level and `docs/` markdown resolvable, and the JSON Schemas under
`docs/schemas/` (protocol, config) are test-pinned to their types.
- **Session persistence seam.** Durable session records (`SavedSession`),
  the format-governance decode pipeline (schema gate, adjacent in-memory
  migrations, unknown-block tolerance, interrupted-turn repair), and the
  `SessionStore` seam live in `codesmith-session` (`session/src/store.rs`;
  round-9 split); the
  file provider (`FileSessionStore`, one JSON file per session +
  crash checkpoints) lives in the same module — provider-in-framework by
  the `FileKeyringStore` precedent. Consumers: the CLI REPL resume flow
  and the runtime API's session endpoints (dyn-dispatched). Known
  limitation: single writer per session; the app-server thread runtime
  keeps its own store (`codesmith-state`) as a query index and does not
  persist REPL sessions — extending resume there means consuming this
  seam, not building a third format (a `ThreadResumeParams.session_id`
  extension was considered and dropped: the CLI runtime API already
  serves resume-from-session, and core/app-server have no
  agent-runtime edge today).
  The durable record of a REPL / ACP transcript is the append-only
  **session journal** (`session/src/journal.rs`, seam
  `SessionJournal`, file provider `FileSessionJournal` →
  `<sessions>/<id>.journal.jsonl`): the engine sends every transcript
  append and every rebuild — including the history a compaction, `/purge`,
  or `/edit` drops — before each `TurnComplete` and after every op, so the
  transcript at any point is a fold of a journal prefix. The snapshot is
  that fold's load cache: `save_session` stamps the journal size it covers
  (`journal_bytes`), and a load whose journal has grown past it recovers
  the transcript from the journal. Every main-turn model request is
  journaled where it was sent (system prompt and tool catalog only when
  they change, messages only when they are not the transcript prefix) and
  failed rounds land as attempt entries, so
  `session_journal::reconstruct_requests` rebuilds each request from the
  journal alone — the session-log form of "model-visible means logged".
  Exec runs and interactive/ACP sub-agents keep no session journal;
  runtime-API threads use their own event stream instead (the desktop
  bridge), and their in-process children write the child-session record
  below. Utility calls sharing the client (compaction summaries) are not
  journaled.
  The **child-session journal** (`agent-runtime/src/child_journal.rs`,
  seam `ChildSessionJournal` + `ChildSessionJournalFactory`) is the
  sub-agent counterpart: `run_child_turn` records the child's
  user/assistant/tool/approval/usage/completion events there before the
  turn lease is released, never into the parent transcript (the parent
  still receives only the completion card). The runtime-thread provider
  (`cli/src/runtime_threads/subagent_journal.rs`) materializes each child
  as an `origin = "subagent"` `ThreadRecord` with its own turn, items and
  append-only JSONL stream, so the desktop bridge opens a child
  conversation through the same `session/follow` path as a top-level
  thread. Replacement semantics: one run is one turn; a restart marks a
  running child `interrupted` and keeps its transcript, and no resume path
  exists. The record is UI/replay only — child content never enters a
  parent model request.
- **Plugin tiers (declared).** Pluggability rides the single `Extension`
  contract (`crates/agent/src/extension.rs`) + `ExtensionRunner` — four
  tiers: user Rhai mods (`~/.codesmith/mods/`, consent-gated), the
  builtin Rhai tier (embedded script defaults,
  `crates/extensions/src/script/builtin.rs`), compiled-in Rust
  extensions (`inventory`), and installed Rust dylibs
  (`/extension install` + reload). New pluggable behavior lands on one
  of these tiers — logic/data in Rhai, async/IO in a Rust extension —
  rather than a fresh compile-time wiring site. The registration surface
  is scope-aware (`Scope::Global | Scope::AgentType(id)`, general-agent alignment):
  a scoped tool shadows its global namesake in that agent type's catalog
  view (`ExtensionRunner::bound_tools_scoped`), a deny-only
  `restrict_tools` list prunes the extension layer's global tools per
  view, the main-turn catalog (`register_extension_tools`) resolves the
  Global view, and sub-agent registries resolve their own view per
  origin tier (5b: script/compiled-in tools mount in children, dylib
  tools stay main-turn-only — a detached background child can outlive a
  reload's two-phase dylib drop). The sole `BASE_PROMPT` and sub-agent tool
  registries' built-in surface are core by contract, not plugins; the
  compaction *strategy* (trigger/plan decisions) is mod-pluggable
  (`register_compaction_strategy`, config `[compaction] strategy`,
  `agent-runtime/src/compaction/strategy.rs` — effects and immune zone
  stay host-owned); one
  turn loop drives every agent (general-agent kernel-composition model) — the
  engine resolves a config-selected factory (`turn_executor`, default
  `builtin`; `"rhai"` runs a mod's `register_loop` script over the host's
  step services, `engine/rhai_executor.rs`) from the process-shared
  `TurnExecutorRegistry` (`engine/executor_composition.rs`), with
  alternates as crate-compiled registry entries, and wraps every turn with
  the `AroundTurnHandler`
  chain (`register_around_turn`, the general agent's `next()`, Rust extensions; a child
  composes it at its agent-type scope, dylib wrappers excluded). Every
  in-process sub-agent turn rides the same factory through
  `engine/child_turn.rs`: the CLI spawn path hands over a `ChildTurnSpec`
  (plain data — the child's delegated tool surface, model route, input /
  cancel channels, progress sink), and `run_child_turn` builds a
  child-shaped `TurnDeps` (`TurnShape::Child`: the main turn's session
  services absent — no compaction, approval prompt, completion delivery
  or plan gate — and every extension dispatch at the child's agent-type
  scope, where dylib-origin handlers and around-tool wrappers are
  skipped). Children stream with the idle watchdog like the main turn;
  the delegation policy (allowlist, approval by role, terminal takeover,
  path questions through the parent's router) rides the child's tool
  surface (`DelegatedTool`), not the loop (per-surface map: MODS.md
  "Which tier when"). Structural ownership
  (general agent, `agent-runtime/src/turn_lease.rs`): every running turn holds a
  `TurnLease` (key: agent id + extension generation + executor id,
  runner-scoped) — the main turn at assembly, each in-process sub-agent
  turn in `run_child_turn` — so a reload drains the runtime's in-flight sub-agents
  (reason-tagged cancel, deepest first, bounded wait) before the
  two-phase dylib unload, an executor id with in-flight leases refuses
  replacement, and the engine op-loop defers `drop_pending` while a
  lease assembled under a retired generation declares dylib exposure.
- `crates/cli` (package `codesmith-cli`, binary `codesmith`) is the live end-user runtime: it hosts both the headless subcommands and the line-based interactive REPL frontend (`crates/cli/src/interactive/` — the ratatui TUI was removed in v0.10.0), plus the runtime API, task manager, and tool registry wiring. The agent execution engine itself (turn loop, compaction, sandbox helpers, prompts) now lives in `crates/agent-runtime`; `crates/cli/src/core/` is a thin re-export + construction bridge (`engine.rs` defines `EngineHost`/`build_engine`).
- Other workspace crates are being split out incrementally, but they are not yet the sole runtime source of truth.
- **Idle wake (round 10 TODO-69).** The engine op loop is the single serial turn runner, so "no turn running" is exactly "the loop is waiting on `rx_op`". With `EngineConfig::idle_wake` on, that wait also listens to a side-band `Notify` poked by `SubAgentCompletionSender::send`: a direct child's completion landing while the parent is idle synthesizes the next `Op::SendMessage` (parameters cloned from the last real turn, content an internal `idle_wake` runtime event — a real user-role message, journaled like any other), and the turn's existing pre-request drain (`HostAgentExecutor::drain_subagent_completions`) delivers the parked completion ahead of the first request. No inbox: the completion never leaves its channel. The goal round driver (TODO-71) rides the same seam from the other direction: after a cleanly-ended turn with an active goal, the op loop decides the next round itself and runs it at the next loop top unless a real op is waiting (`goal_round_after_turn`, `GoalState::next_round` — pauses on a spent `token_budget` or the `MAX_GOAL_ROUNDS` cap), so goals drive without user input under the same pumping contract. Real ops always win the race and a synthesized turn never queues ahead of a waiting op. `EngineHandle::turn_in_flight` (cleared before the final `TurnComplete`) lets a frontend that pumps per `TurnComplete` keep pumping when its own turn queued behind a synthesized one. **A host that enables the flag must read the event channel while idle** — `MessageDelta` backpressure is intentional and would stall an unattended turn at 256 events: the REPL runs `interactive::unattended::IdlePump` during the blocking readline (hold/release handshake with an ack, so the channel has one reader at a time; interactive requests are answered headless-style — denied/cancelled — because a consumed approval event cannot be re-shown). exec exits after one turn and embeds default off; ACP and the runtime-API threads flip it on (ACP's always-on pump reads while idle, and its prompt outcome is turn-scoped: a synthesized turn's `TurnStarted` carries `synthesized: true` and its `TurnComplete` never completes a queued client prompt — only the turn that started while the prompt slot was reserved does; TODO-74, TODO-69b).
- Startup trust-boundary details are tracked in `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md`; that audit is the current reference for pre-trust versus post-trust initialization follow-ups.
- The LSP subsystem (`crates/cli/src/lsp/`) is fully wired into the engine's post-tool-execution path
  (`crates/agent-runtime/src/engine/lsp_hooks.rs` + `engine/turn/postprocess.rs`), providing inline diagnostics after every edit_file/apply_patch/write_file.
- The orchestration surface is persistent sub-agent sessions (`agent_open` / `agent_eval` / `agent_close`) and persistent RLM sessions (`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close`), plus the `workflow` tool for model-writable orchestration (re-opened by founder decision 2026-10-26): a Python script runs on a one-shot RLM kernel and drives sub-agents synchronously through injected helpers (`run_agent` / `agents` / `log`); seam triple — definition `agent-runtime/src/workflow.rs` (`WorkflowEngine` + `SubAgentSpawner`), provider + consumer in `crates/cli/src/tools/workflow/`. Known limitations: no async settlement/wake into a running turn (the script awaits its children inside one tool call); a wall-clock bound (default 600s, max 1800s) that kills the kernel mid-run and cancels in-flight children; sub-agent caps, spawn-depth budget, approval ceiling, and the `SessionBeforeFork` fork-veto are inherited from the sub-agent run path it rides.
  No swarm-style peer-mesh tool remains.
- Sub-agents run in-process by default; the **external sub-agent seam** lets a child run out of process without changing the model-visible surface. Definition: `ExternalSubagentProvider` (+ request/outcome types) in `crates/agent-runtime/src/subagent.rs`; two providers, both in `crates/cli/src/tools/subagent/`: the headless self-provider (`headless.rs` — each non-team child becomes a `codesmith exec --json` subprocess, with the spawn-depth budget inherited through `CODESMITH_SUBAGENT_DEPTH` / `CODESMITH_MAX_SPAWN_DEPTH`) and the ACP provider (`acp.rs`, v0.11.0 — delegates each child run to any Agent-Client-Protocol stdio agent configured under `[subagents.acp]`, speaking the same JSON-RPC baseline as the repo's own `--acp` server adapter; one terminal outcome with no mid-run streaming, no spawn-depth inheritance for a foreign agent, `session/request_permission` auto-answered from the parent's `auto_approve` posture — the full list is on the module); consumer: the CLI spawn path (`run_subagent_task`), selected by `[subagents] external_provider = "headless" | "acp"` (`provider_from_config` in `headless.rs`; an unknown name, or `"acp"` without a `[subagents.acp] command`, fails at config load). Forked contexts, `agent_send_input`, and team/coordinator children stay in-process by design (documented on the seam). Codex / Claude Code providers remain unbuilt slots of this seam.

## High-Level Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         User Interface                          │
│  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐  │
│  │ REPL (line mode)│  │  One-shot Mode  │  │  Config/CLI    │  │
│  └────────┬────────┘  └────────┬────────┘  └────────┬───────┘  │
└───────────┼─────────────────────┼────────────────────┼──────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Core Engine                              │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │     Agent Loop (crates/agent-runtime engine)            │   │
│  │  ┌─────────┐  ┌─────────────┐  ┌──────────────────────┐ │   │
│  │  │ Session │  │ Turn Mgmt   │  │ Tool Orchestration   │ │   │
│  │  └─────────┘  └─────────────┘  └──────────────────────┘ │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Tool & Extension Layer                      │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌────────────────┐   │
│  │  Tools   │  │  Skills  │  │  Hooks  │  │  MCP Servers   │   │
│  │ (shell,  │  │ (plugins)│  │ (pre/   │  │  (external)    │   │
│  │  file)   │  │          │  │  post)  │  │                │   │
│  └──────────┘  └──────────┘  └─────────┘  └────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                  Runtime API + Task Management                  │
│  ┌─────────────────────────────┐  ┌──────────────────────────┐  │
│  │ HTTP/SSE Runtime API        │  │ Persistent Task Manager  │  │
│  │ (runtime_api.rs)            │  │ (task_manager.rs)        │  │
│  └─────────────────────────────┘  └──────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
            │                     │
            ▼                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                        LLM Layer                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │        LLM Client Abstraction (codesmith-agent llm_client)│  │
│  │  ┌─────────────────┐  ┌─────────────────────────────┐    │  │
│  │  │ Provider registry│  │  Provider impls            │    │  │
│  │  │ (codesmith-agent)│  │  (codesmith-providers)     │    │  │
│  │  └─────────────────┘  └─────────────────────────────┘    │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Module Organization

### Entry Point

- **`main.rs`** - CLI argument parsing (clap), configuration loading, entry point routing

### Core Components

- **`crates/agent-runtime/src/engine/`** - The agent execution engine (moved out of the end-user frontend)
  - `mod.rs` - `Engine` struct + event loop (`Engine::run`, `handle_send_message`), system-prompt refresh, image-block assembly
  - `host_executor.rs` - `HostAgentExecutor`, the production LLM↔tool turn loop with the absorbed guardrails (loop-guard, LSP flush, transparent-retry, steer, approval, compaction, capacity, early-tool-start, subagent, cycle)
  - `executor_composition.rs` - turn-executor composition (the kernel composition point): per-turn `TurnDeps` snapshot, config-selected `TurnExecutorFactory` from the process-shared `TurnExecutorRegistry` (default `builtin` = the `host_executor.rs` assembly), and the dyn-safe `TurnHarvest` post-run take_* surface
  - `child_turn.rs` - sub-agent turns on the same factory: `ChildTurnSpec` (the host-built plain-data hand-over), `run_child_turn` (factory resolution, the child's `TurnLease`, child-shaped `TurnDeps`, progress / mailbox sink)
  - `turn/` - per-step phase modules split out of `host_executor.rs`: `stream.rs` (stream reduction + transparent retry), `batches.rs` (tool-batch planning/execution), `approval.rs` (per-tool approval gate), `seams.rs` (cancel checkpoints + steer drain), `postprocess.rs` (sub-agent reap gate, thinking-only status, LSP collect/flush)
  - `capacity_flow.rs` - Capacity guardrail checkpoints and interventions
  - `loop_guard.rs` / `dispatch.rs` / `tool_catalog.rs` / `tool_execution.rs` / `lsp_hooks.rs` - loop-guard state, tool-input parsing + batch planning, deferred tool catalog policy, MCP dispatch fanout, post-edit LSP hook
- **`crates/cli/src/core/`** - Thin re-export + construction bridge onto the engine crate
  - `engine.rs` - `EngineHost` (concrete `ShellManager`/`LspManager`/`SubAgentManager` host services), `EngineHandle` (UI-side mailbox), `build_engine` assembly
  - `engine/{handle,runtime_traits,tool_setup,tests}.rs` - handle plumbing, host traits, tool registry setup, tests
  - `session.rs` / `turn.rs` / `events.rs` / `ops.rs` / `tool_catalog.rs` / `capacity.rs` / `coherence.rs` - re-export shims for the agent-runtime types

### Configuration

- **`config.rs`** - Configuration loading, profiles, environment variables
- **`settings.rs`** - Runtime settings management

### Workspace Crates

- **`crates/agent`** - Framework AI core: `LlmClient` trait + retry, provider registry (ModelRegistry) for resolving model IDs to provider endpoints, wire models.
- **`crates/agent-runtime`** - Unified agent execution core: engine, turn loop, compaction, sandbox helpers, prompts, sub-agents.
- **`crates/app-server`** - HTTP/SSE + JSON-RPC app server transport for headless agent workflows.
- **`crates/config`** - Config loading, profiles, environment variable precedence, CLI runtime overrides.
- **`crates/core`** - Core runtime boundaries.
- **`crates/execpolicy`** - Approval/sandbox policy engine for tool execution decisions.
- **`crates/extensions`** + **`crates/extensions-fixture-dylib`** - Extension runtime (discovery, loading, event dispatch) and its test fixture dylib. Also hosts the Rhai script-mod layer (`src/script/` — same `Extension` contract, script载体; see [MODS.md](MODS.md)).
- **`crates/hooks`** - Lifecycle hooks (stdout, jsonl, webhook) for pre/post tool events.
- **`crates/index`** - Persistent per-workspace code index (see the Code Index section below).
- **`crates/mcp`** - MCP client + stdio server for Model Context Protocol tool servers.
- **`crates/protocol`** - Request/response framing and protocol types.
- **`crates/providers`** - Pluggable LLM client implementations (openai-compat, anthropic, mock; rig adapter) behind Cargo features.
- **`crates/release`** - Release discovery / version comparison.
- **`crates/secrets`** - OS keyring integration for API key storage.
- **`crates/prompts`** - System-prompt assembly (the general agent's `core/system-prompt` counterpart, split out of `agent-runtime` in round 9): `BASE_PROMPT` and all prompt assets (modes, personalities, approvals, styles), prompt-section registration with stability/cache annotations, the `/inspect` prompt render, the `PrefixStabilityManager` drift check, the default active-tool taxonomy, and skill discovery + the model-facing catalogue + built-in system skills. Re-exported as `agent_runtime::{prompts, prompt_runtime, prompt_sources, prompt_inspect, prefix_cache, skills}`. The tool-output spillover (`truncate`) moved to `codesmith_session::spillover`; `AppMode`/`ApprovalMode` to `codesmith_agent::mode`.
- **`crates/mcp-client`** - MCP client pool (the general agent's `mcp/*` counterpart, split out of `agent-runtime` in round 9): the connection pool over stdio / Streamable-HTTP / SSE / WebSocket transports, tool discovery, per-call timeouts, reconnect semantics, and the OAuth device flow for HTTP servers. Re-exported as `agent_runtime::{mcp, mcp_oauth}`. (`crates/mcp` — `codesmith-mcp` — is the server side and is unchanged.)
- **`crates/context`** - Session context plane (the general agent's `context/*` counterpart, split out of `agent-runtime` in round 9): AGENTS/CLAUDE.md project-doc discovery, the working set rendered into `<turn_meta>`, workspace trust and onboarding state, user memory (`MEMORY.md`), knowledge-on-demand, and per-agent memory directories. Re-exported under the historical `agent_runtime::{project_context, claudemd, working_set, workspace_trust, memory, knowledge, agent_memory, onboarding, settings_file, …}` paths.
- **`crates/session`** - Session data plane (the general agent's `core/session` counterpart, split out of `agent-runtime` in round 9): the `AppendLog` / three-zone request contract, the append-only session journal, the saved-session store and its format governance, the artifact registry and fact ledger. Consumers import it directly (the historical `agent_runtime::{session_store, session_journal, prompt_zones, artifacts}` and CLI `session_manager` paths were removed with the split).
- **`crates/state`** - SQLite thread/session persistence layer.
- **`crates/tool-impls`** - Concrete model-visible tool implementations.
- **`crates/tools`** - Shared tool invocation primitives, including tool result/error/capability types used by the CLI runtime.

### LLM Integration

- **`crates/agent/src/llm_client/`** - Abstract `LlmClient` trait with retry logic (`LlmClientHandle`, `with_retry`)
- **`crates/agent/src/models.rs`** - Data structures for API requests/responses (including `ContentBlock` / `ImageSource` wire types)
- **`crates/providers/`** - Concrete provider clients (openai-compat, anthropic, mock) plus the `rig_adapter` request shaper; the CLI resolves the active client through the provider registry (`default_registry`)

#### DeepSeek API Endpoints

DeepSeek exposes OpenAI-compatible endpoints. The CLI uses:
- `https://api.deepseek.com/beta/chat/completions` - default v0.8.16 DeepSeek model turns
- `https://api.deepseek.com/beta/models` - default v0.8.16 live model discovery and health checks

`https://api.deepseek.com/v1` is accepted for OpenAI SDK compatibility, and
can still be configured explicitly to opt out of beta-only features such as
strict tool mode, chat prefix completion, and FIM completion. The public
DeepSeek docs do not document a Responses API path for this workflow; the engine
drives turns through Chat Completions.

### Tool System

Tool implementations are split between the CLI crate (host-coupled tools) and
`crates/tool-impls` (model-visible tools; e.g. `grep_files`/`file_search`
live there).

- **`crates/cli/src/tools/`** - Host-coupled built-in tool implementations and the registry
  - `mod.rs` / `registry.rs` - Tool registry, assembly (`with_subagent_tools`, …), and common types
  - `shell.rs` - Shell command execution (plus `shell_output.rs` alongside it; `command_safety.rs` in the crate root)
  - `file.rs` - File read/write operations
  - `tasks.rs` / `task_v2.rs` - Model-visible durable task, gate, background shell, and PR-attempt tools
  - `github.rs` - Read-only GitHub context and guarded comment/closure tools backed by `gh`
  - `automation.rs` - Model-visible scheduling tools over `AutomationManager`
  - `subagent/` - Persistent sub-agent sessions (`agent_open` / `agent_eval` / `agent_close`) and persistent RLM sessions (`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close` — sandboxed Python REPLs with semantic helper calls and `var_handle` output support; runtime in `crates/cli/src/rlm/`)
  - `skill.rs` / `plugin.rs` / `web_search.rs` / `goal.rs` / `large_output_router.rs` and friends - remaining tool surfaces

### Extension Systems

- **`mcp.rs`** - Model Context Protocol client for external tool servers (lifecycle in `crates/mcp`)
- **`skills/`** - Plugin/skill loading and execution (discovery in `crates/agent-runtime/src/skills/`, state in `skill_state.rs`)
- **`hooks.rs`** - Pre/post execution hooks with conditions (dispatch in `crates/hooks`)

### User Interface

- **`interactive/`** - Line-based interactive REPL frontend (Claude Code / Codex style loop; replaced the removed ratatui TUI)
  - `mod.rs` - REPL loop: read a line, run an engine turn, stream output to the terminal
  - `state.rs` - REPL session state (with its known-limitations note)
  - `render.rs` - Line input handling, Ctrl+C/`Ctrl+D` semantics, and streaming output rendering
  - `status.rs` - Status line above the prompt (running-state dot, preset tier, context-window occupancy, mode, sub-agent count, model, reasoning-effort tier, version)
  - `completion.rs` - Tab completion and the inline hint (rustyline helper: registry names and user commands; closed argument vocabularies only)
  - `commands/` - Slash-command dispatch (`/resume`, `/config`, `/skills`, `/queue`, …); `commands/registry.rs` is the name/alias/help table that dispatch, `/help`, and Tab completion all read

### LSP Integration

- **`crates/cli/src/lsp/`** - Post-edit diagnostics injection
  - `mod.rs` - `LspManager` — lazy per-language transport pool + config
  - `client.rs` - `StdioLspTransport` — JSON-RPC over stdio with `didOpen`/`didChange`/`publishDiagnostics`
  - `diagnostics.rs` - Diagnostic types, severity, and HTML-block renderer
  - `registry.rs` - Language detection and default server map (rust-analyzer, pyright, gopls, clangd, typescript-language-server, jdtls, vue-language-server)
  - Wired into the engine via `crates/agent-runtime/src/engine/lsp_hooks.rs` (`run_post_edit_lsp_hook`), with the collect/flush pair in `engine/turn/postprocess.rs` — called after every successful edit

### Code Index

- **`crates/index`** (`codesmith-index`) — persistent per-workspace code index (see `docs/INDEX.md`)
  - The service seam `IndexServiceApi` / reserved `SemanticIndexApi` and the value vocabulary are defined in the framework layer (`codesmith-tools::index_api`) and re-exported here; `backend.rs` keeps the backend SPI: `IndexBackendFactory`/`IndexBackend` (provider-registry pattern)
  - `registry.rs` — `IndexBackendRegistry` mirroring `ProviderRegistry` (upsert, build error lists registered ids)
  - `tree_sitter.rs` — built-in symbol backend behind the `tree-sitter` cargo feature (rust/python/js/ts/go; container scoping, lexical references)
  - `walk.rs` — `ignore`-based workspace walk (`.gitignore`-aware) feeding the inventory + freshness diff
  - `store.rs` — per-workspace SQLite under `~/.codesmith/index/<ws-hash>/` (schema-version mismatch → rebuild)
  - `service.rs` — `IndexService` orchestration: lazy incremental refresh (mtime+size diff, budget + `stale_files` reporting) with background completion
- Wiring: the CLI builds the service once per workspace (`crates/cli/src/index.rs`, `[index]` config) into `RuntimeToolServices::index_service` → per-turn `ToolContext`; `symbol_search` / `find_references` live in `codesmith-tool-impls` and registration is gated session-constantly via `EngineConfig::index_enabled` (catalog stability, KV prefix cache)

### Security

- **`crates/sandbox/src/runtime/`** - Platform sandbox enforcement helpers (in the `codesmith-sandbox` crate since round 9; re-exported as `agent_runtime::sandbox`)
  - `seatbelt.rs` - macOS Seatbelt profile generation (enforced)
  - `landlock.rs` - Linux Landlock ruleset application (enforced: ruleset built in the parent by `prepare_landlock`, applied between fork and exec via `install_sandbox_preexec`; filesystem-only — see its known limitations)
  - `windows.rs` - Windows sandbox helper contract stub (no in-process Windows sandbox is advertised; sandbox-demanding policies fail closed with `SANDBOX_UNAVAILABLE`)
  - `bwrap.rs` / `process_hardening.rs` - optional bubblewrap passthrough; `PR_SET_DUMPABLE`/core-dump hardening
- **`crates/cli/src/sandbox/`** - Host-side sandbox policy preparation and denial reporting (`mod.rs`, `policy.rs`, `runtime.rs`, `backend.rs` for external OpenSandbox, `opensandbox.rs`)

### Utilities

- **`utils.rs`** - Common utilities
- **`logging.rs`** - Logging infrastructure
- **`compaction/`** - Context compaction for long conversations (engine-side flow in `crates/agent-runtime`)
- **`purge.rs`** - Agent-driven context purging (surgical message removal/rewriting)
- **`pricing.rs`** - Cost estimation
- **`prompts.rs`** - Prompt loading shims (assembled system prompts live in `crates/agent-runtime/src/prompts.rs` + `prompts/` assets: base constitution, mode deltas, personality overlays, approval policies)
- **`project_context.rs`** - Project documentation handling
- **`runtime_api.rs`** - HTTP/SSE runtime API (`codesmith serve --http`)
- **`runtime_threads.rs`** - Durable thread/turn/item store + replayable event timeline
- **`task_manager.rs`** - Durable queue, worker pool, task timelines and artifacts

## Data Flow

### Interactive Session

1. User input received in the REPL
2. Input processed by the engine (`crates/agent-runtime/src/engine/mod.rs`)
3. Message sent to LLM via the `LlmClient` trait (`crates/agent/src/llm_client/`, provider client from `crates/providers`)
4. Response streamed back through the stream reducer (`engine/turn/stream.rs`)
5. Tool calls extracted and executed via `tools/`
6. Hooks triggered before/after tool execution
7. Results aggregated and sent back to LLM
8. Final response rendered in the REPL

### Crash Recovery + Offline Queue

1. Before sending user input, the REPL writes a checkpoint snapshot to `~/.codesmith/sessions/checkpoints/latest.json`
2. Startup remains fresh by default; prior sessions are resumed explicitly via `--resume`/`--continue` (or the `/resume` command in the REPL)
3. While degraded/offline, new prompts are queued in-memory and mirrored to `~/.codesmith/sessions/checkpoints/offline_queue.json`
4. Queue edits (`/queue ...`) are persisted continuously so drafts and queued prompts survive restarts
5. Successful turn completion clears the active checkpoint and writes a durable session snapshot
6. Agent/Yolo turns also take pre/post-turn side-git workspace snapshots under `~/.codesmith/snapshots/<project_hash>/<worktree_hash>/.git`; `/restore N` and `revert_turn` restore file state without changing conversation history or the user's `.git`

### Tool Execution

1. LLM requests tool via `tool_use` content block
2. Tool registry looks up handler
3. Pre-execution hooks run
4. Approval requested if needed (non-yolo mode)
5. Tool executed (possibly sandboxed on macOS)
6. Post-execution hooks run
7. Result metadata is retained on runtime item records
8. **LSP post-edit hook** (v0.8.6): if the tool was `edit_file`/`apply_patch`/`write_file` and LSP is enabled, the engine runs `run_post_edit_lsp_hook()` to collect diagnostics
9. **Diagnostics flush** (v0.8.6): before the next API request, `flush_pending_lsp_diagnostics()` injects any collected errors as a synthetic user message
10. Result returned to agent loop

The steps above are per call, and one assistant message may carry several. The
engine groups them into **batches** (`engine::dispatch::plan_tool_execution_batches`):
a batch runs concurrently inside itself and every batch finishes before the
next starts, so the grouping is the ordering guarantee and declaration order
survives whatever the split does. A call joins a concurrent batch when it is
read-only, or declares `supports_parallel` (the sub-agent spawn family), or
writes files whose named target paths do not collide with the batch's. It gets
its own sequential batch when it is interactive or destructive, or when it
writes without naming a target (`exec_shell` — it can write anywhere). Two
calls naming one file never share a batch, and neither does an unnamed reader
with a writer: `grep_files` over a tree can be looking at the file a write is
rewriting. A batch holds at most `MAX_PARALLEL_BATCH` (10) calls.
**Approval does not force a call out of a concurrent batch**: every member that
needs approval is asked about in a sequential pre-dispatch pass, in declaration
order, before any future spawns — one denial stops that call and leaves its
siblings running. Known gap: an approved sandbox *escalation* cannot thread
into a concurrent batch, so such a call logs and runs with its fixed context.

### Background Tasks

1. Client enqueues task (`/task add ...` or `POST /v1/tasks`)
2. `task_manager.rs` persists task + queue entry under `~/.codesmith/tasks`
3. Worker picks queued task (bounded pool), transitions to `running`
4. Task creates/uses a runtime thread and starts a runtime turn
5. `runtime_threads.rs` persists thread/turn/item records + monotonic event sequence
6. Timeline/tool summaries/artifact references are persisted incrementally
7. Checklist state, verifier gates, PR attempts, and guarded GitHub events are applied from tool metadata to the active task
8. Final state (`completed|failed|canceled`) is durable and queryable via the REPL/runtime API

Model-visible durable task tools are a surface over this same manager. They do
not introduce a parallel work system: `task_create` enqueues normal tasks,
`checklist_*` updates task-local progress, `task_gate_run` and completed
`task_shell_wait` attach verification evidence, and automation runs enqueue
ordinary durable tasks.

### Runtime Thread/Turn Timeline

1. API/REPL creates or resumes a thread (`/v1/threads*`)
2. Turn starts on the thread (`/v1/threads/{id}/turns`)
3. Engine events are mapped to item lifecycle events (`item.started|item.delta|item.completed`)
4. Interrupt/steer operations apply to the active turn only
5. Compaction (auto/manual) is emitted as `context_compaction` item lifecycle
6. Purge (agent-driven) is emitted as `context_purge` item lifecycle
7. Clients replay history and resume with `/v1/threads/{id}/events?since_seq=<n>`

### Durable Schema Gates

- `codesmith-session` (`session/src/store.rs`), `runtime_threads.rs`, and `task_manager.rs` embed `schema_version` on persisted records.
- On load, newer schema versions are rejected with explicit errors instead of silently truncating/overwriting data.
- This allows safe forward migrations and prevents corruption when binaries and stored state are out of sync.

## Extension Points

Everything pluggable rides the `Extension` contract in one of four tiers
(user Rhai mods, the builtin Rhai tier, compiled-in Rust extensions,
installed Rust dylibs) — the per-surface map of which tier fits which
contribution, and what stays core by contract, lives in
[MODS.md](MODS.md) ("Which tier when"). The Rust-side authoring guide is
[EXTENSIONS.md](EXTENSIONS.md).

### Adding a New Tool

1. Create handler in `tools/`
2. Register in `tools/registry.rs`
3. Add tool specification (name, description, input schema)

### Adding an MCP Server

1. Configure in `~/.codesmith/mcp.json`
2. Server auto-discovered at startup
3. Tools exposed to LLM automatically

### Creating a Skill

1. Create skill directory with `SKILL.md`
2. Define skill prompt and optional scripts
3. Place in `~/.codesmith/skills/`

### Adding Hooks

Configure in `~/.codesmith/config.toml` (see `docs/HOOKS.md` for the full schema):

```toml
[hooks]

[[hooks.hooks]]
event = "tool_call_before"
command = "echo 'Running tool: $TOOL_NAME'"
```

## Key Design Decisions

1. **Streaming-first**: All LLM responses stream for responsiveness
2. **Tool safety**: Non-YOLO mode requires approval for destructive operations, including side-effectful MCP tools
3. **Extensibility**: Rhai mods, Rust extensions (compiled-in or dylib),
   MCP, skills, and hooks allow customization without code changes — the
   plugin-tier map in [MODS.md](MODS.md) says which surface goes where
4. **Cross-platform**: Core works on Linux/macOS/Windows. OS-level sandboxing
   is enforced on macOS (Seatbelt) and on Linux via bubblewrap when
   installed or Landlock (filesystem-only) otherwise; no seccomp filter
   exists, and no Windows sandbox is advertised — a sandbox-demanding
   policy that cannot be enforced is refused (`SANDBOX_UNAVAILABLE`) under
   the default fail-closed posture (see `docs/SANDBOX.md` for the platform
   matrix).
5. **Minimal dependencies**: Careful dependency selection for build speed
6. **Local-first runtime API**: HTTP/SSE endpoints are intended for trusted localhost access and are served by the `crates/cli` runtime today
7. **Three-zone prompt contract**: every request is divided into a
   `PinnedPrefix` (system prompt + tool catalog, fingerprinted per step by
   `codesmith_session::zones::PinnedPrefix::freeze` and drift-checked by
   `prefix_cache::PrefixStabilityManager`), an `AppendLog` (the `Session`
   transcript store — `push` is the only everyday mutation the type
   expresses; wholesale replacement must name itself via a `RebuildReason`
   and lands in the log's audit record, surfacing as
   `Event::TranscriptRebuilt` for `/cache zones`), and a `TurnScratch`
   (per-turn composition staging, committed to the log before the request
   loop and cleared at every turn boundary). The per-step
   `MessageRequest` is assembled through `ThreeZoneRequest` in
   `engine/host_executor.rs` — `messages` can only be the log slice plus
   the scratch tail, so hand-assembling a request from an arbitrary
   `Vec<Message>` elsewhere is the pattern this contract exists to
   prevent (byte-identical to the legacy direct construction; see the
   `three_zone_assembly_sends_verbatim_log_snapshot` regression test).

## Configuration Files

- `~/.codesmith/config.toml` - Main configuration (`~/.codesmith/config.toml` is still read as a legacy fallback)
- `/etc/codesmith/managed_config.toml` - Optional managed defaults layer (Unix)
- `/etc/codesmith/requirements.toml` - Optional allowed-policy constraints (Unix)
- `~/.codesmith/mcp.json` - MCP server configuration
- `~/.codesmith/skills/` - User skills directory
- `~/.codesmith/sessions/` - Session history
- `~/.codesmith/sessions/checkpoints/` - Crash checkpoint + offline queue persistence
- `~/.codesmith/snapshots/` - Side-git pre/post-turn workspace snapshots for `/restore` and `revert_turn`
- `~/.codesmith/tasks/` - Background task records, queue, timelines, artifacts
- `~/.codesmith/audit.log` - Append-only audit events for credential + approval/elevation actions
