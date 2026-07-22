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
runner fans lifecycle events to registered handlers and the agent loop sees
extension tools as normal `ToolSpec`s.

> **Slice 1 (§F1) scope.** Only compiled-in extensions are supported. Dylib
> loading, `extension.toml` manifests, install/uninstall, `registerProvider`,
> renderers, shortcuts, flags, the full ~30-event lifecycle, and the
> `EventBus` impl are deferred to §F2–§F8. Hot-load is permanently out
> (spec §2.4) — install + reload only.

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
| `/extension reload` | | ✅ working (invalidate-only) | Invalidates the runner's generation (stale-context guard trips for captured `Arc<dyn ExtensionApi>`s). Re-discovery + re-load happens on next engine build; live reload is §F2. |
| `/extension install <source>` | | 🚧 stub "phase 2" | Returns an error pointing to §F5 (dylib loader). |
| `/extension uninstall <id>` | | 🚧 stub "phase 2" | Returns an error pointing to §F5. |

## Discovery

- **Phase 1 (slice 1, static):** compiled-in extensions register an
  `ExtensionRegistration { factory, metadata }` via `inventory::submit!`.
  `discover_static()` collects every `ExtensionRegistration` linked into
  the binary. The in-tree `scratchpad` sample is the reference
  registration.
- **Phase 2 (§F5, deferred):** dylib loading from an install root +
  `extension.toml` manifest + trust prompt + the `ExtensionSource` /
  `ExtensionBuilder` / `ExtensionPlacer` trait impls. Slice 1 ships the
  **traits only** — no impls.

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
  `register_command(Box<dyn CommandDefinition>)` / `on(Arc<dyn Handler>)` +
  `generation() -> u64` for the stale-context guard.
- **`ExtensionContext`** — read-mostly host state handed to handlers:
  `cwd() / mode() / is_idle() / signal() / generation()` (real in slice 1);
  `abort() / shutdown() / compact() / get_context_usage()` (stubbed →
  `Unimplemented`; §F2 wires them).
- **`ExtensionCommandContext: ExtensionContext`** — strict sub-trait handed
  to command handlers; slice 1 adds zero session-mutation methods (the
  split exists for type-safety + §F2 growth).
- **`ExtensionEvent`** — `#[non_exhaustive]` minimal 6-variant set:
  `SessionStart { reason }` / `TurnStart { turn_id }` /
  `ToolCall(ToolCallEvent)` / `ToolResult(ToolResultEvent)` /
  `TurnEnd { turn_id, reason }` / `SessionShutdown`. The remaining ~25
  variants are §F2.
- **`Handler`** — observer-only in slice 1:
  `async fn handle(&self, event: &ExtensionEvent, ctx: &dyn ExtensionContext)
  -> Result<(), ExtensionError>`. `HandlerOutcome` (cancel/transform/block)
  is §F2.
- **`ToolDefinition`** — extension-side tool contract: `name / description /
  input_schema / capabilities / async execute(input, ctx)`. `execute` receives
  an `ExtensionContext` (NOT the host's `ToolContext`) — keeping extensions
  decoupled from `ToolContext`'s ~30 host-coupled fields.
- **`CommandDefinition`** — extension-side slash-command contract:
  `name / description / async run(ctx, args) -> CommandOutput`. Dispatched
  by the host's `extension_commands::try_dispatch`.
- **`ExtensionError`** — `StaleContext` (the guard signal) + `Config` /
  `Tool` / `Command` / `Conflict` / `Install` / `Load` / `Unimplemented`.

## Sandbox Stance

CodeSmith does **not** sandbox extensions (spec §8.1). Extensions run in the
same process as the agent loop with full host access — **trust the source**.
For untrusted extensions, containerize the whole CodeSmith process. Project
local dylib install (phase 2, §F5) will require a trust prompt before the
first load. Slice 1's compiled-in extensions are trusted by construction
(they ship in the binary).

## Troubleshooting

- **`/extension list` shows nothing.** No `inventory::submit!` reached the
  link — confirm the extension's crate is a workspace member + that
  `crates/extensions/src/lib.rs` declares its module. `cargo test -p
  codesmith-extensions scratchpad_is_discoverable` proves the registration
  is wired.
- **`/extension status` says "not bound".** The engine hasn't built yet
  (pre-startup), or `app.extension_runner` wasn't copied from the handle
  (`crates/tui/src/tui/ui.rs` after `spawn_engine`).
- **Handler silently does nothing.** Slice 1 handlers are observers — they
  cannot cancel/transform the turn (§F2). `emit` discards handler errors
  best-effort per §8.3 (one failing handler does not block others); §F2
  hardens with `catch_unwind`.
- **`configure` captured an `Arc<dyn ExtensionApi>` that now returns
  `StaleContext`.** The runner was `invalidate()`d (via `/extension reload`
  or a future reload/fork/switch); capture a fresh api or check
  `generation()` against the live runner's before use.
- **Tests panic at `tokio runtime blocking/shutdown.rs`.** A nested tokio
  runtime was created + dropped from within a runtime worker thread.
  `build_extension_runtime` drives `configure` on a plain OS thread
  (`std::thread::scope`) precisely to avoid this — if you see it, the
  thread::scope guard was bypassed.
