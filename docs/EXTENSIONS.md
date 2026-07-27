# Extensions

CodeSmith extensions are compiled-in (slice 1, §F1) or to-be-loaded
(phase 2, §F5) modules that contribute **tools**, **slash commands**, and
**lifecycle event handlers** to the agent loop. They are the pi-mono
`Extension` model ported onto the §E framework-core traits.

An extension is a factory (`impl Extension`) that, during `configure`,
registers its contributions against an `ExtensionApi`. The host discovers
compiled-in extensions at startup via `inventory`, reconciles them with the
on-disk `ExtensionStateStore` (skip disabled), loads + configures each
against a stub api, then `bind_core`s the host context — after which the
runner fans lifecycle events to registered handlers. Per §F5d (T1+T2),
extension-contributed tools + slash commands are wired live into the host
per-turn: tools are registered into the per-turn `ToolRegistry` via
`register_extension_tools` in `EngineHost::build_turn_dispatcher`, and
slash commands dispatch via `try_dispatch_extension_command` in
`commands::execute` — so the agent loop sees extension tools as normal
`ToolSpec`s (main-turn only; not inherited by sub-agents — see Sandbox
Stance).

> **Slice status.** §F1 (compiled-in extensions + minimal 6-event contract),
> §F2a (full 23-variant `ExtensionEvent` set + `HandlerOutcome`
> cancel/block/transform chain + per-variant subscription + `catch_unwind`
> isolation), and §F2b (host seam wiring — honor `EmitOutcome` at the 7
> `host_executor` seams + emit 22/23 events + full e2e round-trip + live
> reload) are done. Dylib loading, `extension.toml` manifests,
> install/uninstall, `registerProvider`, renderers, shortcuts, flags, the
> `EventBus` impl are deferred to §F3–§F8. §F2c (reload sharing the engine's
> live `cancel_token`; `on_tool_progress` `Callback` hook as forward-looking
> API surface for `ToolExecutionUpdate`; `ProjectTrust` per-turn wire) is
> done. §F5 slice 1 (`ProjectTrust { FirstLoad }` emit at the onboarding
> trust-accept site — the once-per-session signal extension handlers observe
> when the user accepts the workspace trust prompt) is done; the §F5 dylib
> LOAD side (`libloading` + `extension.toml` manifests + three-shape
> discovery + project-local trust gate [Model A — consume
> `is_workspace_trusted(workspace)`/`FirstLoad`] + reload wiring) landed in
> §F5b; the INSTALL side (Git/LocalPath sources + `CargoBuilder` + `Placer`
> + `Installer` orchestrator + `/extension install`/`uninstall` real impl +
> `installed[]` provenance write) landed in §F5c. §F5e (done) adds the real
> `crate:`/`prebuilt:` source impls (was §F5c "§F5c-later" stub). §F5d (done)
> wires extension tools + slash commands live
> into the host per-turn (T1 tools via `register_extension_tools` in
> `EngineHost::build_turn_dispatcher`; T2 commands via
> `try_dispatch_extension_command` in `commands::execute`) and adds safe
> unload: `clear_tools`/`clear_commands` on reload (T3) + a two-phase
> `Library` drop (`pending_drop` + `drain_libraries_to_pending` on the UI
> thread + `drop_pending` at the engine op-loop turn boundary, T4) — so an
> uninstalled extension's live bindings clear on the next `/extension
> reload` and the dylib unloads safely at the next turn boundary (no UB; ext
> tools are main-turn-only + never inherited by subagents, §4b structural).
> (§F5 slice 1 emitted the `FirstLoad` *event* only — no dylib machinery.)
> `ToolExecutionUpdate` (needs a streaming `Tool` contract — `Tool::run`
> is one-shot), `ResourcesDiscover`, and `SessionBeforeFork` stay deferred
> with corrected rationale (see the host-seam table). Hot-load is permanently
> out (spec §2.4) — install + reload only.

## Bootstrap

Slice 1 extensions are compiled into the binary via
[`inventory::submit!`](https://docs.rs/inventory). A `pub mod` in
`crates/extensions/src/lib.rs` + a `pub mod sample_scratchpad;` declaration
is all that's required for discovery — no runtime registration call. The
host's `build_extension_runtime()` (in `crates/tui/src/core/engine.rs`)
calls `codesmith_extensions::discover_static()` once at engine build.

## In-TUI Manager

The `/extension` command group (spec §6.3) is the user-facing surface. It
dispatches via `extension_commands::try_dispatch`, wired into `execute()`
between user-defined commands and the static `match`.

| Subcommand | Aliases | Status (slice 1) | Effect |
|---|---|---|---|
| `/extension list` | `ls` | ✅ working | Lists compiled-in extensions (id + version). |
| `/extension info <id>` | | ✅ working | Shows metadata for one extension. |
| `/extension enable <id>` | | ✅ working | Marks the extension enabled in `extensions_state.toml`; takes effect on next `/extension reload` (§F2 wires live re-reconcile). |
| `/extension disable <id>` | | ✅ working | Marks the extension disabled; same reload caveat. |
| `/extension status` | | ✅ working | Reports the bound runner's generation + bound command/tool counts. |
| `/extension reload` | | ✅ working (live reload) | Re-populates the **shared runner `Arc`**: `clear_handlers` → `clear_tools` → `clear_commands` → `drain_libraries_to_pending` (§F5d T3+T4) → `invalidate` (bump generation) → `discover_static` + `discover_dylib` → reconcile against state → `load` each → `bind_core` (fresh `HostExtensionContext`). Both `App.extension_runner` and the Engine's field update live (no `Arc` swap — they share the one the engine built). The drained `Library`s are `drop_pending`'d at the next engine op-loop top (turn boundary, §F5d T4). A handler bound before reload stops observing after (cleared, not duplicated); a newly-compiled-in extension is picked up on the next reload. |
| `/extension install <source> [--global]` | | ✅ working (§F5c) | Fetches (`git:`/`path:`) → builds (`cargo build`) → places to `<root>/<id>/` + writes `extension.toml` + records `installed[]` provenance; `--global` opt-in (default project). `crate:` fetches from crates.io (sparse-index → version → sha256-verified `.crate` → `tar` extract → build); `prebuilt:<https-url>` fetches a prebuilt cdylib (HTTPS-only, optional `--checksum <sha256>`); both warn if project + untrusted; `/extension reload` to load. |
| `/extension uninstall <id>` | | ✅ working (§F5c) | Removes `<root>/<id>/` + clears `installed[]` provenance. Live tool/command bindings clear on next `/extension reload`; dylib unloads safely at next turn boundary (§F5d two-phase drop). |

## Discovery

- **Phase 1 (slice 1, static):** compiled-in extensions register an
  `ExtensionRegistration { factory, metadata }` via `inventory::submit!`.
  `discover_static()` collects every `ExtensionRegistration` linked into
  the binary. The in-tree `scratchpad` sample is the reference
  registration.
- **Phase 2 (§F5, delivered):** dylib loading from an install root +
  `extension.toml` manifest + trust prompt + the `ExtensionSource` /
  `ExtensionBuilder` / `ExtensionPlacer` trait impls (Git / LocalPath /
  CratesIo / PrebuiltDylib — §F5c wired Git/LocalPath, §F5e wired
  CratesIo/Prebuilt). Host tools/commands wired live per-turn by §F5d;
  `/extension install`/`uninstall`/`reload` real (two-phase `Library`
  drop safe at the turn boundary).

## Minimal Example

The in-tree `scratchpad` extension
(`crates/extensions/src/sample_scratchpad.rs`) contributes all three
slice-1 contribution points. Verbatim sketch:

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
  variant only — §F2a) + `generation() -> u64` for the stale-context guard.
- **`ExtensionContext`** — read-mostly host state handed to handlers:
  `cwd() / mode() / is_idle() / signal() / generation()` (real in slice 1);
  `abort() / shutdown() / compact() / get_context_usage()` (stubbed →
  `Unimplemented`; §F2 wires them).
- **`ExtensionCommandContext: ExtensionContext`** — strict sub-trait handed
  to command handlers; slice 1 adds zero session-mutation methods (the
  split exists for type-safety + §F2 growth).
- **`ExtensionEvent`** — `#[non_exhaustive]`; §F2a landed the full 23-variant
  set (§F1's 6 + 17 new: `ProjectTrust`/`ResourcesDiscover`/`Input`/
  `BeforeAgentStart`/`AgentStart`/`BeforeProviderHeaders`/
  `BeforeProviderRequest`/`AfterProviderResponse`/`ToolExecutionStart`/
  `ToolExecutionUpdate`/`ToolExecutionEnd`/`AgentEnd`/`AgentSettled`/
  `SessionBeforeSwitch`/`SessionBeforeFork`/`SessionBeforeCompact`/
  `SessionCompact`). `ExtensionEvent::kind()` maps each variant to an
  `ExtensionEventKind` discriminant for per-variant dispatch.
- **`Handler`** — §F2a outcome-returning (§F1 was observer-only; superseded):
  `async fn handle(&self, event: &ExtensionEvent, ctx: &dyn ExtensionContext)
  -> Result<HandlerOutcome, ExtensionError>`. Returns `Continue` (no change;
  proceed), `Cancel { reason }` (abort the surrounding operation — only
  meaningful for `SessionBefore*` variants), `Block { reason }` (prevent the
  operation — only meaningful for `ToolCall`), or
  `Transform(ExtensionEvent)` (replace the running event for subsequent
  handlers AND apply its actionable field at transform-capable seams —
  `Input`/`BeforeAgentStart`/`BeforeProviderRequest`/`ToolResult`).
  Variant-specific semantics are enforced by the host at each seam (§F2b); an
  out-of-place outcome (e.g. `Block` at `TurnEnd`) is ignored (treated as
  `Continue`). `emit` chains handlers in registration order so a `Transform`
  is visible to the next handler; `Cancel`/`Block` short-circuit.
- **`ToolDefinition`** — extension-side tool contract: `name / description /
  input_schema / capabilities / async execute(input, ctx)`. `execute` receives
  an `ExtensionContext` (NOT the host's `ToolContext`) — keeping extensions
  decoupled from `ToolContext`'s ~30 host-coupled fields.
- **`CommandDefinition`** — extension-side slash-command contract:
  `name / description / async run(ctx, args) -> CommandOutput`. Dispatched
  by the host's `extension_commands::try_dispatch`.
- **`ExtensionError`** — `StaleContext` (the guard signal) + `Config` /
  `Tool` / `Command` / `Conflict` / `Install` / `Load` / `Unimplemented`.

## Handlers: outcomes + per-variant subscription (§F2a)

§F1 handlers were observers (`Result<(), _>`). §F2a upgrades them to an
**outcome chain**: `Handler::handle` returns `HandlerOutcome`, and
`ExtensionRunner::emit` chains handlers in registration order — a
`Transform` is visible to the next handler, `Cancel`/`Block` short-circuit.
Each handler call is isolated behind `catch_unwind` (§8.3): a panicking
handler is logged via `tracing` and skipped — it cannot crash the agent loop —
and a handler `Err` is likewise logged + the chain continues (best-effort).

Subscribe to **all** events with `on`, or to **one** variant with
`on_variant` (the runner filters per-variant handlers by `event.kind()`
before dispatch, so a per-variant handler never sees a non-matching event):

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

> The host honors `Cancel`/`Block`/`Transform` at each seam (§F2b — see the
> host-seam mapping below). §F2a landed the contract + the chain in isolation;
> §F2b added `#[must_use]` on `EmitOutcome` so every emit site must inspect the
> outcome (observe-only seams use `let _ =`).

## Host seam mapping (§F2b)

§F2b wires each `ExtensionEvent` variant to its host emit site + defines
which `HandlerOutcome` the host honors at that seam. An out-of-place outcome
(e.g. `Block` at `TurnEnd`) is ignored — treated as `Continue` — so a handler
that returns the wrong capability for its variant is a no-op, not an error.
`EmitOutcome` is `#[must_use]`, so every emit site binds the result (observe-only
seams use `let _ =`; capability seams inspect `out.outcome` / `out.event`).

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
| `ToolCall(ToolCallEvent)` | `host_executor` parallel + serial tool dispatch | **Block** | skips approval + `tool.run` → `Err(ToolError::permission_denied(reason))`, `blocked = true` |
| `ToolResult(ToolResultEvent)` | `host_executor` parallel + serial, emit reordered BEFORE `on_tool_end` | **Transform** | replaces the result; `on_tool_end` + downstream `outcomes[idx].result` see the transformed result |
| `ToolExecutionStart` | `host_executor` tool closure (before `tool.run`) | observe | — |
| `ToolExecutionEnd` | `host_executor` tool closure (after `tool.run`) | observe | — |
| `AgentEnd` | `host_executor::run_inner` each `return Ok(...)` | observe | — |
| `AgentSettled` | `engine/mod.rs` post-run drain (after capacity apply) | observe | — |
| `SessionBeforeCompact` | `host_executor::run_compaction` after `should_compact` gate | **Cancel** | skips compaction (`return`) |
| `SessionCompact` | `host_executor::run_compaction` after summary applied | observe | — |
| `SessionBeforeSwitch` | `tui/ui.rs` `switch_workspace` entry | **Cancel** | aborts the workspace switch |
| `ProjectTrust` | `HostServices::build_turn_dispatcher` (+ `spawn_subagent`) after `build_tool_context_for` (per-turn `Trusted`/`Untrusted`); onboarding trust-accept `tui/ui.rs` `TrustDirectory` y/Y/1 arm after `app.trust_mode = true` (`FirstLoad`) | observe | per-turn `Trusted`/`Untrusted` from `session.trust_mode`; `FirstLoad` once per onboarding trust acceptance (`TrustReason::FirstLoad`) — distinct from the runtime `trust_mode` toggle (`/trust on`), YOLO entry, and persisted-trust startup, which surface per-turn as `Trusted`/`Untrusted`, not `FirstLoad` |
| `—` (dylib LOAD, not an event) | `populate_extension_runtime` (`tui/src/core/engine.rs`) after `discover_static` | n/a (load phase) | §F5b: `discover_dylib(&global_roots, &project_roots)` → `apply_trust_gate(discovered, !is_workspace_trusted(workspace))` drops project-local (`global == false`) → `state.is_enabled` reconcile → `ExtensionRunner::load_dylib` on the OS-thread load runtime; reload auto-picks-up via `reload_extension_runtime`→`populate`. `ExtensionRunner.libraries` holds `Library` handles; on `/extension reload` they `drain_libraries_to_pending` to `pending_drop` (§F5d T4, alongside `clear_tools`/`clear_commands`) + the engine op-loop `drop_pending`s them at the next turn boundary. Lockstep `*mut dyn Extension` via `codesmith_register_extension` (§8.2). |
| `ResourcesDiscover` | — (deferred §F2c) | observe | the only in-process host site is the `list_mcp_resources` pseudo-tool dispatch in `McpPool` (`agent-runtime/src/mcp.rs:3014`), already bracketed by `ToolCall`/`ToolResult` — firing `ResourcesDiscover` there conflates with tool execution and `DiscoverReason` has no clean mapping; no dedicated Startup/Manual/Reload discover seam with the runner `Arc`. The `tui/mcp_server.rs` stdio site is a separate process. (Earlier 'separate process' framing over-stated the blocker.) |
| `SessionBeforeFork` | — (deferred §F2c) | **Cancel** | the live in-TUI backtrack path (`apply_backtrack`, `tui/ui.rs:6922`) is an in-place **rewind** (`truncate_history_to`/`api_messages.truncate`), not a **fork** (new-thread creation) — mislabeled if wired as `SessionBeforeFork`. Genuine fork primitives are dead (`fork_at_user_message`, `#[allow(dead_code)]`, zero non-test callers) or HTTP-only (`fork_thread`, runtime-api, no `App.extension_runner`). tui **does** construct a `RuntimeThreadManager` via `TaskManager::start` (`ui.rs:507`→`task_manager.rs:465`) — the earlier 'no ctor' claim was wrong. (Spec could redefine the event to cover rewind; flagged to spec owner, not done here.) |
| `ToolExecutionUpdate` | — (deferred §F2c) | observe | no streaming `Tool` contract — `Tool::run` is one-shot (`agent/src/tools/mod.rs:71`), so there is no mid-execution progress stream to hook. The `on_tool_progress` `Callback` hook is landed (§F2c T1) as forward-looking API surface; the emit site awaits a streaming `Tool` variant (§F-later). (Earlier 'no `on_tool_progress` hook' framing was the surface symptom, not the root cause.) |

> The `Transform` payload's actionable field is applied at the seam AFTER the
> full handler chain runs (so a `Transform` from handler N is visible to
> handler N+1 as the running event). `Cancel`/`Block` short-circuit the chain.
> The terminal `EmitOutcome.outcome` is never `Transform` (folded into
> `EmitOutcome.event`); capability seams inspect `out.outcome` for
> `Cancel`/`Block` and `out.event` for the transformed actionable field.

## Sandbox Stance

CodeSmith does **not** sandbox extensions (spec §8.1). Extensions run in the
same process as the agent loop with full host access — **trust the source**.
For untrusted extensions, containerize the whole CodeSmith process. Project
local dylib install (phase 2, §F5) will require a trust prompt before the
first load. The `ProjectTrust { FirstLoad }` event (§F5 slice 1) now fires at
onboarding trust acceptance — it is an *observe-only signal* extension
handlers can subscribe to, distinct from (and not delivering) the phase-2
dylib loader that *consumes* project-local trust. The dylib loader (`libloading` + lockstep `*mut dyn Extension` via
`codesmith_register_extension`), `extension.toml` manifest, and project-local
discovery trust gate (Model A — `apply_trust_gate` drops project-local
(`global == false`) dylibs when `is_workspace_trusted(workspace)` is false; the
`ProjectTrust { FirstLoad }` event flips that trust at onboarding accept) are
§F5b (done). §F5c (done) adds the INSTALL side: `/extension install` fetches
(`git:`/`path:`) → `cargo build --release --locked` → `Placer` writes
`<root>/<id>/<default_dylib_filename(id)>` → `extension.toml` → `installed[]`
provenance. `cargo build` runs the source's `build.rs` — **arbitrary code
execution, accepted per §8.1 (trust the source)**; containerize for untrusted
sources. Install is trust-agnostic (it only *reads* trust to warn: a
project-local install won't load until the workspace is trusted). A loaded
dylib runs in-process with full host access — trust the source; containerize
for untrusted sources. `crate:`/`prebuilt:` sources shipped in §F5e (real
`CratesIoSource`/`PrebuiltDylibSource` impls; was §F5c "§F5c-later" stub).
§F5d (done) wires extension tools +
slash commands live into the host per-turn `ToolRegistry` (main-turn only) +
adds safe unload:

- **Ext tools are main-turn-only (§4b structural):** extension tools are
  registered into the host's per-turn `ToolRegistry` (the main agent turn),
  NOT inherited by sub-agents. This is **structural, not a guard**:
  `SubAgentRuntime` has no `extension_runner` field +
  `SubAgentToolRegistry::new` rebuilds its own fresh built-in `ToolRegistry`
  — so ext tools can never reach a sub-agent's effective set, regardless of
  `inherit_full_registry`. No provenance marker / force-subset / runtime
  subagent-check is needed.
- **Two-phase `Library` drop (§4a):** reload on the UI thread MOVES orphaned
  `Library`s to `pending_drop` (`drain_libraries_to_pending`); the engine
  op-loop top DROPs them (`drop_pending`) at the one moment the main-thread
  `HostAgentExecutor` (the only in-flight dylib `Arc` holder) is already
  dropped between turns. This makes `/extension reload` + uninstall safe
  concurrent with in-flight turns.
- **Miri note:** the two-phase drop's safety is proven by the invariant +
  single-call-site discipline; dylib+Miri is unreliable (libloading's
  `Library::drop` runs `dlclose`/`FreeLibrary`, which Miri doesn't model),
  so the invariant — not a Miri run — is the proof.

Slice 1's compiled-in extensions are trusted by construction (they ship in
the binary).

## Troubleshooting

- **`/extension list` shows nothing.** No `inventory::submit!` reached the
  link — confirm the extension's crate is a workspace member + that
  `crates/extensions/src/lib.rs` declares its module. `cargo test -p
  codesmith-extensions scratchpad_is_discoverable` proves the registration
  is wired.
- **`/extension status` says "not bound".** The engine hasn't built yet
  (pre-startup), or `app.extension_runner` wasn't copied from the handle
  (`crates/tui/src/tui/ui.rs` after `spawn_engine`).
- **Handler returns `Continue` but nothing changes.** §F2a handlers return
  `HandlerOutcome`; `Continue` means "no change" by design. To
  cancel/block/transform, return the matching variant — and note
  variant-specific semantics (a `Block` at a non-`ToolCall` seam is ignored;
  see the host-seam mapping below). `emit` isolates each handler call behind
  `catch_unwind` (§8.3): a panicking handler is logged via `tracing` and
  skipped — it cannot crash the agent loop — and a handler `Err` is likewise
  logged + the chain continues.
- **`configure` captured an `Arc<dyn ExtensionApi>` that now returns
  `StaleContext`.** The runner was `invalidate()`d (via `/extension reload`
  or a future reload/fork/switch); capture a fresh api or check
  `generation()` against the live runner's before use.
- **Tests panic at `tokio runtime blocking/shutdown.rs`.** A nested tokio
  runtime was created + dropped from within a runtime worker thread.
  `build_extension_runtime` drives `configure` on a plain OS thread
  (`std::thread::scope`) precisely to avoid this — if you see it, the
  thread::scope guard was bypassed.
