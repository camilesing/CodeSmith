# Hooks (Lifecycle Shell Commands)

CodeSmith can run your shell commands at well-defined points in the agent
lifecycle: before/after each tool execution, around task creation and
completion, immediately before each `exec_shell` invocation, and around
context compaction. Use hooks for audit logging, notifications, and
ephemeral credential injection. Not every accepted event fires in the
current build — see [Events](#events) for which are emitted and which are
configured-but-silent.

Hooks are configured under the `[hooks]` table in `~/.codesmith/config.toml`.
A project-level
`<workspace>/.codesmith/config.toml` may carry its own `[hooks]` table — when
present it **replaces** the user-level table wholesale, it does not merge
(the same rule as `instructions`). If you want both, list the global hooks
again inside the project table.

Hooks are plain shell commands. They run with your full user privileges —
they are not sandboxed and are not a security boundary (see
[Security notes](#security-notes)).

## Quick start

Append to `~/.codesmith/config.toml`:

```toml
[[hooks.hooks]]
event = "tool_call_before"
command = "echo 'CodeSmith tool call starting'"

[[hooks.hooks]]
name = "audit-shell"
event = "tool_call_after"
command = "~/.codesmith/hooks/audit-log.sh"
condition = { type = "tool_name", name = "exec_shell" }
```

Run `/hooks` (or `/hooks list`) inside the REPL to confirm both entries are
loaded, and `/hooks events` to print the event names the REPL recognizes
(eleven of them — see [Inspecting hooks](#inspecting-hooks)).

## Configuration schema

The `[hooks]` table holds global switches; each `[[hooks.hooks]]` entry is
one hook.

```toml
[hooks]
enabled = true                # global kill-switch; false suppresses every hook
default_timeout_secs = 30     # table-level timeout; OVERRIDES per-hook values
working_dir = "/some/dir"     # hook cwd; defaults to the workspace

[[hooks.hooks]]
event = "message_submit"      # one of the events listed below (required)
command = "my-script.sh"      # shell command (required)
name = "inject-context"       # optional; shows up in hook logs
timeout_secs = 2              # per-hook timeout; ignored when
                              # default_timeout_secs is set
background = false            # fire-and-forget; stdout capture lost
                              # (still timeout-bounded)
continue_on_error = true      # what to do when the hook fails
condition = { type = "mode", mode = "agent" }   # optional; see Conditions
```

Field notes:

- `command` runs through the platform shell — `sh -c` on Unix, `cmd /C` on
  Windows — so `~` expansion, pipes, and `&&` chains work on Unix.
- Timeout precedence is table-over-hook: once `default_timeout_secs` is set
  it applies to every hook, even those with their own `timeout_secs`. A hook
  that exceeds its timeout is killed and counts as failed (descendant
  processes that inherit the pipes are not waited on, so the timeout holds).
- `continue_on_error` only affects later hooks and (for `message_submit`)
  whether a failure blocks the submission; see below. Failures always log a
  `tracing::warn!` regardless.
- Hooks run serially in config order. A failed hook with
  `continue_on_error = false` stops the remaining hooks for that event.
- `background = true` spawns the command on a detached thread and returns
  immediately: stdout capture is lost (observer-only everywhere), but the
  process is still bounded by `default_timeout_secs` when set, else the
  hook's own `timeout_secs` — it is killed at the deadline instead of
  leaking forever.

## Events

| Event | Alias | Fires when | Mutability |
|---|---|---|---|
| `session_start` | — | defined but **not emitted** in the current build | observer |
| `session_end` | — | defined but **not emitted** in the current build | observer |
| `message_submit` | — | executor implemented but **not wired** — the REPL submit path never invokes it (see below) | **can replace or block text** (contract) |
| `tool_call_before` | `pre_tool_use` | before each tool execution | observer |
| `tool_call_after` | `post_tool_use` | after each tool execution completes | observer |
| `mode_change` | — | defined but **not emitted** in the current build | observer |
| `on_error` | — | defined but **not emitted** in the current build | observer |
| `shell_env` | — | immediately before each `exec_shell` invocation | **stdout becomes env vars** |
| `task_created` | — | when the task manager creates a task | observer |
| `task_completed` | — | when a tracked task completes | observer |
| `pre_compact` | — | before context compaction (manual, auto, or emergency) | **stdout is preserved context** |
| `turn_end` | — | defined but **not emitted** in the current build | observer |
| `subagent_spawn` | — | defined but **not emitted** in the current build | observer |
| `subagent_complete` | — | defined but **not emitted** in the current build | observer |

The events with production emit sites in the shipped build:
`tool_call_before` / `tool_call_after` (the runtime callback bridge),
`task_created` / `task_completed` (the task tool), `pre_compact` (the
compaction pass), and `shell_env` (the shell tool). The seven marked
**not emitted** have enum variants and config parsing but no production
emit site — a hook configured for them parses and shows up in `/hooks
list` yet never runs.

Details per group:

- **Lifecycle.** `session_start` / `session_end` are defined for
  once-per-REPL-process use (loading state, setting up logging, emitting a
  notification) but are **not emitted in the current build** — hooks
  configured for them never run.
- **`message_submit`** is the only event that can change agent input, and
  its executor is fully implemented, but it is **not wired**: the REPL
  submit path never invokes it, so a configured `message_submit` hook will
  not run. See [Hook output](#hook-output) for the stdin/stdout JSON
  contract and the exit-code-2 block semantics (retained as the contract).
  When wired it will run *before* file-mention expansion,
  skill wrapping, auto routing, and history append — the transformed text
  is what the model sees.
- **Tool calls.** `tool_call_before` / `tool_call_after` are read-only
  observers: they cannot veto a tool call or mutate its arguments. Gating is
  the job of the approval flow, sandbox, and execpolicy rules
  (see [docs/SANDBOX.md](SANDBOX.md)).
- **`shell_env`** runs synchronously right before every `exec_shell`. Its
  stdout is parsed as `KEY=VALUE` lines and merged into the spawned
  process's environment (later hooks override earlier ones). Use it for
  ephemeral credentials or per-skill `PATH` adjustments. Failures and
  timeouts contribute no vars — the shell call itself always proceeds.
- **Tasks.** `task_created` / `task_completed` carry `task_id`,
  `task_subject`, and `task_status` context.
- **`pre_compact`** fires before the context is summarized. Each matching
  non-background hook's stdout is concatenated (separated by `---` rules)
  and merged into the compaction summary, so material you print survives
  summarization. Failures are non-blocking: compaction always proceeds.
- **`turn_end`** is defined but **not emitted in the current build**. The
  intended contract: it fires after a turn completes (`completed` /
  `interrupted` / `failed`) with the status and usage on stdin; failures
  are warn-only and the hook cannot alter turn state (`stop_hook_active`
  always `false`). The stdin envelope below documents that contract.
- **Sub-agents.** `subagent_spawn` / `subagent_complete` are defined but
  **not emitted in the current build**. The intended contract: they fire
  when a sub-agent starts or finishes, with `agent_id` and a bounded
  `summary` on stdin — never the full prompt/result; failures are
  warn-only and never affect the sub-agent lifecycle.

## Conditions

Every hook accepts an optional `condition`. Without one (or with
`{ type = "always" }`) the hook runs unconditionally whenever its event
fires.

```toml
# Only for one tool
condition = { type = "tool_name", name = "exec_shell" }

# Only for a category of tools
condition = { type = "tool_category", category = "shell" }

# Only in one mode
condition = { type = "mode", mode = "yolo" }

# Only when the tool exited with a specific code (tool_call_after)
condition = { type = "exit_code", code = 1 }

# Combine with AND / OR
condition = { type = "all", conditions = [
  { type = "tool_name", name = "exec_shell" },
  { type = "mode", mode = "agent" },
] }
condition = { type = "any", conditions = [ ... ] }
```

The `tool_category` matcher maps tool names to categories:

| Tools | Category |
|---|---|
| `exec_shell` | `shell` |
| `write_file`, `edit_file`, `apply_patch` | `file_write` |
| `read_file`, `list_dir`, `grep_files` | `safe` |
| anything else | `other` |

`mode` matching is case-insensitive; `tool_name` is exact.

## Hook input

### Environment variables

Every hook receives a set of `CODESMITH_*` environment variables describing
the event context. Unset fields are simply absent from the environment.

| Variable | Present for | Content |
|---|---|---|
| `CODESMITH_TOOL_NAME` | tool events, `shell_env` | tool name, e.g. `exec_shell` |
| `CODESMITH_TOOL_ARGS` | tool events | tool arguments as a JSON string |
| `CODESMITH_TOOL_RESULT` | `tool_call_after` | tool output, truncated at 10,000 bytes |
| `CODESMITH_TOOL_EXIT_CODE` | `tool_call_after` | exit code when applicable |
| `CODESMITH_TOOL_SUCCESS` | `tool_call_after` | `true` / `false` |
| `CODESMITH_MODE` | most events | current mode (`plan` / `agent` / `yolo`) |
| `CODESMITH_PREVIOUS_MODE` | `mode_change` | mode before the transition |
| `CODESMITH_SESSION_ID` | most events | **ephemeral** telemetry id, see below |
| `CODESMITH_THREAD_ID` | most events | persistent thread id, see below |
| `CODESMITH_MESSAGE` | `message_submit` | current (possibly already-transformed) text, truncated at 5,000 bytes |
| `CODESMITH_ERROR` | `on_error` | error message |
| `CODESMITH_WORKSPACE` | most events | workspace path |
| `CODESMITH_MODEL` | most events | current model name |
| `CODESMITH_TOTAL_TOKENS` | most events | total tokens used so far |
| `CODESMITH_SESSION_COST` | most events | session cost in USD |
| `CODESMITH_TASK_ID` / `CODESMITH_TASK_SUBJECT` / `CODESMITH_TASK_STATUS` | task events | task metadata |

> **Session vs thread identity.** `CODESMITH_SESSION_ID` is an ephemeral id
> regenerated on every session start — it does **not** correlate across
> restarts. For correlation that survives resume (audit trails, capacity
> memory), use `CODESMITH_THREAD_ID`, which carries the persistent thread id.

### stdin

Only structured-stdin events (`message_submit`, `pre_compact`, `turn_end`,
`subagent_spawn`, `subagent_complete`) receive JSON on stdin; all other
events run with no stdin. Of these only `pre_compact` fires in the
shipped build — `message_submit` is implemented but not wired, and
`turn_end` / `subagent_spawn` / `subagent_complete` are not emitted — so
the envelopes below are those events' contracts, not payloads any current
code constructs.

`message_submit` receives:

```json
{
  "event": "message_submit",
  "text": "original user text",
  "session_id": "sess_12345678",
  "thread_id": "thread-abc",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

`pre_compact` receives the same envelope with `hook_event_name` as the
event key and no `text` field:

```json
{
  "hook_event_name": "pre_compact",
  "session_id": "sess_12345678",
  "thread_id": "thread-abc",
  "workspace": "/path/to/workspace",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

`turn_end` receives the same envelope plus turn status and usage:

```json
{
  "hook_event_name": "turn_end",
  "session_id": "sess_12345678",
  "thread_id": "thread-abc",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "status": "completed",
  "input_tokens": 120,
  "output_tokens": 80,
  "total_tokens": 1234,
  "session_cost": 0.0123,
  "duration_secs": 14.5,
  "stop_hook_active": false
}
```

`subagent_spawn` / `subagent_complete` receive the same envelope plus the
agent id and a bounded summary (truncated, never the full prompt/result):

```json
{
  "hook_event_name": "subagent_spawn",
  "session_id": "sess_12345678",
  "thread_id": "thread-abc",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "agent_id": "agent_7",
  "summary": "research the repo for RFC references"
}
```

## Hook output

Three events interpret hook stdout; for every other event stdout is
ignored.

### `message_submit` (transform or block)

The executor behind this contract is fully implemented but **not wired**:
the REPL submit path never invokes it, so a configured `message_submit`
hook will not run in the current build. The contract:

- Exit `0` + stdout JSON with a non-empty string `text` field → that value
  **replaces** the submitted text: `{"text": "replacement user text"}`
- Exit `0` + empty stdout, or JSON without `text`, or `{"text": ""}` →
  text unchanged
- Exit `0` + malformed stdout JSON → text unchanged, a warning is logged
- Exit `2` → submission **blocked** before the turn starts; a `reason`
  field, stderr, or stdout supplies the message shown in the REPL
- Any other non-zero exit, timeout, or spawn failure → governed by
  `continue_on_error`: `true` keeps the current text and continues to later
  hooks (with a status message); `false` blocks the submission

Multiple `message_submit` hooks run in config order and each receives the
text produced by the previous hook. `background = true` hooks on this event
are observer-only — they can neither transform nor block.

### `shell_env` (KEY=VALUE lines)

```text
AWS_ACCESS_KEY_ID=...
AWS_SECRET_ACCESS_KEY=...
```

One `KEY=VALUE` per line on stdout; an optional `export ` prefix is
accepted. Vars are merged into the `exec_shell` environment; later hooks
override earlier ones. Resolved KEY names (never values) are written to
`~/.codesmith/audit.log` so sessions can be reconciled without leaking
secret material.

### `pre_compact` (free text)

Everything on stdout of each matching non-background hook is concatenated
(separated by `---` rules) and merged into the compaction summary. Print
the facts you want to survive summarization.

## Execution semantics

- **Shell:** `sh -c <command>` on Unix, `cmd /C <command>` on Windows.
- **Working directory:** `[hooks].working_dir` if set, otherwise the
  current workspace.
- **Ordering:** serial, in config order, per event.
- **Timeout:** per-hook `timeout_secs` (default 30) unless the table-level
  `default_timeout_secs` is set, which wins. Timed-out hooks are killed and
  count as failed.
- **Failure handling:** failures log a `tracing::warn!` under the `hooks`
  target. With `continue_on_error = true` (the default) later hooks still
  run; with `false` the remaining hooks for that event are skipped.
- **Enablement:** `[hooks].enabled = false` suppresses everything;
  `/hooks list` shows when this is the case.

## Inspecting hooks

- `/hooks` or `/hooks list` — a count and an enabled/globally-disabled
  header, then one line per hook with its event (Debug-formatted) and
  command. Names, timeouts, and conditions are not rendered, and there is
  no grouping.
- `/hooks events` — a single line listing the eleven event names the REPL
  knows (`session_start` … `pre_compact`); it omits `turn_end` /
  `subagent_spawn` / `subagent_complete` (none of which are emitted in the
  current build) and carries no per-event descriptions.

## Security notes

- Hook commands run **with your full user privileges** and are not
  sandboxed. Anyone who can write your `config.toml` (or a project-level
  `.codesmith/config.toml` in a repo you open) can run arbitrary commands
  as you via hooks — review project configs before working in untrusted
  repositories.
- `shell_env` values live in process environments and can appear in child
  process listings on some platforms. The audit log records key names only,
  never values.
- Tool arguments and results (exposed via `CODESMITH_TOOL_ARGS` /
  `CODESMITH_TOOL_RESULT`) may contain secrets from your repository; treat
  hook stdout/logs accordingly.

## What hooks are not

- **Not a gating mechanism.** `tool_call_before` cannot veto or rewrite a
  tool call. Approval prompts, sandboxing, and execpolicy rules are the
  enforcement layers (see [docs/SANDBOX.md](SANDBOX.md)).
- **Not `[hook_sinks]`.** The `[hook_sinks]` config table feeds an unrelated
  observability system (stdout / JSONL / webhook / Unix-socket event sinks
  for the HTTP API server). Lifecycle hooks live under `[hooks]` only.
- **Not the extension system.** For in-process Rust extensions with an
  event bus, see [docs/EXTENSIONS.md](EXTENSIONS.md).

## Recipes

### Audit-log every shell command

```toml
[[hooks.hooks]]
name = "shell-audit"
event = "tool_call_after"
command = "printf '%s\\t%s\\t%s\\n' \"$CODESMITH_THREAD_ID\" \"$CODESMITH_TOOL_NAME\" \"$CODESMITH_TOOL_EXIT_CODE\" >> ~/.codesmith/hooks/shell-audit.log"
condition = { type = "tool_name", name = "exec_shell" }
```

### Inject context before every submission

```toml
[[hooks.hooks]]
name = "inject-todo"
event = "message_submit"
command = "~/.codesmith/hooks/inject-context.sh"
timeout_secs = 2
continue_on_error = true
```

`~/.codesmith/hooks/inject-context.sh`:

```sh
#!/bin/sh
# Read the JSON payload from stdin, prepend current TODOs to the text.
input=$(cat)
text=$(printf '%s' "$input" | jq -r .text)
todos=$(cat ~/.codesmith/TODO.md 2>/dev/null || true)
if [ -n "$todos" ]; then
  jq -n --arg t "$text

<context>
$todos
</context>" '{text: $t}'
fi
# Empty stdout leaves the submission unchanged.
```

### Ephemeral credentials for every shell call

```toml
[[hooks.hooks]]
name = "aws-creds"
event = "shell_env"
command = "aws-vault export my-profile --format=env"
```

### Keep facts alive across compaction

```toml
[[hooks.hooks]]
name = "preserve-decisions"
event = "pre_compact"
command = "cat .codesmith/DECISIONS.md 2>/dev/null"
```

## Troubleshooting

- **Nothing fires.** Check `/hooks list` first: the hook must appear there,
  and the header shows whether `[hooks].enabled = false` is suppressing
  everything. A project-level `[hooks]` table silently replaces your
  user-level one.
- **Hook fails silently.** Failures log under the `hooks` tracing target —
  run with `RUST_LOG=hooks=warn` (or `=debug`) to see exit codes, stderr
  head, and durations.
- **`message_submit` stdout ignored.** The contract is strict: a single JSON
  object with a non-empty `text` field on exit `0`. `{"text": ""}` and
  non-JSON output are ignored with a warning. Background hooks never
  transform. (And note: `message_submit` is not wired in the current
  build — the hook does not run at all.)
- **Timeout kills long-running hooks.** Either raise `timeout_secs` or set
  `background = true` (accepting that stdout is then ignored).
- **Condition never matches.** `tool_name` is an exact match;
  `tool_category` only knows `shell`, `file_write`, `safe`, and `other`.
  `/hooks list` does not render conditions — check `config.toml`.
