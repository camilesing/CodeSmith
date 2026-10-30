# Extensions

CodeSmith extensions are modules — compiled into the binary or loaded as
dylibs from an install root — that contribute **tools**, **slash
commands**, and **lifecycle event handlers** to the agent loop, plus
**providers**, **prompt sections**, **skills**, **message projections**,
and **tool guards** through the same registration surface. An extension
is a factory (`impl Extension`) that, during `configure`, registers its
contributions against an `ExtensionApi`.

> **Script Mods.** The same `Extension` contract is also implemented by
> Rhai-script **Mods** (`~/.codesmith/mods/<id>/mod.toml` + `mod.rhai`) —
> hooks, tools, slash commands and persistent KV without compiling Rust,
> first-activation-gated and hot-reloaded. They ride the exact runner /
> seam / reload machinery documented here. Author guide:
> [MODS.md](MODS.md) — including the per-surface map of which tier
> (Rhai mod / builtin Rhai / Rust extension) fits which contribution.

The host assembles the extension runtime at engine build
(`build_extension_runtime()`, `crates/cli/src/core/engine.rs`) and again
on every runtime reload (`manage_mods` tool action `"reload"`):
discover compiled-in registrations
(`inventory`) + dylib sources (install roots) → reconcile with the
on-disk `ExtensionStateStore` (skip disabled) → load + configure each
against a stub api → `bind_core` the host context — after which the
runner fans lifecycle events to registered handlers.

Extension tools are wired live into the host per turn: they register
into the per-turn `ToolRegistry` via `register_extension_tools` in
`EngineHost::build_turn_dispatcher`, so the agent loop sees them as
normal `ToolSpec`s (origin-tiered for sub-agents — see
[Sandbox stance](#sandbox-stance)). Extension/mod **slash commands dispatch
in the REPL**: `ExtensionRunner::try_dispatch_command`
(`crates/extensions/src/runner.rs`) is called between user commands and
the built-in static table — mod commands shadow built-ins, user commands
shadow mod commands.

Every populate/reload pass also collects one structured audit entry per
discovered extension/mod — `Loaded` / `Failed { original error }` /
`PendingConsent` / `Disabled` / `TrustGated` — surfaced as a passive
startup notice, in the reload message, and as a `tracing` summary when
anything failed. mod.toml validation is schema-aggregated with
path-tagged errors.

## Registration surface

Everything an extension can register, and what the host does with it:

- **Tools + slash commands** — `register_tool(Box<dyn ToolDefinition>)` /
  `register_command(Box<dyn CommandDefinition>)`; wired live per turn
  (main-turn only) as described above.
- **Builtin-command disables (`disable_command`)** — suppress a built-in
  slash command's row (dispatch, `/help`, and Tab completion skip it)
  without providing a replacement; user commands and mod-registered
  commands of the same name still dispatch (user > mod > builtin). The
  framework stores names opaquely — the host resolves them against its
  builtin table and warns on unknowns. Cleared with the generation's other
  bound registrations on reload.
- **Providers (Rust shape: `register_provider`)** — a full
  `Arc<dyn ProviderFactory>` flushed into the
  `codesmith_agent::provider::SharedProviderRegistry` the host resolves
  clients through. Registration is logged (target `codesmith_extensions`)
  and takes effect at the next client resolution — an already-built
  client is never hot-swapped. Unregistration is symmetric via the
  `ProviderRegistration` drop guard; reload drops the generation's
  guards.
- **Provider aliases (script shape: `register_provider_alias`)** — a
  declarative alias onto a builtin provider (new custom id + optional
  `base_url` / `default_model` / `http_headers` overrides); the Rhai
  `register_provider(spec)` native. Rhai mods cannot implement
  `LlmClient` (no async/net by design), so this is their only provider
  contribution.
- **Prompt sections (`register_prompt_section`)** — named, session-stable
  sections appended to the base system prompt (≤16 sections, validated
  at load). The host appends them each turn **before** the
  `BeforeAgentStart` seam, so an explicit whole-prompt replacement by a
  handler still wins. Reload clears the generation's sections
  (prefix-cache discipline).
- **Message projections (`register_message_projection`)** — a fold the
  host maintains over the session transcript (Rhai shape:
  `register_message_projection(key, init, fold)` + `projection_state(key)`
  reads). Appends fold incrementally; wholesale replacements
  (reload-from-log, compaction, `/edit` rollback) refold from scratch.
  The state is never snapshotted — it is rebuilt from the log.
- **Skills (`register_skill`)** — an in-memory skill in the session
  catalogue (Rhai shape: `register_skill(spec)` with `name` /
  `description` / `body` / optional `when_to_use`): system-prompt
  `## Skills` block, `/skills`, and `load_skill`
  by name, attributed `mod: <owner>`. The filesystem catalogue wins on
  a name collision; a name owned by a different mod fails the load;
  ≤16 skills; reload clears the generation's registrations. Registered
  skills carry no `paths` (no conditional matching) and sub-agents
  render no catalogue.
- **Tool guards (`GuardHandler`; Rhai shape `register_guard(callback)`)** —
  a deny-only closure wrapped over the `ToolCall` seam: returning a
  string denies the call (attributed `guard (mod: <id>)`), anything else
  abstains. There is no allow or transform vocabulary, so a denial is
  monotonic by construction (block short-circuits the chain). Guards
  evaluate pre-approval at the seam and gate sub-agent tool calls too
  (5b: child loops dispatch `ToolCall` at their agent-type scope;
  script-origin handlers ride along); a script error abstains with a warn.
- **Event handlers (`on` / `on_variant`; Rhai shapes: `on(event, cb)` for
  any kind, `on_event(kind, cb)` for the DSH-aligned emit/serial subset —
  a waterfall kind fails the mod's load)** — see
  [Handlers](#handlers-outcomes--per-variant-subscription).

### Tool catalog: `ToolsChange` + the capability manifest

The turn dispatcher diffs the compiled model-visible catalog against the
previous main-turn baseline (origin-classified snapshot in
`cli/src/core/tool_catalog.rs`) and emits `ToolsChange { added, removed }`
(emit-only); `/tools` renders the baseline grouped by origin. The
first build establishes the baseline silently; sub-agent toolsets build
on a separate path (`spawn_subagent`) and never flap the baseline.

Session-level selection lives in the capability manifest
(`~/.codesmith/capabilities.toml`, `[tools] disabled = [...]`; env
override `CODESMITH_CAPABILITIES_MANIFEST`). A disabled tool is removed
at the composition point (`EngineConfig.disabled_tools`) — neither
visible nor executable on the main turn or in any sub-agent, at any
spawn depth. Turn-scoped masks (preset `tools.include`/`exclude`,
slash-command frontmatter, per-turn `allowed_tools`/`blocked_tools`)
compose on top and cannot resurrect a disabled tool. The file is read
once per process; a malformed manifest fails the engine build loudly.
The legacy config.toml `[tools].overrides <name> = { type = "disabled" }`
shape still parses (external config contract) and is honored — unioned
into the effective set with a deprecation warning.

## Bootstrap

Compiled-in extensions register via
[`inventory::submit!`](https://docs.rs/inventory). A `pub mod <name>;`
declaration in `crates/extensions/src/lib.rs` is all that's required for
discovery — no runtime registration call. The host's
`build_extension_runtime()` calls `codesmith_extensions::discover_static()`
once at engine build.

## In-REPL Manager

The `/extension` command group (`crates/cli/src/interactive/commands/extension_cmds.rs`)
is the user-facing surface:

| Subcommand | Effect |
|---|---|
| `/extension list` | Lists compiled-in + installed extensions (id, version, scope, enabled, install provenance). |
| `/extension info <id>` | Shows metadata for one extension (version, source, dylib path, config, provenance, enabled). |
| `/extension enable <id>` | Marks it enabled in `extensions_state.toml`; takes effect on the next `/extension reload`. |
| `/extension disable <id>` | Marks it disabled; same reload caveat. |
| `/extension status` | Reports the bound runner's generation + bound tool/command counts (plus failed loads from the startup audit). |
| `/extension reload` | Re-populates the shared runner `Arc` — the same chokepoint the `manage_mods` tool uses (see below). |
| `/extension install <source> [--global]` | Fetches (`git:` / `path:` / `crate:` / `prebuilt:`) → builds (`cargo build` for source kinds) → places under `<root>/<id>/`, writes `extension.toml` + the sha256 sidecar the loader verifies, and records `installed[]` provenance. Default scope is the project root; `--global` opts into `~/.codesmith/extensions`. A project-scope install into an **untrusted** workspace prints a warning (the discovery-side gate still refuses to load it — `/trust add <path>` to trust the workspace). `/extension reload` loads it. |
| `/extension uninstall <id>` | Removes `<root>/<id>/` and clears the provenance record; bindings clear on the next reload, the dylib unloads at the next turn boundary. |

`prebuilt:<https-url>` is HTTPS-only and accepts an optional
`--checksum <sha256>` (a mismatch refuses the install).

Runtime reload mechanics (shared by `/extension reload`, `/mods reload`,
and the `manage_mods` tool — `crates/cli/src/mod_ops.rs` →
`reload_extension_runtime`, `crates/cli/src/core/engine.rs`):

- It re-populates the **shared runner `Arc`**, and first drains the
  runtime's in-flight sub-agent turns (structural ownership, DSH
  alignment — `agent-runtime/src/turn_lease.rs`): each lease gets a
  reason-tagged cancel (`extension reload`, the DSH `disposed` analog),
  deepest spawn depth first, and a bounded wait (`RELOAD_DRAIN_TIMEOUT`,
  5s) for the runs to settle; a lease that outlives the bound is
  reported in the reload message and the reload continues (children hold
  no dylib handles, so the unload below stays safe). The main turn
  (depth 0) is never drained — from the REPL no main turn is in flight,
  and from the `manage_mods` tool the main turn is the reload's
  initiator (the DSH self-lineage exclusion; draining it would be a
  self-wait). A turn-executor id with in-flight leases also refuses
  replacement (`TurnExecutorRegistry::register` fails loud with the
  count). Then: the runner's clear suite
  (`clear_bound_registrations`: handlers, tools, commands, disabled
  builtin commands, providers (drops registration guards), prompt
  sections, skills, message projections, policies) →
  `drain_libraries_to_pending` →
  `invalidate` (bump generation) → discover (static + dylib) → reconcile
  against state → `load` each → `bind_core` (fresh
  `HostExtensionContext`). Both `App.extension_runner` and the engine's
  field update live (no `Arc` swap — they share the one the engine
  built). The drained `Library`s are dropped (`drop_pending`) at the
  next engine op-loop top (turn boundary). A handler bound before
  reload stops observing after (cleared, not duplicated); a
  newly-installed extension is picked up on the next reload.
- **Install roots + trust gate** — unchanged: dylib installs live under
  the roots documented in [Discovery](#discovery), and project-local
  dylibs stay gated on workspace trust (`apply_trust_gate`) at load
  time — install is trust-agnostic by design.

## Discovery

- **Static (compiled-in):** an extension registers an
  `ExtensionRegistration { factory, metadata }` via `inventory::submit!`;
  `discover_static()` collects every registration linked into the
  binary. The in-tree `scratchpad` sample is the reference registration.
- **Dylib (install root):** `discover_dylib(&global_roots,
  &project_roots)` walks the install roots (`~/.codesmith/extensions`
  global, `.codesmith/extensions` project-local). Each root may be a
  container of extension subdirectories, a single manifest directory
  (containing `extension.toml`), or a bare `.dylib`/`.so`/`.dll` file;
  sources dedup by canonicalized dylib path. Loading goes through
  `libloading` with a lockstep `*mut dyn Extension` handoff
  (`codesmith_register_extension`), and the loader verifies the sha256
  sidecar written at install. A project-local trust gate
  (`apply_trust_gate`) drops project-root dylibs while the workspace is
  untrusted; the `ProjectTrust { FirstLoad }` event flips that trust at
  onboarding acceptance.

## Minimal Example

The in-tree `scratchpad` extension
(`crates/extensions/src/sample_scratchpad.rs`) contributes all three
foundational contribution points — a tool, a slash command, and an event
handler. Verbatim sketch:

```rust
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use codesmith_agent::extension::*;
use codesmith_tools::{ToolCapability, ToolResult};
use serde_json::{json, Value};
use crate::discovery::ExtensionRegistration;
use crate::ExtensionMetadata;

static SCRATCH: Mutex<Option<String>> = Mutex::new(None);

pub struct ScratchpadExtension;

#[async_trait]
impl Extension for ScratchpadExtension {
    fn metadata(&self) -> &ExtensionMetadata {
        static M: ExtensionMetadata = ExtensionMetadata::new("scratchpad");
        &M
    }
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        api.register_tool(Box::new(ScratchTool))?;
        api.register_command(Box::new(ScratchCommand))?;
        api.on(Arc::new(TurnStartLogger))?;
        Ok(())
    }
}

// ScratchTool: impl ToolDefinition (name/description/input_schema/execute)
// ScratchCommand: impl CommandDefinition (name/description/run)
// TurnStartLogger: impl Handler (handle)

inventory::submit! {
    ExtensionRegistration {
        factory: || Box::new(ScratchpadExtension),
        metadata: ExtensionMetadata::new("scratchpad"),
    }
}
```

`cargo test -p codesmith-extensions scratchpad_is_discoverable` proves
discovery picks it up (`/extension list` shows it in the REPL — see
[In-REPL Manager](#in-repl-manager)). See the file for the full
tool/command/handler bodies.

## Extension Fields (trait contracts)

All contracts live in `crates/agent/src/extension.rs`. Extension authors
depend on `codesmith-extensions` (which re-exports `codesmith_agent::extension::*`)
so a single crate gives them both the traits and the runtime helpers.

- **`Extension`** — the factory: `metadata() -> &ExtensionMetadata` +
  `async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError>`.
- **`ExtensionApi`** — the registration surface (two-phase: stub at load,
  real at `bind_core`): `register_tool(Box<dyn ToolDefinition>)` /
  `register_command(Box<dyn CommandDefinition>)` /
  `on(Arc<dyn Handler>)` (subscribe to ALL events) /
  `on_variant(ExtensionEventKind, Arc<dyn Handler>)` (subscribe to ONE
  variant only — the runner filters per-variant handlers by
  `event.kind()` before dispatch) + `generation() -> u64` for the
  stale-context guard, plus the route-A/B registrations above and the
  scoped variants (`register_tool_scoped` / `on_variant_scoped` /
  `restrict_tools` — see
  [Registration scopes](#registration-scopes-dsh-alignment)).
- **`ExtensionContext`** — read-mostly host state handed to handlers:
  `cwd() / mode() / is_idle() / signal() / generation()` are live;
  `abort() / shutdown() / compact() / get_context_usage()` return
  `ExtensionError::Unimplemented` (see [Known limitations](#known-limitations)).
- **`ExtensionCommandContext: ExtensionContext`** — strict sub-trait handed
  to command handlers; it carries no session-mutation methods (the split
  exists for type safety).
- **`ExtensionEvent`** — `#[non_exhaustive]`, 31 variants (`resources-discover` removed in 0.11.0; `provider-request-error` added with the DSH retry waterfall; `turn-stopping` with the DSH serial steer; the five workflow lifecycle events with the model-writable `workflow` tool):
  `SessionStart` / `TurnStart` / `ToolCall` / `ToolResult` / `TurnEnd` /
  `SessionShutdown` / `ProjectTrust` / `Input` /
  `BeforeAgentStart` / `AgentStart` / `BeforeProviderHeaders` /
  `BeforeProviderRequest` / `AfterProviderResponse` /
  `AssistantStream` / `ToolExecutionStart` / `ToolExecutionUpdate` /
  `ToolExecutionEnd` / `AgentEnd` / `AgentSettled` /
  `SessionBeforeSwitch` / `SessionBeforeFork` / `SessionBeforeCompact` /
  `SessionCompact` / `ToolsChange` / `TurnStopping` / `WorkflowStart` /
  `WorkflowLog` / `WorkflowAgentStart` / `WorkflowAgentEnd` /
  `WorkflowEnd`. `ExtensionEvent::kind()` maps each
  variant to an `ExtensionEventKind` discriminant for per-variant
  dispatch.
- **`Handler`** — outcome-returning:
  `async fn handle(&self, event: &ExtensionEvent, ctx: &dyn ExtensionContext)
  -> Result<HandlerOutcome, ExtensionError>`. Returns `Continue` (no
  change; proceed), `Cancel { reason }` (abort the surrounding operation
  — only meaningful for `SessionBefore*` variants and `TurnStopping`,
  where the inverted reading applies: the surrounding operation IS the
  stop, so `Cancel` continues the turn and `reason` becomes the steer
  note appended as a user message), `Block { reason }`
  (prevent the operation — only meaningful for `ToolCall` and
  `ProviderRequestError`), `Ask { reason }` (route the operation through
  the host's approval channel — only meaningful for `ToolCall`;
  **fail-closed**: no channel attached ⇒ the call is denied, never
  silently allowed), or
  `Transform(ExtensionEvent)` (replace the running event for subsequent
  handlers AND apply its actionable field at transform-capable seams —
  `Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolCall`/
  `ToolResult`). Variant-specific semantics are enforced by the host at
  each seam; an out-of-place outcome (e.g. `Block` at `TurnEnd`) is
  ignored (treated as `Continue`). `emit` chains handlers in
  registration order so a `Transform` is visible to the next handler;
  `Cancel`/`Block`/`Ask` short-circuit.
- **`AroundToolHandler`** — DSH around-dispatch (tools/execute):
  `async fn call(&self, call: ToolCallEvent, next: ToolCallNext<'_>) ->
  Result<ToolResult, ToolError>`. Register via
  `ExtensionApi::register_around_tool`; handlers compose in registration
  order (outermost first) around BOTH dispatch arms — parallel and
  serial — wrapping the whole execution transaction (the serial arm's
  approval gate included, so a veto skips the question). Never awaiting
  `next` is the veto; the veto vocabulary is
  `Err(ToolError::permission_denied(reason))`, the same feedback shape
  an approval denial produces. Around-chain results are execution
  results: they count toward the loop-guard failure escalation (unlike a
  `Block` intervention). Not offered to script mods (async seam — see
  `docs/MODS.md`).
- **`AroundTurnHandler`** — kernel composition, runtime half (DSH
  `next()` over the whole main turn): `async fn call(&self, turn:
  TurnAroundEvent, next: TurnNext<'_>) -> anyhow::Result<StopReason>`.
  Register via `ExtensionApi::register_around_turn`; handlers compose in
  registration order (outermost first) around the composed executor's
  `AgentExecutor::run` — pre/post phases around the loop, or a veto by
  returning your own `StopReason` without awaiting `next`. `next` is a
  single-shot future (awaiting it runs the inner turn once; "re-run the
  loop" is not expressible here). Takes effect at the next turn's
  assembly; reload clears it with the generation. Main turns only —
  sub-agent loops never wrap — and the chain sits INSIDE the engine's turn
  bookkeeping (`TurnStarted` / `TurnComplete`, harvest), so a vetoed turn
  still completes with the handler's reason. Pairs with the selection
  half, the `turn_executor` config key (`docs/CONFIGURATION.md`). Not
  offered to script mods (async seam).
- **`ToolDefinition`** — extension-side tool contract: `name / description /
  input_schema / capabilities / async execute(input, ctx)`. `execute`
  receives an `ExtensionContext` (NOT the host's `ToolContext`) — keeping
  extensions decoupled from `ToolContext`'s ~30 host-coupled fields.
- **`CommandDefinition`** — extension-side slash-command contract:
  `name / description / async run(ctx, args) -> CommandOutput`. The
  runtime lookup is `ExtensionRunner::try_dispatch_command`, dispatched
  by the REPL between user commands and the built-in static table.
- **`ExtensionError`** — `StaleContext` (the guard signal) + `Config` /
  `Tool` / `Command` / `Conflict` / `Install` / `Load` / `Unimplemented`.

## Handlers: outcomes + per-variant subscription

`Handler::handle` returns a `HandlerOutcome`, and
`ExtensionRunner::emit` chains handlers in registration order — a
`Transform` is visible to the next handler, `Cancel`/`Block`
short-circuit. Each handler call is isolated behind `catch_unwind`: a
panicking handler is logged via `tracing` and skipped — it cannot crash
the agent loop — and a handler `Err` is likewise logged + the chain
continues (best-effort).

Subscribe to **all** events with `on`, or to **one** variant with
`on_variant` (the runner filters per-variant handlers by `event.kind()`
before dispatch, so a per-variant handler never sees a non-matching
event):

```rust
use codesmith_agent::extension::*;
use async_trait::async_trait;

struct AbortCompaction;
#[async_trait]
impl Handler for AbortCompaction {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError> {
        // Fires ONLY for SessionBeforeCompact (per-variant subscription).
        match event {
            ExtensionEvent::SessionBeforeCompact =>
                Ok(HandlerOutcome::Cancel { reason: "user aborted".into() }),
            _ => Ok(HandlerOutcome::Continue),
        }
    }
}

async fn configure(api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
    api.on_variant(ExtensionEventKind::SessionBeforeCompact, Arc::new(AbortCompaction))?;
    Ok(())
}
```

## Registration scopes (DSH alignment)

Registrations carry a **scope** — the DSH per-agent scope key, reduced to
two states: `Scope::Global` (the default; what every plain
`register_tool` / `on` / `on_variant` uses) and
`Scope::AgentType(id)` (a sub-agent type string: `general`, `explore`,
`implementer`, …). Three scoped registration methods exist:

- **`register_tool_scoped(scope, tool)`** — a tool for one scope. An
  `AgentType(id)` tool **shadows** a same-name global tool in that agent
  type's catalog view and never enters the global (main-turn) view.
- **`on_variant_scoped(kind, scope, handler)`** — a per-variant handler
  for one scope. It receives only events dispatched at that scope
  (`ExtensionRunner::emit_scoped`); the main turn's `emit` (Global) never
  reaches it. No scoped subscribe-all form — register per-variant.
- **`restrict_tools(scope, names)`** — a **deny-only** name list that
  prunes the *global* extension-tool layer for the views `scope` covers
  (a `Global` entry prunes every view including the main turn; an
  `AgentType(id)` entry prunes that type's view only). Monotonic by
  construction, the same ratchet `GuardHandler` gives execution-time
  deny: there is no allow form, entries intersect, and nothing re-admits
  a denied name. Restrictions never prune scoped registrations (what an
  agent type declares for itself cannot be voted away by a deployment
  restriction — DSH: "restrictions do not affect scoped registrations"),
  and they prune only the extension layer's own tools — denying a builtin
  is `GuardHandler` (execution-time deny) or config `disabled_tools`
  territory.

View derivation (`ExtensionRunner::bound_tools_scoped(&scope)`, sorted by
name): the global registrations minus every covering restriction, then —
for an `AgentType` view — that type's own registrations overlaid on top
(same name ⇒ scoped wins). `bound_tools()` is the `Global` view; the
main turn's `register_extension_tools` resolves exactly that, so scoped
tools and globally restricted names never appear in the main catalog.

Known limitations: registration scopes are resolved **per origin tier**
(5b) — sub-agent registries mount only script/compiled-in extension
tools and sub-agent loops dispatch only script/compiled-in handlers at
their scope, because a detached background sub-agent (an `agent_open`
session) can outlive a reload's two-phase dylib drop, so dylib
contributions stay main-turn-only (a dylib cannot even register scoped
tools/handlers — the load fails loud). Sub-agent loops emit only
`ToolCall` / `ToolResult` at their scope (and the depth ≥ 1
`SessionBeforeFork` fork veto dispatches at the child's scope too — the
same dylib-safety rule); turn-level and provider-level events stay
main-turn-only. The scoped surface is also not offered to
script mods. Denying a builtin by name is not this list's job (see
above).

## Dispatch three-states (DSH alignment) + host seam mapping

Every `ExtensionEventKind` declares its dispatch mode
(`dispatch_mode()`) — one of three states, named after the
deepseek-harness (DSH) extension model so both frameworks share one
vocabulary:

| CodeSmith `DispatchMode` | DSH term | Semantics |
|---|---|---|
| `Emit` | emit | broadcast: handlers run in subscription order, outcomes are advisory (`Transform`/`Cancel`/`Block` ignored) |
| `Serial` | serial | ordered listeners with veto: a `Cancel` aborts the pending host operation and short-circuits the chain (`SessionBefore*` seams; `TurnStopping` — where the pending operation is the stop itself, so `Cancel { reason }` continues the turn: the steer, `reason` becoming the model-visible note) |
| `Waterfall { can_deny: false }` | waterfall | transform fold: each handler's `Transform` folds into the running event (visible to the next handler); the terminal field applies at the seam (`Input`, `BeforeAgentStart`, `BeforeProviderRequest`, `ToolResult`) |
| `Waterfall { can_deny: true }` | waterfall + deny | the fold plus `Block`: a handler may rewrite the event's actionable field (`ToolCall`: the call's `input` — the rewritten input is what approval gates, what runs, and what is recorded; `ProviderRequestError`: the retry `plan`) or deny the operation outright; the deny is monotonic (`ToolCall`, `ProviderRequestError`). At `ToolCall` the fold additionally honors `Ask { reason }` — approval-channel routing, fail-closed (see the seam table) |

The match is exhaustive like `kind()` and pinned by the
`event_dispatch_contract_table` test, so the declared contract cannot
drift silently. Of DSH's five modes only these three are ported —
parallel fan-out and first-value-wins (bail) are intentionally not built;
add one only with a real consumer. CodeSmith does **not** adopt DSH's
everything-is-a-plugin kernel — the kernel (turn loop, provider
registry, session log) stays compile-time assembled; only the dispatch
vocabulary is shared. Nor does `Waterfall` wrap the operation DSH-style
with `next()` — it stays a fold chain, each transform visible to the
next handler; the `next()`-wrapping form exists only for tool execution
(`AroundToolHandler`, section below). The capability graph
(`docs/CAPABILITY_GRAPH.md`, generated by
`scripts/capability-graph.py`, CI-checked) lists every seam's
definition/provider/consumer triple.

`EmitOutcome` is `#[must_use]`, so every emit site binds the result
(emit seams use `let _ =`; capability seams inspect
`out.outcome` / `out.event`). An out-of-place outcome (e.g. `Block` at
`TurnEnd`) is ignored — treated as `Continue` — so a handler that
returns the wrong capability for its variant is a no-op, not an error.

The Rhai bridge: script mods register emit listeners and serial vetoes
with `on_event(kind, callback)` — the gate is `dispatch_mode()` itself,
so a waterfall kind fails the mod's load pointing at `on()` (the
pre-alignment full surface, waterfall transforms included — a deliberate
CodeSmith superset) or `register_guard` (the deny floor). Around-dispatch
and `Ask` have no script form in any channel (async / UI decision
channel; no `ask()` control value exists, so a script can never produce
an `Ask` outcome).

### Around-dispatch (DSH tools/execute)

The `next()`-wrapping form exists only for tool execution, as the
`AroundToolHandler` trait (registered via `ExtensionApi::register_around_tool`)
— it is deliberately not a `DispatchMode`: the wrap cannot be expressed
as a fold. `wrap_tool_dispatch` composes the registration-ordered chain
(outermost first) around the dispatch terminal of BOTH arms in
`turn::batches` — the parallel arm's early-reuse-or-run and the serial
arm's approval-gate-plus-run. Never awaiting `next` vetoes. The DSH
`tools/execute` middleware and this seam are one vocabulary; the
`Waterfall` fold above stays a fold.

| Variant | Emit site | Honored outcome | Effect |
|---|---|---|---|
| `SessionStart { reason }` | `engine/mod.rs` pre-op-loop | emit | — |
| `SessionShutdown` | `engine/mod.rs` post-MCP-shutdown | emit | — |
| `TurnStart` | `host_executor` turn entry | emit | — |
| `TurnEnd` | `host_executor` turn exit (interrupted + no-tool-calls) | emit | — |
| `TurnStopping(TurnStoppingEvent)` | `host_executor` no-tool-calls arm — after the steer flush + sub-agent drain resume branches, before the terminal path | **Cancel** | the DSH turn-stopping serial steer: asked before the turn ends on `NoToolCalls`; `Continue` (or no handler) abstains — the default stop; the first `Cancel { reason }` steers — `reason` is appended verbatim as a `user`-role message (model-visible ⇒ session-recorded; append-only, prefix cache untouched) and the turn resumes on a fresh step. Steers are capped at 3 per turn by the loop guard — a capped steer is dropped (`tracing` warn, no note) and the turn ends. `reason` is why the turn would end (`no-tool-calls` at the only emit site); the `MaxSteps`/`Interrupted`/`Error` terminal paths do not ask |
| `Input(InputEvent)` | `host_executor::run_inner` (user-turn seed) | **Transform** | rewrites the submitted `text` |
| `BeforeAgentStart(AgentStartEvent)` | `host_executor::run_inner` top | **Transform** | injects `inject_message` (history push) + overrides `system_prompt` if set |
| `AgentStart` | `host_executor::run_inner` (emit) | emit | — |
| `BeforeProviderHeaders(BeforeProviderHeadersEvent)` | `host_executor` before `request` build | emit | payload = the provider client's extra HTTP headers as a JSON object, credential-bearing values masked; the transport's auth headers (from the api key) are not listed — they never leave the client |
| `BeforeProviderRequest(BeforeProviderRequestEvent)` | `host_executor` after `request` built, before stream | **Transform** | rewrites `request.messages` |
| `AfterProviderResponse(AfterProviderResponseEvent)` | `host_executor` `Content` arm after `accumulate_usage` | emit | payload = the assembled `MessageResponse` serialized (streaming synthesis: `id` is empty — no wire response object reaches the reducer; `content`/`stop_reason`/`usage` come from the stream reduction) |
| `ProviderRequestError(ProviderRequestErrorEvent)` | `stream_with_transparent_retry` `Empty` arm, before each retry decision | **Transform + Block** | the DSH request-error waterfall, asked inside the transparent-retry guard: `Continue` keeps the default policy (bounded retries; rate-limit backoff); `Transform` rewrites `plan` — `Retry { delay_ms }` overrides the backoff (clamped to 5 min) or forces a retry past the default budget (forced retries capped at 6); `Block` gives up and surfaces the failure. `failure` carries the classified category (snake_case) + raw message; `attempt` is the 1-based failing-attempt number. Retries never enter the model context — `tracing` records the extension decisions |
| `AssistantStream(AssistantStreamEvent)` | callback-bridge stream-delta path (`agent-runtime/src/callback_bridge.rs`) | emit | one incremental assistant **text** chunk per wire delta; dispatch runs off-thread under a 250 ms per-delta + 1 s cumulative-per-turn budget — an over-budget handler is detached (runs to completion, outcome discarded, never cancelled mid-flight) and later deltas that turn skip dispatch; no dispatch at all when nothing subscribes; thinking deltas stay UI-only |
| `ToolCall(ToolCallEvent)` | `host_executor` parallel + serial tool dispatch + **sub-agent loops** (5b: every child tool call dispatches at the child's `AgentType` scope — script/compiled-in handlers only, dylib handlers skipped) | **Transform + Block + Ask** | a handler may rewrite the call's `input`, deny it, or ask the user. `Block` skips approval + `tool.run` → `Err(ToolError::permission_denied(reason))`, `blocked = true`. `Ask { reason }` routes the call through the approval decision channel regardless of its static classification — on the serial arm it forces the gate (fail-closed: no channel ⇒ denied); on the parallel arm the batch's sequential pre-dispatch loop asks inline (a read-only call becomes user-gated), a denial records like a block and an approved escalation logs + runs with the fixed context. In the parallel batch a rewritten input must re-classify as auto-approved or the call is blocked there — a rewrite cannot smuggle a non-approved input through the approval-free batch; the model can re-issue the call to route it through the serial approval gate. `on_tool_start` and the audit record see the rewritten input. Around-dispatch (`AroundToolHandler`) wraps the dispatch terminal of both arms — see the section above. In a sub-agent loop: `Block` denies (error-string result), `Ask` fails closed (the child attaches no approval ask channel — denied, reason fed back), a transform's rewritten input is what executes and what the child transcript records |
| `ToolResult(ToolResultEvent)` | `host_executor` parallel + serial, emit reordered BEFORE `on_tool_end` + **sub-agent loops** (5b: after each child tool execution, at the child's scope — the final content is what the child transcript records) | **Transform** | replaces the result; `on_tool_end` + downstream `outcomes[idx].result` see the transformed result. This is the DSH post-execute accept/replace equivalent — `Continue` accepts, `Transform` replaces (a deny does not exist post-execution; use the `ToolCall` seam). `ToolResult` carries a canonical/rendered split: `content` is the model-visible rendering, `canonical` the structured machine value (live-process only, not persisted in the transcript) — rewriting `content` must not rewrite `canonical` |
| `ToolExecutionStart` | `host_executor` tool closure (before `tool.run`) | emit | — |
| `ToolExecutionEnd` | `host_executor` tool closure (after `tool.run`) | emit | — |
| `AgentEnd` | `host_executor::run_inner` each `return Ok(...)` | emit | — |
| `AgentSettled` | `engine/mod.rs` post-run drain (after capacity apply) | emit | — |
| `SessionBeforeCompact` | `host_executor::run_compaction` after `should_compact` gate | **Cancel** | skips compaction (`return`) |
| `SessionCompact` | `host_executor::run_compaction` after summary applied | emit | — |
| `SessionBeforeSwitch` | `/cd` REPL command (`crates/cli/src/interactive/commands/dir_cmds.rs`) — its only trigger point, after the `cd_allowed` trust check, before the workspace assignment + `Op::SyncSession` re-root | **Cancel** | a mod veto keeps the session in the previous directory (the reason prints to the user, attributed to the mod); `Continue` (or no handler) lets the switch proceed |
| `ProjectTrust` | `HostServices::build_turn_dispatcher` (`crates/cli/src/core/engine/runtime_traits.rs`; + `spawn_subagent`) after `build_tool_context_for` (per-turn `Trusted`/`Untrusted`); `FirstLoad` currently has no emit site — the onboarding trust-prompt emit went with the TUI in 0.10.0 | emit | per-turn `Trusted`/`Untrusted` from `session.trust_mode`; `FirstLoad` is defined for onboarding trust acceptance (`TrustReason::FirstLoad`) but has no live emit site — the runtime `trust_mode` toggle (`/trust on`), YOLO entry, and persisted-trust startup surface per-turn as `Trusted`/`Untrusted`, not `FirstLoad` |
| `ToolsChange { added, removed }` | `HostServices::build_turn_dispatcher` after every selection source is applied | emit | diff of the final model-visible catalog vs the previous main-turn baseline; the first build establishes the baseline silently; sub-agent toolsets never reach this seam |
| `—` (dylib LOAD, not an event) | `populate_extension_runtime` (`crates/cli/src/core/engine.rs`) after `discover_static` | n/a (load phase) | `discover_dylib(&global_roots, &project_roots)` → `apply_trust_gate(discovered, !is_workspace_trusted(workspace))` drops project-local (`global == false`) → `state.is_enabled` reconcile → `ExtensionRunner::load_dylib` on the OS-thread load runtime; reload picks up via `reload_extension_runtime`→`populate`. `ExtensionRunner.libraries` holds `Library` handles; on a runtime reload they `drain_libraries_to_pending` to `pending_drop` (alongside the clears) + the engine op-loop `drop_pending`s them at the next turn boundary. Lockstep `*mut dyn Extension` via `codesmith_register_extension`. |
| `SessionBeforeFork` | spawn-family tools (`agent_open` / `agent_spawn` / `agent_run` / `delegate_to_agent`, every depth — the runner rides `SubAgentRuntime`, so grandchildren forks are vetoable too) + `HostServices::spawn_subagent` (`crates/cli/src/tools/subagent/mod.rs`, `core/engine/runtime_traits.rs`) | **Cancel** | emitted before a child forks session state; `Cancel` aborts the spawn with a model-visible error. From a depth ≥ 1 context the veto dispatches at the child's agent-type scope, so dylib-origin handlers are skipped there (§F5d — a detached child can outlive a reload's `Library` drop); at depth 0 and on the engine-internal path it stays the Global emit (the main executor holds the `Library` alive through the turn). Global script/compiled-in handlers receive the veto at every depth |
| `ToolExecutionUpdate` | `exec_shell` (foreground) / `exec_shell_wait` poll loops → `ToolContext::progress` → `callback_bridge::RunnerProgressSink` | emit | throttled checkpoints while a command runs (≥1s apart, new output only): `id` = shell task id, `message` = output since the previous checkpoint (tail-capped at 4 KiB); the same checkpoint rides the UI as `Event::ToolCallProgress`; dispatched under the AssistantStream budget/breaker; main turns only; sandboxed-backend and interactive runs report none |
| `WorkflowStart` | `workflow` engine (`crates/cli/src/tools/workflow/engine.rs`) before the one-shot kernel spawns | emit | a workflow run began; a budget-rejected request emits nothing (the run never began) |
| `WorkflowLog(WorkflowLogEvent)` | workflow RPC bridge, per `log(...)` helper call | emit | `message` = one bounded log entry (the same entry the run's own buffer retains) |
| `WorkflowAgentStart(WorkflowAgentStartEvent)` | workflow RPC bridge (`spawn_with_events`), after the run's agent budget admitted the child | emit | `objective` = the child's objective text |
| `WorkflowAgentEnd(WorkflowAgentEndEvent)` | workflow RPC bridge, when one child finished | emit | `status` + `steps`; a child dropped by the run-level wall-clock bound emits `WorkflowAgentStart` with no matching End — the run-level `WorkflowEnd` (timeout) is the terminal signal there |
| `WorkflowEnd(WorkflowEndEvent)` | `workflow` engine, terminal outcome (success or failure) | emit | `status` (`completed`/`failed`), `error` kind on failure (`timeout`/`script_error`/`kernel_spawn`), `agent_calls`; carries no return value — the transcript's tool result is the record |

> The `Transform` payload's actionable field is applied at the seam AFTER
> the full handler chain runs (so a `Transform` from handler N is visible
> to handler N+1 as the running event). `Cancel`/`Block` short-circuit
> the chain. The terminal `EmitOutcome.outcome` is never `Transform`
> (folded into `EmitOutcome.event`); capability seams inspect
> `out.outcome` for `Cancel`/`Block` and `out.event` for the transformed
> actionable field.

## Sandbox stance

CodeSmith does **not** sandbox extensions. Extensions run in the same
process as the agent loop with full host access — **trust the source**.
For untrusted extensions, containerize the whole CodeSmith process.
Project-local dylib installs require workspace trust before the first
load (the trust gate above); the `ProjectTrust` per-turn emits are the
emit-only signal extension handlers see for the session's trust
state (`FirstLoad` has no live emit site since the TUI removal in
0.10.0).

`cargo build` during an extension install runs the source's `build.rs` —
**arbitrary code execution, accepted (trust the source)**; containerize
for untrusted sources. Install itself is trust-agnostic (it only *reads*
trust to warn: a project-local install won't load until the workspace
is trusted). A loaded dylib runs in-process with full host access —
trust the source; containerize for untrusted sources. Compiled-in
extensions are trusted by construction (they ship in the binary).

Extension tools are **origin-tiered** (5b): the main turn mounts the
Global view regardless of origin; a sub-agent registry mounts its agent
type's view through
`ExtensionRunner::mountable_tools_for_subagents`, which filters out
**dylib-origin** tools — a detached background sub-agent (an
`agent_open` session) can outlive a reload's two-phase `Library` drop,
so dylib `Arc`s never cross into a child (the §F5d T4 safety invariant,
restated per-origin instead of per-surface). Script/compiled-in ext
tools mount in children freely; the capability-manifest removal and the
read-only narrowing govern them like any other tool (an ext tool joins
an explore child's surface by declaring the `ReadOnly` capability).

Safe unload rests on a two-phase `Library` drop plus in-flight turn
leases (structural ownership, `agent-runtime/src/turn_lease.rs`): every
running turn — the main turn at assembly, each in-process sub-agent for
its run — holds a `TurnLease` (keyed agent + extension generation +
turn-executor id, runner-scoped) whose registry answers "who runs what,
under which generation". Reload on the UI thread MOVES orphaned
`Library`s to `pending_drop` (`drain_libraries_to_pending`) **after**
draining the runtime's in-flight sub-agents (above); the engine op-loop
top DROPs them (`drop_pending`) only when no lease assembled under a
retired generation declares dylib exposure — the main turn's lease is
released between turns, and sub-agent leases declare none (5b) —
deferring with a logged reason otherwise (the safety net; the normal
path never triggers it). This makes runtime reload + uninstall safe
concurrent with in-flight turns. The safety is proven by that
lease-checked invariant + the single-call-site discipline; dylib+Miri is
unreliable (libloading's `Library::drop` runs `dlclose`/`FreeLibrary`,
which Miri doesn't model), so the invariant — not a Miri run — is the
proof.

## Known limitations

- `ExtensionContext::abort() / shutdown() / compact() /
  get_context_usage()` return `ExtensionError::Unimplemented`.
- `TurnStopping` steers are capped at 3 per turn (loop guard); only the
  `NoToolCalls` termination asks — `MaxSteps` / `Interrupted` / `Error`
  terminal paths do not consult extensions, and a capped steer's note is
  dropped, not appended.
- `AroundToolHandler` and `Ask` are compiled-extension surfaces only —
  script mods get neither (async / UI decision channel); see
  `docs/MODS.md`. Around-chain results count toward the loop-guard
  failure escalation, unlike `Block` interventions.
- The script-side DSH surface is `on_event` (emit/serial kinds only) +
  `register_guard`; waterfall participation from scripts stays on the
  older `on()` full surface, and scripts see no registration scopes (no
  `on_event_scoped`).
- There is no extension-to-extension pub/sub. The former §F3 `EventBus`
  skeleton (`subscribe`/`publish` returning `Unimplemented`) was deleted
  with zero consumers — it trapped callers at runtime without serving any.
  It returns only when an extension actually needs inter-extension
  messaging, designed against that need.
- The `ExtensionApi` has no renderer, shortcut, or flag registration
  surface.
- `AssistantStream` carries text deltas only — thinking deltas stay
  UI-only.
- The capability manifest is read once per process; edits require a
  restart.
- Hot-load is permanently out — install (`/extension install`) + runtime
  reload only (`/extension reload` or `manage_mods(action="reload")`).

## Troubleshooting

- **Compiled-in extension doesn't load.** No `inventory::submit!` reached
  the link — confirm the extension's crate is a workspace member + that
  `crates/extensions/src/lib.rs` declares its module. `cargo test -p
  codesmith-extensions scratchpad_is_discoverable` proves the registration
  is wired. `/extension list` / `/extension status` show what the host
  sees; failed loads surface in the `tracing` summary (target
  `codesmith_extensions`) and dylib installs can be inspected directly
  under `~/.codesmith/extensions` / `<workspace>/.codesmith/extensions`.
- **Runner reads as "not bound".** The engine hasn't built yet
  (pre-startup), or `state.extension_runner` wasn't copied from the handle
  (`crates/cli/src/interactive/mod.rs` after `spawn_engine`).
- **Mod layer unresponsive after edits.** Runtime reload is
  engine-level — run `/extension reload` (or `/mods reload`)
  (see [MODS.md](MODS.md)).
- **Handler returns `Continue` but nothing changes.** `Continue` means
  "no change" by design. To cancel/block/transform, return the matching
  variant — and note variant-specific semantics (a `Block` at a
  non-`ToolCall` seam is ignored; see the host seam mapping above).
  `emit` isolates each handler call behind `catch_unwind`: a panicking
  handler is logged via `tracing` and skipped — it cannot crash the
  agent loop — and a handler `Err` is likewise logged + the chain
  continues.
- **`configure` captured an `Arc<dyn ExtensionApi>` that now returns
  `StaleContext`.** The runner was `invalidate()`d (via a runtime reload
  or a future reload/fork/switch); capture a fresh api or check
  `generation()` against the live runner's before use.
- **Tests panic at `tokio runtime blocking/shutdown.rs`.** A nested tokio
  runtime was created + dropped from within a runtime worker thread.
  `build_extension_runtime` drives `configure` on a plain OS thread
  (`std::thread::scope`) precisely to avoid this — if you see it, the
  thread::scope guard was bypassed.
