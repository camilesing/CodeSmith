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
| Slash commands | ✓ `register_command` (shadows built-ins) / `disable_command` (retires one) | — | ✓ | layering user > mod > builtin |
| Providers | alias only (`register_provider`) | — | ✓ full `ProviderFactory` | a script cannot implement `LlmClient` (no async/net) |
| Prompt sections, skills | ✓ `register_prompt_section` / `register_skill` | ✓ (defaults) | ✓ | prefix-cache discipline applies |
| Policies (host-mechanism knobs) | ✓ `register_policy` (last-wins) | ✓ — the defaults ship as builtin mods | ✓ | data, not code; `/mods list` shows resolution |
| Turn loop, compaction algorithm, `BASE_PROMPT`, sub-agent tool registries | — | — | — | core by declared contract (one turn loop, sole base prompt, built-in-only sub-agent registries) |

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
- Now **edit `mod.rhai`, save, and reload** with `/mods reload`; it re-runs discovery, no re-approval for the same id. `/mods status` confirms which mods are loaded and the runner generation (`/mods list` shows per-mod activation state and the resolved policy layer; the startup notice also lists discovered-but-not-activated mods). (The automatic file watcher and its `[mods] watch` switch were removed in 0.9.0; reloading is explicit.)

You've now touched all four registration surfaces: event hooks, tools, commands, KV. The full `/mods` command set is `list` / `status` / `info <id>` / `activate <id>` / `enable|disable <id>` / `remove <id>` / `reload`; the model-facing `manage_mods` tool mirrors it with actions `list` / `write` / `activate` / `disable` / `enable` / `remove` / `reload`, plus the session lane `session_write` / `session_remove` (ephemeral experiments — see below).

## Quick Reference

### Event hooks

`on("<event>", |e, ctx| { ... })` — `e` is the event payload map (every payload carries `kind`), `ctx` is `#{cwd, mode, idle, generation}` (`mode` is `"tui"` for the interactive terminal REPL — the retired frontend's name, kept for compatibility — or `"rpc"` / `"json"` / `"print"`); **ctx is optional** (`|e|` works). Event names are the kebab-case spelling of all 26 `ExtensionEventKind` variants:

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
| `turn-start` / `turn-end` | `turn_id`; `turn-end` also `reason` |
| `turn-stopping` | `reason` (why the turn would end — `no-tool-calls` at the only emit site); fires just before the turn ends on no tool calls; `cancel("note")` steers — the note is appended as a user message and the turn resumes (capped at 3 steers per turn) |
| `assistant-stream` | `text` (one incremental chunk; fires per text delta, emit-only) |
| `tool-execution-update` | `id`, `name`, `message` |
| `tools-change` | `added`, `removed` (tool-name arrays) |
| `project-trust` / `session-start` | `reason` |
| `agent-start` / `tool-execution-start` / `tool-execution-end` / `agent-end` / `agent-settled` / `session-before-switch` / `session-before-fork` / `session-shutdown` / `session-before-compact` / `session-compact` | (`kind` only) |

`tool-execution-update` fires from the shell tools' own poll loops while a command keeps
running (`exec_shell` foreground and `exec_shell_wait`): at most once per second and only
when new output arrived. `id` is the shell task id (a tool does not know its wire tool-call
id — correlate by `name` + ordering); `message` is the output since the previous checkpoint
(tail-capped at 4 KiB; a truncated manager excerpt yields its tail). Emit-only, main turns
only (the sub-agent progress sink stays unwired — 5b gave sub-agent loops
`tool-call`/`tool-result` only); sandboxed-backend and interactive runs
report no checkpoints. The same checkpoint reaches the UI as `ToolCallProgress`.
`resources-discover` was removed in 0.11.0 (never had an emitter or a consumer).
`session-before-fork` fires at
every spawn — the spawn-family tools (`agent_open` / `agent_spawn` / `subagent_run` /
`delegate_to_agent`) at every depth (the runner rides the sub-agent runtime, so
grandchildren forks are vetoable too) and the engine-internal spawn path — before the child
forks session state; `cancel()` vetoes the fork. Since 5b the runner is more than the veto
carrier on the child side: sub-agent registries mount the child type's extension-tool view
(script/compiled-in origins only — a mod's guard therefore also gates sub-agent tool calls),
and sub-agent loops dispatch `tool-call`/`tool-result` at their agent-type scope.

`tool-result` payloads carry the canonical/rendered separation: `content` is the model-visible rendering; `canonical` is the structured machine-readable value of the same result (the `ToolResult::json` value, or what a tool attached via `with_canonical`). Read `canonical` instead of re-parsing `content` — prose-rendered results (`todo_add` prefixes a sentence before the JSON) are not parseable at all. Canonical is live-process only: the transcript persists the rendered `content`.

`tools-change` fires at the turn boundary when the compiled model-visible catalog changed since the previous main turn (mode switch, mod reload, selection change). The first turn establishes the baseline silently; sub-agent toolsets never trigger it, and there is no API to read the current catalog — the event carries the diff only. `/tools` renders the current baseline grouped by origin (builtin / plugin / extension / mcp).

Dispatch modes are part of each event's contract (`ExtensionEventKind::dispatch_mode`,
checked by a contract test) — three states named after the DSH (deepseek-harness)
model: **waterfall**, **serial**, **emit**:

- **waterfall**: `input`, `before-agent-start`, `before-provider-request`, `tool-result` — each transform folds in, the next handler sees it, the final field applies
- **waterfall + deny**: `tool-call` — rewrite the call's `input` (the rewritten input is what approval gates and what runs, and it is the recorded input) or `block(reason)` to deny (monotonic — no later handler can flip it); `provider-request-error` — rewrite the retry `plan` or `block(reason)` to give up
- **serial**: `session-before-switch`, `session-before-fork`, `session-before-compact`, `turn-stopping` (inverted: `cancel(reason)` continues the turn — the note steers it)
- **emit** (outcomes advisory): everything else, including `assistant-stream` (fires per text delta — keep handlers cheap) and `tools-change` (catalog diff at the turn-dispatch chokepoint)

#### `on_event` — the DSH-aligned listener/veto form

`on_event("<event>", |e, ctx| { ... })` takes the same callback shape as
`on` (same payloads, same optional `ctx`, same control values) but accepts
only **emit** and **serial** events — the DSH-portable subset. A waterfall
event (`input`, `before-agent-start`, `before-provider-request`,
`tool-result`, `tool-call`, `provider-request-error`) fails the mod's
**load** with an error naming the kind: its transform chain belongs to
`on()`, its deny floor to `register_guard`. At emit kinds every return
value is advisory; at serial kinds `cancel(reason)` is the veto. Use
`on_event` when a mod only listens or vetoes — such a mod maps 1:1 onto
DSH's emit/serial model; use `on()` for the full CodeSmith surface
(waterfall transforms included — a deliberate superset beyond the DSH
script surface).

Scope: `on_event` registrations are global-scope, script-origin — a
global registration covers every dispatch scope, but sub-agent loops
(5b) dispatch only `tool-call`/`tool-result` at their scope (both
waterfall, both rejected here), so in practice an `on_event` callback
observes main-turn events. There is no `on_event_scoped` — scripts do
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

- `input`: `text`
- `tool-call`: `input` (JSON value — the pre-execute rewrite; the host applies it before approval + execution and records it)
- `before-agent-start`: `system_prompt`, `inject_message` (string = set, `()` = clear, absent = keep)
- `before-provider-request`: `messages`
- `tool-result`: `content`, `success`, `is_error`, `canonical` (JSON value — the machine-readable result; rewriting `content` alone leaves it untouched, and it can be replaced explicitly)
- `provider-request-error`: `retry` (bool — `false` gives up), `delay_ms` (int; kept from the current plan when absent; forced retries are capped engine-side)

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
| `on_event(kind, callback)` | Register a DSH-aligned hook — emit/serial events only (waterfall kinds fail the load; see above) |
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
  `cleared_placeholder` — the compaction engine itself stays in Rust;
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
  mechanics stay in Rust).
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
  `builtin-large-output-route`, `builtin-working-set` — sources under
  `crates/extensions/src/script/builtin/`). They always load (even with
  `[mods] enabled = false`; the flag gates user mods only), skip
  consent/hash gates (they ship in the binary), use ephemeral KV, and are
  not listed by `/mods`'s mod list (they are not discoverable ids —
  override by registering the same policy kind, not by shadowing the id;
  their ids appear only as sources in the resolved-policies section of
  `list`).

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
(including `()`) abstains. There is no allow or transform vocabulary by
design: a denial maps to a chain-short-circuiting block, so neither
registration order nor another handler can resurrect a denied call, and
the blocked result is attributed (`guard (mod: <mod-id>): <reason>`). Use
`on("tool-call", ...)` when you need to rewrite inputs; use a guard when
you need a policy floor. A guard whose script errors abstains with a
`tracing` warn (same fail-open policy as handler errors). Guards evaluate
at the tool-call seam (before approval — a denied call is never dispatched,
so no approval path can flip it), gate sub-agent tool calls too (5b: child
loops dispatch `tool-call` at their agent-type scope and script-origin
handlers ride along), and reload clears the generation's guards.

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

The `watch` hot-reload file watcher was removed in 0.9.0; use
`manage_mods(action="reload")` to pick up edits.

## Implementation Index

| Component | Location |
|---|---|
| `ModManifest` / `discover_mods` / trust gate | `crates/extensions/src/script/mod_manifest.rs` |
| `RhaiMod` (Extension impl, native registration, event mapping) | `crates/extensions/src/script/rhai_mod.rs` |
| `ScriptHandler` / `ScriptToolDefinition` / `ScriptCommandDefinition` | `crates/extensions/src/script/adapters.rs` |
| `ModKvStore` (per-mod persistent KV) | `crates/extensions/src/script/kv.rs` |
| `ModStateStore` (activation/disablement state) | `crates/cli/src/mod_state.rs` |
| Shared ops layer (single impl behind the manage_mods tool) | `crates/cli/src/mod_ops.rs` |
| Assembly/gating (populate returns the pending report) | `crates/cli/src/core/engine.rs` |
| `ManageModsTool` (model-visible) | `crates/cli/src/tools/mods.rs` |
| `/mods` REPL command | `crates/cli/src/interactive/commands/mods_cmds.rs` |

## Further Reading

- [EXTENSIONS.md](EXTENSIONS.md) — Rust extensions (dylib / compiled-in): the full-capability form of the same `Extension` contract
- [HOOKS.md](HOOKS.md) — lifecycle hooks as shell commands (out-of-process; complements the in-process hooks on this page)
