# CodeSmith Mods — the Rhai Script Mod Layer

Mods extend CodeSmith with [Rhai](https://rhai.rs) scripts — no Rust compilation required. A few lines of script add **event hooks** (intercept/rewrite tool calls, input, provider requests), register **model-visible tools**, register **slash commands**, and keep state across sessions in a **KV store**. Activate a mod and it works in the current session; save a change and it hot-reloads.

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

In CodeSmith, type:

```
/mods activate hello
```

**What you'll see**: this is the **one-time consent** for this id (a mod is in-process code that persists across sessions — first activation requires your approval). After approval the mod is live immediately; same-id reloads (including hot reloads) never ask again. Any shell call containing `rm -rf` is now blocked, and the model receives your reason and adapts.

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

### Step 4: Register a slash command, watch the hot reload

Add a slash command (backtick strings support `${...}` interpolation):

```rhai
register_command("calls", "Show the tool-call count", |args, ctx| {
    message(`${mod_state_get("calls") ?? 0} tool calls so far`);
});
```

**What you'll see**:

- Typing `/calls` shows the count — `message(...)` displays to the user (use `send(...)` to feed the agent conversation instead)
- Now **edit `mod.rhai` and save** — the watcher monitors both mods roots (500ms debounce + 1s cooldown) and hot-reloads after the quiet window, no re-approval for the same id. `/mods status` confirms which mods are loaded and the runner generation

You've now touched all four registration surfaces: event hooks, tools, commands, KV. Full `/mods` command set: `list` / `status` / `info <id>` / `activate <id>` / `enable|disable <id>` / `remove <id>` / `reload`.

## Quick Reference

### Event hooks

`on("<event>", |e, ctx| { ... })` — `e` is the event payload map (every payload carries `kind`), `ctx` is `#{cwd, mode, idle, generation}`; **ctx is optional** (`|e|` works). Event names are the kebab-case spelling of all 23 `ExtensionEventKind` variants:

| Event | Payload fields |
|---|---|
| `input` | `text` |
| `before-agent-start` | `system_prompt`, `inject_message` (`()` = unset) |
| `before-provider-request` | `messages` (JSON value) |
| `after-provider-response` | `response` (JSON value) |
| `tool-call` | `id`, `name`, `input` (JSON value) |
| `tool-result` | `id`, `name`, `content`, `success`, `is_error` |
| `turn-start` / `turn-end` | `turn_id`; `turn-end` also `reason` |
| `assistant-stream` | `text` (one incremental chunk; fires per text delta, observe-only) |
| `tool-execution-update` | `id`, `name`, `message` |
| `project-trust` / `session-start` / `resources-discover` | `reason` |
| `agent-start` / `before-provider-headers` / `tool-execution-start` / `tool-execution-end` / `agent-end` / `agent-settled` / `session-before-switch` / `session-before-fork` / `session-shutdown` / `session-before-compact` / `session-compact` | (`kind` only) |

Unwired events (host seam not yet connected) never fire: `tool-execution-update`, `resources-discover`, `session-before-fork`.

Dispatch modes are part of each event's contract (`ExtensionEventKind::dispatch_mode`,
checked by a contract test):

- **transform-chain**: `input`, `before-agent-start`, `before-provider-request`, `tool-result` — each transform folds in, the next handler sees it, the final field applies
- **transform + deny**: `tool-call` — rewrite the call's `input` (the rewritten input is what approval gates and what runs, and it is the recorded input) or `block(reason)` to deny (monotonic — no later handler can flip it)
- **cancel veto**: `session-before-switch`, `session-before-fork`, `session-before-compact`
- **observe** (outcomes advisory): everything else, including `assistant-stream` (fires per text delta — keep handlers cheap)

### Hook return value → HandlerOutcome

| Script returns | HandlerOutcome | Effective seams |
|---|---|---|
| `proceed()` or `()` | `Continue` | all |
| `block(reason)` | `Block` (ignored at non-block seams) | `tool-call` |
| `cancel(reason)` | `Cancel` (ignored at non-cancel seams) | `session-before-*` |
| `transform(#{...})` | `Transform` (merges mutable fields, chain continues) | see below |

Transform mutable fields (one handler's rewrite is immediately visible to the next):

- `input`: `text`
- `tool-call`: `input` (JSON value — the pre-execute rewrite; the host applies it before approval + execution and records it)
- `before-agent-start`: `system_prompt`, `inject_message` (string = set, `()` = clear, absent = keep)
- `before-provider-request`: `messages`
- `tool-result`: `content`, `success`, `is_error`

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
| `register_message_projection(key, init, fold)` | Register a session-log fold the host maintains (see below) |
| `projection_state(key)` | Read this mod's projection state (inside hooks/tools) |

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
`kind` is not a builtin, or a header value is not a string — a broken
spec never reaches the client. `api_key` is deliberately not accepted:
secrets live in config, never in scripts.

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

- **First activation requires consent**: a newly discovered mod is skipped + passively announced as pending. Activation has exactly two paths: `/mods activate <id>`, or approving a `manage_mods(action="activate")` tool call. The activation record persists (`~/.codesmith/mods_state.toml`); same-id reloads need no re-approval. **Why**: mods are in-process code that persists across sessions — a prompt injection could plant a resident hook unnoticed; first-activation consent is exactly the guard against that.
- **Let the agent write mods for you**: just ask ("write me a mod that blocks git push") — the model writes files via the `manage_mods` tool (`write`) and requests your approval to activate (`activate`).
- **Project mods** follow workspace trust: an untrusted workspace discovers nothing; `manage_mods` likewise refuses to write project mods into an untrusted workspace.
- **Known boundaries**: mod tools, like Rust-extension tools, are main-turn only (sub-agents structurally never see them); network install sources (git clone into the mods dir) are out of MVP scope.

## Configuration

The `[mods]` section of `config.toml` (both default to true):

```toml
[mods]
enabled = true   # master switch: discovery, manage_mods tool, watcher
watch = true     # file watcher only (500ms debounce + 1s cooldown)
```

## Implementation Index

| Component | Location |
|---|---|
| `ModManifest` / `discover_mods` / trust gate | `crates/extensions/src/script/mod_manifest.rs` |
| `RhaiMod` (Extension impl, native registration, event mapping) | `crates/extensions/src/script/rhai_mod.rs` |
| `ScriptHandler` / `ScriptToolDefinition` / `ScriptCommandDefinition` | `crates/extensions/src/script/adapters.rs` |
| `ModKvStore` (per-mod persistent KV) | `crates/extensions/src/script/kv.rs` |
| `ModStateStore` (activation/disablement state) | `crates/tui/src/mod_state.rs` |
| Shared ops layer (single impl behind /mods & manage_mods, watcher) | `crates/tui/src/mod_ops.rs` |
| Assembly/gating (populate returns the pending report) | `crates/tui/src/core/engine.rs` |
| `ManageModsTool` (model-visible) | `crates/tui/src/tools/mods.rs` |
| `/mods` commands | `crates/tui/src/commands/mod_commands.rs` |

## Further Reading

- [EXTENSIONS.md](EXTENSIONS.md) — Rust extensions (dylib / compiled-in): the full-capability form of the same `Extension` contract
- [HOOKS.md](HOOKS.md) — lifecycle hooks as shell commands (out-of-process; complements the in-process hooks on this page)
