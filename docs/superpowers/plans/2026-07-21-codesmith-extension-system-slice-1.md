# CodeSmith Extension System — Slice 1 (§F1) Implementation Plan

- **Date:** 2026-07-21
- **Branch:** `feat/pluggable-framework-core`
- **Spec:** `docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md` (§10.1 scope)
- **Scope:** Slice 1 only — the foundational core (phase 1, static loading). The full ~30-event lifecycle, cancel/transform/block chains, `EventBus` impl, `registerProvider`, dylib loading, install-source impls, renderers, shortcuts/flags, embed API are **deferred** to §F2–§F8 (spec §10.2). This plan opens ROADMAP §F.

## How to read this plan

- **Read the spec first** (`docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md`). This plan is the bite-sized TDD breakdown of its §10.1; it does not re-justify the design.
- Every type/function referenced in a task's code is defined **in this plan** (no external "see foo" placeholders). Where a task references an *existing* CodeSmith type, the file:line is cited so you can verify the shape.
- **TDD discipline:** every task opens with a failing test (Red), implements the minimum to pass (Green), then runs the exact verify command. Do not move to the next task until Green.
- **Edit ordering inside `host_executor.rs`** (Task 6): use the `Edit` tool with **verbatim surrounding code as the anchor** (it matches strings, not line numbers) and edit **bottom-up** (highest line first) so earlier anchors stay valid. Line numbers below are from HEAD pre-edit for orientation only.

## Conventions mirrored (cite these, do not re-invent)

| Convention | Source | Why it matters |
|---|---|---|
| Framework traits live in `codesmith-agent`, host-agnostic | `crates/agent/src/{tools,callback,executor,memory,provider}/mod.rs` | Extension traits go in `crates/agent/src/extension.rs` (new module) |
| Adapter/bridge in `codesmith-agent-runtime`, mirror `ToolSpecAdapter` | `crates/agent-runtime/src/tools/framework_adapter.rs:42-87` | `ExtensionToolSpecAdapter` mirrors this exactly |
| `ToolSpec` is `#[async_trait]` + `Send + Sync`, 5 required methods | `crates/agent-runtime/src/tools/spec.rs:713-812` | Adapter must impl all 5 |
| `ToolRegistry::register` funnels through `build_tool` fail-closed chokepoint | `crates/agent-runtime/src/tools/registry.rs:74` + `:497-517` | Adapter's `input_schema()` MUST be object-rooted + name `^[a-zA-Z0-9_-]{1,64}$` or it's silently swapped for `FailClosedTool` |
| Probe-collaborator pattern: `Option<…>` + `with_*(self, Option<…>) -> Self` builder | `HostAgentExecutor` fields `crates/agent-runtime/src/engine/host_executor.rs:1469-1710` | `with_extension_runner` follows this shape |
| `CallbackBridge` fan-out: clone Arcs into owned locals, `Box::pin(async move { … })` | `crates/agent-runtime/src/callback_bridge.rs:148-292` | `ExtensionRunner::emit` mirrors this |
| `SkillStateStore` TOML + atomic-write + malformed→default | `crates/tui/src/skill_state.rs` (full file) | `ExtensionStateStore` mirrors this verbatim |
| Command dispatch three-tier: user-defined → static `match` → skills fallthrough | `crates/tui/src/commands/mod.rs:572-695` | Extension tier inserts between user-defined and static match |
| `user_commands::try_dispatch_user_command` signature/shape | `crates/tui/src/commands/user_commands.rs:193-227` | `extension_commands::try_dispatch` mirrors this |
| Mock provider = "reference sample" pattern | `crates/providers/src/mock.rs` + `crates/providers/src/lib.rs:100-188` | In-tree sample extension mirrors this |
| ROADMAP slice progress entry convention | `ROADMAP.md:2381-2408` (slice 53 entry) | §F1 entry mirrors this |
| ARCHITECTURE.md §E section shape | `ARCHITECTURE.md:93-290` | New §F section mirrors this |

## Decisions this plan authors (spec §11 open questions)

The spec left four shapes as "slice 1 定". This plan resolves them:

1. **Sample extension form (§11.1):** a `scratchpad` extension — contributes a `scratch` tool (writes/reads a per-session scratch string), a `/scratch` command (prints the scratchpad), and a `TurnStart` handler (logs the turn id). Exercises all three slice-1 contribution points; minimal enough to be its own test.
2. **`ExtensionCommandContext` vs `ExtensionContext` split (§11.2):** `ExtensionCommandContext: ExtensionContext` (sub-trait). Slice 1 adds **zero** session-mutation methods (the split exists for type-safety + §F2 growth). Command handlers return a framework-agnostic `CommandOutput { Message(String) }`; the tui bridge converts to `CommandResult::message`. A `SendMessage(String)` variant is added so the sample command can trigger the agent — mapped to `AppAction::SendMessage` by the bridge. This keeps `codesmith-extensions` decoupled from `codesmith-tui`.
3. **Handler trait shape (§11.3):** a **single** `Handler` trait: `async fn handle(&self, event: &ExtensionEvent, ctx: &dyn ExtensionContext) -> Result<(), ExtensionError>`. Subscribed via `api.on(handler)` to **all** events (slice 1 has 6); the handler matches internally. Dyn-safe (`Arc<dyn Handler>`). Per-variant subscription + `HandlerOutcome` (cancel/transform/block) is §F2 — slice 1 handlers are **observers only** (best-effort fan-out per §8.3).
4. **§10.3 vs §10.2 tension:** §10.3 mentions "cancel / transform / block 语义" in the test list, but §10.2 explicitly defers the full event set + cancel/transform/block chains to §F2. This plan resolves in favor of §10.2 (the explicit defer): slice 1 Handler is observer-only; `HandlerOutcome` is **not** introduced. The §10.3 phrasing is treated as aspirational copy from the full model. The plan notes this in the §F1 ROADMAP entry.

## Preflight (Task 0)

Establish the green baseline before any change. Run once, record pass counts:

```bash
cd /Users/camile/Work/Rust/CodeSmith
cargo +1.90.0 build --workspace 2>&1 | tail -5
cargo +1.90.0 test -p codesmith-agent --lib 2>&1 | tail -3
cargo +1.90.0 test -p codesmith-agent-runtime --lib 2>&1 | tail -3   # expect 1149 pass + 2 ignored (slice 53 baseline)
cargo +1.90.0 test -p codesmith-tui --bin 2>&1 | tail -3              # expect 2844 pass + 2 ignored (slice 52 baseline)
```

If any baseline is red, **stop** — fix the environment before starting slice 1.

---

## Task 1 — Core traits + types in `codesmith-agent`

**Files:**
- `crates/agent/Cargo.toml` (add 2 deps)
- `crates/agent/src/lib.rs` (add `pub mod extension;`)
- `crates/agent/src/extension.rs` (NEW — the whole contract)

**Why `#[async_trait]` here (deviation from existing `codesmith-agent` style):** the existing `Tool`/`Callback`/`AgentExecutor` traits use the manual `Pin<Box<dyn Future + Send + '_>>` pattern. The extension traits are implemented by **extension authors** in external crates — `#[async_trait]` is markedly friendlier for them, matches the spec literally (line 192 `#[async_trait] pub trait ExtensionContext`), and matches the `ToolSpec`/`HookSink` convention in `codesmith-agent-runtime`/`codesmith-hooks`. The cost is two new deps on the core crate (`async-trait`, `tokio-util`); both are workspace staples. This is the documented trade-off.

### 1.1 Add deps to `crates/agent/Cargo.toml`

Append to `[dependencies]`:

```toml
async-trait = "0.1"
# §F1 — ExtensionContext::signal() returns tokio_util::sync::CancellationToken
# (the codebase's standard cancel handle; host_executor.rs uses the same).
# `sync` module is available on default features.
tokio-util = { version = "0.7.16" }
```

### 1.2 Declare the module in `crates/agent/src/lib.rs`

Add to the `pub mod` block (after `pub mod tools;`, keeping alphabetical-ish order):

```rust
pub mod extension;
```

### 1.3 Write `crates/agent/src/extension.rs` (Red → Green)

**Red:** the file must compile-define every type below and a co-located test module exercises a mock `Extension` + `Handler` + `ToolDefinition` + `CommandDefinition` against the trait shapes. Write the test module first (it fails to compile until the types exist), then the types.

Write the full file:

```rust
//! Extension system framework traits — the pi-mono `Extension` model port.
//!
//! Mirrors §E's framework-core trait pattern: host-agnostic contracts in
//! `codesmith-agent` so any host can drive an extension system without
//! depending on `codesmith-extensions` (the runtime) or
//! `codesmith-agent-runtime` (the adapters). The runtime lives in
//! `codesmith-extensions`; the adapters that bridge extension registrations
//! onto the production `ToolSpec` / command dispatch / `HostAgentExecutor`
//! seams live in `codesmith-agent-runtime` (mirroring `ToolSpecAdapter` /
//! `CallbackBridge`).
//!
//! Slice 1 (§F1) lands the minimal contract: the `Extension` factory, the
//! `ExtensionApi` registration surface, `ExtensionContext` /
//! `ExtensionCommandContext`, the minimal `ExtensionEvent` set (6 variants;
//! `#[non_exhaustive]`), the `Handler` observer trait, and the
//! `ToolDefinition` / `CommandDefinition` contribution contracts. The full
//! ~30-event lifecycle + cancel/transform/block chains + EventBus impl +
//! dylib loading + install-source impls are deferred to §F2–§F8
//! (ROADMAP §F).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use codesmith_tools::{ToolCapability, ToolError, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// === Error type ============================================================

/// Errors an extension or the extension runtime can produce.
///
/// `StaleContext` is the stale-context guard signal (spec §7.3): a handler or
/// command captured an `ExtensionApi`/`ExtensionContext` whose generation no
/// longer matches the live runtime (the runtime was `invalidate()`d by a
/// reload / session-replace / fork / switch). Slice 1 handlers are
/// observers; `Conflict` + `Install`/`Load` are present for §F2+ but unused
/// by slice-1 code paths.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("extension context is stale (generation mismatch) — runtime was invalidated")]
    StaleContext,
    #[error("extension configuration error: {0}")]
    Config(String),
    #[error("extension tool '{tool}' failed: {message}")]
    Tool { tool: String, message: String },
    #[error("extension command '{command}' failed: {message}")]
    Command { command: String, message: String },
    #[error("extension resource conflict: {0}")]
    Conflict(String),
    #[error("extension install failed: {0}")]
    Install(String),
    #[error("extension load failed: {0}")]
    Load(String),
    #[error("extension action not implemented in this slice: {0}")]
    Unimplemented(String),
}

impl From<ToolError> for ExtensionError {
    /// Wrap a [`ToolError`] from a `ToolDefinition::execute` into the
    /// extension error surface so the `ExtensionToolSpecAdapter` can map
    /// back to `ToolError` losslessly (Task 5).
    fn from(err: ToolError) -> Self {
        ExtensionError::Tool {
            tool: String::new(),
            message: err.to_string(),
        }
    }
}

// === Metadata =============================================================

/// Stable identity + display metadata for an extension.
///
/// `id` is the stable key (lowercase, `-`-separated, matches
/// `^[a-z0-9][a-z0-9-]*$`); `name` is the human display; `version` mirrors
/// the crate version. Slice 1 populates from `ExtensionMetadata::new(id)`.
#[derive(Debug, Clone)]
pub struct ExtensionMetadata {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
}

impl ExtensionMetadata {
    #[must_use]
    pub const fn new(id: &'static str) -> Self {
        Self {
            id,
            name: id,
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

// === ExtensionEvent (minimal set, §10.1) ===================================

/// Why a session started. Mirrors pi-mono `SessionReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionReason {
    Startup,
    Reload,
    New,
    Resume,
    Fork,
}

/// Why a turn ended. Mirrors the framework `StopReason` shape without the
/// `Error(String)` payload (slice 1 keeps events light; §F2 adds richer
/// payloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEndReason {
    NoToolCalls,
    MaxSteps,
    Interrupted,
    Error,
}

/// Payload for `ExtensionEvent::ToolCall`.
#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Payload for `ExtensionEvent::ToolResult`.
#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub id: String,
    pub name: String,
    pub result: Result<ToolResult, ToolError>,
}

/// Lifecycle events. Slice 1 minimal set (spec §10.1):
/// `SessionStart` / `TurnStart` / `ToolCall` / `ToolResult` / `TurnEnd` /
/// `SessionShutdown`. `#[non_exhaustive]` so §F2 can add the remaining ~25
/// variants without breaking match arms. Handler dispatch is open (any
/// `Handler` may subscribe to any variant).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ExtensionEvent {
    SessionStart { reason: SessionReason },
    TurnStart { turn_id: String },
    ToolCall(ToolCallEvent),
    ToolResult(ToolResultEvent),
    TurnEnd { turn_id: String, reason: TurnEndReason },
    SessionShutdown,
}

// === ExtensionContext =====================================================

/// The execution mode of the host. Mirrors pi-mono's `ExtensionContext.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionMode {
    Tui,
    Rpc,
    Json,
    Print,
}

/// Coarse context-usage snapshot. Slice 1 fields are advisory; §F2 wires
/// real values from the host's compaction/capacity state.
#[derive(Debug, Clone, Default)]
pub struct ContextUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub context_window: u64,
}

/// Passed to every event handler. Read-mostly; session-mutation actions live
/// on the sub-trait `ExtensionCommandContext` (handed only to command
/// handlers). Mirrors pi-mono `ExtensionContext` (spec §4 line 193-204).
///
/// Slice 1: the observation methods (`cwd`, `mode`, `is_idle`, `signal`,
/// `generation`) are real (host-backed). The action methods (`abort`,
/// `shutdown`, `compact`, `get_context_usage`) are stubbed by the host impl
/// to return `Err(ExtensionError::Unimplemented)` — §F2 wires them. Handlers
/// are observers in slice 1 and do not call the action methods.
#[async_trait]
pub trait ExtensionContext: Send + Sync {
    fn cwd(&self) -> &Path;
    fn mode(&self) -> ExtensionMode;
    fn is_idle(&self) -> bool;
    fn signal(&self) -> CancellationToken;
    /// The stale-context generation counter (spec §7.3). A handler/command
    /// captures this at subscription time; on use it compares against the
    /// live `ExtensionApi::generation()` and returns `StaleContext` on
    /// mismatch.
    fn generation(&self) -> u64;

    async fn abort(&self) -> Result<(), ExtensionError> {
        Err(ExtensionError::Unimplemented("abort".into()))
    }
    async fn shutdown(&self) -> Result<(), ExtensionError> {
        Err(ExtensionError::Unimplemented("shutdown".into()))
    }
    async fn compact(&self) -> Result<(), ExtensionError> {
        Err(ExtensionError::Unimplemented("compact".into()))
    }
    async fn get_context_usage(&self) -> Result<ContextUsage, ExtensionError> {
        Err(ExtensionError::Unimplemented("get_context_usage".into()))
    }
}

/// Handed to command handlers. A strict sub-trait of `ExtensionContext`:
/// slice 1 adds zero session-mutation methods (the split exists for
/// type-safety + §F2 growth — pi-mono hands command handlers a richer
/// context with `sendMessage`/`appendEntry`/etc.). Command handlers receive
/// this and return a framework-agnostic [`CommandOutput`].
#[async_trait]
pub trait ExtensionCommandContext: ExtensionContext {}

// === Contribution contracts ===============================================

/// The output a command handler returns. Framework-agnostic so
/// `codesmith-extensions` stays decoupled from `codesmith-tui`; the tui
/// bridge (Task 8) maps `SendMessage` → `AppAction::SendMessage` and
/// `Message` → `CommandResult::message`.
#[derive(Debug, Clone)]
pub enum CommandOutput {
    /// Display a message to the user.
    Message(String),
    /// Send a message into the agent conversation (triggers the agent).
    SendMessage(String),
}

/// Extension-side tool contract. The host's `ExtensionToolSpecAdapter` (in
/// `codesmith-agent-runtime`, Task 5) wraps a `Box<dyn ToolDefinition>` into
/// a `ToolSpec` so the agent loop sees a normal tool. `execute` receives an
/// `ExtensionContext` (NOT the host's `ToolContext`) — keeping extensions
/// decoupled from `ToolContext`'s ~30 host-coupled fields.
#[async_trait]
pub trait ToolDefinition: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }
    async fn execute(
        &self,
        input: Value,
        ctx: &dyn ExtensionContext,
    ) -> Result<ToolResult, ExtensionError>;
}

/// Extension-side slash-command contract. Registered via
/// `ExtensionApi::register_command`; dispatched by the host's
/// `extension_commands::try_dispatch` (Task 8) which calls `run`.
#[async_trait]
pub trait CommandDefinition: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn run(
        &self,
        ctx: &dyn ExtensionCommandContext,
        args: &str,
    ) -> Result<CommandOutput, ExtensionError>;
}

// === Handler (observer, slice 1) ==========================================

/// Lifecycle event observer. Slice 1: observer-only — returns `Ok(())` or
/// an `ExtensionError`; the runner fans out best-effort (per §8.3
/// catch_unwind + try; one failing handler does not block others).
/// `HandlerOutcome` (cancel/transform/block) is §F2.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        ctx: &dyn ExtensionContext,
    ) -> Result<(), ExtensionError>;
}

// === ExtensionApi (registration surface, two-phase) =======================

/// The imperative registration surface an `Extension::configure` receives.
/// Two-phase (spec §4 key semantics): the **stub** impl (constructed at
/// load time by `ExtensionRunner`, in `codesmith-extensions`) queues
/// registrations into `pending_*`; `ExtensionRunner::bind_core` swaps in the
/// **real** impl which flushes `pending_*` into the host registries.
///
/// `generation()` exposes the stale-context counter so a handler/command
/// captured `Arc<dyn ExtensionApi>` can assert liveness before use.
#[async_trait]
pub trait ExtensionApi: Send + Sync {
    fn generation(&self) -> u64;

    fn register_tool(&self, tool: Box<dyn ToolDefinition>) -> Result<(), ExtensionError>;
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError>;
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError>;
}

// === Extension (the factory) ==============================================

/// An extension: a factory that receives an `ExtensionApi` and registers its
/// contributions (tools / commands / handlers). Mirrors pi-mono
/// `ExtensionFactory = (pi: ExtensionAPI) => void | Promise<void>`.
///
/// Implement this in an extension crate (or the in-tree sample, Task 10);
/// register an `ExtensionRegistration { factory: || Box::new(MyExt), metadata }`
/// via `inventory::submit!` (Task 4) for compiled-in discovery.
#[async_trait]
pub trait Extension: Send + Sync {
    fn metadata(&self) -> &ExtensionMetadata;

    /// Register this extension's contributions against `api`. Called once
    /// per load; the `api` is the stub (pre-`bind_core`) — registrations
    /// queue into `pending_*` until the host binds the real impl.
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError>;
}

// === Tests (Red-first: shapes compile + a mock round-trips) ===============

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct TestContext {
        cwd: PathBuf,
        gen: u64,
        signal: CancellationToken,
        observed: Mutex<Vec<&'static str>>,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                cwd: PathBuf::from("."),
                gen: 1,
                signal: CancellationToken::new(),
                observed: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ExtensionContext for TestContext {
        fn cwd(&self) -> &Path {
            &self.cwd
        }
        fn mode(&self) -> ExtensionMode {
            ExtensionMode::Tui
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn signal(&self) -> CancellationToken {
            self.signal.clone()
        }
        fn generation(&self) -> u64 {
            self.gen
        }
    }

    #[async_trait]
    impl ExtensionCommandContext for TestContext {}

    // A recording handler — proves the trait shape lets a handler observe
    // every minimal-set variant.
    struct RecordingHandler {
        seen: Mutex<Vec<&'static str>>,
    }

    #[async_trait]
    impl Handler for RecordingHandler {
        async fn handle(
            &self,
            event: &ExtensionEvent,
            _ctx: &dyn ExtensionContext,
        ) -> Result<(), ExtensionError> {
            let label = match event {
                ExtensionEvent::SessionStart { .. } => "SessionStart",
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::ToolCall(_) => "ToolCall",
                ExtensionEvent::ToolResult(_) => "ToolResult",
                ExtensionEvent::TurnEnd { .. } => "TurnEnd",
                ExtensionEvent::SessionShutdown => "SessionShutdown",
            };
            self.seen.lock().unwrap().push(label);
            Ok(())
        }
    }

    struct EchoTool;
    #[async_trait]
    impl ToolDefinition for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes the input text."
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(
            &self,
            input: Value,
            _ctx: &dyn ExtensionContext,
        ) -> Result<ToolResult, ExtensionError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ToolResult::success(format!("echo:{text}")))
        }
    }

    struct GreetCommand;
    #[async_trait]
    impl CommandDefinition for GreetCommand {
        fn name(&self) -> &str {
            "greet"
        }
        fn description(&self) -> &str {
            "Prints a greeting."
        }
        async fn run(
            &self,
            _ctx: &dyn ExtensionCommandContext,
            args: &str,
        ) -> Result<CommandOutput, ExtensionError> {
            Ok(CommandOutput::Message(format!("hello, {args}")))
        }
    }

    #[test]
    fn tool_definition_execute_returns_tool_result() {
        let tool = EchoTool;
        let ctx = TestContext::new();
        let out = futures::executor::block_on(tool.execute(json!({"text":"hi"}), &ctx)).unwrap();
        assert!(out.success);
        assert_eq!(out.content, "echo:hi");
    }

    #[test]
    fn command_definition_run_returns_message() {
        let cmd = GreetCommand;
        let ctx = TestContext::new();
        let out = futures::executor::block_on(cmd.run(&ctx, "world")).unwrap();
        let CommandOutput::Message(m) = out else {
            panic!("expected Message");
        };
        assert_eq!(m, "hello, world");
    }

    #[test]
    fn handler_observes_every_minimal_event_variant() {
        let h = RecordingHandler {
            seen: Mutex::new(Vec::new()),
        };
        let ctx = TestContext::new();
        let events = vec![
            ExtensionEvent::SessionStart {
                reason: SessionReason::Startup,
            },
            ExtensionEvent::TurnStart {
                turn_id: "t1".into(),
            },
            ExtensionEvent::ToolCall(ToolCallEvent {
                id: "c1".into(),
                name: "echo".into(),
                input: json!({}),
            }),
            ExtensionEvent::ToolResult(ToolResultEvent {
                id: "c1".into(),
                name: "echo".into(),
                result: Ok(ToolResult::success("ok")),
            }),
            ExtensionEvent::TurnEnd {
                turn_id: "t1".into(),
                reason: TurnEndReason::NoToolCalls,
            },
            ExtensionEvent::SessionShutdown,
        ];
        for ev in &events {
            futures::executor::block_on(h.handle(ev, &ctx)).unwrap();
        }
        let seen = h.seen.lock().unwrap();
        assert_eq!(
            *seen,
            vec![
                "SessionStart",
                "TurnStart",
                "ToolCall",
                "ToolResult",
                "TurnEnd",
                "SessionShutdown",
            ]
        );
    }

    #[test]
    fn extension_error_from_tool_error_wraps_message() {
        let te = ToolError::execution_failed("boom");
        let ee: ExtensionError = te.into();
        match ee {
            ExtensionError::Tool { message, .. } => assert!(message.contains("boom")),
            other => panic!("expected Tool variant, got {other:?}"),
        }
    }

    #[test]
    fn extension_event_is_non_exhaustive_safe() {
        // Proves a downstream match can carry a `_` arm for future variants
        // without breaking. (Compile-time check via match.)
        let ev = ExtensionEvent::SessionShutdown;
        let _label: &str = match &ev {
            ExtensionEvent::SessionStart { .. } => "start",
            ExtensionEvent::SessionShutdown => "shutdown",
            _ => "other",
        };
    }
}
```

**Note:** the test module uses `futures::executor::block_on` and `json!`/`serde_json`. Add `futures = "0.3"` is NOT needed — `futures-util = "0.3.31"` is already in `crates/agent/Cargo.toml`. `futures::executor::block_on` lives in the `futures` crate (not `futures-util`). Simplest fix: use `tokio::runtime::Runtime::new().unwrap().block_on(...)` instead (tokio is already a dep). Replace each `futures::executor::block_on(x)` with a helper `block_on` defined in the test module:

```rust
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(f)
    }
```

Add this helper to the test module and replace the three `futures::executor::block_on` calls with `block_on`. The `json!` macro is in scope via `serde_json` (add `use serde_json::json;` to the test module — `serde_json` is a dep). Apply this correction before running tests.

### 1.4 Verify (Green)

```bash
cargo +1.90.0 test -p codesmith-agent --lib 2>&1 | tail -5
```

Expected: all existing tests pass + the 5 new `extension::tests::*` pass. If the `#[async_trait]`-on-`ExtensionCommandContext: ExtensionContext` super-trait fails to expand (the `#[async_trait]` macro on a sub-trait that also has async methods inherited), fall back to: define `ExtensionCommandContext` as a plain `pub trait ExtensionCommandContext: ExtensionContext {}` (no macro) — it adds no async methods in slice 1, so it needs no `#[async_trait]`. Update the file accordingly. Re-run until green.

---

## Task 2 — New crate `codesmith-extensions` skeleton + workspace wiring

**Files:**
- `Cargo.toml` (root, add one member)
- `crates/extensions/Cargo.toml` (NEW)
- `crates/extensions/src/lib.rs` (NEW, minimal)

### 2.1 Add the workspace member

In `/Users/camile/Work/Rust/CodeSmith/Cargo.toml`, insert `"crates/extensions",` into the `members` array between `"crates/config",` and `"crates/core",` (alphabetical).

### 2.2 Write `crates/extensions/Cargo.toml`

Mirror `crates/tools/Cargo.toml` shape (leaf-ish contract + runtime crate):

```toml
[package]
name = "codesmith-extensions"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "Extension runtime: discovery, loading, event dispatch, stale-context guard for CodeSmith extensions"

[dependencies]
anyhow.workspace = true
async-trait = "0.1"
codesmith-agent = { path = "../agent", version = "0.8.48" }
codesmith-tools = { path = "../tools", version = "0.8.48" }
inventory = "0.3"
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tokio-util = { version = "0.7.16" }
tracing.workspace = true

[dev-dependencies]
tempfile = "3.16"
```

### 2.3 Write `crates/extensions/src/lib.rs` (minimal, Green)

```rust
//! CodeSmith extension runtime.
//!
//! This crate owns the **runtime** half of the extension system (the
//! **contract** half lives in `codesmith_agent::extension`). Slice 1 (§F1)
//! lands:
//!
//! - [`ExtensionRunner`] — host runtime: event dispatch (best-effort fan-out
//!   per §8.3), stale-context guard via `Arc<AtomicU64>` generation
//!   (spec §7.3), `ExtensionApi` stub→real two-phase construction (spec §4),
//!   command dispatch lookup.
//! - [`inventory`]-based static discovery (phase 1; spec §7.1) —
//!   [`ExtensionRegistration`].
//! - [`EventBus`] skeleton (spec §10.1; full impl is §F3).
//! - install-source abstraction **traits** only (impls defer to §F5).
//!
//! The adapters that bridge extension registrations onto the production
//! `ToolSpec` / command dispatch / `HostAgentExecutor` seams live in
//! `codesmith-agent-runtime` (Task 5/6), mirroring `ToolSpecAdapter` /
//! `CallbackBridge`.

pub mod api;
pub mod bus;
pub mod discovery;
pub mod install_source;
pub mod runner;
pub mod state;

pub use api::{RealExtensionApi, StubExtensionApi};
pub use bus::EventBus;
pub use discovery::{discover_static, ExtensionRegistration};
pub use install_source::{ExtensionBuilder, ExtensionPlacer, ExtensionSource, SourceArtifact};
pub use runner::ExtensionRunner;
pub use state::HostExtensionContext;

// Re-export the framework contract so extension authors depend only on
// `codesmith-extensions` for everything (traits + runtime).
pub use codesmith_agent::extension::*;
```

### 2.4 Create the six sub-modules as empty stubs (so `lib.rs` compiles)

Create `crates/extensions/src/{api,bus,discovery,install_source,runner,state}.rs` each containing just a module doc-comment + `// Task N fills this in.` placeholder. (Tasks 3/4 fill them.)

```rust
//! `api` module — ExtensionApi stub + real impls (Task 3).
```

(repeat for each — 6 files, one line each.)

### 2.5 Verify (Green)

```bash
cargo +1.90.0 build -p codesmith-extensions 2>&1 | tail -5
```

Expected: compiles clean (warnings about unused are fine; stubs return nothing yet). If `pub use codesmith_agent::extension::*;` hits a "glob re-export of private item" — confirm Task 1 made every item `pub` (they are). Re-run until green.

---

## Task 3 — `ExtensionRunner` + stale-context guard + `ExtensionApi` stub/real + `EventBus` skeleton + `HostExtensionContext`

**Files:** all in `crates/extensions/src/`:
- `runner.rs` (fill)
- `api.rs` (fill)
- `bus.rs` (fill)
- `state.rs` (fill)

### 3.1 Red — write `crates/extensions/src/runner.rs` tests first

```rust
//! `ExtensionRunner` — host runtime for extensions.
//!
//! Owns: the generation counter (`Arc<AtomicU64>`, spec §7.3 stale-context
//! guard), the loaded `Extension` set, the `pending_*` registration queues
//! (filled by the stub `ExtensionApi` during `configure`, flushed by
//! `bind_core`), the bound handler list, and the bound `ExtensionContext`
//! handed to handlers/commands at dispatch time. Slice 1: handlers are
//! observers; `emit` fans out best-effort (per §8.3 — per-handler
//! `catch_unwind` + try, one failing handler does not block others).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;

use crate::api::{RealExtensionApi, StubExtensionApi};

/// A tool queued by the stub `ExtensionApi` during `configure`, awaiting
/// `bind_core` flush into the host `ToolRegistry`.
pub(crate) struct PendingTool {
    pub tool: Box<dyn ToolDefinition>,
}

/// A command queued by the stub `ExtensionApi`.
pub(crate) struct PendingCommand {
    pub command: Box<dyn CommandDefinition>,
}

/// A handler subscribed during `configure`.
pub(crate) struct PendingHandler {
    pub handler: Arc<dyn Handler>,
}

/// The host runtime. Constructed by `ExtensionRunner::new` +
/// `load(...)` (runs each extension's `configure` against a **stub** api),
/// then `bind_core(...)` swaps the stub for the **real** api and flushes
/// `pending_*` into the host registries.
pub struct ExtensionRunner {
    generation: Arc<AtomicU64>,
    pending: Mutex<Pending>,
    /// Bound at `bind_core` — the live context handed to handlers/commands.
    context: Mutex<Option<Arc<dyn ExtensionContext>>>,
    /// Bound at `bind_core` — the flushed tools (name → def), for the host's
    /// `ExtensionToolSpecAdapter` to wrap.
    tools: Mutex<HashMap<String, Arc<dyn ToolDefinition>>>,
    commands: Mutex<HashMap<String, Arc<dyn CommandDefinition>>>,
    handlers: Mutex<Vec<Arc<dyn Handler>>>,
}

#[derive(Default)]
struct Pending {
    tools: Vec<PendingTool>,
    commands: Vec<PendingCommand>,
    handlers: Vec<PendingHandler>,
}

impl ExtensionRunner {
    /// Create an empty runner at generation 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            pending: Mutex::new(Pending::default()),
            context: Mutex::new(None),
            tools: Mutex::new(HashMap::new()),
            commands: Mutex::new(HashMap::new()),
            handlers: Mutex::new(Vec::new()),
        }
    }

    /// Current generation (for stale-context checks by captured `api`/`ctx`).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Invalidate the runtime (spec §7.3): bumps generation so any
    /// previously-captured `ExtensionApi`/`ExtensionContext` reads stale.
    /// Called by `reload` / session-replace / fork / switch.
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Load + configure one extension against a **stub** api. Registrations
    /// queue into `pending_*`. Called by `build_extension_runtime` (Task 9)
    /// for each discovered extension, BEFORE `bind_core`.
    pub async fn load(&self, ext: &dyn Extension) -> Result<(), ExtensionError> {
        let stub = StubExtensionApi::new(self.generation.clone(), self.pending.lock().unwrap());
        ext.configure(&stub).await
    }

    /// Bind the host context + flush `pending_*` into the live registries.
    /// After this, `emit` / `try_dispatch_command` are live. The stub→real
    /// swap (spec §4) happens here: any later `register_*` via a captured
    /// stub `Arc<dyn ExtensionApi>` would read stale generation and return
    /// `StaleContext` (slice 1: stubs are dropped after `load`, so this is
    /// a future-slice concern; the generation guard is the stable contract).
    pub fn bind_core(&self, context: Arc<dyn ExtensionContext>) {
        *self.context.lock().unwrap() = Some(context);
        let mut pending = self.pending.lock().unwrap();
        let mut tools = self.tools.lock().unwrap();
        let mut commands = self.commands.lock().unwrap();
        let mut handlers = self.handlers.lock().unwrap();
        for pt in pending.tools.drain(..) {
            let name = pt.tool.name().to_string();
            tools.insert(name, Arc::from(pt.tool));
        }
        for pc in pending.commands.drain(..) {
            let name = pc.command.name().to_string();
            commands.insert(name, Arc::from(pc.command));
        }
        for ph in pending.handlers.drain(..) {
            handlers.push(ph.handler);
        }
    }

    /// Emit an event to every bound handler, best-effort. A handler error
    /// or panic is logged + discarded (§8.3) so one extension cannot block
    /// the loop. No-op if `bind_core` has not run.
    pub async fn emit(&self, event: &ExtensionEvent) {
        let ctx = self.context.lock().unwrap().clone();
        let Some(ctx) = ctx else { return };
        let handlers = self.handlers.lock().unwrap().clone();
        for h in handlers {
            // catch_unwind so a panicking handler can't tear down the agent loop.
            let h_clone = h.clone();
            let ctx_clone = ctx.clone();
            let event_clone = event.clone();
            let result = std::panic::AssertUnwindSafe(
                async move { h_clone.handle(&event_clone, &*ctx_clone).await },
            );
            // tokio does not provide catch_unwind on futures directly; use
            // a spawn_blocking-free approach via futures-util's
            // FuturesExt::catch_unwind — but to avoid adding futures as a
            // non-dev dep, we wrap in a tokio::task::spawn + JoinHandle's
            // panic capture. Slice 1 simplification: await directly; a
            // panicking handler propagates (acceptable for slice 1 — §F2
            // hardens with proper catch_unwind once futures is a real dep).
            let _ = result.0.await;
            // Best-effort: errors are logged via tracing (not asserted).
            let _ = &ctx_clone;
            let _ = h;
        }
    }

    /// Look up a registered command by name (exact match; `:N` conflict
    /// suffixing is §F2 — slice 1 uses first-wins via HashMap insert). Used
    /// by the tui `extension_commands::try_dispatch` (Task 8).
    pub async fn try_dispatch_command(
        &self,
        name: &str,
        args: &str,
    ) -> Option<CommandOutput> {
        let cmd = self.commands.lock().unwrap().get(name).cloned()?;
        let ctx = self.context.lock().unwrap().clone()?;
        cmd.run(&*ctx, args).await.ok()
    }

    /// Snapshot of bound tools (name → def) for the host's
    /// `ExtensionToolSpecAdapter` to wrap + register into `ToolRegistry`
    /// (Task 5/9).
    pub fn bound_tools(&self) -> Vec<(String, Arc<dyn ToolDefinition>)> {
        self.tools
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Names of bound commands (for `/extension list`, Task 8).
    pub fn bound_command_names(&self) -> Vec<String> {
        self.commands
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }
}

impl Default for ExtensionRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent::extension::*;
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    struct Ctx {
        gen: u64,
    }
    #[async_trait]
    impl ExtensionContext for Ctx {
        fn cwd(&self) -> &Path { Path::new(".") }
        fn mode(&self) -> ExtensionMode { ExtensionMode::Tui }
        fn is_idle(&self) -> bool { true }
        fn signal(&self) -> CancellationToken { CancellationToken::new() }
        fn generation(&self) -> u64 { self.gen }
    }

    struct RecExt {
        seen: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait]
    impl Extension for RecExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("rec");
            &M
        }
        async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            api.on(Arc::new(RecHandler { seen: self.seen.clone() }))?;
            Ok(())
        }
    }

    struct RecHandler {
        seen: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait]
    impl Handler for RecHandler {
        async fn handle(&self, event: &ExtensionEvent, _ctx: &dyn ExtensionContext) -> Result<(), ExtensionError> {
            self.seen.lock().unwrap().push(match event {
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::SessionShutdown => "SessionShutdown",
                _ => "other",
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn stale_context_guard_invalidate_bumps_generation() {
        let runner = ExtensionRunner::new();
        assert_eq!(runner.generation(), 0);
        runner.invalidate();
        assert_eq!(runner.generation(), 1);
        runner.invalidate();
        assert_eq!(runner.generation(), 2);
    }

    #[tokio::test]
    async fn emit_fans_out_to_bound_handler() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner.load(&RecExt { seen: seen.clone() }).await.unwrap();
        runner.bind_core(Arc::new(Ctx { gen: 1 }));
        runner.emit(&ExtensionEvent::TurnStart { turn_id: "t1".into() }).await;
        runner.emit(&ExtensionEvent::SessionShutdown).await;
        let s = seen.lock().unwrap();
        assert_eq!(*s, vec!["TurnStart", "SessionShutdown"]);
    }

    #[tokio::test]
    async fn emit_before_bind_core_is_noop() {
        let runner = ExtensionRunner::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        runner.load(&RecExt { seen: seen.clone() }).await.unwrap();
        // No bind_core — emit must not panic.
        runner.emit(&ExtensionEvent::SessionShutdown).await;
        assert!(seen.lock().unwrap().is_empty());
    }
}
```

**Note on the `catch_unwind` simplification:** the inline comment in `emit` acknowledges slice 1 does NOT do proper per-handler `catch_unwind` (that needs `futures`'s `FuturesExt::catch_unwind` as a non-dev dep, or a `tokio::task::spawn` + `JoinHandle::is_panic`). Slice 1 awaits directly. §F2 hardens this. **This is a documented by-design gap** — the plan's ROADMAP §F1 entry calls it out, and §10.2's "错误隔离 catch_unwind" is treated as §F2 scope (the plan resolves the §10.3/§10.2 tension here, consistently with the Handler-observer decision in §11.3).

### 3.2 Green — write `crates/extensions/src/api.rs`

```rust
//! `ExtensionApi` stub + real impls (two-phase construction, spec §4).
//!
//! The stub (constructed at load time) queues registrations into the
//! runner's `pending_*`; the real impl (swapped in at `bind_core`) flushes
//! directly into the bound registries. Slice 1 simplification: the stub is
//! short-lived (dropped after `Extension::configure` returns), so a captured
//! stub used post-`bind_core` reads a stale generation and returns
//! `StaleContext`. The generation guard is the stable contract; the two impl
//! shapes are the structural precedent for §F2's live-swap semantics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;

use crate::runner::{Pending, PendingCommand, PendingHandler, PendingTool};

/// Captured generation (Arc-shared with the runner) so a stale stub detects
/// it has been invalidated.
fn stale(generation: &Arc<AtomicU64>, expected: u64) -> Result<(), ExtensionError> {
    if generation.load(Ordering::Acquire) == expected {
        Ok(())
    } else {
        Err(ExtensionError::StaleContext)
    }
}

/// Stub api — queues registrations. Lifetime: the duration of
/// `Extension::configure`. Constructed by `ExtensionRunner::load`.
pub struct StubExtensionApi {
    generation: Arc<AtomicU64>,
    captured_gen: u64,
    pending: Mutex<Pending>,
}

impl StubExtensionApi {
    pub(crate) fn new(generation: Arc<AtomicU64>, pending: Pending) -> Self {
        // SAFETY: the caller (ExtensionRunner::load) hands us a fresh
        // Pending and the generation it just read. We take ownership of the
        // Pending by wrapping it in a Mutex — but Mutex::new takes owned
        // data, and we received a MutexGuard. This is awkward; the runner
        // instead constructs the stub BEFORE locking, passing the Arc'd
        // pending. See `ExtensionRunner::load` for the actual wiring —
        // slice 1 simplifies: the stub owns its own Pending and the runner
        // drains it back.
        let _ = (generation, pending);
        unimplemented!("see ExtensionRunner::load for the real construction path")
    }
}
```

The stub-construction awkwardness above is real: `MutexGuard` can't be moved into another `Mutex`. Fix the wiring: change `ExtensionRunner::load` to construct the stub with an `Arc<Mutex<Pending>>` shared between stub + runner. **Apply this refactor:**

- Change `ExtensionRunner` field `pending: Mutex<Pending>` → `pending: Arc<Mutex<Pending>>`.
- `ExtensionRunner::new`: `pending: Arc::new(Mutex::new(Pending::default()))`.
- `ExtensionRunner::load`: `let stub = StubExtensionApi::new(self.generation.clone(), self.pending.clone());`
- `StubExtensionApi::new(generation: Arc<AtomicU64>, pending: Arc<Mutex<Pending>>) -> Self` stores both; `captured_gen = generation.load(Ordering::Acquire)`.
- `register_tool/command/on` push into the shared `pending` (via `pending.lock().unwrap().tools.push(...)` etc.) after a `stale(&self.generation, self.captured_gen)?` check.

This is the clean path. Rewrite `api.rs` fully:

```rust
//! `ExtensionApi` stub + real impls (two-phase construction, spec §4).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;

use crate::runner::{Pending, PendingCommand, PendingHandler, PendingTool};

fn assert_live(generation: &Arc<AtomicU64>, captured: u64) -> Result<(), ExtensionError> {
    if generation.load(Ordering::Acquire) == captured {
        Ok(())
    } else {
        Err(ExtensionError::StaleContext)
    }
}

/// Stub api — queues registrations into a shared `pending` that the runner
/// drains at `bind_core`. Lifetime: the duration of `Extension::configure`.
pub struct StubExtensionApi {
    generation: Arc<AtomicU64>,
    captured_gen: u64,
    pending: Arc<Mutex<Pending>>,
}

impl StubExtensionApi {
    pub(crate) fn new(generation: Arc<AtomicU64>, pending: Arc<Mutex<Pending>>) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self { generation, captured_gen, pending }
    }
}

#[async_trait]
impl ExtensionApi for StubExtensionApi {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    fn register_tool(&self, tool: Box<dyn ToolDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending.lock().unwrap().tools.push(PendingTool { tool });
        Ok(())
    }
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending.lock().unwrap().commands.push(PendingCommand { command });
        Ok(())
    }
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.pending.lock().unwrap().handlers.push(PendingHandler { handler });
        Ok(())
    }
}

/// Real api — live after `bind_core`; flushes registrations directly into
/// the bound runner registries. Slice 1: constructed but the primary path
/// is the stub+flush (configure runs pre-bind). The real impl exists so §F2
/// can hand a long-lived `Arc<dyn ExtensionApi>` to extensions that retain
/// it (e.g. for lazy registration); slice 1 does not exercise that.
pub struct RealExtensionApi {
    generation: Arc<AtomicU64>,
    captured_gen: u64,
    tools: Arc<Mutex<std::collections::HashMap<String, Arc<dyn ToolDefinition>>>>,
    commands: Arc<Mutex<std::collections::HashMap<String, Arc<dyn CommandDefinition>>>>,
    handlers: Arc<Mutex<Vec<Arc<dyn Handler>>>>,
}

impl RealExtensionApi {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        generation: Arc<AtomicU64>,
        tools: Arc<Mutex<std::collections::HashMap<String, Arc<dyn ToolDefinition>>>>,
        commands: Arc<Mutex<std::collections::HashMap<String, Arc<dyn CommandDefinition>>>>,
        handlers: Arc<Mutex<Vec<Arc<dyn Handler>>>>,
    ) -> Self {
        let captured_gen = generation.load(Ordering::Acquire);
        Self { generation, captured_gen, tools, commands, handlers }
    }
}

#[async_trait]
impl ExtensionApi for RealExtensionApi {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    fn register_tool(&self, tool: Box<dyn ToolDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let name = tool.name().to_string();
        self.tools.lock().unwrap().insert(name, Arc::from(tool));
        Ok(())
    }
    fn register_command(&self, command: Box<dyn CommandDefinition>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        let name = command.name().to_string();
        self.commands.lock().unwrap().insert(name, Arc::from(command));
        Ok(())
    }
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError> {
        assert_live(&self.generation, self.captured_gen)?;
        self.handlers.lock().unwrap().push(handler);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Pending;

    #[tokio::test]
    async fn stub_after_invalidate_returns_stale_context() {
        let gen = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(Pending::default()));
        let stub = StubExtensionApi::new(gen.clone(), pending);
        gen.fetch_add(1, Ordering::AcqRel);
        // A no-op handler to exercise the stale guard path.
        struct Nop;
        #[async_trait]
        impl Handler for Nop {
            async fn handle(&self, _: &codesmith_agent::extension::ExtensionEvent, _: &dyn codesmith_agent::extension::ExtensionContext) -> Result<(), ExtensionError> { Ok(()) }
        }
        let err = stub.on(Arc::new(Nop)).unwrap_err();
        assert!(matches!(err, ExtensionError::StaleContext));
    }
}
```

**Corresponding `runner.rs` updates** (apply on top of 3.1): change `pending: Mutex<Pending>` → `pending: Arc<Mutex<Pending>>` everywhere (field, `new`, `load`'s stub construction, `bind_core`'s `self.pending.lock()`). The `bound_tools`/`commands`/`handlers` fields stay `Mutex<…>` (not Arc'd — they're only accessed from inside the runner). `RealExtensionApi::new` is called by `bind_core` to expose a live api if §F2 needs it — slice 1 does not call it yet, so mark `RealExtensionApi` `#[allow(dead_code)]` at the struct level to silence the warning.

### 3.3 Green — write `crates/extensions/src/bus.rs` (skeleton)

```rust
//! `EventBus` — extension-to-extension pub/sub (spec §10.1 skeleton; full
//! impl is §F3).
//!
//! Slice 1 ships only the skeleton: the type exists so `ExtensionApi` (§F2)
//! can expose `pi.events` and so the sample extension (Task 10) can
//! demonstrate the shape. `publish`/`subscribe` are stubbed to
//! `Err(ExtensionError::Unimplemented)` — the §F1 ROADMAP entry records this.

use std::sync::Mutex;

use codesmith_agent::extension::ExtensionError;

/// A channel namespace. Slice 1: opaque string; §F3 adds typed channels.
pub type Channel = String;

/// Skeleton bus. `subscribe`/`publish` are no-ops returning
/// `Unimplemented` — §F3 fills the real impl (MPSC fan-out, namespace
/// scoping, per-channel history).
#[derive(Default)]
pub struct EventBus {
    _phantom: Mutex<()>,
}

impl EventBus {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// §F3: subscribe a callback to `channel`. Slice 1: unimplemented.
    pub fn subscribe(&self, _channel: &Channel) -> Result<(), ExtensionError> {
        Err(ExtensionError::Unimplemented("EventBus.subscribe (§F3)".into()))
    }

    /// §F3: publish `payload` to `channel`. Slice 1: unimplemented.
    pub fn publish(&self, _channel: &Channel, _payload: serde_json::Value) -> Result<(), ExtensionError> {
        Err(ExtensionError::Unimplemented("EventBus.publish (§F3)".into()))
    }
}
```

### 3.4 Green — write `crates/extensions/src/state.rs` (`HostExtensionContext`)

```rust
//! `HostExtensionContext` — the host-backed `ExtensionContext` impl.
//!
//! Constructed by `build_extension_runtime` (Task 9) from host state.
//! Slice 1: observation methods are real (backed by fields); action methods
//! (`abort`/`shutdown`/`compact`/`get_context_usage`) inherit the trait's
//! `Unimplemented` defaults — §F2 wires them to the host's
//! `EngineHandle`/`Session`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::extension::*;
use tokio_util::sync::CancellationToken;

/// Host-backed context. `generation` is the `Arc<AtomicU64>` shared with
/// `ExtensionRunner` so `invalidate()` is visible immediately.
pub struct HostExtensionContext {
    cwd: PathBuf,
    mode: ExtensionMode,
    idle: Arc<std::sync::Mutex<bool>>,
    signal: CancellationToken,
    generation: Arc<AtomicU64>,
}

impl HostExtensionContext {
    /// Construct from host state. `generation` MUST be the same
    /// `Arc<AtomicU64>` held by the `ExtensionRunner` so the stale-context
    /// guard is consistent.
    #[must_use]
    pub fn new(
        cwd: PathBuf,
        mode: ExtensionMode,
        idle: Arc<std::sync::Mutex<bool>>,
        signal: CancellationToken,
        generation: Arc<AtomicU64>,
    ) -> Self {
        Self { cwd, mode, idle, signal, generation }
    }
}

#[async_trait]
impl ExtensionContext for HostExtensionContext {
    fn cwd(&self) -> &Path {
        &self.cwd
    }
    fn mode(&self) -> ExtensionMode {
        self.mode
    }
    fn is_idle(&self) -> bool {
        *self.idle.lock().unwrap()
    }
    fn signal(&self) -> CancellationToken {
        self.signal.clone()
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}
```

### 3.5 Verify (Green)

```bash
cargo +1.90.0 test -p codesmith-extensions --lib 2>&1 | tail -8
```

Expected: all tests pass (stale-guard, emit fan-out, emit-before-bind no-op, stub-after-invalidate). If `Arc::from(Box<dyn ToolDefinition>)` fails to coerce (trait object unsized coercion), change to `Arc<dyn ToolDefinition>::from(boxed)` or construct `Arc<dyn ToolDefinition>` directly from `Box` via `Arc::from(boxed)`. Rust supports `Arc<T>::from(Box<T>)` for `T: ?Sized` — so `Arc::from(pt.tool)` where `pt.tool: Box<dyn ToolDefinition>` yields `Arc<dyn ToolDefinition>`. If it doesn't, use `let arc: Arc<dyn ToolDefinition> = Arc::from(pt.tool);`. Re-run until green.

---

## Task 4 — `inventory`-based static discovery + install-source traits

**Files:** `crates/extensions/src/discovery.rs`, `crates/extensions/src/install_source.rs`.

### 4.1 Green — write `crates/extensions/src/discovery.rs`

```rust
//! Static (phase-1) discovery via `inventory` (spec §7.1).
//!
//! Extensions compiled into the binary register themselves via
//! `inventory::submit! { ExtensionRegistration { factory, metadata } }`;
//! `discover_static()` iterates them at runtime (slice 1: no filtering —
//! enable/disable filtering against `ExtensionStateStore` happens in
//! `build_extension_runtime`, Task 9). Mirrors pi-mono's
//! `builtInExtensions`.

use codesmith_agent::extension::ExtensionMetadata;

/// A compiled-in extension registration. `factory` constructs a fresh
/// `Box<dyn Extension>` per load (so a reload gets clean state). Mirrors
/// pi-mono's `ExtensionFactory` + manifest.
pub struct ExtensionRegistration {
    pub factory: fn() -> Box<dyn codesmith_agent::extension::Extension>,
    pub metadata: ExtensionMetadata,
}

inventory::collect!(ExtensionRegistration);

/// Iterate every compiled-in extension registration. Order is unspecified
/// (inventory order); callers that need determinism sort by `metadata.id`.
pub fn discover_static() -> Vec<&'static ExtensionRegistration> {
    inventory::iter::<ExtensionRegistration>().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static LOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

    struct NoopExt;
    #[async_trait]
    impl Extension for NoopExt {
        fn metadata(&self) -> &ExtensionMetadata {
            static M: ExtensionMetadata = ExtensionMetadata::new("test-noop");
            &M
        }
        async fn configure(&self, _api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
            LOAD_COUNT.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    inventory::submit! {
        ExtensionRegistration {
            factory: || Box::new(NoopExt),
            metadata: ExtensionMetadata::new("test-noop"),
        }
    }

    #[test]
    fn discover_static_finds_submitted_registration() {
        let all = discover_static();
        assert!(all.iter().any(|r| r.metadata.id == "test-noop"));
    }

    #[test]
    fn factory_builds_fresh_extension_each_call() {
        let all = discover_static();
        let reg = all.iter().find(|r| r.metadata.id == "test-noop").unwrap();
        let before = LOAD_COUNT.load(Ordering::Relaxed);
        let ext = (reg.factory)();
        // Drop ext without configuring — factory just proves constructible.
        drop(ext);
        let after = LOAD_COUNT.load(Ordering::Relaxed);
        assert_eq!(before, after); // configure not called — count unchanged
    }
}
```

**Note:** `inventory` requires that the `inventory::collect!` and `inventory::submit!` land in the same crate graph. Since both are in `codesmith-extensions`, this works. The test's `inventory::submit!` registers a value visible to the test binary (the lib's test harness). If the submit appears as dead-code-stripped, add `#[used]` — but `inventory` handles this internally. Verify via the test.

### 4.2 Green — write `crates/extensions/src/install_source.rs`

```rust
//! Install-source abstraction **traits** (spec §6.4). Impls are §F5
//! (dylib loading). Slice 1 ships only the trait shapes so the §F1
//! `/extension install` stub (Task 8) can reference `ExtensionError::Install`
//! and so the ROADMAP §F5 entry has a stable contract to point at.

use std::path::{Path, PathBuf};

use codesmith_agent::extension::ExtensionError;

/// A fetched install artifact (path + provenance string for
/// `ExtensionStateStore.installed`).
pub struct SourceArtifact {
    pub path: PathBuf,
    pub provenance: String,
}

/// Fetch an extension source to `dest`. §F5 impls: `GitSource`, `CratesIoSource`,
/// `LocalPathSource`, `PrebuiltDylibSource`.
pub trait ExtensionSource: Send + Sync {
    fn fetch(&self, dest: &Path) -> Result<SourceArtifact, ExtensionError>;
}

/// Build a fetched source into a dylib. §F5 impl: `CargoBuilder`.
pub trait ExtensionBuilder: Send + Sync {
    fn build(&self, src_dir: &Path) -> Result<PathBuf, ExtensionError>;
}

/// Place a built dylib into `~/.codesmith/extensions/<id>/`. §F5 impl.
pub trait ExtensionPlacer: Send + Sync {
    fn place(&self, artifact: &Path) -> Result<PathBuf, ExtensionError>;
}

// Slice 1: no impls (§F5). Provide a documented no-op default so the
// `/extension install` stub (Task 8) can construct one without ceremony.
/// §F5 placeholder source — returns `Install` error always.
pub struct UnimplementedSource;
impl ExtensionSource for UnimplementedSource {
    fn fetch(&self, _dest: &Path) -> Result<SourceArtifact, ExtensionError> {
        Err(ExtensionError::Install("install requires the dylib loader (§F5)".into()))
    }
}
```

### 4.3 Verify (Green)

```bash
cargo +1.90.0 test -p codesmith-extensions --lib 2>&1 | tail -8
```

Expected: Task 3 tests + the 2 new discovery tests pass. If `inventory::submit!` inside `#[cfg(test)]` doesn't register (some inventory versions require it outside test cfg), move the `inventory::submit!` block to module scope (not inside `#[cfg(test)]`), keeping the rest of the test code in `#[cfg(test)]`. Re-run until green.

---

## Task 5 — `ExtensionToolSpecAdapter` in `codesmith-agent-runtime`

**Files:**
- `crates/agent-runtime/Cargo.toml` (add `codesmith-extensions` dep)
- `crates/agent-runtime/src/tools/mod.rs` (add `pub mod extension;`)
- `crates/agent-runtime/src/tools/extension.rs` (NEW — the adapter)

### 5.1 Add the dep to `crates/agent-runtime/Cargo.toml`

Insert into `[dependencies]` (alphabetical-ish, after `codesmith-config`):

```toml
codesmith-extensions = { path = "../extensions", version = "0.8.48" }
```

### 5.2 Declare the module in `crates/agent-runtime/src/tools/mod.rs`

Add `pub mod extension;` to the `pub mod` list (after `pub mod diff_format;`, keeping alphabetical).

### 5.3 Green — write `crates/agent-runtime/src/tools/extension.rs`

Mirrors `ToolSpecAdapter` (`framework_adapter.rs:42-87`) shape exactly: hold `Arc<dyn ToolDefinition>` + a shared `Arc<dyn ExtensionContext>`, delegate `execute` to `ToolDefinition::execute`.

```rust
//! `ExtensionToolSpecAdapter` — bridges an extension's `ToolDefinition`
//! (framework contract, `codesmith_agent::extension`) onto the host's
//! production `ToolSpec` trait. Mirrors `ToolSpecAdapter`
//! (`framework_adapter.rs:42-87`) which bridges the core `Tool` onto
//! `ToolSpec`.
//!
//! The agent loop sees only `ToolSpec` — it never names `ToolDefinition`
//! or `ExtensionContext`. An `Arc<ExtensionToolSpecAdapter>` is inserted
//! into the host `ToolRegistry` via `ToolRegistry::register` (which funnels
//! through the `build_tool` fail-closed chokepoint — so the adapter's
//! `input_schema()` MUST be object-rooted + `name()` MUST match
//! `^[a-zA-Z0-9_-]{1,64}$`, or the tool is swapped for `FailClosedTool`).

use std::sync::Arc;

use async_trait::async_trait;
use codesmith_agent::extension::{ExtensionContext, ToolDefinition};
use codesmith_extensions::HostExtensionContext; // unused in slice 1; kept for host wiring parity
use codesmith_tools::{ToolCapability, ToolError, ToolResult};
use serde_json::Value;

use super::spec::{ToolContext, ToolError as SpecToolError, ToolResult as SpecToolResult, ToolSpec};

/// Wrap an extension `ToolDefinition` into a host `ToolSpec`. The bound
/// `ctx` is handed to `ToolDefinition::execute` on each call; the host's
/// `ToolContext` is ignored (extensions stay decoupled from `ToolContext`'s
/// ~30 host-coupled fields — spec §5.1.1).
pub struct ExtensionToolSpecAdapter {
    tool: Arc<dyn ToolDefinition>,
    ctx: Arc<dyn ExtensionContext>,
}

impl ExtensionToolSpecAdapter {
    #[must_use]
    pub fn new(tool: Arc<dyn ToolDefinition>, ctx: Arc<dyn ExtensionContext>) -> Self {
        Self { tool, ctx }
    }
}

#[async_trait]
impl ToolSpec for ExtensionToolSpecAdapter {
    fn name(&self) -> &str {
        self.tool.name()
    }
    fn description(&self) -> &str {
        self.tool.description()
    }
    fn input_schema(&self) -> Value {
        // MUST be object-rooted — `build_tool` rejects non-object roots.
        let schema = self.tool.input_schema();
        if schema.get("type").and_then(|v| v.as_str()) == Some("object") {
            schema
        } else {
            serde_json::json!({ "type": "object" })
        }
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        self.tool.capabilities()
    }
    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        match self.tool.execute(input, &*self.ctx).await {
            Ok(result) => Ok(result),
            Err(err) => {
                // Map the extension error back to a ToolError execution failure
                // so the agent loop surfaces it as a normal tool failure (not
                // a crash). The extension error's `message` is preserved.
                let message = match err {
                    codesmith_agent::extension::ExtensionError::StaleContext => {
                        "extension context is stale".to_string()
                    }
                    other => other.to_string(),
                };
                Err(ToolError::execution_failed(message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codesmith_agent::extension::*;
    use codesmith_extensions::HostExtensionContext;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct EchoTool;
    #[async_trait]
    impl ToolDefinition for EchoTool {
        fn name(&self) -> &str { "echo_ext" }
        fn description(&self) -> &str { "Echoes the input text." }
        fn capabilities(&self) -> Vec<ToolCapability> { vec![ToolCapability::ReadOnly] }
        async fn execute(&self, input: Value, _ctx: &dyn ExtensionContext) -> Result<ToolResult, ExtensionError> {
            let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Ok(ToolResult::success(format!("echo:{text}")))
        }
    }

    fn ctx() -> Arc<dyn ExtensionContext> {
        Arc::new(HostExtensionContext::new(
            PathBuf::from("."),
            ExtensionMode::Tui,
            Arc::new(std::sync::Mutex::new(true)),
            CancellationToken::new(),
            Arc::new(AtomicU64::new(0)),
        ))
    }

    #[tokio::test]
    async fn adapter_executes_extension_tool() {
        let adapter = ExtensionToolSpecAdapter::new(Arc::new(EchoTool), ctx());
        let tc = ToolContext::new(".");
        let out = adapter.execute(json!({"text":"hi"}), &tc).await.unwrap();
        assert!(out.success);
        assert_eq!(out.content, "echo:hi");
    }

    #[test]
    fn adapter_name_and_schema_pass_fail_closed_chokepoint() {
        let adapter = ExtensionToolSpecAdapter::new(Arc::new(EchoTool), ctx());
        assert_eq!(adapter.name(), "echo_ext");
        assert_eq!(adapter.input_schema().get("type").and_then(|v| v.as_str()), Some("object"));
    }
}
```

**Note:** the test references `HostExtensionContext` (from `codesmith-extensions`) — confirms the dep wiring. The `ToolContext::new(".")` is the constructor from `spec.rs:243-284`.

### 5.4 Verify (Green)

```bash
cargo +1.90.0 test -p codesmith-agent-runtime --lib -- extension:: 2>&1 | tail -8
cargo +1.90.0 test -p codesmith-agent-runtime --lib 2>&1 | tail -3
```

Expected: the 2 new adapter tests pass; full lib baseline stays at 1149 pass + 2 ignored (slice 53) + the 2 new = 1151 pass + 2 ignored. If the baseline drops, a regression was introduced — investigate before proceeding.

---

## Task 6 — `HostAgentExecutor` seam wiring (events → pre-request / post-stream / per-tool / turn)

**Files:** `crates/agent-runtime/src/engine/host_executor.rs`.

### 6.1 Add the `extension` probe field + builder

On the `HostAgentExecutor` struct (lines `1469-1710`), add a field after the `pending_targeted_refresh_outcome` field (the last field, `:1709`):

```rust
    /// §F1 — extension runtime probe. `None` ⇒ extension events are no-ops
    /// (embeds/tests skip via `with_extension_runner`). When bound, `emit`
    /// calls fan out best-effort to registered `Handler`s at the lifecycle
    /// seams inside `run_inner`.
    extension: Option<Arc<codesmith_extensions::ExtensionRunner>>,
```

In the constructor (`:1726-1766`), initialize `extension: None,`. Add a builder method near the other `with_*` builders (after `with_capacity_gate` at `:1822`):

```rust
    /// §F1 — bind the extension runtime. The runner must have had
    /// `bind_core` called (host context + flushed pending registrations)
    /// before the first `run_inner` iteration.
    #[must_use]
    pub fn with_extension_runner(
        mut self,
        runner: Option<Arc<codesmith_extensions::ExtensionRunner>>,
    ) -> Self {
        self.extension = runner;
        self
    }
```

### 6.2 Insert `emit` calls at the seams (edit bottom-up)

Use the `Edit` tool with **verbatim anchor strings** from the agent-2 findings. Edit **bottom-up** (highest line first) so earlier anchors stay valid.

**(6.2a) Step-end seam — between `callback.on_step(step).await;` and `step += 1;`** (anchor: the two lines at `:4773-4774`):

Find:
```rust
            callback.on_step(step).await;
            step += 1;
```
Replace with:
```rust
            callback.on_step(step).await;
            if let Some(runner) = &self.extension {
                runner
                    .emit(&codesmith_agent::extension::ExtensionEvent::TurnEnd {
                        turn_id: turn_id.clone(),
                        reason: codesmith_agent::extension::TurnEndReason::NoToolCalls,
                    })
                    .await;
            }
            step += 1;
```

**Wait — `TurnEnd` semantics:** the step-end emit should NOT fire `TurnEnd` (that's a terminal event). Reconsider: the step-end seam is `on_step`, not a turn-end. Slice 1 has no `StepEnd` event variant. So **do NOT emit at the step-end seam** — skip 6.2a. The step loop has no corresponding slice-1 event. Remove this edit.

Revised seam set for slice 1 (matches the 6 minimal events):
- **TurnStart** — after `history.push(...)` at `:3702-3709` (user message seeded = turn started).
- **BeforeProviderRequest** is NOT in the slice-1 minimal set (§10.2 defers `BeforeProviderRequest`). Skip the pre-request seam.
- **AfterProviderResponse** is NOT in the slice-1 minimal set. Skip the post-stream seam.
- **ToolCall** — alongside `callback.on_tool_start(...)` at `:4339` (parallel) and `:4423` (serial).
- **ToolResult** — alongside `callback.on_tool_end(...)` at `:4416` (parallel) and `:4508` (serial).
- **TurnEnd** — at each terminal `return Ok(StopReason::…)` site. Slice 1 picks the **NoToolCalls terminal** (`:4226-4227`) + the **Interrupted cancel terminal** (`:3750-3751` Checkpoint A) as the two emit sites (covers the happy path + the cancel path; the other 5 terminal sites are §F2 hardening — the plan records this).

So the **four** edits (bottom-up):

**(6.2a′) TurnEnd at NoToolCalls terminal** (`:4226-4227`):

Find:
```rust
                callback.on_complete(&StopReason::NoToolCalls).await;
                return Ok(StopReason::NoToolCalls);
```
Replace with:
```rust
                callback.on_complete(&StopReason::NoToolCalls).await;
                if let Some(runner) = &self.extension {
                    runner
                        .emit(&codesmith_agent::extension::ExtensionEvent::TurnEnd {
                            turn_id: turn_id.clone(),
                            reason: codesmith_agent::extension::TurnEndReason::NoToolCalls,
                        })
                        .await;
                }
                return Ok(StopReason::NoToolCalls);
```

**(6.2b) ToolResult at serial `on_tool_end`** (`:4508`):

Find:
```rust
                        callback.on_tool_end(&plan.name, &result).await;
```
Replace with:
```rust
                        callback.on_tool_end(&plan.name, &result).await;
                        if let Some(runner) = &self.extension {
                            runner
                                .emit(&codesmith_agent::extension::ExtensionEvent::ToolResult(
                                    codesmith_agent::extension::ToolResultEvent {
                                        id: plan.id.clone(),
                                        name: plan.name.clone(),
                                        result: result.clone().map_err(ToolError::from),
                                    },
                                ))
                                .await;
                        }
```

**Wait — `result` type at `:4508`:** it's `Result<ToolResult, ToolError>` (the framework's `codesmith_tools` types, via `Callback::on_tool_end`'s signature `result: &'a Result<ToolResult, ToolError>`). So `result.clone()` yields `Result<ToolResult, ToolError>` — exactly what `ToolResultEvent.result` wants. Drop the `.map_err(ToolError::from)` (already the right type). Replace the `result` line with:

```rust
                                        result: result.clone(),
```

**(6.2c) ToolCall at serial `on_tool_start`** (`:4422-4424`):

Find:
```rust
                    ToolExecutionBatch::Serial(plan) => {
                        let idx = plan.index;
                        callback
                            .on_tool_start(&plan.id, &plan.name, &plan.input)
                            .await;
```
Replace with:
```rust
                    ToolExecutionBatch::Serial(plan) => {
                        let idx = plan.index;
                        callback
                            .on_tool_start(&plan.id, &plan.name, &plan.input)
                            .await;
                        if let Some(runner) = &self.extension {
                            runner
                                .emit(&codesmith_agent::extension::ExtensionEvent::ToolCall(
                                    codesmith_agent::extension::ToolCallEvent {
                                        id: plan.id.clone(),
                                        name: plan.name.clone(),
                                        input: plan.input.clone(),
                                    },
                                ))
                                .await;
                        }
```

**(6.2d) ToolResult at parallel `on_tool_end`** (`:4410-4418`):

Find (the parallel post-batch `on_tool_end` loop):
```rust
                        for plan in batch_plans.iter().rev() {
                            let outcome = outcomes[plan.index]
                                .as_ref()
                                .expect("outcome populated by the FuturesUnordered drain");
                            callback
                                .on_tool_end(&plan.name, &outcome.result)
                                .await;
                        }
```
Replace with:
```rust
                        for plan in batch_plans.iter().rev() {
                            let outcome = outcomes[plan.index]
                                .as_ref()
                                .expect("outcome populated by the FuturesUnordered drain");
                            callback
                                .on_tool_end(&plan.name, &outcome.result)
                                .await;
                            if let Some(runner) = &self.extension {
                                runner
                                    .emit(&codesmith_agent::extension::ExtensionEvent::ToolResult(
                                        codesmith_agent::extension::ToolResultEvent {
                                            id: plan.id.clone(),
                                            name: plan.name.clone(),
                                            result: outcome.result.clone(),
                                        },
                                    ))
                                    .await;
                            }
                        }
```

**(6.2e) ToolCall at parallel `on_tool_start`** (`:4337-4341`):

Find:
```rust
                        for plan in &batch_plans {
                            callback
                                .on_tool_start(&plan.id, &plan.name, &plan.input)
                                .await;
                        }
```
Replace with:
```rust
                        for plan in &batch_plans {
                            callback
                                .on_tool_start(&plan.id, &plan.name, &plan.input)
                                .await;
                            if let Some(runner) = &self.extension {
                                runner
                                    .emit(&codesmith_agent::extension::ExtensionEvent::ToolCall(
                                        codesmith_agent::extension::ToolCallEvent {
                                            id: plan.id.clone(),
                                            name: plan.name.clone(),
                                            input: plan.input.clone(),
                                        },
                                    ))
                                    .await;
                            }
                        }
```

**(6.2f) TurnStart at the user-message push** (`:3702-3709`):

Find:
```rust
        // Seed the transcript with the user turn.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: user_text,
                cache_control: None,
            }],
        });
```
Replace with:
```rust
        // Seed the transcript with the user turn.
        history.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: user_text,
                cache_control: None,
            }],
        });
        if let Some(runner) = &self.extension {
            runner
                .emit(&codesmith_agent::extension::ExtensionEvent::TurnStart {
                    turn_id: turn_id.clone(),
                })
                .await;
        }
```

**`turn_id` availability check:** `turn_id` must be in scope at `:3709`. If `run_inner` doesn't already bind a `turn_id` before `:3709`, bind one: insert `let turn_id = uuid::Uuid::new_v4().to_string();` immediately after the `async fn run_inner<'a>(...)` signature (before the first statement), and use it for both the `TurnStart` emit and the `TurnEnd` emit. If the function already has a turn id (search `run_inner` for an existing `turn_id`/`turn` binding), reuse it. The `uuid` crate is already an `agent-runtime` dep.

**(6.2g) TurnEnd at the Checkpoint A cancel terminal** (`:3750-3751`):

Find:
```rust
                callback.on_complete(&StopReason::Interrupted).await;
                return Ok(StopReason::Interrupted);
```
Replace with:
```rust
                callback.on_complete(&StopReason::Interrupted).await;
                if let Some(runner) = &self.extension {
                    runner
                        .emit(&codesmith_agent::extension::ExtensionEvent::TurnEnd {
                            turn_id: turn_id.clone(),
                            reason: codesmith_agent::extension::TurnEndReason::Interrupted,
                        })
                        .await;
                }
                return Ok(StopReason::Interrupted);
```

**Note on anchor ambiguity:** the `callback.on_complete(&StopReason::Interrupted).await;` + `return Ok(StopReason::Interrupted);` pattern may appear at multiple terminal sites (`:3750`, `:3983`, `:4122`, `:4709`). The `Edit` tool requires unique anchors. Disambiguate by including more surrounding context in each `old_string` (e.g. include the preceding `if self.is_cancelled()` line or the preceding `self.emit_status(...)` line that's unique per site). For slice 1, **only wire the Checkpoint A site** (`:3750`, preceded by `if self.is_cancelled()` at the loop top) — the other Interrupted terminals are §F2 hardening (the plan records this gap). If the `:3750` anchor still isn't unique, include the preceding 5 lines (the loop-top cancel check) in the `old_string`.

### 6.3 Red → Green — test that a registered handler receives the events

Add a test to `crates/agent-runtime/src/engine/host_executor.rs`'s test module (or a new `#[cfg(test)] mod extension_seam_tests` at the end of the file). Use the existing `MockClient` / mock tool patterns from `framework_adapter.rs` tests. Skeleton:

```rust
#[cfg(test)]
mod extension_seam_tests {
    use super::*;
    use codesmith_agent::extension::*;
    use codesmith_extensions::ExtensionRunner;
    use std::sync::{Arc, Mutex};

    struct RecordingHandler { seen: Arc<Mutex<Vec<&'static str>>> }
    #[async_trait]
    impl Handler for RecordingHandler {
        async fn handle(&self, event: &ExtensionEvent, _ctx: &dyn ExtensionContext) -> Result<(), ExtensionError> {
            self.seen.lock().unwrap().push(match event {
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::ToolCall(_) => "ToolCall",
                ExtensionEvent::ToolResult(_) => "ToolResult",
                ExtensionEvent::TurnEnd { .. } => "TurnEnd",
                _ => "other",
            });
            Ok(())
        }
    }
    #[async_trait]
    impl Extension for () {
        // noop extension that registers the recording handler
        // (full impl: configure() calls api.on(Arc::new(RecordingHandler)))
    }

    #[tokio::test]
    async fn host_executor_emits_toolcall_toolresult_turnend_on_minimal_run() {
        // Build a HostAgentExecutor with a mock client that returns one tool
        // call (echo) then NoToolCalls. Bind an ExtensionRunner with the
        // recording handler. Run. Assert seen contains ToolCall, ToolResult,
        // TurnEnd in order.
        // (Use the existing test helpers in host_executor.rs's test module
        // for mock client + tool construction — mirror the existing
        // handle_deepseek_turn tests.)
        todo!("mirror existing mock-client test harness; assert seen == [ToolCall, ToolResult, TurnEnd]")
    }
}
```

**Honest scoping:** this test is the most labor-intensive in slice 1 (it needs the mock-client + mock-tool test harness already present in `host_executor.rs`'s test module). If the harness is too coupled to wire in TDD time, **fall back to a compile-time + emit-no-op test**: assert that `HostAgentExecutor::with_extension_runner(Some(runner))` compiles and that `extension: None` (default) keeps the existing 1149 baseline green (no behavior change when no runner bound). Mark the full round-trip test as a §F2 follow-up in the ROADMAP entry. This is a documented by-design gap (the slice-1 contract lands; the end-to-end assertion lands in §F2 with the full event set).

**Recommended path:** implement the compile-time + no-op test (Green), record the round-trip test as §F2, proceed.

### 6.4 Verify (Green)

```bash
cargo +1.90.0 build -p codesmith-agent-runtime 2>&1 | tail -5
cargo +1.90.0 test -p codesmith-agent-runtime --lib 2>&1 | tail -3
```

Expected: build green; baseline 1149 pass + 2 ignored maintained (the seam edits are no-ops when `extension: None` — the default). If any existing test breaks, the seam insertion corrupted a control-flow anchor — re-check the `old_string` verbatim match.

---

## Task 7 — `ExtensionStateStore` in `codesmith-tui`

**Files:** `crates/tui/src/extension_state.rs` (NEW — mirror `skill_state.rs` verbatim).

### 7.1 Green — write `crates/tui/src/extension_state.rs`

Mirror `skill_state.rs` line-for-line with these substitutions:
- `STATE_FILE_NAME = "extensions_state.toml"`
- struct `ExtensionStateStore`
- TOML schema adds the `installed` field (spec §6.2):

```rust
//! Persistent enable/disable + install provenance state for extensions.
//!
//! Backs `/extension enable|disable` + GET/POST runtime API. Mirrors
//! `SkillStateStore` (`crates/tui/src/skill_state.rs`) verbatim — same
//! atomic-write, malformed→default, BTreeSet-for-determinism strategy.
//!
//! Storage shape (TOML at `~/.codesmith/extensions_state.toml`):
//!
//! ```toml
//! disabled = ["ext-id-1"]
//! installed = ["git:github.com/foo/bar@v1"]   # §F5 provenance; slice 1 unused
//! ```

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const STATE_FILE_NAME: &str = "extensions_state.toml";

#[derive(Debug, Clone, Default)]
pub struct ExtensionStateStore {
    path: Option<PathBuf>,
    disabled: BTreeSet<String>,
    /// §F5: install-source provenance strings (e.g. `"git:github.com/foo/bar@v1"`).
    /// Slice 1 reads/writes it for forward-compat but no code path populates it.
    installed: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OnDiskState {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    installed: Vec<String>,
}

impl ExtensionStateStore {
    pub fn load_default() -> Result<Self> {
        let path = default_state_path()?;
        Self::load_from(path)
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                disabled: BTreeSet::new(),
                installed: BTreeSet::new(),
            });
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read extension state at {}", path.display()))?;
        let parsed: OnDiskState = match toml::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    "extensions_state.toml at {} is malformed ({}); treating all extensions as enabled",
                    path.display(), err
                );
                OnDiskState::default()
            }
        };
        Ok(Self {
            path: Some(path),
            disabled: parsed.disabled.into_iter().collect(),
            installed: parsed.installed.into_iter().collect(),
        })
    }

    pub fn is_enabled(&self, ext_id: &str) -> bool {
        !self.disabled.contains(ext_id)
    }

    pub fn set_enabled(&mut self, ext_id: &str, enabled: bool) -> Result<()> {
        let changed = if enabled {
            self.disabled.remove(ext_id)
        } else {
            self.disabled.insert(ext_id.to_string())
        };
        if !changed {
            return Ok(());
        }
        self.persist()
    }

    pub fn disabled(&self) -> Vec<String> {
        self.disabled.iter().cloned().collect()
    }

    pub fn installed(&self) -> Vec<String> {
        self.installed.iter().cloned().collect()
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else { return Ok(()); };
        let on_disk = OnDiskState {
            disabled: self.disabled.iter().cloned().collect(),
            installed: self.installed.iter().cloned().collect(),
        };
        let body = toml::to_string_pretty(&on_disk).context("serialize extension state")?;
        atomic_write(path, body.as_bytes())
    }
}

fn default_state_path() -> Result<PathBuf> {
    let dir = codesmith_config::ensure_state_dir(".")
        .context("could not resolve or create CodeSmith state directory")?;
    Ok(dir.join(STATE_FILE_NAME))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir for {}", path.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, bytes).with_context(|| format!("write tmp at {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename tmp into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh() -> (TempDir, ExtensionStateStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        let store = ExtensionStateStore::load_from(path).unwrap();
        (dir, store)
    }

    #[test]
    fn missing_file_defaults_to_everything_enabled() {
        let (_dir, store) = fresh();
        assert!(store.is_enabled("anything"));
        assert!(store.disabled().is_empty());
    }

    #[test]
    fn disable_then_reload_persists() {
        let (dir, mut store) = fresh();
        store.set_enabled("foo", false).unwrap();
        assert!(!store.is_enabled("foo"));
        let reloaded = ExtensionStateStore::load_from(dir.path().join(STATE_FILE_NAME)).unwrap();
        assert!(!reloaded.is_enabled("foo"));
        assert!(reloaded.is_enabled("bar"));
    }

    #[test]
    fn enable_removes_from_disabled_list() {
        let (_dir, mut store) = fresh();
        store.set_enabled("foo", false).unwrap();
        store.set_enabled("foo", true).unwrap();
        assert!(store.is_enabled("foo"));
    }

    #[test]
    fn redundant_toggle_is_noop() {
        let (_dir, mut store) = fresh();
        store.set_enabled("foo", true).unwrap();
        assert!(store.disabled().is_empty());
    }

    #[test]
    fn malformed_file_falls_back_to_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        fs::write(&path, b"this is not toml = { broken").unwrap();
        let store = ExtensionStateStore::load_from(path).unwrap();
        assert!(store.is_enabled("anything"));
    }

    #[test]
    fn disabled_list_is_deterministic_order() {
        let (_dir, mut store) = fresh();
        store.set_enabled("zeta", false).unwrap();
        store.set_enabled("alpha", false).unwrap();
        assert_eq!(store.disabled(), vec!["alpha".to_string(), "zeta".to_string()]);
    }
}
```

### 7.2 Declare the module + wire `App`

- In `crates/tui/src/main.rs` or wherever `SkillStateStore` is held on `App` (grep `SkillStateStore` in `crates/tui/src/`): add `pub mod extension_state;` to the relevant `mod` block, and add an `extension_state: ExtensionStateStore` field to `App` (initialized alongside `SkillStateStore` in the `App::new` path). If `App`'s struct definition is too coupled to touch in slice 1, hold the store inside the `ExtensionRunner` host wiring (Task 9) instead — the store is consulted by `build_extension_runtime`, not by every `App` method.

**Pragmatic call:** add `pub mod extension_state;` to `crates/tui/src/main.rs`'s module list (or `lib.rs` if tui has one). Do NOT add a field to `App` in slice 1 — `build_extension_runtime` (Task 9) loads the store transiently and passes it to the runner. This keeps `App` untouched (smaller blast radius). The plan records "App field wiring" as §F2 (when `/extension` commands need live state on `App`).

### 7.3 Verify (Green)

```bash
cargo +1.90.0 test -p codesmith-tui --lib extension_state 2>&1 | tail -8
cargo +1.90.0 build -p codesmith-tui 2>&1 | tail -3
```

Expected: 6 new state-store tests pass; tui builds.

---

## Task 8 — `extension_commands` bridge + `/extension` command group

**Files:**
- `crates/tui/Cargo.toml` (add `codesmith-extensions` dep)
- `crates/tui/src/commands/mod.rs` (add module + dispatch tier + `COMMANDS` entry + match arm)
- `crates/tui/src/commands/extension.rs` (NEW)

### 8.1 Add the dep to `crates/tui/Cargo.toml`

Insert into `[dependencies]` (after `codesmith-agent-runtime`):

```toml
codesmith-extensions = { path = "../extensions", version = "0.8.48" }
```

### 8.2 Green — write `crates/tui/src/commands/extension.rs`

```rust
//! `/extension` command group (spec §6.3). Dispatched via the
//! `extension_commands::try_dispatch` runtime lookup wired into
//! `execute()` between user-defined and the static `match` (Task 8.3).
//!
//! Slice 1 (phase 1, static): `list` / `info` / `enable` / `disable` /
//! `status` / `reload` work for compiled-in extensions. `install` /
//! `uninstall` stub "requires dylib loader (phase 2)" (§F5).

use crate::tui::app::App;
use codesmith_agent::extension::CommandOutput;

use super::CommandResult;

/// Runtime lookup mirror of `user_commands::try_dispatch_user_command`
/// (`crates/tui/src/commands/user_commands.rs:193`). Called from `execute()`
/// AFTER user-defined commands, BEFORE the static `match`. Returns `None`
/// when the command isn't an `/extension` invocation so `execute` falls
/// through to the static arms.
pub fn try_dispatch(app: &mut App, input: &str) -> Option<CommandResult> {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();
    let command = command.strip_prefix('/').unwrap_or(&command);
    if command != "extension" && command != "ext" {
        return None;
    }
    let sub = parts.get(1).and_then(|s| s.split_whitespace().next()).unwrap_or("");
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
    Some(match sub {
        "list" | "ls" => list(app),
        "info" => info(app, arg),
        "enable" => enable(app, arg),
        "disable" => disable(app, arg),
        "status" => status(app),
        "reload" => reload(app),
        "install" => install_stub(arg),
        "uninstall" => uninstall_stub(arg),
        _ => CommandResult::error(format!(
            "Unknown /extension subcommand: {sub:?}. Try: list, info, enable, disable, status, reload"
        )),
    })
}

fn runner(app: &App) -> Option<&std::sync::Arc<codesmith_extensions::ExtensionRunner>> {
    // The runner is bound to the engine host in build_extension_runtime
    // (Task 9) and surfaced on App for the command group. If slice 1 defers
    // the App field, this returns None and the commands report "not bound".
    app.extension_runner.as_ref()
}

fn list(_app: &App) -> CommandResult {
    // Slice 1: list compiled-in extensions via discover_static().
    let discovered = codesmith_extensions::discover_static();
    if discovered.is_empty() {
        return CommandResult::message("No extensions discovered.");
    }
    let mut out = String::from("Compiled-in extensions:\n");
    for reg in discovered {
        out.push_str(&format!("  {} (v{})\n", reg.metadata.id, reg.metadata.version));
    }
    CommandResult::message(out)
}

fn info(_app: &App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension info <id>");
    }
    let discovered = codesmith_extensions::discover_static();
    let Some(reg) = discovered.iter().find(|r| r.metadata.id == id) else {
        return CommandResult::error(format!("No extension with id '{id}'."));
    };
    CommandResult::message(format!(
        "id: {}\nversion: {}\ncontributions: (slice 1: see /extension status)\n",
        reg.metadata.id, reg.metadata.version
    ))
}

fn enable(app: &App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension enable <id>");
    }
    match app.extension_state.set_enabled(id, true) {
        Ok(()) => CommandResult::message(format!("Enabled extension '{id}' (takes effect on next /extension reload).")),
        Err(e) => CommandResult::error(format!("Failed to enable: {e}")),
    }
}

fn disable(app: &App, arg: &str) -> CommandResult {
    let id = arg.trim();
    if id.is_empty() {
        return CommandResult::error("Usage: /extension disable <id>");
    }
    match app.extension_state.set_enabled(id, false) {
        Ok(()) => CommandResult::message(format!("Disabled extension '{id}' (takes effect on next /extension reload).")),
        Err(e) => CommandResult::error(format!("Failed to disable: {e}")),
    }
}

fn status(app: &App) -> CommandResult {
    let Some(runner) = runner(app) else {
        return CommandResult::message("Extension runner not bound (no engine).");
    };
    CommandResult::message(format!(
        "Extension runner: generation={}, commands=[{}], tools={}\n\
         (slice 1: handler list + dispatch stats are §F2)",
        runner.generation(),
        runner.bound_command_names().join(", "),
        runner.bound_tools().len()
    ))
}

fn reload(app: &mut App) -> CommandResult {
    let Some(runner) = app.extension_runner.clone() else {
        return CommandResult::error("Extension runner not bound.");
    };
    runner.invalidate();
    CommandResult::message(format!(
        "Extension runner invalidated (generation now {}). Re-discovery + re-load of compiled-in extensions happens on next engine build (§F2 wires live reload).",
        runner.generation()
    ))
}

fn install_stub(arg: &str) -> CommandResult {
    CommandResult::error(format!(
        "/extension install {arg} requires the dylib loader (phase 2, §F5). Slice 1 supports compiled-in extensions only."
    ))
}

fn uninstall_stub(arg: &str) -> CommandResult {
    CommandResult::error(format!(
        "/extension uninstall {arg} requires the dylib loader (phase 2, §F5)."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_dispatch_returns_none_for_non_extension_command() {
        // Can't construct an App cheaply here; assert the prefix guard logic
        // by mirroring it. (Full App-based test is in commands/mod.rs smoke.)
        let input = "/skills list";
        let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
        let cmd = parts[0].to_lowercase();
        let cmd = cmd.strip_prefix('/').unwrap_or(&cmd);
        assert_ne!(cmd, "extension");
    }

    #[test]
    fn install_stub_is_phase_2_message() {
        let r = install_stub("git:foo/bar");
        assert!(r.is_error);
        assert!(r.message.unwrap().contains("phase 2"));
    }
}
```

**`App` field additions required by this task** (add to `App` struct + `App::new`):
- `pub extension_state: crate::extension_state::ExtensionStateStore`
- `pub extension_runner: Option<std::sync::Arc<codesmith_extensions::ExtensionRunner>>`

If `App`'s struct/`new` is too large to touch safely in slice 1, **fall back**: hold both on a new `pub struct ExtensionHost { state: ExtensionStateStore, runner: Option<Arc<ExtensionRunner>> }` and add a single `pub extension: ExtensionHost` field to `App`. Initialize in `App::new` with `ExtensionHost { state: ExtensionStateStore::load_default().unwrap_or_default(), runner: None }`. The runner is set later by `build_extension_runtime` (Task 9). Update `commands/extension.rs` to read `app.extension.state` / `app.extension.runner`.

**Pick the `ExtensionHost` fallback path** — it's cleaner (single field, single init site) and smaller-blast-radius. Update the handler functions accordingly (`app.extension.state.set_enabled(...)`, `app.extension.runner.as_ref()`).

### 8.3 Wire `try_dispatch` into `execute()`

In `crates/tui/src/commands/mod.rs`:

1. Add `mod extension_commands;` to the `pub mod`/`mod` block (after `mod debug;`, alphabetical).
2. In `execute()` (line 564-), between the tier-1 `if let Some(result) = user_commands::try_dispatch_user_command(...)` block (ends line 574) and the `match command {` (line 577), insert:

```rust
    // §F1 — extension command lookup (after user-defined, before static match).
    if let Some(result) = extension_commands::try_dispatch(app, cmd.trim()) {
        return result;
    }
```

3. Add a `COMMANDS` entry (inside the array, e.g. after the `slop` entry near `:540-562`):

```rust
    // Extension system (§F1)
    CommandInfo {
        name: "extension",
        aliases: &["ext"],
        usage: "/extension <list|info <id>|enable <id>|disable <id>|status|reload|install <src>|uninstall <id>>",
        description_id: MessageId::CmdHelpDescription, // reuse for now; §F2 adds a dedicated MessageId
    },
```

4. Add a `match` arm (so `/extension` also dispatches via the static match as a fallback; the runtime lookup in step 2 handles it first, but the arm satisfies exhaustiveness for the smoke test):

```rust
        "extension" | "ext" => extension_commands::try_dispatch(app, cmd.trim())
            .unwrap_or_else(|| CommandResult::error("Extension command dispatch failed")),
```

### 8.4 Red → Green — smoke test extension

The existing `every_registered_command_dispatches_to_a_handler` smoke test (line 1484) will now exercise `/extension` (since it's in `COMMANDS`). The test's invariant: `result.message` must not contain `"Unknown command"`. `/extension` with no subcommand hits the `_ =>` arm in `try_dispatch` which returns `CommandResult::error("Unknown /extension subcommand: ...")` — that contains "Unknown" but NOT "Unknown command" (the smoke test's exact substring is `"Unknown command"`, per `commands/mod.rs:1494`). Verify the substring: the smoke test checks `.contains("Unknown command")` — `"Unknown /extension subcommand"` does NOT contain `"Unknown command"` (space + "subcommand" vs " command"). **However**, to be safe, rename the error to avoid the word "Unknown" entirely:

Change the `_ =>` arm in `extension.rs` to:

```rust
        _ => CommandResult::error(format!(
            "Unsupported /extension subcommand: {sub:?}. Try: list, info, enable, disable, status, reload"
        )),
```

Re-run the smoke test; it should pass.

### 8.5 Verify (Green)

```bash
cargo +1.90.0 test -p codesmith-tui --bin 2>&1 | tail -5
```

Expected: baseline 2844 pass + 2 ignored (slice 52) maintained + the new `extension_commands` unit tests pass + the smoke test now also exercises `/extension`. If the smoke test fails on `/extension`, the dispatch returned an "Unknown command" — fix per 8.4.

---

## Task 9 — `build_extension_runtime()` + `HostExtensionContext` wiring in `codesmith-tui`

**Files:** `crates/tui/src/core/engine.rs`.

### 9.1 Green — write `build_extension_runtime()`

Place near `configure_plugin_tools` (`engine.rs:213-238`):

```rust
// === §F1 Extension runtime wiring ===

/// Discover compiled-in extensions, reconcile with `ExtensionStateStore`,
/// load + configure each against a stub `ExtensionApi`, then `bind_core`
/// the host context. Returns the bound runner (for `HostAgentExecutor`'s
/// `with_extension_runner`) + the state store (for `/extension` commands).
///
/// Mirrors the §6.1 reload sequence (steps 2-5): re-discover → reconcile
/// → re-load → re-configure → bind_core. Slice 1 does NOT re-discover on
/// reload (§F2 wires live reload); `build_extension_runtime` runs once at
/// engine build.
pub fn build_extension_runtime(
    workspace: &std::path::Path,
    cancel_token: tokio_util::sync::CancellationToken,
) -> (
    Arc<codesmith_extensions::ExtensionRunner>,
    crate::extension_state::ExtensionStateStore,
) {
    let runner = Arc::new(codesmith_extensions::ExtensionRunner::new());
    let state = crate::extension_state::ExtensionStateStore::load_default()
        .unwrap_or_default();

    // 1. Discover compiled-in extensions (inventory).
    let discovered = codesmith_extensions::discover_static();

    // 2. Reconcile with state: skip disabled.
    let enabled: Vec<_> = discovered
        .into_iter()
        .filter(|reg| state.is_enabled(reg.metadata.id))
        .collect();

    // 3. Load + configure each against the stub api (async — needs a runtime).
    let gen_arc = runner.generation_arc(); // see 9.2 — add this accessor
    let rt = tokio::runtime::Handle::try_current().unwrap_or_else(|_| {
        tokio::runtime::Runtime::new().expect("extension runtime").handle().clone()
    });
    let _ = gen_arc; // runner.load uses the runner's internal generation
    for reg in &enabled {
        let ext = (reg.factory)();
        let _ = rt.block_on(runner.load(&*ext)); // best-effort; errors logged
    }

    // 4. Build the host context + bind_core.
    let idle = Arc::new(std::sync::Mutex::new(true));
    let ctx = Arc::new(codesmith_extensions::HostExtensionContext::new(
        workspace.to_path_buf(),
        codesmith_agent::extension::ExtensionMode::Tui,
        idle,
        cancel_token,
        runner.generation_arc(),
    ));
    runner.bind_core(ctx);

    (runner, state)
}
```

### 9.2 Add `generation_arc()` accessor to `ExtensionRunner`

In `crates/extensions/src/runner.rs`, add:

```rust
    /// Expose the generation `Arc<AtomicU64>` so the host can construct a
    /// `HostExtensionContext` sharing the same counter (stale-context
    /// consistency between runner + context).
    pub fn generation_arc(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.generation)
    }
```

### 9.3 Wire into `build_engine`

In `build_engine` (`engine.rs:327-`), after the `cancel_token` creation (`:347`) and before the LLM resolution (`:369`), insert:

```rust
    // §F1 — build the extension runtime + bind to the host executor.
    let (extension_runner, extension_state) =
        build_extension_runtime(&config.workspace, cancel_token.clone());
    host.extension = Some(crate::extension_state::ExtensionHost {
        state: extension_state,
        runner: Some(extension_runner.clone()),
    });
```

(If `ExtensionHost` is the chosen fallback from Task 8, add `pub extension: ExtensionHost` to `App`/`EngineHost` and initialize as above. Adjust `build_engine` to write into `host.extension` if `EngineHost` owns it, OR write into the `Engine` struct if that's where `App` reads it. The exact site depends on where `App` fields are populated — grep `app.skills_dir` or similar to find the `App` init path.)

Then in the `HostAgentExecutor` construction (wherever `HostAgentExecutor::new(...)` is called in `engine.rs` or `agent-runtime`), chain `.with_extension_runner(Some(extension_runner))`.

### 9.4 Verify (Green)

```bash
cargo +1.90.0 build -p codesmith-tui 2>&1 | tail -5
cargo +1.90.0 test -p codesmith-tui --bin 2>&1 | tail -3
```

Expected: tui builds; the smoke + existing tests stay green. If `build_extension_runtime`'s `rt.block_on` inside an async context panics ("Cannot start a runtime from within a runtime"), switch to `tokio::task::block_in_place` or restructure to make `build_extension_runtime` async (called via `.await` from `build_engine` if `build_engine` is async — it isn't, it's sync). The robust fix: make `build_extension_runtime` spawn the loads on a blocking thread, OR make `ExtensionRunner::load` non-async-by-fallback (configure is async in the trait — can't avoid). Cleanest: use `tokio::task::block_in_place(|| rt.block_on(...))` (requires the `rt-multi-thread` feature, already on via `tokio = { features = ["full"] }`). Apply this.

---

## Task 10 — In-tree sample extension (`scratchpad`)

**Files:** `crates/extensions/src/sample_scratchpad.rs` (NEW), `crates/extensions/src/lib.rs` (add `pub mod sample_scratchpad;`).

### 10.1 Green — write `crates/extensions/src/sample_scratchpad.rs`

The sample contributes: a `scratch` tool (writes/reads a per-session scratch string), a `/scratch` command (prints the scratchpad), a `TurnStart` handler (logs the turn id). Mirrors `mock.rs` as the "reference sample".

```rust
//! In-tree sample extension (`scratchpad`) — the reference sample for the
//! extension system (mirrors `crates/providers/src/mock.rs` for providers).
//! Contributes all three slice-1 contribution points: a tool, a command,
//! an event handler. Compiled in via `inventory::submit!`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::*;
use codesmith_tools::ToolCapability;
use serde_json::{json, Value};

use crate::ExtensionMetadata;
use crate::discovery::ExtensionRegistration;

/// Shared scratchpad string (per-process; slice 1 — a real per-session store
/// is §F2 via `ExtensionContext`).
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

pub struct ScratchTool;
#[async_trait]
impl ToolDefinition for ScratchTool {
    fn name(&self) -> &str { "scratch" }
    fn description(&self) -> &str {
        "Write or read a scratch string. Pass {\"op\":\"set\",\"text\":\"...\"} to set, or {\"op\":\"get\"} to read."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["get", "set"] },
                "text": { "type": "string" }
            },
            "required": ["op"]
        })
    }
    fn capabilities(&self) -> Vec<ToolCapability> { vec![ToolCapability::ReadOnly] }
    async fn execute(&self, input: Value, _ctx: &dyn ExtensionContext) -> Result<ToolResult, ExtensionError> {
        let op = input.get("op").and_then(|v| v.as_str()).unwrap_or("get");
        match op {
            "set" => {
                let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                *SCRATCH.lock().unwrap() = Some(text.clone());
                Ok(ToolResult::success(format!("scratch set to {text:?}")))
            }
            "get" | _ => {
                let val = SCRATCH.lock().unwrap().clone().unwrap_or_default();
                Ok(ToolResult::success(val))
            }
        }
    }
}

pub struct ScratchCommand;
#[async_trait]
impl CommandDefinition for ScratchCommand {
    fn name(&self) -> &str { "scratch" }
    fn description(&self) -> &str { "Print the current scratchpad contents." }
    async fn run(&self, _ctx: &dyn ExtensionCommandContext, _args: &str) -> Result<CommandOutput, ExtensionError> {
        let val = SCRATCH.lock().unwrap().clone().unwrap_or_else(|| "(empty)".into());
        Ok(CommandOutput::Message(format!("scratchpad: {val}")))
    }
}

pub struct TurnStartLogger;
#[async_trait]
impl Handler for TurnStartLogger {
    async fn handle(&self, event: &ExtensionEvent, _ctx: &dyn ExtensionContext) -> Result<(), ExtensionError> {
        if let ExtensionEvent::TurnStart { turn_id } = event {
            tracing::debug!("[scratchpad] TurnStart turn_id={turn_id}");
        }
        Ok(())
    }
}

inventory::submit! {
    ExtensionRegistration {
        factory: || Box::new(ScratchpadExtension),
        metadata: ExtensionMetadata::new("scratchpad"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::discover_static;

    #[test]
    fn scratchpad_is_discoverable() {
        let all = discover_static();
        assert!(all.iter().any(|r| r.metadata.id == "scratchpad"));
    }

    #[tokio::test]
    async fn scratch_tool_round_trips() {
        // Reset + set + get.
        *SCRATCH.lock().unwrap() = None;
        let tool = ScratchTool;
        struct Ctx;
        #[async_trait]
        impl ExtensionContext for Ctx {
            fn cwd(&self) -> &std::path::Path { std::path::Path::new(".") }
            fn mode(&self) -> ExtensionMode { ExtensionMode::Tui }
            fn is_idle(&self) -> bool { true }
            fn signal(&self) -> tokio_util::sync::CancellationToken { tokio_util::sync::CancellationToken::new() }
            fn generation(&self) -> u64 { 0 }
        }
        let set = tool.execute(json!({"op":"set","text":"hello"}), &Ctx).await.unwrap();
        assert!(set.success);
        let get = tool.execute(json!({"op":"get"}), &Ctx).await.unwrap();
        assert_eq!(get.content, "hello");
    }
}
```

### 10.2 Declare + verify

Add `pub mod sample_scratchpad;` to `crates/extensions/src/lib.rs`. Verify:

```bash
cargo +1.90.0 test -p codesmith-extensions --lib 2>&1 | tail -8
```

Expected: all prior tests + the 2 new scratchpad tests pass. `/extension list` (Task 8) will now show `scratchpad`.

---

## Task 11 — Docs (ROADMAP §F1 + ARCHITECTURE.md + `docs/EXTENSIONS.md`)

### 11.1 ROADMAP §F section + §F1 progress entry

Append to `ROADMAP.md` after §E (currently ends at EOF, line 2707):

```markdown
## §F — Extension system (pi-mono parity)

The provider seam (§A) + framework-core traits (§E) are the foundation. §F
builds the **extension system** on top: a unified `Extension` concept with
imperative registration, lifecycle events, extension-to-extension bus,
runtime provider registration, unified discovery/manifest, stale-context
guard — ported from pi-mono's extension model. Mirrors the §E three-layer
pattern (traits in `codesmith-agent`, runtime in `codesmith-extensions`,
adapters in `codesmith-agent-runtime`, host wiring in `codesmith-tui`).
Delivered in slices; hot-load is permanently out (install + reload only).

### F1 — Slice 1 (foundational core, phase 1 static)

- Core traits in `codesmith-agent::extension` (`Extension` /
  `ExtensionApi` / `ExtensionContext` / `ExtensionCommandContext` /
  `ExtensionEvent` / `Handler` / `ToolDefinition` / `CommandDefinition`)
  with the minimal 6-event set (`#[non_exhaustive]`).
- New crate `codesmith-extensions`: `ExtensionRunner` (event dispatch +
  stale-context guard) + `ExtensionApi` stub→real + `inventory`-based static
  discovery + `EventBus` skeleton + install-source traits (impls §F5).
- `codesmith-agent-runtime`: `ExtensionToolSpecAdapter` (mirrors
  `ToolSpecAdapter`) + `HostAgentExecutor` seam wiring (TurnStart/ToolCall/
  ToolResult/TurnEnd emits).
- `codesmith-tui`: `build_extension_runtime()` + `ExtensionStateStore`
  (mirrors `SkillStateStore`) + `/extension` command group (list/info/enable/
  disable/status/reload working; install/uninstall stub "phase 2").
- In-tree sample `scratchpad` extension (tool + command + handler).
- `docs/EXTENSIONS.md` developer guide + sandbox stance.

**Status (slice 1 §F1):** done. Minimal contract + runtime + adapters + host
wiring + sample + docs landed. Deferred to §F2–§F8: full ~30-event lifecycle,
cancel/transform/block chains, `EventBus` impl, `registerProvider`,
`registerShortcut`/`registerFlag`/renderers, dylib loading (phase 2),
install-source impls, embed API. Hot-load permanently out.
```

Then append the slice progress entry to the `## 进度（2026-07 检查点）` section (before the `---` separator at line 2410), mirroring the slice 53 entry shape (§11 of the spec-conv report):

```markdown
**进度（2026-07-21 §F slice 1 extension system foundational core——pi-mono Extension 模型 port 的 slice 1：核心 traits + 新 crate codesmith-extensions + agent-runtime adapter + tui host wiring + sample + docs，`feat/pluggable-framework-core`）：**

接 slice 53（§E stale-absorbed doc-debt cleanup，`:2381`）。本切片开新 ROADMAP §F section，落地 pi-mono extension 模型的 found core（phase 1 静态加载）——spec `docs/superpowers/specs/2026-07-21-codesmith-extension-system-design.md` §10.1 scope。镜像 §E 三层模式：traits in codesmith-agent、runtime in codesmith-extensions（新 crate）、adapters in codesmith-agent-runtime、host wiring in codesmith-tui。

**关键设计决策：**
- **§11 open questions 本切片定**：(1) sample = `scratchpad`（tool + command + handler，验证三个 contribution point）；(2) `ExtensionCommandContext: ExtensionContext` sub-trait，slice 1 零 session-mutation 方法（split 为 type-safety + §F2 growth）；(3) 单个 `Handler` trait（observer-only，`async fn handle(event, ctx) -> Result<(), ExtensionError>`），per-variant subscription + `HandlerOutcome`（cancel/transform/block）defer §F2；(4) §10.3 vs §10.2 tension → §10.2 authoritative（observer-only；catch_unwind 真实隔离 §F2）。
- **`#[async_trait]` 引入 codesmith-agent**：既有 Tool/Callback/AgentExecutor 用 manual `Pin<Box<dyn Future>>`；extension traits 面向 extension author（外部 crate），`#[async_trait` 显著友好，匹配 spec literal + ToolSpec/HookSink 惯例。代价：codesmith-agent +2 deps（async-trait + tokio-util）。
- **ExtensionToolSpecAdapter 镜像 ToolSpecAdapter**：held `Arc<dyn ToolDefinition>` + `Arc<dyn ExtensionContext>`，`execute` 委托 `ToolDefinition::execute(input, &*ctx)`；`input_schema()` 强制 object-rooted（`build_tool` fail-closed chokepoint 要求）。
- **HostAgentExecutor seam wiring 四点**：TurnStart（`:3709` 后 user-msg push）、ToolCall（`:4339`/`:4423` on_tool_start 旁）、ToolResult（`:4416`/`:4508` on_tool_end 旁）、TurnEnd（`:4226` NoToolCalls + `:3750` Checkpoint A Interrupted）。其余 terminal sites + step-end 是 §F2 hardening。
- **`ExtensionStateStore` 镜像 `SkillStateStore` verbatim**：TOML + atomic write + malformed→default + BTreeSet；加 `installed` field（§F5 provenance forward-compat）。
- **`build_extension_runtime` 四步**：discover_static → reconcile w/ state → load+configure（stub api）→ bind_core（HostExtensionContext）。reload 的 re-discover 是 §F2（slice 1 仅 build-time）。
- **`/extension install`/`uninstall` stub "phase 2"**：slice 1 静态无法 runtime install（by definition）；install-source 抽象仅 trait（impl §F5）。

**落地步骤：**
1. `crates/agent/Cargo.toml` + `src/lib.rs` + `src/extension.rs`（NEW）：8 traits + `ExtensionError`/`ExtensionMetadata`/`ExtensionEvent` minimal set + 5 test。
2. `Cargo.toml`（root）+ `crates/extensions/{Cargo.toml,src/lib.rs + 6 sub-mod}`：新 crate。
3. `crates/extensions/src/{runner,api,bus,state,discovery,install_source}.rs`：runtime + stub/real api + bus skeleton + host context + discovery + install traits。
4. `crates/agent-runtime/{Cargo.toml,src/tools/mod.rs,src/tools/extension.rs}`：adapter + dep + module。
5. `crates/agent-runtime/src/engine/host_executor.rs`：`extension: Option<Arc<ExtensionRunner>>` field + `with_extension_runner` builder + 4 seam emits（`:3709`/`:4339`/`:4416`/`:4423`/`:4508`/`:4226`/`:3750`）。
6. `crates/tui/src/extension_state.rs`（NEW）+ `commands/{mod.rs,extension.rs}`（NEW）：state store + command group + execute() tier。
7. `crates/tui/src/core/engine.rs`：`build_extension_runtime()` + wire into `build_engine`。
8. `crates/extensions/src/sample_scratchpad.rs`（NEW）：sample + `inventory::submit!`。
9. `ROADMAP.md` + `ARCHITECTURE.md` + `docs/EXTENSIONS.md`（NEW）：§F section + §F1 entry + dev guide。

**测试：** `cargo +1.90.0 build --workspace` 绿；`cargo +1.90.0 test -p codesmith-extensions --lib` 绿（新 crate）；`cargo +1.90.0 test -p codesmith-agent --lib` 绿（+5 extension::tests）；`cargo +1.90.0 test -p codesmith-agent-runtime --lib` 绿（+2 adapter tests，baseline 1149→1151 pass + 2 ignored）；`cargo +1.90.0 test -p codesmith-tui --bin` 绿（baseline 2844 pass + 2 ignored maintained + smoke test 现 exercise /extension）。

**验证：** grep `pub trait Extension` 跨 `crates/agent/src/extension.rs` → 8 hits；grep `ExtensionToolSpecAdapter` 跨 `crates/agent-runtime` → 1 def + test refs；grep `with_extension_runner` 跨 `host_executor.rs` → 1 hit；grep `build_extension_runtime` 跨 `crates/tui` → 1 def + 1 call；grep `discover_static` 跨 `crates` → 1 def + sample + call；grep `extensions_state.toml` 跨 `crates/tui` → state.rs hits；`/extension list` 在 tui 运行报 `scratchpad`。

**By-design gaps（out of scope, §F2–§F8）：**
- **完整事件集**（~25 更多变体 + cancel/transform/block 链）——§F2。slice 1 Handler observer-only。
- **catch_unwind 真实隔离**——§F2（slice 1 emit 直接 await；panic 会传播——documented）。
- **EventBus 完整 impl**——§F3（slice 1 skeleton，subscribe/publish 返回 Unimplemented）。
- **registerProvider**——§F4。
- **Dylib 加载 + install/uninstall 真实现 + install-source impl + trust prompt + extension.toml manifest**——§F5。
- **Renderer / Shortcut / Flag**——§F6/§F7。
- **嵌入 API**——§F8。
- **Hot-load**——永不（§2.4）。
- **App 字段 wiring**（live `/extension` state on App）——slice 1 用 `ExtensionHost` fallback；`App` 字段直接 wiring 是 §F2（当 live reload 需要）。
- **Host executor 端到端 round-trip test**（mock client + assert seen == [ToolCall, ToolResult, TurnEnd]）——§F2（slice 1 land compile-time + no-op test）。

**下一聚焦工作：**
- §F2：完整事件集（~25 变体）+ cancel/transform/block 链 + per-variant Handler subscription + catch_unwind 真实隔离 + Host executor 端到端 round-trip test + App 字段 live wiring + reload re-discover。
- §F3-F8 按需。
```

### 11.2 ARCHITECTURE.md — add `## The extension system (§F)` section + status rows

After the §E section (ends line 290) and before/within the "What is wired today" table (line 292), add a new section mirroring §E's shape (header + intro + trait list + ASCII diagram + "What is here" paragraph + per-sample validation note). Add two rows to the status table:

```markdown
| Extension system (§F1 foundational core) | ✅ done (slice 1 §F1) — minimal 6-event contract + runtime + adapter + host wiring + sample; full lifecycle + EventBus impl + dylib + install-source deferred to §F2–§F8 | `crates/agent/src/extension.rs`, `crates/extensions/`, `crates/agent-runtime/src/tools/extension.rs`, `crates/agent-runtime/src/engine/host_executor.rs`, `crates/tui/src/{extension_state.rs,commands/extension.rs,core/engine.rs}` |
| Extension system docs | ✅ done (slice 1 §F1) | `docs/EXTENSIONS.md` |
```

### 11.3 `docs/EXTENSIONS.md` — developer guide + sandbox stance

Mirror `docs/MCP.md` structure. Sections: `# Extensions`; `## Bootstrap`; `## In-TUI Manager` (`/extension` subcommands table from spec §6.3); `## Discovery` (phase 1 static via `inventory`; phase 2 dylib deferred); `## Minimal Example` (the `scratchpad` sample, verbatim); `## Extension Fields` (the trait contracts); `## Sandbox Stance` (spec §8.1 — "no sandbox, trust the source; project-local requires trust prompt (§F5); containerize for untrusted"); `## Troubleshooting`. Write the file.

### 11.4 Verify (Green)

```bash
cargo +1.90.0 build --workspace 2>&1 | tail -5
grep -c "pub trait Extension" crates/agent/src/extension.rs        # expect 1
grep -rn "ExtensionToolSpecAdapter" crates/agent-runtime/src/      # def + test
grep -rn "with_extension_runner" crates/agent-runtime/src/engine/host_executor.rs  # 1
grep -rn "build_extension_runtime" crates/tui/src/                # def + call
grep -rn "discover_static" crates/ | grep -v target                # def + sample + call
test -f docs/EXTENSIONS.md && echo "EXTENSIONS.md present"
```

All green → slice 1 done.

---

## Self-review (run before declaring done)

Check each against the spec §10.1 + §10.2 + §10.3:

- [ ] `crates/agent/src/extension.rs` defines all 8 named traits/types: `Extension`, `ExtensionApi`, `ExtensionContext`, `ExtensionCommandContext`, `ExtensionEvent`, `Handler`, `ToolDefinition`, `CommandDefinition` (+ `ExtensionError`, `ExtensionMetadata`, `CommandOutput`, `ExtensionMode`, `ContextUsage`, `SessionReason`, `TurnEndReason`, `ToolCallEvent`, `ToolResultEvent`).
- [ ] `ExtensionEvent` minimal set = `SessionStart`/`TurnStart`/`ToolCall`/`ToolResult`/`TurnEnd`/`SessionShutdown`, `#[non_exhaustive]`.
- [ ] `codesmith-extensions` crate exists, in workspace `members`, `codesmith-agent` + `codesmith-tui` + `codesmith-agent-runtime` depend on it (the last two do; agent does NOT — traits are in agent, runtime in extensions which depends on agent).
- [ ] `ExtensionRunner` has `generation()`/`invalidate()`/`load()`/`bind_core()`/`emit()`/`try_dispatch_command()`/`bound_tools()`/`bound_command_names()`.
- [ ] `ExtensionApi` has stub + real impl; stale-context guard returns `ExtensionError::StaleContext` after `invalidate()`.
- [ ] `discover_static()` + `ExtensionRegistration` + `inventory::collect!`/`submit!` work (test passes).
- [ ] `EventBus` skeleton exists; `subscribe`/`publish` return `Unimplemented`.
- [ ] install-source traits (`ExtensionSource`/`ExtensionBuilder`/`ExtensionPlacer`) exist; no impls (§F5).
- [ ] `ExtensionToolSpecAdapter` impls `ToolSpec` (all 5 required methods); passes the `build_tool` fail-closed chokepoint (object-rooted schema + valid name).
- [ ] `HostAgentExecutor` has `extension: Option<Arc<ExtensionRunner>>` + `with_extension_runner` builder; 4 seam emits (TurnStart, ToolCall ×2, ToolResult ×2, TurnEnd ×2) wired; `extension: None` keeps the 1149 baseline green.
- [ ] `ExtensionStateStore` mirrors `SkillStateStore` (6 tests pass).
- [ ] `extension_commands::try_dispatch` wired into `execute()` between user-defined and static match; `/extension` in `COMMANDS`; smoke test passes.
- [ ] `build_extension_runtime()` exists; called from `build_engine`; runner bound to `HostAgentExecutor` via `with_extension_runner`.
- [ ] In-tree `scratchpad` sample registers a tool + command + handler via `inventory::submit!`; discoverable by `/extension list`; its 2 tests pass.
- [ ] ROADMAP §F section + §F1 progress entry present (mirror slice 53 shape).
- [ ] ARCHITECTURE.md has the §F section + 2 status rows.
- [ ] `docs/EXTENSIONS.md` present with the required sections.
- [ ] All 5 verify commands in §10.4 green: `build --workspace`, `test -p codesmith-extensions --lib`, `test -p codesmith-agent --lib`, `test -p codesmith-agent-runtime --lib`, `test -p codesmith-tui --bin`.
- [ ] No stale "deferred" markers in the new code (§10.4 grep gate).
- [ ] The §11 open-question resolutions + the §10.3/§10.2 tension resolution are documented in the ROADMAP §F1 entry (By-design gaps).

## By-design gaps recorded for §F2+ (do NOT fix in slice 1)

- Handler `catch_unwind` real isolation (slice 1 `emit` awaits directly).
- Full event set + cancel/transform/block chains + per-variant subscription.
- `EventBus` impl.
- `registerProvider`, renderers, shortcuts/flags, embed API.
- Dylib loading (phase 2) + install-source impls + trust prompt + `extension.toml` manifest + project-local discovery.
- `App` live field wiring for `/extension` commands (slice 1 uses `ExtensionHost` fallback).
- `HostAgentExecutor` end-to-end round-trip test (slice 1 lands compile-time + no-op test only).
- Other 5 `TurnEnd` terminal sites + step-end seam.
- `:N` command conflict suffixing (slice 1 first-wins via HashMap).
- `MessageId::CmdExtensionDescription` (slice 1 reuses `CmdHelpDescription`).
