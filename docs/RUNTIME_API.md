# Runtime API & Integration Contract

codesmith exposes a local runtime API through `codesmith serve --http` and
machine-readable health via `codesmith doctor --json`. It also exposes
`codesmith serve --acp` for editor clients that speak the Agent Client Protocol
over stdio. This document is the stable integration contract for native macOS
workbench applications (and other local supervisors) that embed the CodeSmith
engine without screen-scraping terminal output.

## Architecture

```
macOS workbench (or any local supervisor)
        │
        ├─ codesmith doctor --json   → machine-readable health & capability
        ├─ codesmith serve --http    → HTTP/SSE runtime API
        ├─ codesmith serve --acp     → ACP stdio agent for editors such as Zed
        ├─ codesmith serve --mcp     → MCP stdio server
        └─ codesmith [args]          → interactive REPL session
```

The engine runs as a local-only process. All APIs bind to `localhost` by
default. No hosted relay, no provider-token custody, no secret leakage.

## ACP stdio adapter: `codesmith serve --acp`

`codesmith serve --acp` speaks JSON-RPC 2.0 over newline-delimited stdio for
ACP-compatible editor clients. The initial adapter implements the ACP baseline:

- `initialize`
- `session/new`
- `session/prompt`
- `session/cancel`

Prompt requests are routed through the configured LLM client and current
default model. Responses are emitted as `session/update` agent message chunks
followed by a `session/prompt` response with `stopReason: "end_turn"`.

The adapter is intentionally conservative: it does not yet expose shell tools,
file-write tools, checkpoint replay, or session loading through ACP. Use
`codesmith serve --http` for the full local runtime API and `codesmith serve --mcp`
when another client needs CodeSmith's tools as MCP tools.

In Zed, add a custom agent server:

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

Community-maintained adapter: [acp-codesmith-adapter](https://github.com/rockeverm3m/acp-codesmith-adapter)
bridges `codesmith exec --auto` to `cc-connect` for users who need tool-backed
ACP workflows outside the built-in Zed slice.

## Capability endpoint: `codesmith doctor --json`

Returns a JSON object describing the current installation's readiness state.
Suitable for health-check polling from a macOS workbench.

```bash
codesmith doctor --json
```

### Response schema (key fields)

| Field | Type | Description |
|---|---|---|
| `version` | string | Installed version (e.g. `"0.8.9"`) |
| `config_path` | string | Resolved config file path |
| `config_present` | bool | Whether the config file exists |
| `workspace` | string | Default workspace directory |
| `api_key.source` | string | `env`, `config`, or `missing` |
| `base_url` | string | API base URL |
| `default_text_model` | string | Default model |
| `memory.enabled` | bool | Whether the `CODESMITH_MEMORY` env override is set to a truthy value. Reflects the env var only — a default install reports `false` while the memory feature itself defaults on |
| `memory.path` | string | Path to memory file |
| `memory.file_present` | bool | Whether memory file exists |
| `mcp.config_path` | string | MCP config file path |
| `mcp.present` | bool | Whether MCP config exists |
| `mcp.servers` | array | Per-server health: `{name, enabled, status, detail}` |
| `skills.selected` | string | Resolved skills directory |
| `skills.global.path` / `.present` / `.count` | — | CodeSmith global skills dir (default `~/.codesmith/skills`; follows `CODESMITH_HOME` when set) |
| `skills.agents.path` / `.present` / `.count` | — | Workspace `.agents/skills/` dir |
| `skills.agents_global.path` / `.present` / `.count` | — | agentskills.io global skills dir (`~/.agents/skills`) |
| `skills.local.path` / `.present` / `.count` | — | `skills/` dir |
| `skills.opencode.path` / `.present` / `.count` | — | `.opencode/skills/` dir |
| `skills.claude.path` / `.present` / `.count` | — | `.claude/skills/` dir |
| `tools.path` / `.present` / `.count` | — | Global tools directory |
| `plugins.path` / `.present` / `.count` | — | Global plugins directory |
| `sandbox.available` | bool | Whether sandbox is supported on this OS |
| `sandbox.kind` | string or null | Sandbox kind (e.g. `"macos_seatbelt"`) |
| `storage.spillover.path` / `.present` / `.count` | — | Tool output spillover dir |

### Example

```json
{
  "version": "0.8.9",
  "config_path": "/Users/you/.codesmith/config.toml",
  "config_present": true,
  "workspace": "/Users/you/projects/codesmith",
  "api_key": {
    "source": "env"
  },
  "base_url": "https://api.deepseek.com/beta",
  "default_text_model": "deepseek-v4-pro",
  "memory": {
    "enabled": false,
    "path": "/Users/you/.codesmith/memory.md",
    "file_present": true
  },
  "mcp": {
    "config_path": "/Users/you/.codesmith/mcp.json",
    "present": true,
    "servers": [
      {"name": "filesystem", "enabled": true, "status": "ok", "detail": "ready"}
    ]
  },
  "sandbox": {
    "available": true,
    "kind": "macos_seatbelt"
  }
}
```

## HTTP/SSE runtime API: `codesmith serve --http`

```bash
codesmith serve --http [--host 127.0.0.1] [--port 7878] [--workers 2] [--auth-token TOKEN]
codesmith serve --mobile [--host 0.0.0.0] [--port 7878] [--auth-token TOKEN]
```

Defaults: host `127.0.0.1`, port `7878`, 2 workers (clamped 1–8).

The server binds to `localhost` by default. Configuration is via CLI flags —
there is no `[app_server]` config section.

`/v1/*` routes require a bearer token unless `--insecure` is explicitly set.
Pass `--auth-token TOKEN` or set `CODESMITH_RUNTIME_TOKEN=TOKEN` before starting
the server. If neither is set, the process generates a one-time token and prints
it at startup. `/health` and `/v1/runtime/info` remain public for local
supervision and bootstrap. `/mobile` returns 404 when mobile mode is disabled;
when mobile mode is enabled and auth is enabled, `/mobile` returns 401 unless
the request supplies the runtime token.

Authenticated clients can provide the token as `Authorization: Bearer TOKEN`,
`X-CodeSmith-Runtime-Token: TOKEN`, or `?token=TOKEN` for EventSource-style
clients that cannot set custom headers.

### Mobile control page

`codesmith serve --mobile` starts the same HTTP/SSE runtime API and serves a
phone-friendly control page at `/mobile`. When the bind host is left at the
default, mobile mode binds to `0.0.0.0`, prints a warning, and prints local/LAN
URLs. Pass `--host 127.0.0.1` to keep the mobile page loopback-only. If a
runtime token is generated or supplied, the printed mobile URL includes it as a
query parameter; the page stores it locally and removes it from the address bar.
The static HTML page contains no secrets, but it is still token-gated when auth
is enabled so unauthenticated LAN clients cannot fingerprint the mobile surface.

The mobile page can list/create threads, send prompts, follow live SSE events,
steer or interrupt an active turn, and resolve normal tool approvals through
`POST /v1/approvals/{approval_id}`. It is still a local/LAN convenience surface:
do not expose it directly to the public internet without TLS and a trusted
fronting layer.

### Endpoints

**Health**
- `GET /health`

**Sessions** (legacy session manager)
- `GET /v1/sessions?limit=50&search=<substring>`
- `GET /v1/sessions/{id}`
- `DELETE /v1/sessions/{id}`
- `POST /v1/sessions/{id}/resume-thread`

**Threads** (durable runtime data model)
- `GET /v1/threads?limit=50&include_archived=false&archived_only=false`
- `GET /v1/threads/summary?limit=50&search=<optional>&include_archived=false&archived_only=false`
- `POST /v1/threads`
- `GET /v1/threads/{id}`
- `PATCH /v1/threads/{id}` (see body shape below)
- `POST /v1/threads/{id}/resume`
- `POST /v1/threads/{id}/fork`

Thread forks are sibling runtime threads, not an in-place tree projection.
`thread.forked` events include `source_thread_id`; internal backtrack-aware
forks may also include `backtrack_depth_from_tail` and `dropped_turn_id`.
Thread list and summary responses remain flat in v0.8.40, so clients that need
a graph should reconstruct it from events instead of assuming list order is a
complete tree.

**Goal** (the live thread goal the desktop GoalBar reads)
- `GET /v1/threads/{id}/goal` — `{ "loaded": bool, "goal": {…} | null }`.
  `loaded:false` means the thread's engine is not resident; a client falls
  back to the durable `create_goal`/`update_goal` tool records. `loaded:true`
  is authoritative, including "no goal" after a clear. A read never loads an
  engine.
- `POST /v1/threads/{id}/goal` — body
  `{ "action": "edit" | "pause" | "resume" | "clear", "objective"?: "…" }`
  (`objective` required for `edit`). The mutation applies to the engine's
  shared goal state, so the next prompt/round reads it; `pause` requires an
  active goal, `resume` an existing one. The response carries the same
  `{ "loaded": true, "goal": … }` envelope. Pause/clear are process-local:
  an engine eviction re-seeds an active/completed goal from the journal, not
  a desktop pause or clear.

`archived_only=true` returns archived threads only (mutually overrides
`include_archived`). Default behavior: `include_archived=false`
and `archived_only=false` returns active threads.

`PATCH /v1/threads/{id}` body — every field is optional, missing means
"no change". At least one field must be present. `title` and `system_prompt`
accept an empty string to clear a previously-set value:

```json
{
  "archived": true,
  "allow_shell": false,
  "trust_mode": false,
  "auto_approve": false,
  "model": "deepseek-v4-pro",
  "mode": "agent",
  "title": "User-set thread title",
  "system_prompt": "You are a useful assistant."
}
```

**Turns** (within a thread)
- `POST /v1/threads/{id}/turns` — body `{ "prompt": "…", "attachments": [...] }`.
  `attachments` is optional (absent = the text-only request this route always
  took); each member carries one file inline:

  ```json
  {
    "data": "<canonical base64 of the file bytes>",
    "name": "shot.png",
    "media_type": "image/png",
    "width": 3,
    "height": 2
  }
  ```

  `name` (display leaf name — separator-carrying names are refused to a
  fallback `attachment.<ext>`) and `media_type` decide how the attachment
  reaches the model, through the same intake the REPL uses
  (`~/.codesmith/attachments/`, content-addressed): `png/jpg/jpeg/gif/webp`
  images travel on the wire as native image blocks (declared image bytes are
  verified against their magic first; `width`/`height`, when the caller
  knows them, are echoed on the user item's `metadata.attachments` for
  client rendering), inline text types are folded into the message text, and
  every other binary appends a durable path + size + media-type reference
  line the model can read with `read_file` on demand. Limits: 10MB per
  file, 8 attachments per message, audio/video refused — one refused
  attachment fails the whole request (nothing is half-sent). A prompt may be
  empty when attachments are present (an image-only turn keeps a
  `[image]` placeholder text). The request body limit is 16MB (base64
  overhead over the 10MB file cap).
- `POST /v1/threads/{id}/turns/{turn_id}/steer` — body
  `{ "prompt": "…", "attachments": [...], "rpc_id": "…" }` (`attachments` as
  on `/turns`, admitted through the same intake — images ride the steer as
  native image blocks, text/binary fold into the message; a prompt may be
  empty when attachments are present). `rpc_id` (optional, opaque) is
  recorded on the steer's user item as `metadata.rpc_id` so a client can
  match the settled message to its own submission.
- `POST /v1/threads/{id}/turns/{turn_id}/interrupt`
- `POST /v1/threads/{id}/compact` (manual compaction)

`POST /turns` answers **409** while the thread has an active turn. To send
without racing that check, use the queue.

**Queue** (prompts parked behind a running turn)
- `POST /v1/threads/{id}/queue` — body
  `{ "prompt": "…", "attachments": [...], "rpc_id": "…" }` (`attachments` as on
  `/turns`, admitted at enqueue time — all-or-nothing; `rpc_id` optional and
  opaque). If the thread is idle with nothing queued, the prompt starts right
  away: `201 { "status": "started", "turn": TurnRecord }`. Otherwise it is
  parked at the tail: `201 { "status": "queued", "item": QueuedItemRecord }`.
  Queued items drain FIFO, **one item per new turn**, as soon as the running
  turn ends with any outcome (an interrupt keeps the queue). A drained turn
  takes the thread's defaults (queued items carry no per-turn overrides), and
  its user item carries the item's `rpc_id` as `metadata.rpc_id`.
- `PATCH /v1/threads/{id}/queue/{item_id}` — body `{ "prompt": "…" }`;
  text-only replacement: the item's attachments are dropped, and blank text
  is refused (400).
- `DELETE /v1/threads/{id}/queue/{item_id}` — drop one item.
- `POST /v1/threads/{id}/queue/{item_id}/steer` — pull the item out of the
  queue and steer the running turn with it, attachments included (the item's
  composed content plus its image store paths). Returns 409 when there is no
  active turn, and 404 for an unknown or already-drained item. If the engine
  refuses the steer, the item goes back to the head of the queue.

Every queue mutation emits `queue.updated` with the whole queue
(`{ "items": [QueuedItemRecord…] }`). `GET /v1/threads/{id}` carries the
queue at the top level as `queue`.

Known limitations: `POST /turns` bypasses queued items (the queue route is
the ordered path). A queue steer that races the turn's completion can be
dropped by the engine's stale-steer discard; the direct steer route and the
REPL's flush have the same window.

**Approvals**
- `POST /v1/approvals/{approval_id}` with body
  `{ "decision": "allow" | "deny", "remember": false }`

**User input**
- `POST /v1/user-input/{thread_id}/{input_id}` — resolve a pending
  `request_user_input` prompt. Body:
  `{ "answers": [{ "id": "...", "label": "...", "value": "..." }] }`.
  Response: `{ "ok": true, "input_id": "...", "delivered": true }`.
  An unknown thread or input id surfaces as an API error.

**Sub-agent child sessions** (in-process children of a runtime thread)
- `GET /v1/threads/{id}/subagents` — the direct-child catalog
  (`SubagentSummary[]`: id, parent, name, agent type, depth, mode, status,
  model, timestamps, aggregated usage, open-turn activity).
- `POST /v1/subagents/{id}/input` — body `{ "text": "…" }`; maps to the
  child's `agent_send_input` steer seam. Only a running `continuable`
  child accepts it.
- `POST /v1/subagents/{id}/cancel` — stop a running `continuable` child;
  absent or terminal children are accepted no-ops
  (`{ "accepted": true }`). A running one-shot child is refused: its
  lifetime belongs to the parent turn, not the child-session API.

A child is a normal thread/turn/item record with `origin = "subagent"`,
`parent_id`, `agent_type`, `spawn_depth`, `subagent_mode` and
`subagent_status`. It is filtered from `GET /v1/threads` and reached
through its parent's catalog; `GET /v1/threads/{child}` and its event
stream serve the transcript. Replacement semantics: one run is one turn,
a restart marks a running child `interrupted` and keeps the transcript,
and there is no resume route. Child content is UI/replay only — it never
enters the parent model request.

**Events** (SSE replay + live stream)
- `GET /v1/threads/{id}/events?since_seq=<u64>`

**Compatibility stream** (one-shot, backwards-compatible)
- `POST /v1/stream`

**Tasks** (durable background work)
- `GET /v1/tasks`
- `POST /v1/tasks`
- `GET /v1/tasks/{id}`
- `POST /v1/tasks/{id}/cancel`

**Automations** (scheduled recurring work)
- `GET /v1/automations`
- `POST /v1/automations`
- `GET /v1/automations/{id}`
- `PATCH /v1/automations/{id}`
- `DELETE /v1/automations/{id}`
- `POST /v1/automations/{id}/run`
- `POST /v1/automations/{id}/pause`
- `POST /v1/automations/{id}/resume`
- `GET /v1/automations/{id}/runs?limit=20`

**Introspection**
- `GET /v1/workspace/status`
- `GET /v1/skills`
- `POST /v1/skills/{name}` — enable or disable a skill. Body:
  `{ "enabled": true }`. Response: `{ "name": "...", "enabled": true }`.
  Returns 404 when the skill is not found in the searched directories.
- `GET /v1/apps/mcp/servers`
- `GET /v1/apps/mcp/tools?server=<optional>`

**Usage** (token/cost aggregation across threads)
- `GET /v1/usage?since=<rfc3339>&until=<rfc3339>&group_by=<day|model|provider|thread>`

`since` / `until` are inclusive RFC 3339 timestamps and may be omitted (no
bound). `group_by` defaults to `day`. Buckets are sorted by ascending key.
Empty time ranges produce empty `buckets` (never a 404). Cost is computed via
the model→pricing map; turns whose model has no pricing entry contribute
tokens but `0.0` cost.

```json
{
  "since": "2026-04-01T00:00:00Z",
  "until": "2026-04-30T23:59:59Z",
  "group_by": "day",
  "totals": {
    "input_tokens": 12345,
    "output_tokens": 6789,
    "cached_tokens": 0,
    "reasoning_tokens": 0,
    "cost_usd": 0.012,
    "turns": 42
  },
  "buckets": [
    {
      "key": "2026-04-30",
      "input_tokens": 1234,
      "output_tokens": 678,
      "cached_tokens": 0,
      "reasoning_tokens": 0,
      "cost_usd": 0.001,
      "turns": 3
    }
  ]
}
```

## Runtime data model

The runtime uses a durable Thread/Turn/Item lifecycle.

- **ThreadRecord** — `id`, `created_at`, `updated_at`, `model`, `workspace`,
  `mode`, `task_id`, `coherence_state`, `system_prompt`, `latest_turn_id`,
  `latest_response_bookmark`, `archived`, `queue`
- **TurnRecord** — `id`, `thread_id`, `status` (`queued|in_progress|completed|
  failed|interrupted|canceled`), timestamps, duration, usage, error summary
- **TurnItemRecord** — `id`, `turn_id`, `kind` (`user_message|agent_message|
  tool_call|file_change|command_execution|context_compaction|status|error`),
  lifecycle `status`, `metadata`
- **QueuedItemRecord** — `id` (`qitem_…`), `text`, `appendix` (file
  references folded into the message at drain), `image_paths`,
  `attachments_metadata`, `rpc_id`, `input_summary`, `created_at`

Events are append-only with a global monotonic `seq` for replay/resume.

### Restart semantics

- If the process restarts while a turn or item is `queued` or `in_progress`,
  the recovered record is marked `interrupted` with an `"Interrupted by
  process restart"` error.
- The thread queue persists on the thread record and is still there after a
  restart, but it does not start on its own (resume-idle): the next enqueue
  or the next turn end drains it. Loading the thread, or reading its
  detail, never starts a queued item.
- Task execution performs its own recovery on top of the same persisted
  thread/turn store.

### Approval model

- The `auto_approve` flag applies to the runtime approval bridge and engine
  tool context. When enabled for a thread/turn/task, approval-required tools
  are auto-approved in the non-interactive runtime path, shell safety checks
  run in auto-approved mode, and spawned sub-agents inherit that setting.
- When omitted, `auto_approve` defaults to `false`.

### SSE event stream

The SSE event payload shape for `/v1/threads/{id}/events`:

```json
{
  "schema_version": 1,
  "seq": 42,
  "event": "item.delta",
  "kind": "item.delta",
  "thread_id": "thr_1234abcd",
  "turn_id": "turn_5678efgh",
  "item_id": "item_90ab12cd",
  "timestamp": "2026-02-11T20:18:49.123Z",
  "created_at": "2026-02-11T20:18:49.123Z",
  "payload": {
    "delta": "partial output",
    "kind": "agent_message"
  }
}
```

Compatibility notes:

- `schema_version` is the HTTP/SSE envelope schema version. It is independent of
  the runtime store schema used for persisted thread/turn/event records.
- `event` remains the SSE event name in existing clients; it is preserved as-is.
- `kind` mirrors `event` in the stable envelope for typed clients.
- `thread.started`, `turn.started`, and `turn.completed` are emitted as SSE event
  names exactly as before.
- `timestamp` remains the canonical event time for schema version 1. `created_at`
  is an equivalent alias for clients that use `created_at` naming elsewhere; do
  not require both fields to be present.

Common event names: `thread.started`, `thread.forked`, `turn.started`,
`turn.lifecycle`, `turn.steered`, `turn.interrupt_requested`,
`turn.completed`, `item.started`, `item.delta`, `item.completed`,
`item.failed`, `item.interrupted`, `approval.required`, `approval.decided`,
`approval.timeout`, `sandbox.denied`, `coherence.state`,
`background.agent_completed`, `background.status`, `queue.updated`,
`workflow.run_started`, `workflow.agent_started`, `workflow.agent_ended`,
`workflow.run_ended`.

A steer's user item emits `item.started` and then `item.completed`, the same
lifecycle as a turn's opening message, so a live client can show the steered
text as soon as it lands.

Workflow-run events journal the `workflow` tool's lifecycle on runtime
threads (thread-scoped, no item linkage; the field names are the desktop
client's workflow-run node vocabulary): `workflow.run_started`
`{runId, name}`; `workflow.agent_started` `{runId, seq, label, phase?,
childId}` (one `seq` per spawned member, drawn in issuance order; `phase`
omitted when the run has none; `childId` is synthetic — in-process children
are not sessions); `workflow.agent_ended` `{runId, seq, outcome:
completed|cancelled|failed}`; `workflow.run_ended` `{runId, stopReason:
completed|error}`. A rejected budget emits nothing; an interrupted run
emits no `run_ended`. A successful run's durable record also lands on its
tool item's `metadata.workflow_run` (`{runId, name, agents:
[{seq,label,childId,outcome?}…], stopReason}`) — a failed run's record is
live-only (its tool item settles as an error without metadata), and nested
runs (a sub-agent's own workflow) record nothing.

### Idle-wake turns (round 10)

Thread engines run with the engine's idle wake enabled: a background
sub-agent completing while no turn is running synthesizes the next turn on
its own. Each thread has one event reader that owns its engine's event
channel for the engine's whole life and attributes every engine turn by
what the engine says about it (round 12): a `TurnStarted` tagged
`synthesized: true` is materialized as a first-class turn record
(`input_summary` prefixed `idle wake`, `turn.started` payload carries
`"idle_wake": true`) and monitored exactly like a client turn — approvals
follow the normal approval model, and `interrupt` works against its turn
id; a real `TurnStarted` is the client turn `start_turn` claimed. Events
that arrive while the thread is idle (a background sub-agent completing,
engine status lines) are forwarded as thread-scoped `background.*` events
instead of leaking into the next client turn's item log. A client `POST
start_turn` while an idle-wake turn is active still gets the existing
"thread already has an active turn" rejection; a synthesized turn the
engine squeezes in between a client's claim and its op simply runs first,
with its own record, and the client turn follows with its own.

## Security boundary

- **Localhost by default**. The server binds to `127.0.0.1` by default.
  `--mobile` binds to `0.0.0.0` when no host is supplied so phones on the same
  LAN can reach it, and the CLI prints a warning for that rebind. Pass
  `--host 127.0.0.1` for a loopback-only mobile page. Set a non-loopback host
  only when you trust the network path or have a reverse-proxy / VPN that
  authenticates. The runtime does not provide user isolation or TLS.
- **Optional token guard**. `--auth-token` or `CODESMITH_RUNTIME_TOKEN`
  requires a matching bearer token for `/v1/*` routes. This is a local
  convenience guard, not a replacement for TLS, VPN, or a trusted reverse
  proxy on public networks.
- **No provider-token custody**. The server never returns the API key. The
  `api_key.source` capability field reports `env`, `config`, or `missing` —
  never the key itself.
- **No hosted relay**. The app-server is a local process under the user's
  control. There is no cloud component.
- **Capability responses** never leak secrets, file contents, or session
  message bodies. They report *metadata*: presence, counts, status flags.

### CORS allow-list

The runtime API ships with a built-in dev-origin allow-list:
`http://localhost:3000`, `http://127.0.0.1:3000`, `http://localhost:1420`,
`http://127.0.0.1:1420`, `tauri://localhost`. To add additional origins (e.g.
when developing a UI on Vite's default `:5173`), use any of:

- CLI flag (repeatable): `codesmith serve --http --cors-origin http://localhost:5173`
- Env var (comma-separated): `CODESMITH_CORS_ORIGINS="http://localhost:5173,http://localhost:8080"`
- Config (`~/.codesmith/config.toml`):
  ```toml
  [runtime_api]
  cors_origins = ["http://localhost:5173"]
  ```

User-supplied origins **stack on top of** the built-in defaults; they do not
replace them. Wildcard origins are not supported — the explicit allow-list
model is preserved.

## Session lifecycle (native UI supervision)

| Operation | Endpoint |
|---|---|
| List sessions | `GET /v1/sessions` |
| Get session | `GET /v1/sessions/{id}` |
| Delete session | `DELETE /v1/sessions/{id}` |
| Resume into thread | `POST /v1/sessions/{id}/resume-thread` |
| Create thread | `POST /v1/threads` |
| List threads | `GET /v1/threads` |
| Read / mutate goal | `GET` / `POST /v1/threads/{id}/goal` |
| Attach to events | `GET /v1/threads/{id}/events?since_seq=0` |
| Send message | `POST /v1/threads/{id}/turns` |
| Queue message | `POST /v1/threads/{id}/queue` |
| Edit / remove queued | `PATCH` / `DELETE /v1/threads/{id}/queue/{item_id}` |
| Steer queued | `POST /v1/threads/{id}/queue/{item_id}/steer` |
| Steer | `POST /v1/threads/{id}/turns/{turn_id}/steer` |
| Interrupt | `POST /v1/threads/{id}/turns/{turn_id}/interrupt` |
| Compact | `POST /v1/threads/{id}/compact` |

## Compatibility tests

Contract snapshots live in `crates/protocol/tests/`. Run:

```bash
cargo test -p codesmith-protocol --test parity_protocol --locked
```

This validates that the app-server's event schema hasn't drifted from the
documented contract. CI runs this on every push to `main` and on release tags.

The machine-readable contract is `docs/schemas/protocol.schema.json` — a JSON
Schema (draft 2020-12) generated from the `codesmith-protocol` types, with one
entry per top-level message under `messages` (`ThreadRequest`,
`AppRequest`, `PromptRequest`, `EventFrame`, `RuntimeEventEnvelope`, …) and
their nested types under `$defs`. `cargo test -p codesmith-protocol --test
protocol_schema` fails when the committed file no longer matches the types;
regenerate it with `CODESMITH_UPDATE_SCHEMA=1` set.
