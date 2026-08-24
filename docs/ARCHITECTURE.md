# codesmith Architecture

This document provides an overview of the codesmith architecture for developers and contributors.

Current boundary note (v0.8.6):
- `crates/tui` is still the live end-user runtime for the TUI, runtime API, task manager, and tool registry wiring. The agent execution engine itself (turn loop, compaction, sandbox helpers, prompts) now lives in `crates/agent-runtime`; `crates/tui/src/core/` is a thin re-export + construction bridge (`engine.rs` defines `EngineHost`/`build_engine`).
- Other workspace crates are being split out incrementally, but they are not yet the sole runtime source of truth.
- Startup trust-boundary details are tracked in `docs/STARTUP_TRUST_BOUNDARY_AUDIT.md`; that audit is the current reference for pre-trust versus post-trust initialization follow-ups.
- The LSP subsystem (`crates/tui/src/lsp/`) is fully wired into the engine's post-tool-execution path
  (`crates/agent-runtime/src/engine/lsp_hooks.rs` + `engine/turn/postprocess.rs`), providing inline diagnostics after every edit_file/apply_patch/write_file.
- The swarm agent system was removed in v0.8.5. The active v0.8.35 orchestration surface is persistent sub-agent sessions (`agent_open` / `agent_eval` / `agent_close`) and persistent RLM sessions (`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close`).
  No model-visible swarm tool remains in the active codebase.

## High-Level Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         User Interface                          │
│  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐  │
│  │   TUI (ratatui) │  │  One-shot Mode  │  │  Config/CLI    │  │
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

- **`crates/agent-runtime/src/engine/`** - The agent execution engine (moved out of the TUI)
  - `mod.rs` - `Engine` struct + event loop (`Engine::run`, `handle_send_message`), system-prompt refresh, image-block assembly
  - `host_executor.rs` - `HostAgentExecutor`, the production LLM↔tool turn loop with the absorbed guardrails (loop-guard, LSP flush, transparent-retry, steer, approval, compaction, capacity, early-tool-start, subagent, cycle)
  - `turn/` - per-step phase modules split out of `host_executor.rs`: `stream.rs` (stream reduction + transparent retry), `batches.rs` (tool-batch planning/execution), `approval.rs` (per-tool approval gate), `seams.rs` (cancel checkpoints + steer drain), `postprocess.rs` (sub-agent reap gate, thinking-only status, LSP collect/flush)
  - `capacity_flow.rs` - Capacity guardrail checkpoints and interventions
  - `loop_guard.rs` / `dispatch.rs` / `tool_catalog.rs` / `tool_execution.rs` / `lsp_hooks.rs` - loop-guard state, tool-input parsing + batch planning, deferred tool catalog policy, MCP dispatch fanout, post-edit LSP hook
- **`crates/tui/src/core/`** - Thin re-export + construction bridge onto the engine crate
  - `engine.rs` - `EngineHost` (concrete `ShellManager`/`LspManager`/`SubAgentManager` host services), `EngineHandle` (UI-side mailbox), `build_engine` assembly
  - `engine/{handle,runtime_traits,tool_setup,tests}.rs` - handle plumbing, host traits, tool registry setup, tests
  - `session.rs` / `turn.rs` / `events.rs` / `ops.rs` / `tool_parser.rs` / `capacity.rs` / `coherence.rs` - re-export shims for the agent-runtime types

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
- **`crates/extensions`** + **`crates/extensions-fixture-dylib`** - Extension runtime (discovery, loading, event dispatch) and its test fixture dylib.
- **`crates/hooks`** - Lifecycle hooks (stdout, jsonl, webhook) for pre/post tool events.
- **`crates/index`** - Persistent per-workspace code index (see the Code Index section below).
- **`crates/mcp`** - MCP client + stdio server for Model Context Protocol tool servers.
- **`crates/protocol`** - Request/response framing and protocol types.
- **`crates/providers`** - Pluggable LLM client implementations (openai-compat, anthropic, mock; rig adapter) behind Cargo features.
- **`crates/release`** - Release discovery / version comparison.
- **`crates/secrets`** - OS keyring integration for API key storage.
- **`crates/state`** - SQLite thread/session persistence layer.
- **`crates/tool-impls`** - Concrete model-visible tool implementations migrated from the TUI's `tools/` subtree.
- **`crates/tools`** - Shared tool invocation primitives, including tool result/error/capability types used by the TUI runtime.
- **`crates/tui-core`** - Event-driven TUI state machine scaffold.

### LLM Integration

- **`crates/agent/src/llm_client.rs`** - Abstract `LlmClient` trait with retry logic (`LlmClientHandle`, `with_retry`)
- **`crates/agent/src/models.rs`** - Data structures for API requests/responses (including `ContentBlock` / `ImageSource` wire types)
- **`crates/providers/`** - Concrete provider clients (openai-compat, anthropic, mock) plus the `rig_adapter` request shaper; the TUI resolves the active client through the provider registry (`default_registry`)

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

Tool implementations are split between the TUI (host-coupled tools) and
`crates/tool-impls` (model-visible tools migrated out of the TUI's
`tools/` subtree; e.g. `grep_files`/`file_search` live there).

- **`crates/tui/src/tools/`** - Host-coupled built-in tool implementations and the registry
  - `mod.rs` / `registry.rs` - Tool registry, assembly (`with_subagent_tools`, …), and common types
  - `shell.rs` - Shell command execution (plus `shell_output.rs`, `command_safety.rs` in the crate root)
  - `file.rs` - File read/write operations
  - `tasks.rs` / `task_v2.rs` - Model-visible durable task, gate, background shell, and PR-attempt tools
  - `github.rs` - Read-only GitHub context and guarded comment/closure tools backed by `gh`
  - `automation.rs` - Model-visible scheduling tools over `AutomationManager`
  - `subagent/` - Persistent sub-agent sessions (`agent_open` / `agent_eval` / `agent_close`, replaces the removed `agent_swarm` surface) and persistent RLM sessions (`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close` — sandboxed Python REPLs with semantic helper calls and `var_handle` output support; runtime in `crates/tui/src/rlm/`)
  - `skill.rs` / `plugin.rs` / `web_search.rs` / `goal.rs` / `js_execution.rs` / `large_output_router.rs` and friends - remaining tool surfaces

### Extension Systems

- **`mcp.rs`** - Model Context Protocol client for external tool servers (lifecycle in `crates/mcp`)
- **`skills/`** - Plugin/skill loading and execution (discovery in `crates/agent-runtime/src/skills/`, state in `skill_state.rs`)
- **`hooks.rs`** - Pre/post execution hooks with conditions (dispatch in `crates/hooks`)

### User Interface

- **`tui/`** - Terminal UI components (ratatui-based)
  - `app.rs` - Application state and message handling
  - `ui.rs` - Event handling, streaming state, and rendering logic
  - `approval.rs` - Tool approval dialog
  - `clipboard.rs` - Clipboard handling
  - `streaming/` - Streaming text collector (chunking, line buffer, commit tick)

### LSP Integration

- **`lsp/`** - Post-edit diagnostics injection (#136)
  - `mod.rs` - `LspManager` — lazy per-language transport pool + config
  - `client.rs` - `StdioLspTransport` — JSON-RPC over stdio with `didOpen`/`didChange`/`publishDiagnostics`
  - `diagnostics.rs` - Diagnostic types, severity, and HTML-block renderer
  - `registry.rs` - Language detection and default server map (rust-analyzer, pyright, gopls, clangd, typescript-language-server, jdtls, vue-language-server)
  - Wired into the engine via `crates/agent-runtime/src/engine/lsp_hooks.rs` (`run_post_edit_lsp_hook`), with the collect/flush pair in `engine/turn/postprocess.rs` — called after every successful edit

### Code Index

- **`crates/index`** (`codesmith-index`) — persistent per-workspace code index (see `docs/INDEX.md`)
  - `types.rs` / `backend.rs` — value types + the three seams: `IndexBackendFactory`/`IndexBackend` (provider-registry pattern), `IndexServiceApi` (LspManagerApi-style injection), reserved `SemanticIndexApi`
  - `registry.rs` — `IndexBackendRegistry` mirroring `ProviderRegistry` (upsert, build error lists registered ids)
  - `tree_sitter.rs` — built-in symbol backend behind the `tree-sitter` cargo feature (rust/python/js/ts/go; container scoping, lexical references)
  - `walk.rs` — `ignore`-based workspace walk (`.gitignore`-aware) feeding the inventory + freshness diff
  - `store.rs` — per-workspace SQLite under `~/.codesmith/index/<ws-hash>/` (schema-version mismatch → rebuild)
  - `service.rs` — `IndexService` orchestration: lazy incremental refresh (mtime+size diff, budget + `stale_files` reporting) with background completion
- Wiring: tui builds the service once per workspace (`tui/index.rs`, `[index]` config) into `RuntimeToolServices::index_service` → per-turn `ToolContext`; `symbol_search` / `find_references` live in `codesmith-tool-impls` and registration is gated session-constantly via `EngineConfig::index_enabled` (catalog stability, KV prefix cache)

### Security

- **`crates/agent-runtime/src/sandbox/`** - Platform sandbox enforcement helpers (re-exported through `crates/tui/src/sandbox/`)
  - `seatbelt.rs` - macOS Seatbelt profile generation (enforced)
  - `landlock.rs` - Linux Landlock ruleset application (enforced; `landlock_restrict_self` + `PR_SET_NO_NEW_PRIVS`)
  - `seccomp.rs` - Linux seccomp-BPF syscall filter (enforced)
  - `windows.rs` - Windows Job Object process containment (v1)
  - `bwrap.rs` / `process_hardening.rs` - optional bubblewrap passthrough; `PR_SET_DUMPABLE`/core-dump hardening
- **`crates/tui/src/sandbox/`** - Host-side sandbox policy preparation and denial reporting (`mod.rs`, `policy.rs`, `runtime.rs`, `backend.rs` for external OpenSandbox, `opensandbox.rs`)

### Utilities

- **`utils.rs`** - Common utilities
- **`logging.rs`** - Logging infrastructure
- **`compaction/`** - Context compaction for long conversations (engine-side flow in `crates/agent-runtime`)
- **`purge.rs`** - Agent-driven context purging (surgical message removal/rewriting)
- **`pricing.rs`** - Cost estimation
- **`prompts.rs`** - Prompt loading shims (assembled system prompts live in `crates/agent-runtime/src/prompts.rs` + `prompts/` assets: base constitution, mode deltas, personality overlays, approval policies)
- **`project_doc.rs`** / **`project_context.rs`** - Project documentation handling
- **`session_manager.rs`** - Session serialization
- **`runtime_api.rs`** - HTTP/SSE runtime API (`codesmith serve --http`)
- **`runtime_threads.rs`** - Durable thread/turn/item store + replayable event timeline
- **`task_manager.rs`** - Durable queue, worker pool, task timelines and artifacts

## Data Flow

### Interactive Session

1. User input received in TUI
2. Input processed by the engine (`crates/agent-runtime/src/engine/mod.rs`)
3. Message sent to LLM via the `LlmClient` trait (`crates/agent/src/llm_client.rs`, provider client from `crates/providers`)
4. Response streamed back through the stream reducer (`engine/turn/stream.rs`)
5. Tool calls extracted and executed via `tools/`
6. Hooks triggered before/after tool execution
7. Results aggregated and sent back to LLM
8. Final response rendered in TUI

### Crash Recovery + Offline Queue

1. Before sending user input, the TUI writes a checkpoint snapshot to `~/.codesmith/sessions/checkpoints/latest.json`
2. Startup remains fresh by default; prior sessions are resumed explicitly via `--resume`/`--continue` (or `Ctrl+R` in TUI)
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

### Background Tasks

1. Client enqueues task (`/task add ...` or `POST /v1/tasks`)
2. `task_manager.rs` persists task + queue entry under `~/.codesmith/tasks`
3. Worker picks queued task (bounded pool), transitions to `running`
4. Task creates/uses a runtime thread and starts a runtime turn
5. `runtime_threads.rs` persists thread/turn/item records + monotonic event sequence
6. Timeline/tool summaries/artifact references are persisted incrementally
7. Checklist state, verifier gates, PR attempts, and guarded GitHub events are applied from tool metadata to the active task
8. Final state (`completed|failed|canceled`) is durable and queryable via TUI/API

Model-visible durable task tools are a surface over this same manager. They do
not introduce a parallel work system: `task_create` enqueues normal tasks,
`checklist_*` updates task-local progress, `task_gate_run` and completed
`task_shell_wait` attach verification evidence, and automation runs enqueue
ordinary durable tasks.

### Runtime Thread/Turn Timeline

1. API/TUI creates or resumes a thread (`/v1/threads*`)
2. Turn starts on the thread (`/v1/threads/{id}/turns`)
3. Engine events are mapped to item lifecycle events (`item.started|item.delta|item.completed`)
4. Interrupt/steer operations apply to the active turn only
5. Compaction (auto/manual) is emitted as `context_compaction` item lifecycle
6. Purge (agent-driven) is emitted as `context_purge` item lifecycle
7. Clients replay history and resume with `/v1/threads/{id}/events?since_seq=<n>`

### Durable Schema Gates

- `session_manager.rs`, `runtime_threads.rs`, and `task_manager.rs` embed `schema_version` on persisted records.
- On load, newer schema versions are rejected with explicit errors instead of silently truncating/overwriting data.
- This allows safe forward migrations and prevents corruption when binaries and stored state are out of sync.

## Extension Points

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
3. **Extensibility**: MCP, skills, and hooks allow customization without code changes
4. **Cross-platform**: Core works on Linux/macOS/Windows. OS-level sandboxing
   is enforced per platform — macOS Seatbelt, Linux Landlock + seccomp (plus
   optional bubblewrap), Windows Job Object v1 (see `docs/SANDBOX.md` for the
   platform matrix).
5. **Minimal dependencies**: Careful dependency selection for build speed
6. **Local-first runtime API**: HTTP/SSE endpoints are intended for trusted localhost access and are served by the `crates/tui` runtime today

## Configuration Files

- `~/.codesmith/config.toml` - Main configuration (`~/.deepseek/config.toml` is still read as a legacy fallback)
- `/etc/deepseek/managed_config.toml` - Optional managed defaults layer (Unix)
- `/etc/deepseek/requirements.toml` - Optional allowed-policy constraints (Unix)
- `~/.codesmith/mcp.json` - MCP server configuration
- `~/.codesmith/skills/` - User skills directory
- `~/.codesmith/sessions/` - Session history
- `~/.codesmith/sessions/checkpoints/` - Crash checkpoint + offline queue persistence
- `~/.codesmith/snapshots/` - Side-git pre/post-turn workspace snapshots for `/restore` and `revert_turn`
- `~/.codesmith/tasks/` - Background task records, queue, timelines, artifacts
- `~/.codesmith/audit.log` - Append-only audit events for credential + approval/elevation actions
