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
//!
//! ## Style note
//!
//! The existing `Tool` / `Callback` / `AgentExecutor` traits in this crate
//! use the manual `Pin<Box<dyn Future + Send + '_>>` pattern (no
//! `#[async_trait]`). The extension traits are implemented by **extension
//! authors** in external crates, where `#[async_trait]` is markedly
//! friendlier; it also matches the spec literally (line 192) and the
//! `ToolSpec` / `HookSink` convention in `codesmith-agent-runtime` /
//! `codesmith-hooks`. The cost is two new deps on this core crate
//! (`async-trait`, `tokio-util`); both are workspace staples.

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
/// `id` is the stable key (lowercase, `-`-separated); `name` is the human
/// display; `version` mirrors the crate version. Slice 1 populates from
/// [`ExtensionMetadata::new`]`(id)`.
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

/// Payload for [`ExtensionEvent::ToolCall`].
#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Payload for [`ExtensionEvent::ToolResult`].
#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub id: String,
    pub name: String,
    /// The framework `ToolResult`/`ToolError` pair (re-exported via
    /// `codesmith_tools`), so the extension runtime stays decoupled from
    /// `codesmith-agent-runtime`'s `ToolSpec`/`ToolContext`.
    pub result: Result<ToolResult, ToolError>,
}

/// Lifecycle events. Slice 1 minimal set (spec §10.1):
/// `SessionStart` / `TurnStart` / `ToolCall` / `ToolResult` / `TurnEnd` /
/// `SessionShutdown`. `#[non_exhaustive]` so §F2 can add the remaining ~25
/// variants without breaking downstream match arms. Handler dispatch is
/// open (any `Handler` may subscribe to any variant).
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
/// on the sub-trait [`ExtensionCommandContext`] (handed only to command
/// handlers). Mirrors pi-mono `ExtensionContext` (spec §4 line 193-204).
///
/// Slice 1: the observation methods (`cwd`, `mode`, `is_idle`, `signal`,
/// `generation`) are real (host-backed). The action methods (`abort`,
/// `shutdown`, `compact`, `get_context_usage`) are stubbed by the host impl
/// to return [`ExtensionError::Unimplemented`] — §F2 wires them. Handlers
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

/// Handed to command handlers. A strict sub-trait of [`ExtensionContext`]:
/// slice 1 adds **zero** session-mutation methods (the split exists for
/// type-safety + §F2 growth — pi-mono hands command handlers a richer
/// context with `sendMessage`/`appendEntry`/etc.). Command handlers
/// receive this and return a framework-agnostic [`CommandOutput`].
///
/// Declared without `#[async_trait]`: it adds no async methods in slice 1,
/// so the macro is unnecessary (and avoids the macro-on-sub-trait edge
/// case). §F2 adds async session-mutation methods + `#[async_trait]` here.
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
/// [`ExtensionContext`] (NOT the host's `ToolContext`) — keeping extensions
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
/// [`ExtensionApi::register_command`]; dispatched by the host's
/// `extension_commands::try_dispatch` (Task 8) which calls [`CommandDefinition::run`].
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
/// an [`ExtensionError`]; the runner fans out best-effort (per §8.3; one
/// failing handler does not block others — slice 1 awaits directly, §F2
/// hardens with proper `catch_unwind`). `HandlerOutcome`
/// (cancel/transform/block) is §F2.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        ctx: &dyn ExtensionContext,
    ) -> Result<(), ExtensionError>;
}

// === ExtensionApi (registration surface, two-phase) =======================

/// The imperative registration surface an [`Extension::configure`] receives.
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

/// An extension: a factory that receives an [`ExtensionApi`] and registers its
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
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Run a future on a one-shot tokio runtime. (`codesmith-agent` does
    /// not depend on the `futures` crate's executor; tokio is already a dep.)
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new()
            .expect("tokio runtime for extension::tests")
            .block_on(f)
    }

    struct TestContext {
        cwd: PathBuf,
        generation: u64,
        signal: CancellationToken,
    }

    impl TestContext {
        fn new() -> Self {
        Self {
            cwd: PathBuf::from("."),
            generation: 1,
            signal: CancellationToken::new(),
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
            self.generation
        }
    }

    // ExtensionCommandContext adds no methods — a blanket marker impl for
    // any TestContext that already impls ExtensionContext.
    impl ExtensionCommandContext for TestContext {}

    /// A recording handler — proves the trait shape lets a handler observe
    /// every minimal-set variant.
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
        let out = block_on(tool.execute(json!({"text":"hi"}), &ctx)).unwrap();
        assert!(out.success);
        assert_eq!(out.content, "echo:hi");
    }

    #[test]
    fn command_definition_run_returns_message() {
        let cmd = GreetCommand;
        let ctx = TestContext::new();
        let out = block_on(cmd.run(&ctx, "world")).unwrap();
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
            block_on(h.handle(ev, &ctx)).unwrap();
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
        // without breaking (compile-time check via match).
        let ev = ExtensionEvent::SessionShutdown;
        let _label: &str = match &ev {
            ExtensionEvent::SessionStart { .. } => "start",
            ExtensionEvent::SessionShutdown => "shutdown",
            _ => "other",
        };
    }
}
