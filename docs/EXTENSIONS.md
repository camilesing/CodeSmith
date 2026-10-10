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
> [MODS.md](MODS.md).

The host assembles the extension runtime at engine build
(`build_extension_runtime()`, `crates/tui/src/core/engine.rs`) and again
on every `/extension reload`: discover compiled-in registrations
(`inventory`) + dylib sources (install roots) → reconcile with the
on-disk `ExtensionStateStore` (skip disabled) → load + configure each
against a stub api → `bind_core` the host context — after which the
runner fans lifecycle events to registered handlers.

Extension tools + slash commands are wired live into the host per turn:
tools register into the per-turn `ToolRegistry` via `register_extension_tools` in
`EngineHost::build_turn_dispatcher`, and slash commands dispatch via
`try_dispatch_extension_command` in `commands::execute` — so the agent
loop sees extension tools as normal `ToolSpec`s (main-turn only; not
inherited by sub-agents — see [Sandbox stance](#sandbox-stance)).

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
  `## Skills` block, `/skills`, the command palette, and `load_skill`
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
  evaluate pre-approval at the seam and cover main-turn calls only; a
  script error abstains with a warn.
- **Event handlers (`on` / `on_variant`)** — see
  [Handlers](#handlers-outcomes--per-variant-subscription).

### Tool catalog: `ToolsChange` + the capability manifest

The turn dispatcher diffs the compiled model-visible catalog against the
previous main-turn baseline (origin-classified snapshot in
`tui/src/core/tool_catalog.rs`) and emits `ToolsChange { added, removed }`
(observe-only); `/tools` renders the baseline grouped by origin. The
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

## In-TUI Manager

The `/extension` command group is the user-facing surface. It dispatches
via `extension_commands::try_dispatch`, wired into `execute()` between
user-defined commands and the static `match`.

| Subcommand | Aliases | Effect |
|---|---|---|
| `/extension list` | `ls` | Lists compiled-in + installed extensions (id + version). |
| `/extension info <id>` | | Shows metadata for one extension. |
| `/extension enable <id>` | | Marks the extension enabled in `extensions_state.toml`; takes effect on the next `/extension reload`. |
| `/extension disable <id>` | | Marks the extension disabled; same reload caveat. |
| `/extension status` | | Reports the bound runner's generation + bound command/tool counts. |
| `/extension reload` | | Re-populates the **shared runner `Arc`**: `clear_handlers` → `clear_tools` → `clear_commands` → `clear_providers` (drops registration guards) → `clear_prompt_sections` → `clear_skills` → `clear_message_projections` → `drain_libraries_to_pending` → `invalidate` (bump generation) → discover (static + dylib) → reconcile against state → `load` each → `bind_core` (fresh `HostExtensionContext`). Both `App.extension_runner` and the engine's field update live (no `Arc` swap — they share the one the engine built). The drained `Library`s are dropped (`drop_pending`) at the next engine op-loop top (turn boundary). A handler bound before reload stops observing after (cleared, not duplicated); a newly-installed extension is picked up on the next reload. |
| `/extension install <source> [--global]` | | Fetches (`git:`/`path:`/`crate:`/`prebuilt:`) → builds (`cargo build`) → places to `<root>/<id>/` + writes `extension.toml` + records `installed[]` provenance + writes a sha256 sidecar (`<dylib>.sha256`) that the loader verifies — a dylib swapped on disk after install is refused at load; `--global` opt-in (default project). `crate:` fetches from crates.io (sparse-index → version → sha256-verified `.crate` → `tar` extract → build); `prebuilt:<https-url>` fetches a prebuilt cdylib (HTTPS-only, redirects cannot downgrade to plain HTTP, optional `--checksum <sha256>`); both warn if project + untrusted; `/extension reload` to load. |
| `/extension uninstall <id>` | | Removes `<root>/<id>/` + clears `installed[]` provenance. Live tool/command bindings clear on next `/extension reload`; the dylib unloads safely at the next turn boundary (two-phase `Library` drop). |

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

`/extension list` reports `scratchpad`; `/extension info scratchpad` shows
its metadata. See the file for the full tool/command/handler bodies.

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
  stale-context guard, plus the route-A/B registrations above.
- **`ExtensionContext`** — read-mostly host state handed to handlers:
  `cwd() / mode() / is_idle() / signal() / generation()` are live;
  `abort() / shutdown() / compact() / get_context_usage()` return
  `ExtensionError::Unimplemented` (see [Known limitations](#known-limitations)).
- **`ExtensionCommandContext: ExtensionContext`** — strict sub-trait handed
  to command handlers; it carries no session-mutation methods (the split
  exists for type safety).
- **`ExtensionEvent`** — `#[non_exhaustive]`, 25 variants:
  `SessionStart` / `TurnStart` / `ToolCall` / `ToolResult` / `TurnEnd` /
  `SessionShutdown` / `ProjectTrust` / `ResourcesDiscover` / `Input` /
  `BeforeAgentStart` / `AgentStart` / `BeforeProviderHeaders` /
  `BeforeProviderRequest` / `AfterProviderResponse` /
  `AssistantStream` / `ToolExecutionStart` / `ToolExecutionUpdate` /
  `ToolExecutionEnd` / `AgentEnd` / `AgentSettled` /
  `SessionBeforeSwitch` / `SessionBeforeFork` / `SessionBeforeCompact` /
  `SessionCompact` / `ToolsChange`. `ExtensionEvent::kind()` maps each
  variant to an `ExtensionEventKind` discriminant for per-variant
  dispatch.
- **`Handler`** — outcome-returning:
  `async fn handle(&self, event: &ExtensionEvent, ctx: &dyn ExtensionContext)
  -> Result<HandlerOutcome, ExtensionError>`. Returns `Continue` (no
  change; proceed), `Cancel { reason }` (abort the surrounding operation
  — only meaningful for `SessionBefore*` variants), `Block { reason }`
  (prevent the operation — only meaningful for `ToolCall`), or
  `Transform(ExtensionEvent)` (replace the running event for subsequent
  handlers AND apply its actionable field at transform-capable seams —
  `Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolCall`/
  `ToolResult`). Variant-specific semantics are enforced by the host at
  each seam; an out-of-place outcome (e.g. `Block` at `TurnEnd`) is
  ignored (treated as `Continue`). `emit` chains handlers in
  registration order so a `Transform` is visible to the next handler;
  `Cancel`/`Block` short-circuit.
- **`ToolDefinition`** — extension-side tool contract: `name / description /
  input_schema / capabilities / async execute(input, ctx)`. `execute`
  receives an `ExtensionContext` (NOT the host's `ToolContext`) — keeping
  extensions decoupled from `ToolContext`'s ~30 host-coupled fields.
- **`CommandDefinition`** — extension-side slash-command contract:
  `name / description / async run(ctx, args) -> CommandOutput`. Dispatched
  by the host's `extension_commands::try_dispatch`.
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

## Dispatch contracts + host seam mapping

Every `ExtensionEventKind` declares its dispatch mode
(`dispatch_mode()`): **observe** (advisory outcomes ignored),
**transform-chain** (`Input`/`BeforeAgentStart`/`BeforeProviderRequest`/
`ToolResult`), **cancel-veto** (the `SessionBefore*` seams), or
**transform-and-deny** (`ToolCall`: a handler may rewrite the call's
`input` — the rewritten input is what approval gates, what runs, and
what is recorded — or deny it; the deny is monotonic). The match is
exhaustive like `kind()` and pinned by the `event_dispatch_contract_table`
test, so the declared contract cannot drift silently. The capability
graph (`docs/CAPABILITY_GRAPH.md`, generated by
`scripts/capability-graph.py`, CI-checked) lists every seam's
definition/provider/consumer triple.

`EmitOutcome` is `#[must_use]`, so every emit site binds the result
(observe-only seams use `let _ =`; capability seams inspect
`out.outcome` / `out.event`). An out-of-place outcome (e.g. `Block` at
`TurnEnd`) is ignored — treated as `Continue` — so a handler that
returns the wrong capability for its variant is a no-op, not an error.

| Variant | Emit site | Honored outcome | Effect |
|---|---|---|---|
| `SessionStart { reason }` | `engine/mod.rs` pre-op-loop | observe | — |
| `SessionShutdown` | `engine/mod.rs` post-MCP-shutdown | observe | — |
| `TurnStart` | `host_executor` turn entry | observe | — |
| `TurnEnd` | `host_executor` turn exit (interrupted + no-tool-calls) | observe | — |
| `Input(InputEvent)` | `host_executor::run_inner` (user-turn seed) | **Transform** | rewrites the submitted `text` |
| `BeforeAgentStart(AgentStartEvent)` | `host_executor::run_inner` top | **Transform** | injects `inject_message` (history push) + overrides `system_prompt` if set |
| `AgentStart` | `host_executor::run_inner` (observe) | observe | — |
| `BeforeProviderHeaders` | `host_executor` before `request` build | observe | — |
| `BeforeProviderRequest(BeforeProviderRequestEvent)` | `host_executor` after `request` built, before stream | **Transform** | rewrites `request.messages` |
| `AfterProviderResponse(AfterProviderResponseEvent)` | `host_executor` `Content` arm after `accumulate_usage` | observe | — |
| `AssistantStream(AssistantStreamEvent)` | callback-bridge stream-delta path (`agent-runtime/src/callback_bridge.rs`) | observe | one incremental assistant **text** chunk per wire delta; dispatch runs off-thread under a 250 ms per-delta + 1 s cumulative-per-turn budget — an over-budget handler is detached (runs to completion, outcome discarded, never cancelled mid-flight) and later deltas that turn skip dispatch; no dispatch at all when nothing subscribes; thinking deltas stay UI-only |
| `ToolCall(ToolCallEvent)` | `host_executor` parallel + serial tool dispatch | **Transform + Block** | a handler may rewrite the call's `input` or deny it. `Block` skips approval + `tool.run` → `Err(ToolError::permission_denied(reason))`, `blocked = true`. In the parallel batch a rewritten input must re-classify as auto-approved or the call is blocked there — a rewrite cannot smuggle a non-approved input through the approval-free batch; the model can re-issue the call to route it through the serial approval gate. `on_tool_start` and the audit record see the rewritten input |
| `ToolResult(ToolResultEvent)` | `host_executor` parallel + serial, emit reordered BEFORE `on_tool_end` | **Transform** | replaces the result; `on_tool_end` + downstream `outcomes[idx].result` see the transformed result. `ToolResult` carries a canonical/rendered split: `content` is the model-visible rendering, `canonical` the structured machine value (live-process only, not persisted in the transcript) — rewriting `content` must not rewrite `canonical` |
| `ToolExecutionStart` | `host_executor` tool closure (before `tool.run`) | observe | — |
| `ToolExecutionEnd` | `host_executor` tool closure (after `tool.run`) | observe | — |
| `AgentEnd` | `host_executor::run_inner` each `return Ok(...)` | observe | — |
| `AgentSettled` | `engine/mod.rs` post-run drain (after capacity apply) | observe | — |
| `SessionBeforeCompact` | `host_executor::run_compaction` after `should_compact` gate | **Cancel** | skips compaction (`return`) |
| `SessionCompact` | `host_executor::run_compaction` after summary applied | observe | — |
| `SessionBeforeSwitch` | `tui/ui.rs` `switch_workspace` entry | **Cancel** | aborts the workspace switch |
| `ProjectTrust` | `HostServices::build_turn_dispatcher` (+ `spawn_subagent`) after `build_tool_context_for` (per-turn `Trusted`/`Untrusted`); onboarding trust-accept `tui/ui.rs` `TrustDirectory` y/Y/1 arm after `app.trust_mode = true` (`FirstLoad`) | observe | per-turn `Trusted`/`Untrusted` from `session.trust_mode`; `FirstLoad` once per onboarding trust acceptance (`TrustReason::FirstLoad`) — distinct from the runtime `trust_mode` toggle (`/trust on`), YOLO entry, and persisted-trust startup, which surface per-turn as `Trusted`/`Untrusted`, not `FirstLoad` |
| `ToolsChange { added, removed }` | `HostServices::build_turn_dispatcher` after every selection source is applied | observe | diff of the final model-visible catalog vs the previous main-turn baseline; the first build establishes the baseline silently; sub-agent toolsets never reach this seam |
| `—` (dylib LOAD, not an event) | `populate_extension_runtime` (`tui/src/core/engine.rs`) after `discover_static` | n/a (load phase) | `discover_dylib(&global_roots, &project_roots)` → `apply_trust_gate(discovered, !is_workspace_trusted(workspace))` drops project-local (`global == false`) → `state.is_enabled` reconcile → `ExtensionRunner::load_dylib` on the OS-thread load runtime; reload picks up via `reload_extension_runtime`→`populate`. `ExtensionRunner.libraries` holds `Library` handles; on `/extension reload` they `drain_libraries_to_pending` to `pending_drop` (alongside the clears) + the engine op-loop `drop_pending`s them at the next turn boundary. Lockstep `*mut dyn Extension` via `codesmith_register_extension`. |
| `ResourcesDiscover` | — (no emit site) | observe | defined but never emitted: the only in-process candidate site (the `list_mcp_resources` pseudo-tool dispatch in `McpPool`, `agent-runtime/src/mcp.rs`) is already bracketed by `ToolCall`/`ToolResult` — firing `ResourcesDiscover` there would conflate with tool execution, and `DiscoverReason` has no clean mapping there; no dedicated Startup/Manual/Reload discover seam holds the runner `Arc` (the `tui/mcp_server.rs` stdio site is a separate process) |
| `SessionBeforeFork` | — (no emit site) | **Cancel** | defined but never emitted: the in-TUI backtrack path (`apply_backtrack`, `tui/ui.rs`) is an in-place **rewind** (`truncate_history_to`/`api_messages.truncate`), not a **fork** (new-thread creation) — wiring it as `SessionBeforeFork` would mislabel; genuine fork primitives are dead (`fork_at_user_message`, no non-test callers) or HTTP-only (`fork_thread`, runtime-api, no runner access) |
| `ToolExecutionUpdate` | — (no emit site) | observe | defined but never emitted: `Tool::run` is one-shot (`agent/src/tools/mod.rs`), so there is no mid-execution progress stream to hook. The `on_tool_progress` `Callback` hook exists as forward-looking API surface; the emit site awaits a streaming `Tool` contract |

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
load (the trust gate above); the `ProjectTrust { FirstLoad }` event is
the once-per-session observe-only signal extension handlers see when
the user accepts the workspace trust prompt.

`cargo build` during `/extension install` runs the source's `build.rs` —
**arbitrary code execution, accepted (trust the source)**; containerize
for untrusted sources. Install itself is trust-agnostic (it only *reads*
trust to warn: a project-local install won't load until the workspace
is trusted). A loaded dylib runs in-process with full host access —
trust the source; containerize for untrusted sources. Compiled-in
extensions are trusted by construction (they ship in the binary).

Extension tools are **main-turn-only, structurally**: they are
registered into the host's per-turn `ToolRegistry` (the main agent
turn), NOT inherited by sub-agents. This is structural, not a guard:
`SubAgentRuntime` has no `extension_runner` field +
`SubAgentToolRegistry::new` rebuilds its own fresh built-in
`ToolRegistry` — so ext tools can never reach a sub-agent's effective
set, regardless of `inherit_full_registry`. No provenance marker /
force-subset / runtime subagent-check is needed.

Safe unload rests on a two-phase `Library` drop: reload on the UI
thread MOVES orphaned `Library`s to `pending_drop`
(`drain_libraries_to_pending`); the engine op-loop top DROPs them
(`drop_pending`) at the one moment the main-thread `HostAgentExecutor`
(the only in-flight dylib `Arc` holder) is already dropped between
turns. This makes `/extension reload` + uninstall safe concurrent with
in-flight turns. The safety is proven by that invariant + the
single-call-site discipline; dylib+Miri is unreliable (libloading's
`Library::drop` runs `dlclose`/`FreeLibrary`, which Miri doesn't model),
so the invariant — not a Miri run — is the proof.

## Known limitations

- `ExtensionContext::abort() / shutdown() / compact() /
  get_context_usage()` return `ExtensionError::Unimplemented`.
- `ResourcesDiscover`, `SessionBeforeFork`, and `ToolExecutionUpdate`
  are defined in the contract but have no host emit site (rationale in
  the seam table above).
- `EventBus` (`codesmith_extensions::EventBus`) is a skeleton —
  `subscribe`/`publish` return `Unimplemented`; there is no
  extension-to-extension pub/sub.
- The `ExtensionApi` has no renderer, shortcut, or flag registration
  surface.
- `AssistantStream` carries text deltas only — thinking deltas stay
  UI-only.
- The capability manifest is read once per process; edits require a
  restart.
- Hot-load is permanently out — install + `/extension reload` only.

## Troubleshooting

- **`/extension list` shows nothing.** No `inventory::submit!` reached the
  link — confirm the extension's crate is a workspace member + that
  `crates/extensions/src/lib.rs` declares its module. `cargo test -p
  codesmith-extensions scratchpad_is_discoverable` proves the registration
  is wired.
- **`/extension status` says "not bound".** The engine hasn't built yet
  (pre-startup), or `app.extension_runner` wasn't copied from the handle
  (`crates/tui/src/tui/ui.rs` after `spawn_engine`).
- **Handler returns `Continue` but nothing changes.** `Continue` means
  "no change" by design. To cancel/block/transform, return the matching
  variant — and note variant-specific semantics (a `Block` at a
  non-`ToolCall` seam is ignored; see the host seam mapping above).
  `emit` isolates each handler call behind `catch_unwind`: a panicking
  handler is logged via `tracing` and skipped — it cannot crash the
  agent loop — and a handler `Err` is likewise logged + the chain
  continues.
- **`configure` captured an `Arc<dyn ExtensionApi>` that now returns
  `StaleContext`.** The runner was `invalidate()`d (via `/extension
  reload` or a future reload/fork/switch); capture a fresh api or check
  `generation()` against the live runner's before use.
- **Tests panic at `tokio runtime blocking/shutdown.rs`.** A nested tokio
  runtime was created + dropped from within a runtime worker thread.
  `build_extension_runtime` drives `configure` on a plain OS thread
  (`std::thread::scope`) precisely to avoid this — if you see it, the
  thread::scope guard was bypassed.
