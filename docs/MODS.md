# CodeSmith Mods — the Rhai Script Mod Layer

Mods extend CodeSmith with [Rhai](https://rhai.rs) scripts — no Rust compilation required. A few lines of script add **event hooks** (intercept/rewrite tool calls, input, provider requests), register **model-visible tools**, register **slash commands**, and keep state across sessions in a **KV store**. Activate a mod and it works in the current session; reload and a saved change takes effect without re-approval.

The smallest possible mod is 3 lines of script — this one blocks any `rm -rf` before it runs:

```rhai
on("tool-call", |e| {
    if e.name == "exec_shell" && e.input.command.contains("rm -rf") {
        return block("rm -rf needs human confirmation");
    }
    proceed()
});
```

> Need file/network access or the full Rust ecosystem? That's the other path — Rust extensions (dylib), see [EXTENSIONS.md](EXTENSIONS.md). Both ride the same assembly line; this guide covers Rhai Mods only. 中文版：[MODS_cn.md](MODS_cn.md).

## Which tier when (the pluggable-surface map)

Everything pluggable rides one contract: `Extension`
(`crates/agent/src/extension.rs`) + the `ExtensionRunner` assembly line.
Four tiers implement it — user Rhai mods, the builtin Rhai tier (embedded
script defaults), compiled-in Rust extensions, and installed Rust dylibs —
and "pluggable" means *registered through one of them at runtime*, not
wired into the binary at compile time. Which tier fits a given surface:

| Surface | Rhai mod | Builtin Rhai tier | Rust extension | Notes |
|---|---|---|---|---|
| Event hooks, tool guards | ✓ `on` / `on_event` / `register_guard` | — | ✓ | load/unload per mod; hook errors fail open |
| Model-visible tools | ✓ `register_tool` (pure logic — no fs/net natives) | ✓ (scriptable defaults) | ✓ (full IO/async) | sub-agents mount script/compiled-in tools; dylib-origin Rust tools stay main-turn only (5b tiering) |
| Existing tool implementations | ✓ `replace_tool` (substitute execution; `delegate()` reaches the original) | — | ✓ | metadata except description/schema stays the incumbent's — a replacement cannot relax an approval gate; main and sub-agent registries apply the same replacements (see below) |
| Slash commands | ✓ `register_command` (shadows built-ins) / `disable_command` (retires one) | — | ✓ | layering user > mod > builtin |
| Providers | alias only (`register_provider`) | — | ✓ full `ProviderFactory` | a script cannot implement `LlmClient` (no async/net) |
| Prompt sections, skills | ✓ `register_prompt_section` / `register_skill` | ✓ (defaults) | ✓ | prefix-cache discipline applies |
| Policies (host-mechanism knobs) | ✓ `register_policy` (last-wins) | ✓ — the defaults ship as builtin mods | ✓ | data, not code; `/mods list` shows resolution |
| Turn loop (every agent) | ✓ `register_loop` (drive, via the `rhai` executor) | — | ✓ `register_around_turn` (wrap) | kernel composition — *selection*: config `turn_executor` picks a crate-compiled `TurnExecutorRegistry` entry (`builtin`, or `"rhai"` to run a mod's loop script) for the main turn and every in-process sub-agent turn alike; *composition*: a Rust extension wraps the run with the general agent's `next()` (every turn — a sub-agent composes the chain at its agent-type scope); a script loop owns only the loop SHAPE — its capability calls (`llm_request`, `run_tools`) execute the host's step services (see "Writing a loop script") |
| Compaction strategy | ✓ `register_compaction_strategy` (trigger + plan decisions) | ✓ (opt-in ports select via `[compaction] strategy`) | — | the mod owns the *decisions* (when to compact, how much to keep, summary directives); the host owns the *effects* and the immune zone (pairing invariant, system-prompt prefix, append-only storage, the summary LLM call, the circuit breaker). Selection: config `[compaction] strategy = "builtin" \| "<name>"` (see "Writing a compaction strategy") |
| `BASE_PROMPT`, sub-agent tool registries | — | — | — | core by declared contract (sole base prompt, built-in-only sub-agent registries) |

Rule of thumb: **logic and data → Rhai** (no recompile, consent-gated,
sandboxed by the absence of I/O natives); **async, filesystem, network, or
full-ecosystem work → a Rust extension** (dylib install +
`/extension reload`); **a new default for an existing host mechanism →
the builtin Rhai tier** (ships in the binary, user mods override by
kind). This split is the declared architecture: new pluggable work lands
on one of these tiers instead of adding another compile-time wiring site.

## Five-Minute Quick Start

### Step 1: Create the directory, write two files

Mods live under a global root `~/.codesmith/mods/<id>/` (visible in every workspace) or a project root `<workspace>/.codesmith/mods/<id>/` (gated by workspace trust). The directory name is the mod id:

```bash
mkdir -p ~/.codesmith/mods/hello
```

`~/.codesmith/mods/hello/mod.toml` — just two required fields:

```toml
id = "hello"
version = "0.1.0"
```

`~/.codesmith/mods/hello/mod.rhai` — the 3-line guard hook from the top of this page. The top-level script runs once at load; `on(...)` registers the hook with the runtime:

```rhai
on("tool-call", |e| {
    if e.name == "exec_shell" && e.input.command.contains("rm -rf") {
        return block("rm -rf needs human confirmation");
    }
    proceed()
});
```

> `&&` short-circuits: non-`exec_shell` tool calls never evaluate `e.input.command`, so a missing field is not a concern.

### Step 2: Activate

Activate from the REPL with `/mods activate hello` — this is the
**one-time consent** for this content (a mod is in-process code that
persists across sessions — first activation is explicit). Alternatively
ask the agent — "activate the mod hello" — and approve the
`manage_mods(action="activate", id="hello")` tool call it issues.

**What you'll see**: after activation the mod is live immediately; same-id reloads never ask again — unless the mod's entry file changed since you approved it (git pull, an edit): a changed mod goes back to pending and asks for a fresh activation. Any shell call containing `rm -rf` is now blocked, and the model receives your reason and adapts.

### Step 3: Register a model-visible tool + KV state

Append to `mod.rhai`: one hook counting every tool call, and one tool the model can call to read the count. `mod_state_get/set` is a per-mod private persistent KV (survives sessions, isolated between mods):

```rhai
on("tool-call", |e| {
    let n = mod_state_get("calls") ?? 0;   // default 0 when absent
    mod_state_set("calls", n + 1);
    proceed()
});

register_tool(#{
    name: "call_count",
    description: "Report the cumulative tool-call count for this session",
    schema: #{ type: "object", properties: #{}, additionalProperties: false },
}, |input| ok(mod_state_get("calls") ?? 0));
```

**What you'll see**: from the next turn, `call_count` appears in the model's tool catalog; calling it returns the current count. `ok(...)` builds a successful result (strings verbatim, other values JSON-encoded).

### Step 4: Register a slash command, reload the change

Add a slash command (backtick strings support `${...}` interpolation):

```rhai
register_command("calls", "Show the tool-call count", |args, ctx| {
    message(`${mod_state_get("calls") ?? 0} tool calls so far`);
});
```

**What you'll see**:

- Typing `/calls some args` in the REPL dispatches through the extension runner: `message(...)` prints its text, `send(...)` submits it into the conversation. Mod commands sit between user commands and the built-in table (user commands shadow them; they shadow built-ins), appear in `/help` under "mod commands", and Tab completes their names live.
- Now **edit `mod.rhai`, save, and reload** with `/mods reload`; it re-runs discovery, no re-approval for the same id. `/mods status` confirms which mods are loaded and the runner generation (`/mods list` shows per-mod activation state and the resolved policy layer; the startup notice also lists discovered-but-not-activated mods). (The automatic file watcher and its `[mods] watch` switch were removed in 0.10.0; reloading is explicit.)

You've now touched all four registration surfaces: event hooks, tools, commands, KV. The full `/mods` command set is `list` / `status` / `info <id>` / `activate <id>` / `enable|disable <id>` / `remove <id>` / `reload`; the model-facing `manage_mods` tool mirrors it with actions `list` / `write` / `activate` / `disable` / `enable` / `remove` / `reload`, plus the session lane `session_write` / `session_remove` (ephemeral experiments — see below).

## Quick Reference

### Event hooks

`on("<event>", |e, ctx| { ... })` — `e` is the event payload map (every payload carries `kind`), `ctx` is `#{cwd, mode, idle, generation, agent}` (`mode` is `"tui"` for the interactive terminal REPL — the retired frontend's name, kept for compatibility — or `"rpc"` / `"json"` / `"print"`); **ctx is optional** (`|e|` works). `agent` is the dispatch's agent identity — `#{main, type}` where `main` is `true` on the main turn and `type` is the sub-agent type (`()` on the main turn) — so a callback can tell a child turn from the main turn (`if !ctx.agent.main { return (); }` is the standard "main turn only" guard). Event names are the kebab-case spelling of all 32 `ExtensionEventKind` variants:

| Event | Payload fields |
|---|---|
| `input` | `text` |
| `before-agent-start` | `system_prompt`, `inject_message` (`()` = unset) |
| `before-provider-request` | `messages` (JSON value) |
| `after-provider-response` | `response` (JSON value — the assembled `MessageResponse`; `id` is empty on the streaming path) |
| `before-provider-headers` | `headers` (JSON object; credential-bearing values masked) |
| `provider-request-error` | `failure` (`#{category, message}`), `attempt`, `plan` (`#{action: "retry" \| "give-up", delay_ms}`) |
| `tool-call` | `id`, `name`, `input` (JSON value) |
| `tool-result` | `id`, `name`, `content`, `success`, `is_error`, `canonical` (JSON value, `()` when the tool produced none) |
| `turn-start` / `turn-end` | `turn_id`; `turn-end` also `reason` plus the activity summary `steps` (loop iterations begun), `tool_calls`, `usage` (`#{input_tokens, output_tokens}`) |
| `turn-stopping` | `reason` (why the turn would end — `no-tool-calls` at the only emit site); fires just before the turn ends on no tool calls; `cancel("note")` steers — the note is appended as a user message and the turn resumes (capped at 3 steers per turn) |
| `assistant-stream` | `text` (one incremental chunk; fires per text delta, emit-only) |
| `tool-execution-update` | `id`, `name`, `message` |
| `tools-change` | `added`, `removed` (tool-name arrays) |
| `workflow-start` | (`kind` only) — a `workflow` run began (budget-rejected runs emit nothing) |
| `workflow-log` | `message` (one bounded `log(...)` entry from a workflow run) |
| `workflow-agent-start` | `objective` (one workflow child spawn began) |
| `workflow-agent-end` | `status`, `steps` (one workflow child finished) |
| `workflow-end` | `status`, `error` (error kind on failure), `agent_calls` (a workflow run's terminal outcome; no return value — the transcript's tool result is the record) |
| `project-trust` / `session-start` | `reason` |
| `result-verified` | `verdict` (`verified-pass` \| `verified-fail` \| `verify-error` \| `unsubstantiated`), `claim`, `command` (`()` when nothing could be re-run), `exit_code` (`()` when it did not run) — the result-claim verifier's outcome, fired from the post-turn background replay, so it lands between turns |
| `session-before-compact` | `current_tokens` (whole-transcript estimate), `threshold` (the configured token threshold), `pinned_hints` (string array — transform-writable, chain-visible, not consumed by the host in v1) — advisory readings of the trigger inputs; the decision itself compares a plan-shaped subset against a pinned-adjusted effective threshold, so a veto weighs these, not the literal operands |
| `session-compact` | `summary` (head of the compaction summary, truncated for preview — the full prompt is recorded for the host's post-run merge), `kept_messages` (transcript length after the compacted replacement and reinjection applied) |
| `agent-start` / `tool-execution-start` / `tool-execution-end` / `agent-end` / `agent-settled` / `session-before-switch` / `session-before-fork` / `session-shutdown` | (`kind` only) |

`tool-execution-update` fires from the shell tools' own poll loops while a command keeps
running (`exec_shell` foreground and `exec_shell_wait`): at most once per second and only
when new output arrived. `id` is the shell task id (a tool does not know its wire tool-call
id — correlate by `name` + ordering); `message` is the output since the previous checkpoint
(tail-capped at 4 KiB; a truncated manager excerpt yields its tail). Emit-only, main turns
only (a sub-agent turn's callback wires no tool-progress sink); sandboxed-backend and
interactive runs report no checkpoints. The same checkpoint reaches the UI as `ToolCallProgress`.
`resources-discover` was removed in 0.11.0 (never had an emitter or a consumer).
`session-before-fork` fires at
every spawn — the spawn-family tools (`agent_open` / `agent_spawn` /
`agent_run`) at every depth (the runner rides the sub-agent runtime, so
grandchildren forks are vetoable too) and the engine-internal spawn path — before the child
forks session state; `cancel()` vetoes the fork. Since 5b the runner is more than the veto
carrier on the child side: sub-agent registries mount the child type's extension-tool view
(script/compiled-in origins only — a mod's guard therefore also gates sub-agent tool calls),
and every sub-agent turn — the same loop as the main turn — dispatches its lifecycle at its
agent-type scope: `before-agent-start`, `agent-start` / `agent-end`, `turn-start` /
`turn-end`, `turn-stopping`, the provider events, `assistant-stream`, `tool-call` / `tool-result` and
`tool-execution-start` / `-end` (script/compiled-in handlers only; dylib handlers never see a
child), and its follow-up input (`agent_send_input`) meets `input` as a steer. Session-level
events (`session-*`, `agent-settled`, `tools-change`) stay main-turn-only.

`tool-result` payloads carry the canonical/rendered separation: `content` is the model-visible rendering; `canonical` is the structured machine-readable value of the same result (the `ToolResult::json` value, or what a tool attached via `with_canonical`). Read `canonical` instead of re-parsing `content` — prose-rendered results (`todo_add` prefixes a sentence before the JSON) are not parseable at all. Canonical is live-process only: the transcript persists the rendered `content`.

`tools-change` fires at the turn boundary when the compiled model-visible catalog changed since the previous main turn (mode switch, mod reload, selection change). The first turn establishes the baseline silently; sub-agent toolsets never trigger it, and there is no API to read the current catalog — the event carries the diff only. `/tools` renders the current baseline grouped by origin (builtin / plugin / extension / mcp).

Dispatch modes are part of each event's contract (`ExtensionEventKind::dispatch_mode`,
checked by a contract test) — three states named after the general-agent model: **waterfall**, **serial**, **emit**:

- **waterfall**: `input`, `before-agent-start`, `before-provider-request`, `tool-result` — each transform folds in, the next handler sees it, the final field applies
- **waterfall + deny**: `tool-call` — rewrite the call's `input` (the rewritten input is what approval gates and what runs, and it is the recorded input) or `block(reason)` to deny (monotonic — no later handler can flip it); `provider-request-error` — rewrite the retry `plan` or `block(reason)` to give up
- **serial**: `session-before-switch`, `session-before-fork`, `session-before-compact`, `turn-stopping` (inverted: `cancel(reason)` continues the turn — the note steers it)
- **emit** (outcomes advisory): everything else, including `assistant-stream` (fires per text delta — keep handlers cheap) and `tools-change` (catalog diff at the turn-dispatch chokepoint)

#### `on_event` — the general-agent-aligned listener/veto form

`on_event("<event>", |e, ctx| { ... })` takes the same callback shape as
`on` (same payloads, same optional `ctx`, same control values) but accepts
only **emit** and **serial** events — the general-agent-portable subset. A waterfall
event (`input`, `before-agent-start`, `before-provider-request`,
`tool-result`, `tool-call`, `provider-request-error`) fails the mod's
**load** with an error naming the kind: its transform chain belongs to
`on()`, its deny floor to `register_guard`. At emit kinds every return
value is advisory; at serial kinds `cancel(reason)` is the veto. Use
`on_event` when a mod only listens or vetoes — such a mod maps 1:1 onto
the general agent's emit/serial model; use `on()` for the full CodeSmith surface
(waterfall transforms included — a deliberate superset beyond the general-agent
script surface).

Scope: `on_event` registrations are global-scope, script-origin — a
global registration covers every dispatch scope, so an `on_event`
callback observes the main turn's emit/serial events and every
sub-agent turn's (`turn-start`, `turn-end`, `agent-start`, `agent-end`,
`turn-stopping`, the provider emits, `assistant-stream`, `tool-execution-*`).
The dispatch identity rides `ctx.agent` (`#{main, type}`): a callback
tells a child's turn from the main turn's by `ctx.agent.main` — but not
one child instance from another (`type` is the scope unit; there is no
per-instance id). There is no `on_event_scoped` — scripts do
not see registration scopes (a compiled-in extension can register
`on_variant_scoped`). Around-execute and `ask` have no script form in
any channel (async / UI decision channel — see
[EXTENSIONS.md](EXTENSIONS.md); there is no `ask()` control value, so a
script can never produce an `Ask` outcome).

### Hook return value → HandlerOutcome

| Script returns | HandlerOutcome | Effective seams |
|---|---|---|
| `proceed()` or `()` | `Continue` | all |
| `block(reason)` | `Block` (ignored at non-block seams) | `tool-call`, `provider-request-error` |
| `cancel(reason)` | `Cancel` (ignored at non-cancel seams) | `session-before-*`, `turn-stopping` (steers the turn onward) |
| `transform(#{...})` | `Transform` (merges mutable fields, chain continues) | see below |

Transform mutable fields (one handler's rewrite is immediately visible to the next):

- `input`: `text` — the text of each submitted message and of each message typed while a turn runs (a steer); the rewrite is what is recorded and sent
- `tool-call`: `input` (JSON value — the pre-execute rewrite; the host applies it before approval + execution and records it)
- `before-agent-start`: `system_prompt`, `inject_message` (string = set, `()` = clear, absent = keep)
- `before-provider-request`: `messages`
- `tool-result`: `content`, `success`, `is_error`, `canonical` (JSON value — the machine-readable result; rewriting `content` alone leaves it untouched, and it can be replaced explicitly)
- `provider-request-error`: `retry` (bool — `false` gives up), `delay_ms` (int; kept from the current plan when absent; forced retries are capped engine-side)
- `session-before-compact`: `pinned_hints` (array of strings = set, `()` = clear, absent = keep) — chain-visible only in v1: the host does not consume text hints yet (the compaction planner pins by message index), so a transform writes what later handlers in the chain read

### Capability functions

| Function | Description |
|---|---|
| `mod_state_get(key)` | Read persistent KV; `()` when absent (pair with `??`) |
| `mod_state_set(key, value)` | Write persistent KV (atomic write, `~/.codesmith/mods-state/`) |
| `mod_log(msg)` | Log (target `codesmith_mods`) |
| `now_ms()` | Unix timestamp in milliseconds |
| `proceed / block / cancel / transform` | Hook control values |
| `ok(value) / err(msg)` | Tool-result constructors |
| `message(msg) / send(msg)` | Command output (display / feed the agent) |
| `register_provider(spec)` | Register a provider alias (see below) |
| `register_prompt_section(id, text)` | Append a named section to the base system prompt (see below) |
| `register_skill(spec)` | Contribute an in-memory skill to the session catalogue (see below) |
| `register_guard(callback)` | Register a deny-only tool-call guard (see below) |
| `replace_tool(spec, callback)` | Take over a registered tool's execution (see below) |
| `delegate(input?)` | Replacement-callback control: run the original implementation — `()` passes the input verbatim, a map rewrites it |
| `on_event(kind, callback)` | Register a general-agent-aligned hook — emit/serial events only (waterfall kinds fail the load; see above) |
| `register_message_projection(key, init, fold)` | Register a session-log fold the host maintains (see below) |
| `projection_state(key)` | Read this mod's projection state (inside hooks/tools) |
| `register_policy(kind, config)` | Override a host-mechanism policy (see below) |
| `disable_command(name)` | Suppress a built-in slash command (see below) |

### Policies (route C — tune host mechanisms)

```rhai
register_policy("shell-output", #{
    max_output_size: 20000,   // bytes before truncation
    head_bytes: 15000,        // kept head when truncated
    tail_bytes: 5000,         // kept tail when truncated
});
```

A policy is **data, not code**: `kind` names a host mechanism seam, `config`
is a map of its knobs; the mechanism (algorithm, spill writes, security
guards) stays in Rust. This is the "policy in script, effects in host"
split — the agent can read and retune these per workspace without a
recompile.

- Kinds: `output-diff` (`context_radius`), `shell-output`
  (`max_output_size`, `head_bytes`, `tail_bytes`, `summary_max_lines`,
  `summary_max_chars`, `preserved_summary_lines`, `preserved_line_bytes`,
  `preserved_total_bytes`), `compact-policy` (`cache_trigger_threshold`,
  `time_trigger_gap_secs`, `micro_compactable_tools`,
  `cleared_placeholder`, `keep_head_chars`, `keep_tail_chars` (dsh-style
  partial clears — the head/tail a cleared result keeps around the
  placeholder), `elide_uneventful`, `elide_placeholder` (omp-style
  per-turn elision: successful media-free results in completed rounds
  collapse on their presence alone; the live round stays byte-for-byte)
  — the compaction engine itself stays in Rust;
  the summarizer prompt is merged from several session sources and is
  not a single template, so it stays out of this seam),
  `large-output-route` (`threshold_tokens`, `per_tool` — the routing
  decision; the utility-LLM synthesis call stays in Rust. Explicit
  `[workshop]` TOML (`large_output_threshold_tokens` /
  `per_tool_thresholds`) sits above this kind: config values win, the
  script tier supplies the default when TOML is silent),
  `working-set` (`recent_read_file_limit`,
  `recent_read_file_snippet_chars` — the recent-read-files window the
  session retains so compacted-away reads stay available; the retention
  mechanics stay in Rust), `knowledge-on-demand`
  (`max_session_bytes`, `max_entrypoint_lines`, `max_entrypoint_bytes`,
  `enabled_types` — the surfaced-memory session budget, the MEMORY.md
  index budgets, and the memory-type subset eligible for surfacing;
  scanning, the relevance side-query and truncation stay in Rust.
  Budgets are bytes, not tokens. A relevance *threshold* is
  deliberately absent — the side-query picks filenames, it does not
  score them), `tool-output-gate` (`read_max_lines`,
  `read_max_bytes`, `read_hard_max_lines`, `grep_max_results` — the
  omp-style unified output tiers for the read/search surfaces; the gates
  themselves — UTF-8-safe slicing, continuation notices phrased as
  executable read-more instructions — stay in Rust).
- Resolution is **last-wins by load order** (builtin < global < project):
  a user mod overrides the builtin default by registering the same kind —
  deliberately unlike tools/commands, where the incumbent keeps the name.
- **Observability**: `/mods list` and `manage_mods action=list` end with a
  *resolved policies* section — each kind, its effective config, and the
  winning mod's id (builtin entries show their `builtin-*` id). Unloading
  or disabling the winning mod makes the section fall back live to the
  next winner, so the agent can verify its own policy overrides.
- Bad values (unknown field types, over-ceiling numbers, head+tail > max)
  warn and fall back to the compiled defaults at read time — a mod bug
  cannot brick the mechanism it tunes.
- **Builtin tier**: the defaults themselves ship as embedded Rhai mods
  (`builtin-output-diff`, `builtin-shell-output`, `builtin-compact-policy`,
  `builtin-large-output-route`, `builtin-working-set`,
  `builtin-knowledge-on-demand` — sources under
  `crates/extensions/src/script/builtin/`). They always load (even with
  `[mods] enabled = false`; the flag gates user mods only), skip
  consent/hash gates (they ship in the binary), use ephemeral KV, and are
  not listed by `/mods`'s mod list (they are not discoverable ids —
  override by registering the same policy kind, not by shadowing the id;
  their ids appear only as sources in the resolved-policies section of
  `list`).
- **Opt-in builtins**: behavior-changing builtin mods (a tool replacement,
  a loop, a compaction strategy) ship in the binary too, but load only
  when `[mods] builtin = ["<id>", …]` names them — that config key is the
  uniform "load by configuration" surface for every port that must not be
  always-on. Opt-in ids carry no `builtin-` prefix (the id is the
  user-facing selection key, shared with whatever config selects the
  behavior — e.g. `[compaction] strategy`). An id matching no opt-in
  builtin fails the startup audit loudly; an unselected opt-in mod stays
  compiled into the binary but never runs. A reload replays the same
  selection. Current opt-in ids:
  - `compact-pi`, `compact-omp`, `compact-minimax`, `compact-dsh` —
    compaction strategies ported from the five-agent comparison (values
    and provenance in each mod's header comment; select via
    `[compaction] strategy`, see "Writing a compaction strategy").
  - `grep-guard` — grep input normalization (empty-pattern refusal with
    a corrective action; include-glob comma splitting, brace groups
    preserved).
  - `edit-input-guard` — `edit_file` input guards: identical
    search/replace and disproportionate replacements refuse with the
    corrective next step.
  - `reject-templates` — deny-only guard over destructive shell
    patterns (`rm -rf /`, force-push, `reset --hard`) with corrective
    rejection templates.
  - `runaway-guard` — runaway detection (minimax `exact_action_repeat`,
    remind-strength): three consecutive identical tool calls get a
    denial naming the loop and the way out.
  - `goal-loop` — the dsh goal state machine as a loop script: phase
    (active/complete/blocked/paused) + revision counter in the mod KV,
    per-round `compact_now()`, fail-streak blocking (needs
    `turn_executor = "rhai"` + `[rhai_loop] mod = "goal-loop"`).
  - `verify-loop` — omp's diagnosticsOnWrite as a loop script: an edit
    batch is immediately followed by a `diagnostics` batch (same
    three-key enablement as `goal-loop`).

### Disabling a built-in slash command

```rhai
disable_command("model");
```

Disabling suppresses a **built-in** command's row — dispatch, `/help`, and
Tab completion all skip it — so a mod can retire a built-in without
shadowing it with a replacement. User commands and mod-registered commands
of the same name still dispatch (the user > mod > builtin layering is
unchanged). The host resolves the name against its builtin table (canonical
names and aliases); an unknown name warns and no-ops (`codesmith_mods`
target — the script-bug posture, same as a bad policy knob). Disables clear
with the generation's other bound registrations: `/mods reload`,
`disable`, and `remove` re-derive them from the surviving mods. The
config-side equivalent is `[commands] disabled = [...]` in config.toml
(unknown names there fail REPL startup loudly — user-authored config gets
the loud posture, script bugs get the warn).

### Session mods (ephemeral experiments)

```
manage_mods {"action": "session_write", "id": "session-shell-tuning",
             "source": "register_policy(\"shell-output\", #{ max_output_size: 20000 });"}
```

`manage_mods` `session_write` binds an agent-authored experiment live from
inline Rhai source — no files, no activation state, ephemeral KV (the
builtin tier's compile path with a runtime-provided source). The loop it
serves: try a mod/policy in this session, verify it via `list`, then
persist a proven experiment through `write` + `activate`.

- Ids must carry the `session-` prefix and must not collide with a
  discovered disk mod; a same-id rewrite is a no-residue replace.
- Consent: the first write of a given `(id, source)` asks for approval;
  an **identical re-write this session skips the prompt** (the
  session-scoped analogue of the activation hash) — any content change
  asks again. `session_remove` always asks.
- Position: session mods bind after the disk tiers, so their policy
  registrations win last-wins over everything (the resolved-policies
  section shows `from session-*`).
- Lifetime: gone when the session ends (nothing is persisted, not even
  KV). `/mods reload` and `manage_mods action=reload` replay them after
  the disk tiers (retained compiled forms, no recompile). Not restored on
  session resume. Their policies are engine-scoped: every turn of this
  engine — including its sub-agents' mechanisms — reads the same store,
  while another engine in the same process keeps its own set; the
  single-engine CLI registers its store process-wide, so scope-less
  utilities see it too; their tools/commands never enter
  sub-agent registries (built-in-only by design); the fork veto reaches
  every generation.
- The listing shows them as `session-* [session] — live (session-scoped,
  not persisted)`.

### Contributing a system-prompt section (route B)

```rhai
register_prompt_section("style", "Prefer small, reviewable diffs.");
```

Sections append to the base system prompt in registration order
(re-registering an id replaces its text). They register at mod load and
are stable for the session — prefix-cache friendly. Limits: ≤16 sections,
id must match `[a-zA-Z0-9_-]`, non-empty text (a violation fails the
mod's **load**). An explicit `before-agent-start` whole-prompt replacement
by any handler still wins over sections. Reload clears the generation's
sections.

### Contributing a skill (route B)

```rhai
register_skill(#{
    name: "commit-helper",
    description: "Write well-scoped commit messages",
    body: "# Steps\n1. Read the diff.\n2. Draft the message.",
    when_to_use: "the user asks for a commit",
});
```

Registered skills are in-memory catalogue entries (no `SKILL.md` on disk):
they appear in the system-prompt `## Skills` block and in `/skills`,
and `load_skill` resolves them by name — attributed to
the mod (`mod: <mod-id>`) instead of a file path. The filesystem catalogue
wins on a name collision (the registration is skipped with a warning in
`/skills`); a name already registered by a *different* mod fails the
mod's **load** (the error names the incumbent). Limits: ≤16 skills,
name must match `[a-zA-Z0-9_-]` (1-64 chars), non-empty description and
body. Registered skills carry no `paths`, so they never match the
conditional (working-set) skills block, and sub-agent sessions render no
skills catalogue at all. Reload clears the generation's registrations.

### Registering a guard (deny-only tool policy)

```rhai
register_guard(|e| {
    if e.name == "exec_shell" && e.input.command.contains("rm -rf") {
        "destructive command"
    }
});
```

A guard is a **deny-only** tool-call policy. The closure takes the
tool-call payload (`|e|` — same shape as a `tool-call` handler's payload);
returning a string denies the call with that reason, anything else
(including `()`) abstains. The callback must be a single-parameter function
defined in the mod itself — a `|e, ctx|`-arity callback or an unresolvable
name fails the mod's **load** (a guard that can never bind must not load
as a silent permanent bypass). There is no allow or transform vocabulary by
design: a denial maps to a chain-short-circuiting block, so neither
registration order nor another handler can resurrect a denied call, and
the blocked result is attributed (`guard (mod: <mod-id>): <reason>`). Use
`on("tool-call", ...)` when you need to rewrite inputs; use a guard when
you need a policy floor. A guard whose script errors abstains with a
`tracing` warn (same fail-open policy as handler errors). Guards evaluate
at the tool-call seam (before approval — a denied call is never dispatched,
so no approval path can flip it), gate sub-agent tool calls too (a child's
turn dispatches `tool-call` at its agent-type scope and script-origin
handlers ride along), and reload clears the generation's guards.

### Replacing a built-in tool (general-agent pattern d)

```rhai
replace_tool(#{
    name: "grep_files",              // the registered tool to take over (required)
    // description: "grep, capped",  // optional model-visible override
    // schema: #{ type: "object", ... },  // optional override — must be object-rooted
}, |input, ctx| {
    if input.query.contains("secret") { return err("not over secrets"); }
    delegate(#{ query: input.query, max_results: 50 })   // rewrite, then run the original
});
```

`register_tool` *adds* a new tool; `replace_tool` substitutes the
**execution** of one that is already registered. The callback receives
`(input, ctx)` (same shapes as a `register_tool` callback — ctx optional)
and decides per call:

| Callback returns | Effect |
|---|---|
| `ok(value)` / `err(msg)` / a bare value | substitute that result — the original never runs |
| `delegate()` | run the original with the (post-waterfall) input verbatim |
| `delegate(#{...})` | run the original with the rewritten input (validated against it first) |
| any other control (`block`, …) | an error result telling the model to use `ok`/`err`/`delegate` |

Properties and limits:

- **Safety metadata is not yours to change.** Capabilities, the approval
  ladder, destructiveness, path questions, and concurrency all stay the
  incumbent's — a replacement can never relax an approval gate, and a
  read-only surface (an explore child) still narrows by the incumbent's
  capabilities. Only `description` and `schema` may be overridden (the
  catalog changes with them; a replaced tool is always eagerly visible,
  never deferred behind tool search).
- **Composition order**: builtin → config `[tools]` override → mod
  replacements, later mods wrapping earlier ones — `delegate()` reaches
  whatever was the incumbent when the replacement was applied, so a chain
  composes and never loops. Two mods replacing one tool warn (both names)
  and chain; a `register_tool` + `replace_tool` on one name **in the same
  mod** fails the load.
- **A missing target is a warning, not an error** — surfaces are
  mode-dependent (Plan mode prunes write tools; an explore child is
  read-only), so the replacement simply skips there. Main-turn MCP tools
  are catalog-merged, not registry-resident, and cannot be replaced in v1.
- **Scoped surface**: replacements are global — every turn (main and
  sub-agent alike) resolves them at registry assembly, effective next
  turn; reload/deactivate restores the builtin the same way. The
  `tool-call` rewrite and `tool-result` transform waterfalls still run
  around a replaced tool (dispatch-level seams, unchanged), so the
  compose pattern is: `replace_tool` for execution policy + rewrite,
  `on("tool-result", ...)` for post-processing.
- A replacement runs your script synchronously (bounded by the mod engine
  op cap); `delegate` executes under the turn's normal cancellation.

### Registering a message projection (session log folds)

```rhai
register_message_projection("counts", #{ users: 0 }, |state, m| {
    if m.role == "user" { state.users = state.users + 1; }
    state
});
```

The host folds every transcript message through `fold(state, message)` —
appends fold incrementally, wholesale replacements (session reload,
compaction, `/edit` rollback) refold the whole log from `init`. Read the
state anywhere natives run (hooks, tools, commands) with
`projection_state(key)`. The state is never persisted as a snapshot: it is
always "the fold of the current transcript", so it survives session reload
by rebuild. This differs from `mod_state_get/set` (mod-written persistent
state): a projection is host-maintained and log-derived.

Limits: `message` is the wire-format message map (`role`, `content`
blocks); a fold error drops the projection for the session (logged, never
kills the turn); duplicate `key` within a mod fails the mod's **load**;
≤16 projections across all mods; after `/extension reload` the new
generation's states rebuild at the next turn start.

### Registering a provider (route A)

A mod can register a **provider alias** — a new provider id that delegates
to a builtin provider with its own `base_url` / `default_model` / `headers`
overrides. Scripts cannot implement an LLM client (no async/net by
design), so a mod's provider is always such an alias, e.g. onto an
OpenAI-compatible gateway:

```rhai
register_provider(#{
    id: "acme-gw",                        // new id; must not shadow a builtin
    kind: "openai",                       // builtin provider to delegate to
    base_url: "https://gw.example.test/v1",
    default_model: "acme-large",
    headers: #{ "X-Gateway": "acme" },    // optional
});
```

Validation fails the mod's **load** when the `id` shadows a builtin, the
`kind` is not a builtin, a header value is not a string, or the
`base_url` is neither `https://` nor a loopback `http://` endpoint (the
alias factory sends the user's api_key and full prompts to this URL —
plain-HTTP remotes are refused; local gateways such as Ollama, vLLM, or
LM Studio stay allowed) — a broken spec never reaches the client.
`api_key` is deliberately not accepted: secrets live in config, never
in scripts.

To use the alias, declare a matching `[[providers.custom]]` entry (same
`id`; it carries the API key) and select it with
`custom_provider = "acme-gw"` in config.toml. The alias's `base_url` /
`default_model` / `headers` overrides apply at client build, on top of the
entry's values. Registration is logged (target `codesmith_extensions`)
and takes effect at the next client resolution (new session, provider
switch) — a running session keeps its current client. Unloading the mod
or `/extension reload` removes the alias.

### Resource limits and error posture

Per script call: 200,000 operations, 64 call levels, 8 MiB strings / 100k array & map elements. On limit breach or script error: **hooks fail open** (`warn` + `Continue` — one broken mod cannot break the chain); tools/commands return a normal error to the model. No fs/net/process — the engine registers no I/O natives; absence is the sandbox.

One Rhai semantic worth knowing before a handler silently fails open: **a named top-level `fn` cannot see top-level `const`s** (its call state is fresh each call), while event callbacks can (closures capture them). Pass everything a named fn needs as a parameter — `fn record(lesson, max)` called as `record(lesson, MAX_REFLECTIONS)`, never `fn record(lesson)` reading `MAX_REFLECTIONS` inside.

A second, for dense loop scripts: **Rhai caps expression depth per body (default 32)** — a long doubly-nested block (a `for` inside a `for`, each statement a map-literal call) trips "Expression exceeds maximum complexity" at compile time. Hoist loop bodies into named functions (`fn search_round(frontier, …)`), which parse with a fresh budget.

## Example mods

The repository ships two behavior-plugin exemplars under [`examples/mods/`](../examples/mods/) — copy either into `~/.codesmith/mods/` (or the workspace's `.codesmith/mods/`) and activate it with `/mods activate <id>`. Both are covered by load-and-behave tests in `crates/extensions`, so they cannot rot silently.

- **`reflexion`** — Shinn et al. (2023) as a mod: failing tool results and `result-verified` `verified-fail` verdicts accumulate into a reflection ledger (the mod's KV), the ledger rides the next attempt as one injected user message (the `before-agent-start` waterfall), and the first clean turn expires it. Main-turn only (`ctx.agent.main` guard).
- **`voyager-capture`** — the capture half of Wang et al. (2023): a clean main-turn action sequence (≥ 3 successful action-class calls, zero failures) is distilled into a skill and stored in the mod's KV; every load re-registers the accumulated library via `register_skill`, so captured flows come back after `/mods reload`.
- **`react-min`** — a LOOP exemplar: the builtin ReAct shape as a ~20-line script. Activate it, set `turn_executor = "rhai"`, and the turn runs the script instead of the compiled loop — same streaming, approval and event discipline, because every `llm_request()` / `run_tools()` call executes the host's step services. The engine's equivalence test drives it against a mock provider.
- **`lats`** — a LOOP exemplar with **fan-out**: best-first tree search — `llm_request(#{prompt})` expansion and scoring, `spawn_child` candidate simulation, beam backtracking. Children run the session's loop too, so the script branches on `spawn_depth()` (searcher at 0, evaluator above). Knobs ride the `lats-policy` policy kind (`width`/`depth`/`beam`, read via `loop_policy_int`). See "Writing a loop script" for the tutorial.

## Writing a loop script

`register_loop("main")` declares the entry; `fn main() { ... }` runs once per
turn on a dedicated thread when `turn_executor = "rhai"` is selected. The
capability surface is a set of global functions the loop engine registers:

| Call | Returns / does |
|---|---|
| `llm_request()` | `#{text, tool_uses: [#{id, name, input}], stop_reason, usage: #{input_tokens, output_tokens}, interrupted}` — one LLM round: request assembly (three-zone, prefix-cache-observed), the `before-provider-headers` / `before-provider-request` waterfalls, streaming with transparent retry, transcript persistence. Queued steers are drained into the transcript ahead of the request (the builtin loop's top-of-step behavior). |
| `llm_request(#{prompt})` | Same round, but pushes `prompt` as a user message first (persisted — model-visible means logged). This is how a script distinguishes rounds by purpose — expand, score, answer — over one shared transcript. |
| `run_tools(calls)` | `#{halt, plan_approved, errors}` — one tool batch through approval, sandbox, guards, the `tool-call` / `tool-result` waterfalls and result persistence. `halt != ()` means the loop guard tripped — stop the turn. |
| `spawn_child(spec)` | `handle` (int) — spawn an in-process child turn from `#{prompt, type?, allowed_tools?}` through the same child-turn seam the CLI spawn path uses (`run_child_turn`: the session's loop runs in the child, `SessionBeforeFork` veto fires, the child inherits the parent's tool surface — narrowed when `allowed_tools` names a subset, unknown names fail loud). Ceilings error loudly: `[runtime] max_spawn_depth` (fan-out is **one level** in v1 — a child's own `spawn_child` is rejected) and `max_subagents` (live children per turn). |
| `eval_child(handle, timeout_ms)` | `#{status, final_text, steps}` — await a spawned child (`status` is `completed`/`cancelled`/`failed`; `final_text` is `()` unless completed). A timeout aborts the child and errors — an abandoned branch must not linger. |
| `spawn_depth()` | This turn's depth (`0` = the main turn) — the role switch for mods that both search and evaluate, since children run the same script. |
| `loop_policy_int(field, default)` | Read one `lats-policy` knob (`width`/`depth`/`beam`) — last-wins policy resolution with the script's compiled `default` as fallback, so a user mod's `register_policy("lats-policy", …)` retunes a running loop without a recompile. |
| `compact_now()` | `#{ran, messages}` — run the host's compaction step service on demand (micro-compact, the trigger gate — including any selected `[compaction] strategy` — the LLM summary). `ran == false` is a clean no-op (under threshold / disabled / breaker open), not an error. Unlike the builtin loop there is no automatic pre-request preflight: the script calls this where its shape needs one, typically before each `llm_request`. |
| `cancelled()` | `true` once the turn's cancel token fired — check it in long-running loops. |
| `note(text)` | Records a script decision on the session log path (live presentation and the persisted record agree). |

Error posture is **fail loud** (the opposite of a behavior hook): a script
error fails the turn. `[rhai_loop]` in `config.toml` pins the mod
(`mod = "react-min"`) and bounds one turn's script execution
(`timeout_secs`, default 3600). A script loop does not inherit the builtin
loop's LSP flush, capacity gates or the `turn-stopping`
steer seam — see the `rhai_executor` module's known limitations before
porting a loop that needs them.

### Tutorial: LATS — a searching loop

[`examples/mods/lats`](../examples/mods/lats/) is best-first tree search
(Zhou et al., 2023) as one loop script — the shape react-min lacks: the
script *drives* what gets asked next instead of reacting to one stream.
Four moves cover the whole pattern:

1. **Purpose-distinguished rounds.** Every `llm_request(#{prompt})` call
   appends its prompt to the transcript before the round, so expansion
   ("propose W refinements"), scoring and the final answer are separate
   model calls over one shared history — and every one of them is
   persisted (model-visible means logged).
2. **Fan-out.** `spawn_child(#{prompt, type})` starts a child turn
   through the same child-turn seam the host uses; `eval_child(handle,
   timeout_ms)` awaits it and returns `#{status, final_text, steps}`.
   Children run the *session's* loop — the same script — so:
3. **Role switch.** `if spawn_depth() > 0 { llm_request(); return; }`
   makes the child an evaluator: one round against its assignment, then
   done. Without this branch a searching script would search in its own
   children (and their spawns are rejected at the one-level v1 ceiling
   anyway).
4. **Backtrack.** The frontier is plain script state — scored children
   and their parents, pruned best-first to `beam` each round. Nothing in
   the transcript needs unwinding; the search narrative *is* the log.

Search knobs (`width`/`depth`/`beam`) ride the `lats-policy` policy
kind: the builtin tier ships defaults, `loop_policy_int("width", 3)`
reads the resolved value with the script's compiled fallback, and a user
mod's `register_policy("lats-policy", …)` retunes a running session —
data, not code, verified via `/mods list`'s resolved-policies section.

## Writing a compaction strategy

`register_compaction_strategy(name, spec)` makes a mod own the
compaction **decisions**; config `[compaction] strategy = "<name>"`
selects it (`"builtin"` — the default — keeps the compiled algorithm).
The seam splits cleanly:

- **Script-ownable (pure decisions over a host-built snapshot)**: *when*
  to compact (the `trigger` closure) and *how much survives* + summary
  directives (the `plan` closure).
- **Host-owned (the immune zone, not strategy-reachable)**: tool-call /
  tool-result pairing, the system-prompt prefix, append-only storage, the
  summary LLM call and its retries, the circuit breaker, the
  rapid-refill detector, the minimum-summarizable floor.

```rhai
register_compaction_strategy("compact-pi", #{
    // snapshot: #{ total_tokens, window, threshold, messages,
    //             rounds: [#{ start, len, tokens, has_tool_use, has_media }] }
    trigger: |snap| snap.total_tokens > snap.window - 16384,
    plan: |snap| #{
        keep_recent_tokens: 20000,        // OR keep_recent_rounds / keep_recent_messages
        summary_instructions: "…",        // optional, appended to the summary prompt
        summary_max_tokens: 16384,        // optional, overrides the output cap (256..=65536)
    },
});
```

Semantics worth knowing before porting a strategy from another agent:

- **Snapshot shape.** Token counts are host-precomputed; rounds are
  assistant-start groups (a round boundary is a pairing-safe cut point by
  construction). `window` is the resolved context window, `threshold`
  the configured token threshold.
- **Host gates still run.** `enabled`, the auto-compaction floor, the
  breaker, and the minimum message count are checked before/around the
  strategy — a strategy tightens timing, it cannot disable safety.
- **Plan interpretation is validated.** Exactly one of
  `keep_recent_rounds` / `keep_recent_tokens` / `keep_recent_messages`
  (> 0); a rejected plan logs loudly and the attempt uses the builtin
  boundary (the trigger already decided compaction is needed).
- **Round-aligned keeps.** `keep_recent_tokens` backward-accumulates
  *whole rounds* until the budget is met, then `enforce_tool_call_pairs`
  runs regardless — the immune zone holds even for a strategy that names
  a boundary splitting a pair.
- **Selection and failure posture.** The name resolves against loaded
  mods at compaction time (the strategy set is session state — mods load
  and reload around config, so unlike `turn_executor` this cannot be
  validated at config-load time). An unknown name is a loud per-attempt
  error that falls back to builtin: compaction is a safety mechanism, a
  config typo must not wedge it. Resolution is last-wins by load order —
  a user mod overrides a builtin strategy by reusing its name.
- **Scope (v1).** The strategy governs the auto-compaction path
  (`run_compaction`). Capacity interventions and recovery compaction keep
  builtin thresholds; the summary request *shape* (cache-aligned vs
  formatted) stays the host's. Known limitations are written down beside
  the seam (`crates/agent-runtime/src/compaction/strategy.rs`).

## mod.toml Fields

| Field | Required | Description |
|---|---|---|
| `id` | ✓ | Stable identity, `[a-zA-Z0-9._-]` — the directory name / state key |
| `version` | ✓ | Semantic version string (display only) |
| `name` | | Human-readable name, defaults to id |
| `description` | | One-liner shown at activation approval |
| `entry` | | Entry script path relative to the mod dir, defaults to `mod.rhai`; absolute paths and `..` are rejected |

Manifest validation is schema-aggregated: every field problem is reported
with its path in one error (e.g. `version: missing required field;
name: expected a string, got integer`), instead of failing on the first.
Unknown fields are ignored with a warning — forward compatibility, not a
rejection. A mod whose manifest fails validation is skipped at discovery
(warned in the log) and never reaches activation.

## Lifecycle & Security Model

- **First activation requires consent**: a newly discovered mod is skipped + passively announced as pending. Activation has exactly two paths: `/mods activate <id>` in the REPL, or approving a `manage_mods(action="activate")` tool call. The activation record persists (`~/.codesmith/mods_state.toml`); same-id reloads need no re-approval — **the consent is bound to the entry file's content hash**, so an activated mod whose `.rhai` later changes (git pull, an auto-approved edit) goes back to pending and asks for a fresh consent. **Why**: mods are in-process code that persists across sessions — a prompt injection could plant a resident hook unnoticed, and one approval must not become standing code-execution for that id; first-activation consent (plus re-consent on content change) is exactly the guard against that.
- **Single-mod load/unload**: `activate` / `enable` compile + bind just that mod; `disable` / `remove` evict only its compiled form and re-register the survivors from their retained compiled ASTs — no recompile of other mods, no whole-layer reload. Each transition bumps the runner generation once, and registration changes still take effect at the next turn boundary (the `tools-change` discipline is unchanged). `/mods reload` (and `manage_mods(action="reload")`) stays the whole-layer path: re-discovery, re-consent checks, fresh compiles. Unload reclaims the compiled engine + AST only — the mod's KV file persists by design (it is the mod's opt-in store).
- **Let the agent write mods for you**: just ask ("write me a mod that blocks git push") — the model writes files via the `manage_mods` tool (`write`) and requests your approval to activate (`activate`).
- **Mod writes follow workspace trust, both roots**: an untrusted workspace discovers no project mods, and `manage_mods` refuses to write mods into it — project root *and* the global `~/.codesmith/mods` (a global write would install code that loads in every workspace, including other untrusted ones, so the trust signal must not be bypassable via the `global` flag). Trust the workspace first, then write.
- **Known boundaries**: mod (script) tools and compiled-in Rust-extension tools mount in sub-agents too (5b); dylib-origin Rust-extension tools stay main-turn only (unload safety). Network install sources (git clone into the mods dir) are out of MVP scope.

## Configuration

The `[mods]` section of `config.toml` (defaults to true):

```toml
[mods]
enabled = true   # master switch: discovery, manage_mods tool
```

The `watch` hot-reload file watcher was removed in 0.10.0; use
`manage_mods(action="reload")` to pick up edits.

## Implementation Index

| Component | Location |
|---|---|
| `ModManifest` / `discover_mods` / trust gate | `crates/extensions/src/script/mod_manifest.rs` |
| `RhaiMod` (Extension impl, native registration, event mapping) | `crates/extensions/src/script/rhai_mod.rs` |
| `ScriptHandler` / `ScriptToolDefinition` / `ScriptToolReplacement` / `ScriptCommandDefinition` | `crates/extensions/src/script/adapters.rs` |
| `ToolReplacement` seam + `ReplacementOutcome` | `crates/agent/src/extension.rs` |
| `ReplacementToolSpec` + registry apply (main / sub-agent) | `crates/agent-runtime/src/tools/replacement.rs` |
| `ModKvStore` (per-mod persistent KV) | `crates/extensions/src/script/kv.rs` |
| `ModStateStore` (activation/disablement state) | `crates/cli/src/mod_state.rs` |
| Shared ops layer (single impl behind the manage_mods tool) | `crates/cli/src/mod_ops.rs` |
| Assembly/gating (populate returns the pending report) | `crates/cli/src/core/engine.rs` |
| `ManageModsTool` (model-visible) | `crates/cli/src/tools/mods.rs` |
| `/mods` REPL command | `crates/cli/src/interactive/commands/mods_cmds.rs` |

## Further Reading

- [EXTENSIONS.md](EXTENSIONS.md) — Rust extensions (dylib / compiled-in): the full-capability form of the same `Extension` contract
- [HOOKS.md](HOOKS.md) — lifecycle hooks as shell commands (out-of-process; complements the in-process hooks on this page)
