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
//! ~30-event lifecycle + cancel/transform/block chains + dylib loading +
//! install-source impls are deferred to §F2–§F8 (ROADMAP §F). Extension-to-
//! extension pub/sub (the former §F3 EventBus skeleton) was deleted with
//! zero consumers; it returns only when an extension actually needs it.
//!
//! ## Kernel composition (DSH alignment)
//!
//! CodeSmith adopts DSH's kernel-composition model at two seams, by
//! founder decision (2026-10-30), replacing the earlier stance that the
//! kernel stayed compile-time assembled with the extension surface as a
//! seam onto it:
//!
//! - **Selection** — the main turn loop is a composed kernel service: the
//!   engine resolves a config-selected factory (`turn_executor`, default
//!   `builtin`) from the process-shared `TurnExecutorRegistry`
//!   (`codesmith-agent-runtime`, `engine/executor_composition.rs`) at
//!   every turn assembly; alternates land as registry entries.
//! - **Composition** — [`AroundTurnHandler`] wraps the whole main turn
//!   with DSH's `next()` form ([`ExtensionApi::register_around_turn`]),
//!   so an extension composes behaviour around whichever loop runs, with
//!   generation-scoped unload.
//!
//! What stays core by contract: the provider registry *mechanism* (its
//! factories are extension-contributed, route A), the session log format,
//! the compaction algorithm (its knobs are policy), `BASE_PROMPT`, and the
//! sub-agent loops (children run the builtin machinery; neither seam
//! reaches them). Known limitations (deliberate): `Waterfall` remains a
//! fold chain (each handler's `Transform` is visible to the next);
//! `next()`-wrapping exists for tool execution ([`AroundToolHandler`])
//! and the main turn ([`AroundTurnHandler`]) — not as a general event
//! form — and parallel fan-out / first-value-wins dispatch are not built;
//! add one only with a real consumer. A dylib cannot yet ship a
//! from-scratch loop implementation (the factory contract would have to
//! name runtime-side types across this crate's boundary) — it wraps.
//!
//! [`DispatchMode`] names its three states after the deepseek-harness
//! (DSH) extension model — `Emit` (broadcast, advisory outcomes),
//! `Serial` (ordered listeners, veto short-circuits), `Waterfall
//! { can_deny }` (transform fold, optionally deny) — so both frameworks
//! share one vocabulary for the same dispatch semantics.
//! Registration scopes ([`Scope`]) are
//! resolved per origin tier: sub-agent registries mount only script /
//! compiled-in extension tools, and sub-agent loops dispatch only script /
//! compiled-in handlers at their scope (a detached background child can
//! outlive a reload's two-phase dylib drop, so dylib contributions stay
//! main-turn-only; a dylib cannot register scoped tools/handlers at all).
//! Sub-agent loops emit only `ToolCall` / `ToolResult` at their scope —
//! turn-level and provider-level events stay main-turn-only.
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

use std::borrow::Cow;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_tools::{ToolCapability, ToolError, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::callback::StopReason;
use crate::provider::{ProviderAlias, ProviderFactory};

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
    /// extension error surface. Lossy: only the rendered message survives
    /// (the structured variant is flattened and `tool` is left empty — the
    /// source carries no tool name), and the
    /// `ExtensionToolSpecAdapter` re-flattens to
    /// `ToolError::execution_failed` on the way back anyway.
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
/// display; `version` mirrors the crate version — note that
/// [`ExtensionMetadata::new`] can only bake the `codesmith-agent` framework
/// crate's version (`env!` is evaluated when this crate is compiled); an
/// extension whose own crate version should be reported must carry it via
/// [`ExtensionMetadata::from_strings`].
///
/// Fields are `Cow<'static, str>` so both registration styles coexist:
/// compiled-in extensions keep the const [`ExtensionMetadata::new`] (borrowed
/// `&'static str`), while script Mods (§F script mod layer) — whose
/// id/name/version are parsed from `mod.toml` at discovery time — use
/// [`ExtensionMetadata::from_strings`] (owned `String`s).
#[derive(Debug, Clone)]
pub struct ExtensionMetadata {
    pub id: Cow<'static, str>,
    pub name: Cow<'static, str>,
    pub version: Cow<'static, str>,
}

impl ExtensionMetadata {
    /// Const constructor for compiled-in extensions. `version` reports the
    /// `codesmith-agent` framework crate's version, NOT the calling
    /// extension's own version — `env!` here is evaluated against this
    /// crate. Use [`ExtensionMetadata::from_strings`] to carry the
    /// extension's real version.
    #[must_use]
    pub const fn new(id: &'static str) -> Self {
        Self {
            id: Cow::Borrowed(id),
            name: Cow::Borrowed(id),
            version: Cow::Borrowed(env!("CARGO_PKG_VERSION")),
        }
    }

    /// Owned-string constructor for runtime-discovered sources (script Mods).
    /// `name` defaults to the id when empty.
    #[must_use]
    pub fn from_strings(id: String, name: String, version: String) -> Self {
        let name = if name.trim().is_empty() {
            id.clone()
        } else {
            name
        };
        Self {
            id: Cow::Owned(id),
            name: Cow::Owned(name),
            version: Cow::Owned(version),
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

// === §F2a additions: reason enums + payload structs ======================

/// Why a project-trust event fires. Mirrors pi-mono `TrustReason`. §F2a
/// defines the variant + reason enum so handlers can subscribe; the host
/// emits it in §F2b/§F5 (trust prompt is §F5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustReason {
    FirstLoad,
    Trusted,
    Untrusted,
}

/// Payload for [`ExtensionEvent::Input`] — user input that may be intercepted
/// or transformed. `text` is the actionable field: a handler returning
/// [`HandlerOutcome::Transform`] with a modified `InputEvent` replaces the
/// input the host injects into the conversation (§F2b wires the seam).
#[derive(Debug, Clone)]
pub struct InputEvent {
    pub text: String,
}

/// Payload for [`ExtensionEvent::BeforeAgentStart`]. Both fields are
/// actionable: a handler returning [`HandlerOutcome::Transform`] may set
/// `system_prompt` (replace the system prompt) and/or `inject_message`
/// (prepend a user message before the agent starts). `None` ⇒ no change.
#[derive(Debug, Clone)]
pub struct AgentStartEvent {
    pub system_prompt: Option<String>,
    pub inject_message: Option<String>,
}

/// Payload for [`ExtensionEvent::BeforeProviderHeaders`]. Emit-only.
/// `headers` is a JSON object (name → value) of the extra HTTP headers the
/// provider client rides on every request, with credential-bearing values
/// masked via [`mask_sensitive_header`]. Known limitation: the transport's
/// auth headers (built from the api key inside the client) are not listed —
/// they never leave the client, and would render fully masked anyway.
#[derive(Debug, Clone)]
pub struct BeforeProviderHeadersEvent {
    pub headers: Value,
}

/// Whether a header NAME marks its value as credential-bearing (masked in
/// the [`ExtensionEvent::BeforeProviderHeaders`] payload). Substring match
/// on the lowercased name — the same deny-list shape config's
/// `is_sensitive_config_key` applies to config keys, here for HTTP header
/// names.
#[must_use]
pub fn is_sensitive_header_name(name: &str) -> bool {
    const NEEDLES: [&str; 8] = [
        "authorization",
        "api-key",
        "apikey",
        "token",
        "secret",
        "cookie",
        "session",
        "password",
    ];
    let lower = name.to_ascii_lowercase();
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

/// One header rendered for the [`ExtensionEvent::BeforeProviderHeaders`]
/// payload: the value masked (config's `redact_secret` shape) when the
/// name is credential-bearing, verbatim otherwise.
#[must_use]
pub fn mask_sensitive_header(name: &str, value: &str) -> String {
    if is_sensitive_header_name(name) {
        codesmith_config::redact_secret(value)
    } else {
        value.to_string()
    }
}

/// Payload for [`ExtensionEvent::BeforeProviderRequest`]. `messages` is the
/// actionable field (the provider request body); a handler may transform it.
#[derive(Debug, Clone)]
pub struct BeforeProviderRequestEvent {
    pub messages: Value,
}

/// Payload for [`ExtensionEvent::AfterProviderResponse`]. Emit-only.
#[derive(Debug, Clone)]
pub struct AfterProviderResponseEvent {
    pub response: Value,
}

/// What failed on a provider request, as the host classified it.
/// `category` is the engine's error taxonomy rendered snake_case
/// (`rate_limit`, `network`, `state`, …); `message` is the raw error text.
#[derive(Debug, Clone)]
pub struct ProviderFailure {
    pub category: String,
    pub message: String,
}

/// The retry plan for [`ExtensionEvent::ProviderRequestError`] — the
/// event's actionable field.
#[derive(Debug, Clone)]
pub enum RetryPlan {
    /// Retry after `delay_ms` milliseconds (0 = immediately). A `Transform`
    /// may set this to force a retry even when the host's default budget is
    /// exhausted — the engine bounds such overrides so a handler that
    /// always rewrites to `Retry` cannot spin the round forever.
    Retry { delay_ms: u64 },
    /// Stop retrying and surface the failure.
    GiveUp,
}

/// Payload for [`ExtensionEvent::ProviderRequestError`] — the DSH
/// request-error waterfall, asked inside the engine's transparent-retry
/// guard before each retry decision. `plan` arrives pre-filled with the
/// host's default policy: `Continue` keeps it, a `Transform` rewrites it
/// (`Retry` rewrites the backoff or forces past the default budget;
/// `GiveUp` stops), and `Block` is equivalent to `GiveUp`.
#[derive(Debug, Clone)]
pub struct ProviderRequestErrorEvent {
    pub failure: ProviderFailure,
    /// 1-based number of the attempt that just failed (retries included).
    pub attempt: u32,
    pub plan: RetryPlan,
}

/// Payload for [`ExtensionEvent::TurnStopping`] — the DSH turn-stopping
/// serial steer, asked when the turn is about to end on `NoToolCalls`
/// (after the steer flush and sub-agent drain resume branches, before the
/// terminal path). `reason` is why the turn would end (`NoToolCalls` at
/// the only emit site).
///
/// Naming inversion at this seam: the pending host operation is
/// *stopping the turn*, so a handler's `Continue` abstains (the turn
/// stops — the default) and [`HandlerOutcome::Cancel { reason }`]
/// cancels the stop — a steer. The first non-abstaining handler decides
/// (Serial short-circuit), and its `reason` becomes the steer note: the
/// host appends it verbatim as a `user`-role message (model-visible ⇒
/// session-recorded; append-only, the prefix cache is untouched) and
/// resumes the turn on a fresh step. Steers are capped per turn by the
/// engine's loop guard — a capped steer is dropped (logged) and the turn
/// ends on the default path. Known limitation: only the `NoToolCalls`
/// termination asks — `MaxSteps` / `Interrupted` / `Error` terminal
/// paths do not consult extensions.
#[derive(Debug, Clone)]
pub struct TurnStoppingEvent {
    pub reason: TurnEndReason,
}

/// Payload for [`ExtensionEvent::AssistantStream`] (streaming
/// observation, route B): one incremental assistant **text** chunk as the
/// wire stream yields it. Emit-only; handlers must be cheap. The host
/// dispatches each delta's emit off-thread under a wall-clock budget: a
/// handler that exceeds the budget is detached — it runs to completion
/// with its outcome discarded and is never cancelled mid-flight — and
/// later deltas' handlers may then overlap it, so handlers must also
/// tolerate concurrent invocation.
#[derive(Debug, Clone)]
pub struct AssistantStreamEvent {
    pub text: String,
}

/// Payload for [`ExtensionEvent::ToolExecutionUpdate`]. Emit-only progress.
#[derive(Debug, Clone)]
pub struct ToolExecutionUpdateEvent {
    pub id: String,
    pub name: String,
    pub message: String,
}

// --- workflow lifecycle (§W2: observe-only Emit family) ====================

/// Payload for [`ExtensionEvent::WorkflowLog`] — one `log(...)` helper
/// call inside a `workflow` run (the same bounded entry the run's own log
/// buffer retains). Emit-only.
#[derive(Debug, Clone)]
pub struct WorkflowLogEvent {
    pub message: String,
}

/// Payload for [`ExtensionEvent::WorkflowAgentStart`] — one workflow child
/// spawn beginning. `objective` is the child's objective text. Emit-only.
#[derive(Debug, Clone)]
pub struct WorkflowAgentStartEvent {
    pub objective: String,
}

/// Payload for [`ExtensionEvent::WorkflowAgentEnd`] — one workflow child
/// finished. `status` uses the sub-agent run path's own vocabulary
/// (`"completed" | "failed" | "interrupted" | "cancelled"`); a failed
/// child is data the script sees, and this event says only that it ended.
/// A child dropped by the run-level wall-clock bound emits
/// `WorkflowAgentStart` with no matching End — the run-level
/// `WorkflowEnd` (timeout) is the terminal signal there.
/// Emit-only.
#[derive(Debug, Clone)]
pub struct WorkflowAgentEndEvent {
    pub status: String,
    pub steps: u32,
}

/// Payload for [`ExtensionEvent::WorkflowEnd`] — a workflow run's terminal
/// outcome. Deliberately carries no return value: it can be huge and the
/// transcript's tool result is already the record. `error` is the error
/// *kind* (`"timeout" | "script_error" | "kernel_spawn"`), not the full
/// text — the tool error carries the text. Emit-only.
#[derive(Debug, Clone)]
pub struct WorkflowEndEvent {
    /// `"completed" | "failed"`.
    pub status: String,
    /// Error classification on failure; `None` on success.
    pub error: Option<String>,
    /// Children the run actually spawned.
    pub agent_calls: u32,
}

/// Lifecycle events. §F1 minimal set (spec §10.1) + §F2a full set (spec
/// §10.2 + §4): 31 variants total (23 §F2a + `AssistantStream` +
/// `ToolsChange`, minus the removed `ResourcesDiscover`, plus the
/// DSH-alignment `ProviderRequestError` + `TurnStopping`, plus the
/// 5-variant workflow lifecycle family — all `Emit`), `#[non_exhaustive]`
/// so future
/// slices can add variants without breaking downstream match arms. Handler dispatch is
/// open (any `Handler` may subscribe to any variant via `on` /
/// `on_variant`). Variant-specific outcome semantics are part of the
/// contract and live with the kinds:
/// [`ExtensionEventKind::dispatch_mode`](ExtensionEventKind::dispatch_mode)
/// (discipline 3 — the `SessionBefore*` seams are `Serial` (cancel
/// veto) and so is `TurnStopping` (serial steer: `Cancel` continues the
/// turn); `ToolCall` is a denying `Waterfall` — input rewrite or
/// monotonic deny; `Input` / `BeforeAgentStart` / `BeforeProviderRequest`
/// / `ToolResult` are non-denying `Waterfall`s; the rest `Emit`).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ExtensionEvent {
    // --- §F1 minimal set (unchanged) ---
    SessionStart {
        reason: SessionReason,
    },
    TurnStart {
        turn_id: String,
    },
    ToolCall(ToolCallEvent),
    ToolResult(ToolResultEvent),
    TurnEnd {
        turn_id: String,
        reason: TurnEndReason,
    },
    SessionShutdown,
    // --- §F2a additions (spec §10.2 + §4) ---
    ProjectTrust {
        reason: TrustReason,
    },
    Input(InputEvent),
    BeforeAgentStart(AgentStartEvent),
    AgentStart,
    BeforeProviderHeaders(BeforeProviderHeadersEvent),
    BeforeProviderRequest(BeforeProviderRequestEvent),
    AfterProviderResponse(AfterProviderResponseEvent),
    /// The DSH request-error waterfall: asked inside the engine's
    /// transparent-retry guard before each retry decision (a denying
    /// `Waterfall` — see [`ProviderRequestErrorEvent`]).
    ProviderRequestError(ProviderRequestErrorEvent),
    /// The DSH turn-stopping serial steer: asked when the turn is about
    /// to end on `NoToolCalls`; `Cancel { reason }` steers the turn
    /// onward (see [`TurnStoppingEvent`]).
    TurnStopping(TurnStoppingEvent),
    ToolExecutionStart,
    AssistantStream(AssistantStreamEvent),
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    ToolExecutionEnd,
    AgentEnd,
    AgentSettled,
    SessionBeforeSwitch,
    SessionBeforeFork,
    SessionBeforeCompact,
    SessionCompact,
    // --- capability composition (tools-change) ---
    /// The model-visible tool catalog changed between main turns. Emitted
    /// at the dispatcher-build chokepoint after every selection source has
    /// been applied; `added`/`removed` are tool names (sorted, possibly
    /// empty on one side). Emit-only.
    ToolsChange {
        added: Vec<String>,
        removed: Vec<String>,
    },
    // --- workflow lifecycle (observe-only, Emit) ---
    /// A `workflow` run began (before the one-shot kernel spawns; a
    /// budget-rejected request emits nothing — the run never began).
    /// Emit-only — outcomes are advisory at this seam.
    WorkflowStart,
    /// One `log(...)` helper call inside a workflow run. Emit-only.
    WorkflowLog(WorkflowLogEvent),
    /// One workflow child spawn began (after the run's agent budget
    /// admitted it). Emit-only.
    WorkflowAgentStart(WorkflowAgentStartEvent),
    /// One workflow child finished. Emit-only.
    WorkflowAgentEnd(WorkflowAgentEndEvent),
    /// A workflow run reached its terminal outcome, success or failure
    /// (see [`WorkflowEndEvent`] for what it deliberately does not carry).
    /// Emit-only.
    WorkflowEnd(WorkflowEndEvent),
}

/// Discriminant of an [`ExtensionEvent`], for per-variant handler subscription
/// via [`ExtensionApi::on_variant`] (§F2a). One variant per `ExtensionEvent`
/// variant; `#[non_exhaustive]` to grow with the event enum.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionEventKind {
    ProjectTrust,
    SessionStart,
    Input,
    BeforeAgentStart,
    AgentStart,
    TurnStart,
    BeforeProviderHeaders,
    BeforeProviderRequest,
    AfterProviderResponse,
    ProviderRequestError,
    TurnStopping,
    ToolExecutionStart,
    AssistantStream,
    ToolCall,
    ToolExecutionUpdate,
    ToolResult,
    ToolExecutionEnd,
    TurnEnd,
    AgentEnd,
    AgentSettled,
    SessionBeforeSwitch,
    SessionBeforeFork,
    SessionShutdown,
    SessionBeforeCompact,
    SessionCompact,
    ToolsChange,
    WorkflowStart,
    WorkflowLog,
    WorkflowAgentStart,
    WorkflowAgentEnd,
    WorkflowEnd,
}

impl ExtensionEvent {
    /// The discriminant of this event, for per-variant dispatch. Exhaustive
    /// within this crate: adding an `ExtensionEvent` variant without a
    /// `kind()` arm is a compile error (the future-variant guard).
    #[must_use]
    pub fn kind(&self) -> ExtensionEventKind {
        match self {
            ExtensionEvent::ProjectTrust { .. } => ExtensionEventKind::ProjectTrust,
            ExtensionEvent::SessionStart { .. } => ExtensionEventKind::SessionStart,
            ExtensionEvent::Input(_) => ExtensionEventKind::Input,
            ExtensionEvent::BeforeAgentStart(_) => ExtensionEventKind::BeforeAgentStart,
            ExtensionEvent::AgentStart => ExtensionEventKind::AgentStart,
            ExtensionEvent::TurnStart { .. } => ExtensionEventKind::TurnStart,
            ExtensionEvent::BeforeProviderHeaders(_) => ExtensionEventKind::BeforeProviderHeaders,
            ExtensionEvent::BeforeProviderRequest(_) => ExtensionEventKind::BeforeProviderRequest,
            ExtensionEvent::AfterProviderResponse(_) => ExtensionEventKind::AfterProviderResponse,
            ExtensionEvent::ProviderRequestError(_) => ExtensionEventKind::ProviderRequestError,
            ExtensionEvent::TurnStopping(_) => ExtensionEventKind::TurnStopping,
            ExtensionEvent::ToolExecutionStart => ExtensionEventKind::ToolExecutionStart,
            ExtensionEvent::AssistantStream(_) => ExtensionEventKind::AssistantStream,
            ExtensionEvent::ToolCall(_) => ExtensionEventKind::ToolCall,
            ExtensionEvent::ToolExecutionUpdate(_) => ExtensionEventKind::ToolExecutionUpdate,
            ExtensionEvent::ToolResult(_) => ExtensionEventKind::ToolResult,
            ExtensionEvent::ToolExecutionEnd => ExtensionEventKind::ToolExecutionEnd,
            ExtensionEvent::TurnEnd { .. } => ExtensionEventKind::TurnEnd,
            ExtensionEvent::AgentEnd => ExtensionEventKind::AgentEnd,
            ExtensionEvent::AgentSettled => ExtensionEventKind::AgentSettled,
            ExtensionEvent::SessionBeforeSwitch => ExtensionEventKind::SessionBeforeSwitch,
            ExtensionEvent::SessionBeforeFork => ExtensionEventKind::SessionBeforeFork,
            ExtensionEvent::SessionShutdown => ExtensionEventKind::SessionShutdown,
            ExtensionEvent::SessionBeforeCompact => ExtensionEventKind::SessionBeforeCompact,
            ExtensionEvent::SessionCompact => ExtensionEventKind::SessionCompact,
            ExtensionEvent::ToolsChange { .. } => ExtensionEventKind::ToolsChange,
            ExtensionEvent::WorkflowStart => ExtensionEventKind::WorkflowStart,
            ExtensionEvent::WorkflowLog(_) => ExtensionEventKind::WorkflowLog,
            ExtensionEvent::WorkflowAgentStart(_) => ExtensionEventKind::WorkflowAgentStart,
            ExtensionEvent::WorkflowAgentEnd(_) => ExtensionEventKind::WorkflowAgentEnd,
            ExtensionEvent::WorkflowEnd(_) => ExtensionEventKind::WorkflowEnd,
        }
    }
}

// === Dispatch contract (discipline 3) ======================================

/// How handlers for an event are dispatched and which
/// [`HandlerOutcome`]s the host honors — part of the event's contract, not
/// an implementation detail of a seam (discipline 3: the dispatch mode is
/// declared with the event). See [`ExtensionEventKind::dispatch_mode`].
///
/// Three states named after the DSH (deepseek-harness) dispatch model,
/// which this contract aligns with: `Emit` = DSH emit, `Serial` = DSH
/// serial, `Waterfall` = DSH waterfall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DispatchMode {
    /// DSH **emit** — broadcast: handlers run in subscription order and
    /// outcomes are advisory — `Transform`/`Cancel`/`Block` are ignored
    /// at these seams.
    Emit,
    /// DSH **serial** — ordered listeners with veto: handlers run in
    /// subscription order; a handler's `Cancel` aborts the pending host
    /// operation and short-circuits the chain (the `SessionBefore*`
    /// seams). [`ExtensionEventKind::TurnStopping`] is serial with an
    /// inverted reading: the pending operation there IS the stop, so
    /// `Cancel { reason }` continues the turn — the steer.
    Serial,
    /// DSH **waterfall** — transform fold: each handler's `Transform`
    /// folds into the running event (immediately visible to the next
    /// handler); the terminal actionable field is applied to the host
    /// operation. `can_deny: true` additionally honors `Block` — a
    /// handler may deny the operation outright; the deny is monotonic, no
    /// later handler can flip it (the chain has already stopped). The
    /// fold stays a fold — DSH's `next()`-wrapping around-dispatch exists
    /// separately for tool execution ([`AroundToolHandler`]); no event
    /// kind dispatches around-style.
    Waterfall {
        /// Whether `Block` is honored at this seam: deny the operation
        /// outright, short-circuiting the fold (the `ToolCall` seam).
        can_deny: bool,
    },
}

impl ExtensionEventKind {
    /// The dispatch mode this event's contract declares (discipline 3).
    /// Exhaustive like [`ExtensionEvent::kind`]: adding an event variant
    /// without an arm here is a compile error, and the
    /// `event_dispatch_contract_table` test pins the expected mode per kind
    /// so the declared contract cannot drift silently.
    #[must_use]
    pub fn dispatch_mode(self) -> DispatchMode {
        match self {
            ExtensionEventKind::Input
            | ExtensionEventKind::BeforeAgentStart
            | ExtensionEventKind::BeforeProviderRequest
            | ExtensionEventKind::ToolResult => DispatchMode::Waterfall { can_deny: false },
            ExtensionEventKind::ToolCall | ExtensionEventKind::ProviderRequestError => {
                DispatchMode::Waterfall { can_deny: true }
            }
            ExtensionEventKind::SessionBeforeSwitch
            | ExtensionEventKind::SessionBeforeFork
            | ExtensionEventKind::SessionBeforeCompact
            | ExtensionEventKind::TurnStopping => DispatchMode::Serial,
            ExtensionEventKind::ProjectTrust
            | ExtensionEventKind::SessionStart
            | ExtensionEventKind::AgentStart
            | ExtensionEventKind::TurnStart
            | ExtensionEventKind::BeforeProviderHeaders
            | ExtensionEventKind::AfterProviderResponse
            | ExtensionEventKind::AssistantStream
            | ExtensionEventKind::ToolExecutionStart
            | ExtensionEventKind::ToolExecutionUpdate
            | ExtensionEventKind::ToolExecutionEnd
            | ExtensionEventKind::TurnEnd
            | ExtensionEventKind::AgentEnd
            | ExtensionEventKind::AgentSettled
            | ExtensionEventKind::SessionShutdown
            | ExtensionEventKind::SessionCompact
            | ExtensionEventKind::ToolsChange
            | ExtensionEventKind::WorkflowStart
            | ExtensionEventKind::WorkflowLog
            | ExtensionEventKind::WorkflowAgentStart
            | ExtensionEventKind::WorkflowAgentEnd
            | ExtensionEventKind::WorkflowEnd => DispatchMode::Emit,
        }
    }
}

// === ExtensionContext =====================================================

/// The execution mode of the host. Mirrors pi-mono's `ExtensionContext.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionMode {
    /// The interactive terminal REPL. Its mod-visible mode string stays
    /// `"tui"` — the retired frontend's name, kept so existing mods'
    /// `ctx.mode` checks keep working.
    Interactive,
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
/// `codesmith-extensions` stays decoupled from `codesmith-cli`; the CLI
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

// === HandlerOutcome (§F2a) ================================================

/// What a [`Handler`] returns. Drives the cross-handler chain in
/// [`ExtensionRunner::emit`](codesmith_extensions::ExtensionRunner::emit)
/// (spec §4: "一个 handler 的修改对下一个可见"). Which outcomes the host
/// honors is the event's
/// [`dispatch mode`](ExtensionEventKind::dispatch_mode) — the three DSH
/// states:
///
/// - `Continue` — no change; proceed to the next handler.
/// - `Cancel { reason }` — abort the surrounding operation. Honored only
///   at `Serial` seams (the `SessionBefore*` variants, spec §4); ignored
///   (treated as `Continue`) elsewhere so a stray `Cancel` cannot break
///   unrelated flows. [`ExtensionEvent::TurnStopping`] is also `Serial`
///   with an inverted reading — the "surrounding operation" is stopping
///   the turn, so `Cancel { reason }` continues the turn (a steer) and
///   `Continue` abstains (the turn stops); the `reason` doubles as the
///   steer note the host appends as a `user`-role message (see
///   [`TurnStoppingEvent`]).
/// - `Block { reason }` — prevent the operation. Honored only at a
///   denying `Waterfall` seam (`ToolCall`); ignored elsewhere.
/// - `Ask { reason }` — route the operation through the host's approval
///   decision channel before it proceeds. Honored only at the denying
///   `ToolCall` waterfall: the call is user-gated regardless of its
///   static approval classification (an asked parallel-batch call is
///   gated inline before dispatch), and **fail-closed** — with no
///   approval channel attached the call is DENIED and the reason fed
///   back as a permission error, never silently allowed. Like `Block`,
///   it short-circuits the handler chain (the first `Ask` decides) and
///   is ignored (treated as `Continue`) at every other seam.
/// - `Transform(event)` — replace the running event with `event` for
///   subsequent handlers AND (at `Waterfall` seams) apply `event`'s
///   actionable field to the live operation. Honored at the `Waterfall`
///   seams — `Input` / `BeforeAgentStart` / `BeforeProviderRequest` /
///   `ToolCall` / `ToolResult`; ignored at `Emit`/`Serial` seams.
///
/// The terminal [`codesmith_extensions::EmitOutcome::outcome`] is never
/// `Transform` — a transform is folded into the running event and the chain
/// continues; the terminal is `Continue` unless a handler short-circuits
/// with `Cancel`/`Block`/`Ask`.
#[derive(Debug, Clone)]
pub enum HandlerOutcome {
    Continue,
    Cancel {
        reason: String,
    },
    Block {
        reason: String,
    },
    /// User-decision request — honored only at the denying `ToolCall`
    /// waterfall (approval-channel routing, fail-closed); see the variant
    /// list above.
    Ask {
        reason: String,
    },
    /// Invariant: the replacement event MUST carry the same
    /// [`ExtensionEvent::kind`] as the running event — a cross-variant
    /// transform violates the event's
    /// [`dispatch_mode`](ExtensionEventKind::dispatch_mode) contract and the
    /// downstream actionable-field extraction at the seam.
    Transform(ExtensionEvent),
}

// === Handler (observer, slice 1) ==========================================

/// Lifecycle event handler. §F2a: returns [`HandlerOutcome`]
/// (cancel/transform/block), chained across handlers in
/// [`ExtensionRunner::emit`](codesmith_extensions::ExtensionRunner::emit).
/// §F1 was observer-only (`Result<(), _>`); §F2a upgrades to the outcome
/// chain. `catch_unwind` isolation is in `emit` (§F2a T8), so a panicking
/// handler cannot tear down the agent loop.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError>;
}

// === Tool guards (deny-only, route B alignment) ===========================

/// A deny-only tool-call guard (the `ctx.tools.guard()` alignment surface).
/// `Some(reason)` denies the call, `None` abstains. A guard has **no allow
/// result**: the [`HandlerOutcome::Block`] it maps to short-circuits the
/// handler chain, so neither registration order nor other `ToolCall`
/// handlers can turn a denial back into permission — the monotonic ratchet
/// is structural, not conventional.
pub type ToolGuardFn = Arc<dyn Fn(&ToolCallEvent) -> Option<String> + Send + Sync>;

/// A [`ToolGuardFn`] wrapped as a [`Handler`] for the [`ExtensionEventKind::ToolCall`]
/// seam. Register via [`ExtensionApi::on_variant`] with
/// [`ExtensionEventKind::ToolCall`] (Rhai shape: the `register_guard(callback)`
/// native); reload clears it with the generation's handlers. A denial is
/// attributed: the block reason reads `"<label>: <reason>"`, so the blocked
/// result names the guard that denied the call.
///
/// Known limitations (deliberate, dsh `tools.guard()` placement differs):
/// - Evaluated at the `ToolCall` seam, i.e. before approval. A denied call
///   is never dispatched, so no approval path observes or flips it; there
///   is no post-approval evaluation point (CodeSmith has no execution path
///   that bypasses the seam, so the deny-only floor is equivalent).
/// - Main turns and sub-agent loops both evaluate guards: a sub-agent's
///   loop dispatches `ToolCall` at its agent-type scope, so a `Global`
///   guard gates child calls too (5b). Origin caveat: a guard registered
///   by a **dylib** extension stays main-turn-only (§F5d unload safety —
///   sub-agent-scoped dispatch skips dylib-origin handlers); script and
///   compiled-in guards gate every scope.
/// - A guard closure that panics abstains (fail-open); the panic is caught
///   and logged here, so containment does not depend on the emit seam
///   (same isolation policy as the message-projection fold path).
pub struct GuardHandler {
    label: String,
    guard: ToolGuardFn,
}

impl GuardHandler {
    /// Wrap `guard` with the attribution `label` (e.g. `guard (mod: safety)`).
    pub fn new(label: impl Into<String>, guard: ToolGuardFn) -> Self {
        Self {
            label: label.into(),
            guard,
        }
    }
}

#[async_trait]
impl Handler for GuardHandler {
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _ctx: &dyn ExtensionContext,
    ) -> Result<HandlerOutcome, ExtensionError> {
        match event {
            ExtensionEvent::ToolCall(e) => {
                let denied =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.guard)(e)));
                Ok(match denied {
                    Ok(Some(reason)) => HandlerOutcome::Block {
                        reason: format!("{}: {}", self.label, reason),
                    },
                    Ok(None) => HandlerOutcome::Continue,
                    Err(_) => {
                        tracing::error!(
                            target: "codesmith_extensions",
                            "tool guard {} panicked; abstaining (fail-open)",
                            self.label
                        );
                        HandlerOutcome::Continue
                    }
                })
            }
            // Unreachable behind a `ToolCall` kind filter; a misrouted
            // guard abstains rather than denying an event it never saw.
            _ => Ok(HandlerOutcome::Continue),
        }
    }
}

// === Around-dispatch (DSH tools/execute) ==================================

/// The boxed continuation an [`AroundToolHandler`] receives: awaiting it
/// runs the next around handler, or — after the last one — the host's
/// dispatch (approval gate included on the serial path). Never awaiting
/// it is the veto.
pub type ToolCallNext<'a> =
    Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>>;

/// DSH-style around-dispatch for tool execution ("waterfall with next"):
/// `call` receives the tool call and `next`, and may inspect, wrap, defer,
/// or veto the execution by never awaiting `next`. Handlers compose in
/// registration order — the first registered is outermost; the last one's
/// `next` is the host's dispatch. This is deliberately NOT a
/// [`Handler`]-outcome form: the `next()` wrap cannot be expressed as a
/// fold, so it is a separate registration
/// ([`ExtensionApi::register_around_tool`]) with its own dispatch shape.
///
/// Veto vocabulary: return `Err(ToolError::permission_denied(reason))` —
/// the same feedback shape an approval denial produces, so the model sees
/// one denial vocabulary (the result feeds back through the existing
/// tool-result path).
///
/// Known limitations: results from the around chain are execution results
/// — they count toward the loop-guard failure escalation, unlike a
/// `Block` intervention at the `ToolCall` seam (a veto is the handler's
/// own decision; a veto-happy handler tripping the guard is the guard
/// working); the chain wraps the host dispatch INCLUDING the approval
/// gate on the serial path (a veto skips the question); and the seam is
/// not offered to script mods (async boundary — see `docs/MODS.md`).
#[async_trait]
pub trait AroundToolHandler: Send + Sync {
    async fn call(
        &self,
        call: ToolCallEvent,
        next: ToolCallNext<'_>,
    ) -> Result<ToolResult, ToolError>;
}

/// Compose `handlers` (registration order, outermost first) around
/// `terminal` (the host's dispatch): each layer's `next` is the boxed
/// future of the layers below. The chain runs outermost-first; a handler
/// that never awaits its `next` vetoes — the layers below never run.
#[must_use]
pub fn wrap_tool_dispatch<'a>(
    handlers: &[Arc<dyn AroundToolHandler>],
    call: ToolCallEvent,
    terminal: ToolCallNext<'a>,
) -> ToolCallNext<'a> {
    let mut next = terminal;
    for handler in handlers.iter().rev() {
        let handler = Arc::clone(handler);
        let call = call.clone();
        let inner = next;
        next = Box::pin(async move { handler.call(call, inner).await });
    }
    next
}

// === Around-turn (DSH waterfall next() over the whole main turn) ==========

/// The boxed continuation an [`AroundTurnHandler`] receives: awaiting it
/// runs the next around-turn handler, or — after the last one — the
/// composed executor's `AgentExecutor::run`. Never awaiting it is the veto.
pub type TurnNext<'a> = Pin<Box<dyn Future<Output = anyhow::Result<StopReason>> + Send + 'a>>;

/// The payload an [`AroundTurnHandler`] receives — enough to correlate with
/// the turn lifecycle events (`TurnStarted` / `TurnComplete` carry the same
/// id). Deliberately lean: the transcript is not exposed here (a handler
/// that needs to shape the turn's input or stop decision has the
/// `Input` / `TurnStopping` seams); this seam composes *whole turns*.
#[derive(Debug, Clone)]
pub struct TurnAroundEvent {
    pub turn_id: String,
}

/// DSH-style around-dispatch for the whole main turn ("waterfall with
/// next" over `AgentExecutor::run`): `call` receives the turn payload and
/// `next`, and may run pre/post phases around the loop or veto by
/// returning its own `StopReason` without awaiting `next`. Handlers
/// compose in registration order — the first registered is outermost; the
/// last one's `next` is the composed executor's run. Together with the
/// turn-executor registry (selection) this is the kernel-composition
/// surface: selection says which loop runs, around-turn composes
/// behaviour around it at runtime.
///
/// Known limitations: `next` is a single-shot future — awaiting it runs
/// the inner turn once, so "re-run the loop" (DSH's callable `next()`)
/// is not expressible here (a handler wanting retries needs a factory
/// form; add it with a real consumer); main-turn only (sub-agent loops
/// never wrap); the chain wraps the executor run INSIDE the engine's turn
/// bookkeeping (TurnStarted / TurnComplete, harvest, snapshots), so a
/// vetoed turn still completes its bookkeeping with the handler's returned
/// `StopReason`; and the seam is not offered to script mods (async
/// boundary — see `docs/MODS.md`).
#[async_trait]
pub trait AroundTurnHandler: Send + Sync {
    async fn call(&self, turn: TurnAroundEvent, next: TurnNext<'_>) -> anyhow::Result<StopReason>;
}

/// Compose `handlers` (registration order, outermost first) around
/// `terminal` (the composed executor's run). Same shape as
/// [`wrap_tool_dispatch`]: each layer's `next` is the boxed future of the
/// layers below; a handler that never awaits its `next` vetoes.
#[must_use]
pub fn wrap_turn_dispatch<'a>(
    handlers: &[Arc<dyn AroundTurnHandler>],
    turn: TurnAroundEvent,
    terminal: TurnNext<'a>,
) -> TurnNext<'a> {
    let mut next = terminal;
    for handler in handlers.iter().rev() {
        let handler = Arc::clone(handler);
        let turn = turn.clone();
        let inner = next;
        next = Box::pin(async move { handler.call(turn, inner).await });
    }
    next
}

// === Registration scope (DSH alignment) ===================================

/// Where a registration applies — the DSH per-agent scope key, reduced to
/// CodeSmith's two states. Every registration method has a `Global`
/// default (the plain `register_tool` / `on` / `on_variant` forms); the
/// `*_scoped` variants name an explicit scope.
///
/// Scope resolution rules (DSH `ToolLayer` alignment):
/// - A **tool** registered for `AgentType(id)` shadows a same-name global
///   tool in that agent type's catalog view; the global view (the main
///   turn) never sees it.
/// - A **handler** registered for `AgentType(id)` receives only events
///   dispatched at that scope ([`ExtensionRunner::emit_scoped`]); the
///   main turn's `emit` (Global) never reaches it.
/// - A **restrict entry** (`restrict_tools`) prunes the *global* tool
///   layer for the views its scope covers — deny-only, and it never prunes
///   scoped registrations (what an agent type declares for itself cannot
///   be voted away by a deployment restriction; DSH: "restrictions do not
///   affect scoped registrations").
///
/// `AgentType` ids are sub-agent type strings (`SubAgentType::as_str()`:
/// `general`, `explore`, `implementer`, …) — opaque at this layer; the
/// host resolves them against its sub-agent type table. Known
/// limitations: no scoped variants of `on` (subscribe-all — register
/// per-variant instead) or `register_around_tool` (the around chain is
/// main-turn-only today); script mods do not see scopes.
///
/// [`ExtensionRunner::emit_scoped`]: codesmith_extensions::ExtensionRunner::emit_scoped
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Applies everywhere: global views and every agent-type view.
    Global,
    /// Applies to one sub-agent type's registry/catalog/events only.
    AgentType(String),
}

impl Scope {
    /// Whether this is the [`Scope::Global`] default.
    #[must_use]
    pub fn is_global(&self) -> bool {
        matches!(self, Scope::Global)
    }

    /// Whether a registration made at `self` applies to a view / dispatch
    /// at `scope`: a global registration covers every scope; an agent-type
    /// registration covers only its own. This is the one predicate behind
    /// both catalog-view derivation and handler dispatch filtering.
    #[must_use]
    pub fn covers(&self, scope: &Scope) -> bool {
        self == &Scope::Global || self == scope
    }

    /// Fail-loud validation shared by the scoped registration surfaces: an
    /// `AgentType` id must be non-empty (trimmed). Whitespace-only ids are
    /// how a typo'd format string would otherwise register silently.
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if let Scope::AgentType(id) = self
            && id.trim().is_empty()
        {
            return Err(ExtensionError::Config(
                "AgentType scope id must be a non-empty sub-agent type string".into(),
            ));
        }
        Ok(())
    }
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
    /// Subscribe `handler` to ALL events (backward-compat with §F1).
    fn on(&self, handler: Arc<dyn Handler>) -> Result<(), ExtensionError>;
    /// §F2a — subscribe `handler` to a single event variant only. The runner
    /// dispatches only matching events to this handler (per-variant dispatch,
    /// spec §F2). Equivalent to [`on`](Self::on) with a `None` filter.
    fn on_variant(
        &self,
        kind: ExtensionEventKind,
        handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError>;

    /// DSH registration scope — register a tool for one scope. A
    /// `Scope::AgentType(id)` tool shadows a same-name global tool in that
    /// agent type's catalog view and never enters the global (main-turn)
    /// view; `Scope::Global` is equivalent to
    /// [`register_tool`](Self::register_tool).
    ///
    /// Default: hosts that do not support scoped registration get a loud
    /// error, keeping the trait implementable (source- and
    /// binary-compatible) for api surfaces that predate this method.
    fn register_tool_scoped(
        &self,
        _scope: Scope,
        _tool: Box<dyn ToolDefinition>,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "scoped tool registration not supported by this api".into(),
        ))
    }

    /// DSH registration scope — subscribe `handler` to a single event
    /// variant **at a scope**. A `Scope::AgentType(id)` handler receives
    /// only events dispatched at that scope (`emit_scoped`); the main
    /// turn's `emit` (Global) never reaches it. `Scope::Global` is
    /// equivalent to [`on_variant`](Self::on_variant). No scoped
    /// subscribe-all form exists — register per-variant.
    ///
    /// Default: hosts that do not support scoped registration get a loud
    /// error (same posture as [`register_tool_scoped`](Self::register_tool_scoped)).
    fn on_variant_scoped(
        &self,
        _kind: ExtensionEventKind,
        _scope: Scope,
        _handler: Arc<dyn Handler>,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "scoped handler registration not supported by this api".into(),
        ))
    }

    /// DSH tool restriction — a **deny-only** name list that prunes the
    /// *global* extension-tool layer for the views `scope` covers (a
    /// `Global` entry prunes every view including the main turn; an
    /// `AgentType(id)` entry prunes that type's view only). Monotonic by
    /// construction, the same ratchet [`GuardHandler`] gives execution-time
    /// deny: there is no allow form, nothing can re-admit a denied name,
    /// and entries intersect. Restrictions never prune scoped
    /// registrations (see [`Scope`]). They prune only the extension
    /// layer's own tools — denying a builtin is [`GuardHandler`]
    /// (execution-time deny) or config `disabled_tools` territory, not
    /// this list.
    ///
    /// Default: hosts that do not support tool restrictions get a loud
    /// error (same posture as [`register_tool_scoped`](Self::register_tool_scoped)).
    fn restrict_tools(&self, _scope: Scope, _names: Vec<String>) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "tool restriction not supported by this api".into(),
        ))
    }

    /// DSH around-dispatch — register a handler that wraps every tool
    /// execution (the `next()` chain; see [`AroundToolHandler`]).
    /// Registration order composes outermost-first; reload clears the
    /// generation's registrations with the other bound registrations.
    /// Not offered to script mods (async seam).
    ///
    /// Default: hosts that do not support it get a loud error, keeping
    /// the trait implementable (source- and binary-compatible) for api
    /// surfaces that predate this method.
    fn register_around_tool(
        &self,
        _handler: Arc<dyn AroundToolHandler>,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "around-tool registration not supported by this api".into(),
        ))
    }

    /// Kernel composition, runtime half — wrap the whole main turn
    /// (`AgentExecutor::run`) with a `next()`-style [`AroundTurnHandler`].
    /// Registration order composes outermost-first; the registration takes
    /// effect at the next turn's assembly and reload clears it with the
    /// generation's other bound registrations. Main-turn only; not offered
    /// to script mods (async seam).
    ///
    /// Default: hosts that do not support it get a loud error, keeping
    /// the trait implementable (source- and binary-compatible) for api
    /// surfaces that predate this method.
    fn register_around_turn(
        &self,
        _handler: Arc<dyn AroundTurnHandler>,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "around-turn registration not supported by this api".into(),
        ))
    }

    /// Route A — register (or replace) a provider factory, keyed by its
    /// [`ProviderFactory::id`]. The runner owns the resulting registration
    /// guard for the extension's generation: reload / `clear_providers`
    /// drops it, which removes the factory (symmetric unload via
    /// [`crate::provider::ProviderRegistration`]). A registration takes
    /// effect at the next client resolution — an already-built client is
    /// not hot-swapped.
    ///
    /// Default: hosts that do not support provider registration get a loud
    /// error, keeping the trait implementable (source- and binary-compatible)
    /// for api surfaces that predate the route-A methods.
    fn register_provider(&self, _factory: Arc<dyn ProviderFactory>) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "provider registration not supported by this api".into(),
        ))
    }

    /// Route A, script-mod shape — register a declarative alias onto a
    /// builtin provider (new custom id + optional `base_url` /
    /// `default_model` / `http_headers` overrides). Rhai mods cannot
    /// implement `LlmClient` (no async/net by design), so their provider
    /// contribution is this alias form; see [`ProviderAlias`].
    fn register_provider_alias(&self, _alias: ProviderAlias) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "provider alias registration not supported by this api".into(),
        ))
    }

    /// Route B (systemPrompt sections) — contribute a named, append-only
    /// section to the base system prompt. Sections register at mod load and
    /// are stable for the session (prefix-cache discipline); the host
    /// appends them to the base prompt each turn, BEFORE the
    /// `BeforeAgentStart` seam — an explicit whole-prompt replacement by a
    /// handler still wins. `id` is diagnostics/dedup identity, not display.
    fn register_prompt_section(&self, _id: String, _text: String) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "prompt-section registration not supported by this api".into(),
        ))
    }

    /// Route B (skills) — contribute a skill to the session's skill
    /// catalogue. Registered skills are in-memory only (no `SKILL.md` on
    /// disk): they appear in the system-prompt `## Skills` block, in
    /// `/skills`, and the `load_skill` tool resolves them by name.
    /// [`SkillRegistration::owner`] is the contributing mod's id
    /// (attribution + replace identity). Registration happens at mod load
    /// and is session-stable (prefix-cache discipline, same as prompt
    /// sections). A name already registered by a *different* owner is
    /// rejected (fail loud); the filesystem catalogue wins over a
    /// registered name at merge time.
    fn register_skill(&self, _skill: SkillRegistration) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "skill registration not supported by this api".into(),
        ))
    }

    /// Register a **message projection**: a fold the host runs over the
    /// session transcript, maintaining a per-`(owner, key)` state that
    /// survives session reload (rebuilt from the log — the event-sourcing
    /// projection model; no snapshot is persisted). `init` is the fold's
    /// starting value; `fold` folds one transcript message into the state
    /// and returns the new state. Keys are mod-namespaced (`owner`), so
    /// mods cannot collide. A fold error drops the projection (logged,
    /// never kills the turn); re-registering happens at the next mod load.
    fn register_message_projection(
        &self,
        _owner: String,
        _key: String,
        _init: Value,
        _fold: MessageFoldFn,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "message-projection registration not supported by this api".into(),
        ))
    }

    /// Register a **policy** for a host-behavior seam ([`PolicyKind`]) —
    /// the "policy in script, effects in host" surface. A policy is data:
    /// `config` is a JSON map whose keys the owning mechanism validates
    /// (bad values warn + fall back to compiled defaults at read time).
    /// Registration is **last-wins by load order** (builtin mods < global
    /// < project mods), so a user mod overrides a builtin policy by
    /// registering the same kind — deliberately different from
    /// tools/commands, where the incumbent keeps the name. `owner` is the
    /// registering extension's own id — the script seam stamps it (a mod
    /// cannot name another owner); it rides the resolved entry
    /// ([`crate::policy::ResolvedPolicy`]) for the observability view.
    fn register_policy(
        &self,
        _owner: String,
        _kind: crate::policy::PolicyKind,
        _config: Value,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "policy registration not supported by this api".into(),
        ))
    }

    /// Disable a **built-in slash command** by name (the host resolves
    /// aliases). The REPL layers user > mod > builtin; this removes the
    /// builtin row so an extension can suppress a built-in without
    /// shadowing it with a replacement of its own — user commands and
    /// mod-registered commands of the same name still dispatch. The
    /// generation's disables clear on reload (surviving mods re-register).
    ///
    /// Default: hosts that do not support it get a loud error, keeping the
    /// trait implementable (source- and binary-compatible) for api surfaces
    /// that predate this method.
    fn disable_command(&self, _name: String) -> Result<(), ExtensionError> {
        Err(ExtensionError::Load(
            "command disabling not supported by this api".into(),
        ))
    }
}

// === Message projections (session log folds) ===============================

/// The fold function of a mod-registered message projection. Folds one
/// transcript message into the projection state and returns the new state;
/// `Err` poisons the projection (host drops it and logs — a mod bug must
/// not kill the turn). The state currency is [`serde_json::Value`] so the
/// framework crate stays script-runtime-agnostic (the Rhai adapter converts
/// at the boundary).
pub type MessageFoldFn =
    Arc<dyn Fn(Value, &crate::models::Message) -> Result<Value, String> + Send + Sync>;

/// Cap on registered projections across all mods (fail loud beyond, at
/// registration — mirrors `MAX_PROMPT_SECTIONS`).
pub const MAX_MESSAGE_PROJECTIONS: usize = 16;

/// Bound on [`MessageProjectionHub::refold_all`] follow-up passes for
/// projections registered while a refold was running (caps a pathological
/// register-during-refold loop; one pass is the norm).
const REFOLD_MAX_PASSES: usize = 3;

/// Route B (skills) — a mod-contributed skill, registered via
/// [`ExtensionApi::register_skill`]. The framework-level mirror of a
/// `SKILL.md`: only the fields the catalogue lists and `load_skill`
/// returns are carried. No `paths` (workspace path-trigger matching is a
/// filesystem-skill concept), no `allowed_tools`/`model`/`effort` tuning
/// — add them here when a mod needs them.
#[derive(Debug, Clone)]
pub struct SkillRegistration {
    /// Owning mod id, also the replace identity: a mod re-registering its
    /// own name replaces in place, and a name already registered by a
    /// *different* owner is rejected at registration. Caller-supplied at
    /// the trait level (a compiled-in/dylib extension names itself); the
    /// script-mod seam stamps the mod's own id and offers no way to name
    /// another owner.
    pub owner: String,
    /// Skill name — the `load_skill` / `/skill` key. `[a-zA-Z0-9_-]`,
    /// 1-64 chars.
    pub name: String,
    /// One-line catalogue description (must be non-empty).
    pub description: String,
    /// Full skill body shown when the skill is loaded.
    pub body: String,
    /// Optional "use when ..." hint rendered in the catalogue listing.
    pub when_to_use: Option<String>,
}

/// Cap on registered skills across all mods (fail loud beyond, at
/// registration — mirrors `MAX_PROMPT_SECTIONS`; each entry is one line
/// in the system-prompt catalogue).
pub const MAX_REGISTERED_SKILLS: usize = 16;

/// Host-side store of mod-registered message projections. The engine folds
/// every transcript mutation through this hub (append → [`fold_message`],
/// wholesale replacement → [`refold_all`]), so a projection's state is
/// always "the fold of the current transcript" — session restore, compaction,
/// and `/edit` rollback rebuild it for free. `clear` (reload) marks the hub
/// dirty; the engine refolds from the transcript at the next turn start.
///
/// [`fold_message`]: MessageProjectionHub::fold_message
/// [`refold_all`]: MessageProjectionHub::refold_all
///
/// Known limitations: a fold error removes the projection until the next
/// mod load (no retry); projection states are not persisted — reload/restore
/// rebuilds them from the transcript, which is the point, but a projection
/// whose fold depends on messages compaction drops will diverge from the
/// pre-compaction state (same boundary as the fact-ledger snapshot note).
#[derive(Default)]
pub struct MessageProjectionHub {
    inner: Mutex<HubState>,
}

/// Shared-handle alias — the runner owns one, the engine folds through the
/// same `Arc`, mod scripts read via `projection_state(key)`.
pub type MessageProjectionHubArc = Arc<MessageProjectionHub>;

/// Manual `Debug` (the fold closures are not `Debug`); reports the count.
impl std::fmt::Debug for MessageProjectionHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.inner.lock().map_or(0, |state| state.projections.len());
        f.debug_struct("MessageProjectionHub")
            .field("projections", &count)
            .finish()
    }
}

#[derive(Default)]
struct HubState {
    projections: Vec<StoredProjection>,
    /// Set by `clear`; the engine consumes it (`take_dirty`) to refold from
    /// the live transcript after a reload.
    dirty: bool,
    /// Bumped by `clear`; a `refold_all` whose folds straddled a `clear`
    /// leaves the reload signal set instead of clobbering it.
    clear_generation: u64,
    /// Monotonic source of per-projection lineage versions. Every state
    /// commit draws a fresh one, so a fold snapshotted before a `clear` /
    /// re-register / `refold_all` commit can never land on the projection
    /// incarnation that exists afterwards.
    next_version: u64,
}

impl HubState {
    fn draw_version(&mut self) -> u64 {
        let version = self.next_version;
        self.next_version += 1;
        version
    }
}

struct StoredProjection {
    owner: String,
    key: String,
    /// Lineage token: drawn from [`HubState::next_version`] at registration
    /// and re-drawn at every wholesale state commit; a fold commits only
    /// onto the incarnation it snapshotted.
    version: u64,
    init: Value,
    state: Value,
    fold: MessageFoldFn,
}

impl MessageProjectionHub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a projection. Fails loud on a bad key, a duplicate
    /// `(owner, key)` (pick a new key — states are not migratable), or the
    /// global cap.
    pub fn register(
        &self,
        owner: &str,
        key: &str,
        init: Value,
        fold: MessageFoldFn,
    ) -> Result<(), String> {
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(format!(
                "projection key {key:?} must match [a-zA-Z0-9_-] and be 1-64 chars"
            ));
        }
        let mut state = self.inner.lock().expect("projection hub poisoned");
        if state
            .projections
            .iter()
            .any(|p| p.owner == owner && p.key == key)
        {
            return Err(format!(
                "projection key {key:?} already registered by mod {owner:?} — pick a new key"
            ));
        }
        if state.projections.len() >= MAX_MESSAGE_PROJECTIONS {
            return Err(format!(
                "too many message projections (>{MAX_MESSAGE_PROJECTIONS})"
            ));
        }
        let version = state.draw_version();
        state.projections.push(StoredProjection {
            owner: owner.to_string(),
            key: key.to_string(),
            version,
            init: init.clone(),
            state: init,
            fold,
        });
        Ok(())
    }

    /// Fold one newly-appended transcript message into every projection.
    /// No-ops when empty. A fold error (or panic) drops that projection
    /// (logged via `tracing`) — the turn keeps running.
    ///
    /// Folds run OUTSIDE the hub mutex: a script-mod fold re-enters the
    /// same Rhai engine whose `projection_state` native locks this hub, so
    /// folding under the lock deadlocks the turn on the same thread; a
    /// panicking fold is contained by `catch_unwind` instead of poisoning
    /// the mutex; a slow fold (Rhai op budget, `mod_state_set` file I/O)
    /// cannot block other hub operations while it runs. The re-lock after
    /// the folds commits by `(owner, key)` identity plus the projection's
    /// lineage `version`, so a concurrent `register`/`clear`/`refold_all`
    /// interleaving never applies a fold result to a projection it did not
    /// start from.
    pub fn fold_message(&self, message: &crate::models::Message) {
        let snapshot: Vec<(String, String, u64, Value, MessageFoldFn)> = {
            let state = self.inner.lock().expect("projection hub poisoned");
            if state.projections.is_empty() {
                return;
            }
            state
                .projections
                .iter()
                .map(|p| {
                    (
                        p.owner.clone(),
                        p.key.clone(),
                        p.version,
                        p.state.clone(),
                        Arc::clone(&p.fold),
                    )
                })
                .collect()
        };
        let mut folded: Vec<(String, String, u64, Value)> = Vec::new();
        let mut doomed: Vec<(String, String, u64)> = Vec::new();
        for (owner, key, version, current, fold) in snapshot {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fold(current, message)))
            {
                Ok(Ok(next)) => folded.push((owner, key, version, next)),
                Ok(Err(e)) => {
                    tracing::error!(
                        target: "codesmith_extensions",
                        "message projection {owner}/{key} failed and was dropped: {e}"
                    );
                    doomed.push((owner, key, version));
                }
                Err(_) => {
                    tracing::error!(
                        target: "codesmith_extensions",
                        "message projection {owner}/{key} panicked and was dropped"
                    );
                    doomed.push((owner, key, version));
                }
            }
        }
        let mut state = self.inner.lock().expect("projection hub poisoned");
        for (owner, key, version, next) in folded {
            let fresh = state.draw_version();
            if let Some(p) = state
                .projections
                .iter_mut()
                .find(|p| p.owner == owner && p.key == key && p.version == version)
            {
                p.state = next;
                p.version = fresh;
            }
        }
        state
            .projections
            .retain(|p| !doomed.contains(&(p.owner.clone(), p.key.clone(), p.version)));
    }

    /// Reset every projection to its `init` and fold the whole transcript
    /// (wholesale replacement: session restore, compaction, rollback).
    ///
    /// The folds run outside the lock (see [`fold_message`]) but the
    /// resulting states land in ONE lock step, so no window exists where a
    /// concurrent `register`, `clear`, or `fold_message` (the reload thread
    /// races the engine here) can observe a reset-then-partially-folded
    /// hub. A projection registered DURING the fold would otherwise stay
    /// at its init forever; it is picked up by a bounded follow-up pass
    /// ([`REFOLD_MAX_PASSES`] caps registration storms).
    pub fn refold_all(&self, messages: &[crate::models::Message]) {
        let clear_generation;
        let mut pending: Vec<(String, String, u64, Value, MessageFoldFn)> = {
            let state = self.inner.lock().expect("projection hub poisoned");
            clear_generation = state.clear_generation;
            if state.projections.is_empty() {
                return;
            }
            state
                .projections
                .iter()
                .map(|p| {
                    (
                        p.owner.clone(),
                        p.key.clone(),
                        p.version,
                        p.init.clone(),
                        Arc::clone(&p.fold),
                    )
                })
                .collect()
        };
        let mut passes = 0;
        loop {
            if pending.is_empty() {
                return;
            }
            passes += 1;
            if passes > REFOLD_MAX_PASSES {
                tracing::warn!(
                    target: "codesmith_extensions",
                    "message projection refold stopped after {REFOLD_MAX_PASSES} passes; \
                     {} projection(s) registered mid-refold stay at init until the next refold",
                    pending.len()
                );
                return;
            }
            let mut folded: Vec<(String, String, u64, Option<Value>)> = Vec::new();
            for (owner, key, version, init, fold) in &pending {
                let mut state = init.clone();
                let mut failed = false;
                for message in messages {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        fold(state.clone(), message)
                    })) {
                        Ok(Ok(next)) => state = next,
                        Ok(Err(e)) => {
                            tracing::error!(
                                target: "codesmith_extensions",
                                "message projection {owner}/{key} failed during refold \
                                 and was dropped: {e}"
                            );
                            failed = true;
                            break;
                        }
                        Err(_) => {
                            tracing::error!(
                                target: "codesmith_extensions",
                                "message projection {owner}/{key} panicked during refold \
                                 and was dropped"
                            );
                            failed = true;
                            break;
                        }
                    }
                }
                let next = if failed { None } else { Some(state) };
                folded.push((owner.clone(), key.clone(), *version, next));
            }
            pending = {
                let mut state = self.inner.lock().expect("projection hub poisoned");
                // A `clear` that landed while we folded owns the dirty flag;
                // consuming it here would swallow the reload signal.
                if state.clear_generation == clear_generation {
                    state.dirty = false;
                }
                let done: Vec<(String, String)> = folded
                    .iter()
                    .map(|(owner, key, _, _)| (owner.clone(), key.clone()))
                    .collect();
                state.projections.retain(|p| {
                    match folded.iter().find(|(owner, key, version, _)| {
                        *owner == p.owner && *key == p.key && *version == p.version
                    }) {
                        // Not ours: registered (or re-registered) while we
                        // were folding — keep, and pick it up on the next
                        // pass.
                        None => true,
                        Some((_, _, _, Some(_))) => true,
                        Some((_, _, _, None)) => false,
                    }
                });
                for (owner, key, version, next) in folded {
                    let fresh = state.draw_version();
                    if let Some(next) = next
                        && let Some(p) = state
                            .projections
                            .iter_mut()
                            .find(|p| p.owner == owner && p.key == key && p.version == version)
                    {
                        p.state = next;
                        p.version = fresh;
                    }
                }
                state
                    .projections
                    .iter()
                    .filter(|p| !done.contains(&(p.owner.clone(), p.key.clone())))
                    .map(|p| {
                        (
                            p.owner.clone(),
                            p.key.clone(),
                            p.version,
                            p.init.clone(),
                            Arc::clone(&p.fold),
                        )
                    })
                    .collect()
            };
        }
    }

    /// Drop all registered projections and mark the hub dirty (reload drops
    /// the generation's contributions; the engine refolds after the new
    /// generation registers).
    pub fn clear(&self) {
        let mut state = self.inner.lock().expect("projection hub poisoned");
        state.projections.clear();
        state.clear_generation += 1;
        state.dirty = true;
    }

    /// Consume the dirty flag (the engine refolds the transcript when this
    /// returns `true`).
    pub fn take_dirty(&self) -> bool {
        let mut state = self.inner.lock().expect("projection hub poisoned");
        std::mem::replace(&mut state.dirty, false)
    }

    /// Read a projection's current state (mod-namespaced).
    #[must_use]
    pub fn state(&self, owner: &str, key: &str) -> Option<Value> {
        self.inner
            .lock()
            .expect("projection hub poisoned")
            .projections
            .iter()
            .find(|p| p.owner == owner && p.key == key)
            .map(|p| p.state.clone())
    }
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

    /// Discipline 3 — the dispatch-contract table. Every kind pins its
    /// declared mode; the exhaustive `dispatch_mode` match makes a new
    /// variant a compile error without an arm, and this table makes
    /// CHANGING an existing kind's mode a test failure — the contract
    /// cannot drift silently. 31 rows = the full §F2a set +
    /// assistant-stream + tools-change (minus the removed
    /// `ResourcesDiscover`) + provider-request-error + turn-stopping
    /// (DSH alignment) + the 5 workflow lifecycle events (Emit-only).
    #[test]
    fn event_dispatch_contract_table() {
        use DispatchMode as M;
        use ExtensionEventKind as K;
        let contract: &[(K, M)] = &[
            // Waterfall (actionable field folds through the chain).
            (K::Input, M::Waterfall { can_deny: false }),
            (K::BeforeAgentStart, M::Waterfall { can_deny: false }),
            (K::BeforeProviderRequest, M::Waterfall { can_deny: false }),
            (K::ToolResult, M::Waterfall { can_deny: false }),
            // Denying waterfall (fold + monotonic deny).
            (K::ToolCall, M::Waterfall { can_deny: true }),
            (K::ProviderRequestError, M::Waterfall { can_deny: true }),
            // Serial (ordered listeners; veto short-circuits).
            (K::SessionBeforeSwitch, M::Serial),
            (K::SessionBeforeFork, M::Serial),
            (K::SessionBeforeCompact, M::Serial),
            (K::TurnStopping, M::Serial),
            // Emit (broadcast; outcomes advisory).
            (K::AssistantStream, M::Emit),
            (K::ProjectTrust, M::Emit),
            (K::SessionStart, M::Emit),
            (K::AgentStart, M::Emit),
            (K::TurnStart, M::Emit),
            (K::BeforeProviderHeaders, M::Emit),
            (K::AfterProviderResponse, M::Emit),
            (K::ToolExecutionStart, M::Emit),
            (K::ToolExecutionUpdate, M::Emit),
            (K::ToolExecutionEnd, M::Emit),
            (K::TurnEnd, M::Emit),
            (K::AgentEnd, M::Emit),
            (K::AgentSettled, M::Emit),
            (K::SessionShutdown, M::Emit),
            (K::SessionCompact, M::Emit),
            (K::ToolsChange, M::Emit),
            // Workflow lifecycle (observe-only, advisory).
            (K::WorkflowStart, M::Emit),
            (K::WorkflowLog, M::Emit),
            (K::WorkflowAgentStart, M::Emit),
            (K::WorkflowAgentEnd, M::Emit),
            (K::WorkflowEnd, M::Emit),
        ];
        assert_eq!(
            contract.len(),
            31,
            "the full §F2a set + assistant-stream + tools-change + provider-request-error \
             + turn-stopping + the workflow lifecycle family"
        );
        for (kind, mode) in contract {
            assert_eq!(&kind.dispatch_mode(), mode, "contract drift at {kind:?}");
        }
    }

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
            ExtensionMode::Interactive
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
        ) -> Result<HandlerOutcome, ExtensionError> {
            let label = match event {
                ExtensionEvent::SessionStart { .. } => "SessionStart",
                ExtensionEvent::TurnStart { .. } => "TurnStart",
                ExtensionEvent::ToolCall(_) => "ToolCall",
                ExtensionEvent::ToolResult(_) => "ToolResult",
                ExtensionEvent::TurnEnd { .. } => "TurnEnd",
                ExtensionEvent::SessionShutdown => "SessionShutdown",
                _ => "other",
            };
            self.seen.lock().unwrap().push(label);
            Ok(HandlerOutcome::Continue)
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

    #[test]
    fn f2a_payload_structs_construct_and_debug() {
        let _ = TrustReason::FirstLoad;
        let input = InputEvent { text: "hi".into() };
        assert_eq!(input.text, "hi");
        let start = AgentStartEvent {
            system_prompt: Some("s".into()),
            inject_message: None,
        };
        assert!(start.system_prompt.is_some());
        let req = BeforeProviderRequestEvent {
            messages: json!({}),
        };
        let resp = AfterProviderResponseEvent {
            response: json!({}),
        };
        let upd = ToolExecutionUpdateEvent {
            id: "c1".into(),
            name: "echo".into(),
            message: "ok".into(),
        };
        let hdrs = BeforeProviderHeadersEvent {
            headers: json!({"x-custom": "v"}),
        };
        let pre = ProviderRequestErrorEvent {
            failure: ProviderFailure {
                category: "state".into(),
                message: "503".into(),
            },
            attempt: 2,
            plan: RetryPlan::GiveUp,
        };
        let stop = TurnStoppingEvent {
            reason: TurnEndReason::NoToolCalls,
        };
        // Debug renders without panic for every new type.
        let _ = format!("{start:?} {req:?} {resp:?} {upd:?} {hdrs:?} {pre:?} {stop:?}");
    }

    #[test]
    fn header_masking_hides_credential_bearing_values() {
        // Benign names pass through verbatim.
        assert_eq!(mask_sensitive_header("x-custom", "abc"), "abc");
        assert_eq!(
            mask_sensitive_header("accept", "text/event-stream"),
            "text/event-stream"
        );
        // Credential-bearing names (case-insensitive, substring) are
        // redacted — the masked form never contains the raw value.
        for name in [
            "Authorization",
            "x-api-key",
            "X-Api-Key",
            "x-goog-api-key",
            "cookie",
            "x-session-token",
        ] {
            let masked = mask_sensitive_header(name, "sk-secret-value-123456");
            assert_ne!(masked, "sk-secret-value-123456", "{name} must be masked");
            assert!(
                !masked.contains("secret-value"),
                "{name} mask leaked content"
            );
        }
        // The deny-list matches by substring, so a benign name carrying a
        // needle is masked too (fail-closed direction).
        assert_ne!(mask_sensitive_header("my-token-header", "v"), "v");
    }

    #[test]
    fn f2a_event_kind_round_trips_every_variant() {
        use ExtensionEventKind as K;
        let cases: Vec<(ExtensionEvent, ExtensionEventKind)> = vec![
            (
                ExtensionEvent::ProjectTrust {
                    reason: TrustReason::Trusted,
                },
                K::ProjectTrust,
            ),
            (
                ExtensionEvent::SessionStart {
                    reason: SessionReason::Startup,
                },
                K::SessionStart,
            ),
            (
                ExtensionEvent::Input(InputEvent { text: "x".into() }),
                K::Input,
            ),
            (
                ExtensionEvent::BeforeAgentStart(AgentStartEvent {
                    system_prompt: None,
                    inject_message: None,
                }),
                K::BeforeAgentStart,
            ),
            (ExtensionEvent::AgentStart, K::AgentStart),
            (
                ExtensionEvent::TurnStart {
                    turn_id: "t".into(),
                },
                K::TurnStart,
            ),
            (
                ExtensionEvent::BeforeProviderHeaders(BeforeProviderHeadersEvent {
                    headers: json!({}),
                }),
                K::BeforeProviderHeaders,
            ),
            (
                ExtensionEvent::BeforeProviderRequest(BeforeProviderRequestEvent {
                    messages: json!({}),
                }),
                K::BeforeProviderRequest,
            ),
            (
                ExtensionEvent::AfterProviderResponse(AfterProviderResponseEvent {
                    response: json!({}),
                }),
                K::AfterProviderResponse,
            ),
            (
                ExtensionEvent::ProviderRequestError(ProviderRequestErrorEvent {
                    failure: ProviderFailure {
                        category: "rate_limit".into(),
                        message: "429".into(),
                    },
                    attempt: 1,
                    plan: RetryPlan::Retry { delay_ms: 0 },
                }),
                K::ProviderRequestError,
            ),
            (
                ExtensionEvent::TurnStopping(TurnStoppingEvent {
                    reason: TurnEndReason::NoToolCalls,
                }),
                K::TurnStopping,
            ),
            (ExtensionEvent::ToolExecutionStart, K::ToolExecutionStart),
            (
                ExtensionEvent::ToolCall(ToolCallEvent {
                    id: "c".into(),
                    name: "n".into(),
                    input: json!({}),
                }),
                K::ToolCall,
            ),
            (
                ExtensionEvent::ToolExecutionUpdate(ToolExecutionUpdateEvent {
                    id: "c".into(),
                    name: "n".into(),
                    message: "m".into(),
                }),
                K::ToolExecutionUpdate,
            ),
            (
                ExtensionEvent::ToolResult(ToolResultEvent {
                    id: "c".into(),
                    name: "n".into(),
                    result: Ok(ToolResult::success("ok")),
                }),
                K::ToolResult,
            ),
            (ExtensionEvent::ToolExecutionEnd, K::ToolExecutionEnd),
            (
                ExtensionEvent::TurnEnd {
                    turn_id: "t".into(),
                    reason: TurnEndReason::NoToolCalls,
                },
                K::TurnEnd,
            ),
            (ExtensionEvent::AgentEnd, K::AgentEnd),
            (ExtensionEvent::AgentSettled, K::AgentSettled),
            (ExtensionEvent::SessionBeforeSwitch, K::SessionBeforeSwitch),
            (ExtensionEvent::SessionBeforeFork, K::SessionBeforeFork),
            (ExtensionEvent::SessionShutdown, K::SessionShutdown),
            (
                ExtensionEvent::SessionBeforeCompact,
                K::SessionBeforeCompact,
            ),
            (ExtensionEvent::SessionCompact, K::SessionCompact),
            (
                ExtensionEvent::AssistantStream(AssistantStreamEvent { text: "x".into() }),
                K::AssistantStream,
            ),
            (
                ExtensionEvent::ToolsChange {
                    added: vec![],
                    removed: vec![],
                },
                K::ToolsChange,
            ),
            // Workflow lifecycle (observe-only, Emit family — same values
            // the W2 engine emits: `status` per the documented vocabulary,
            // `error: None` on success).
            (ExtensionEvent::WorkflowStart, K::WorkflowStart),
            (
                ExtensionEvent::WorkflowLog(WorkflowLogEvent {
                    message: "m".into(),
                }),
                K::WorkflowLog,
            ),
            (
                ExtensionEvent::WorkflowAgentStart(WorkflowAgentStartEvent {
                    objective: "o".into(),
                }),
                K::WorkflowAgentStart,
            ),
            (
                ExtensionEvent::WorkflowAgentEnd(WorkflowAgentEndEvent {
                    status: "completed".into(),
                    steps: 1,
                }),
                K::WorkflowAgentEnd,
            ),
            (
                ExtensionEvent::WorkflowEnd(WorkflowEndEvent {
                    status: "completed".into(),
                    error: None,
                    agent_calls: 1,
                }),
                K::WorkflowEnd,
            ),
        ];
        assert_eq!(cases.len(), 31, "all variants covered");
        for (ev, kind) in cases {
            assert_eq!(ev.kind(), kind, "kind mismatch for {ev:?}");
        }
    }

    #[test]
    fn f2a_handler_outcome_constructs_each_variant() {
        let _ = HandlerOutcome::Continue;
        let c = HandlerOutcome::Cancel {
            reason: "no".into(),
        };
        let b = HandlerOutcome::Block {
            reason: "denied".into(),
        };
        let a = HandlerOutcome::Ask {
            reason: "confirm?".into(),
        };
        let t = HandlerOutcome::Transform(ExtensionEvent::SessionShutdown);
        let _ = format!("{c:?} {b:?} {a:?} {t:?}");
    }

    #[test]
    fn f2a_handler_handle_returns_continue_by_default() {
        let h = RecordingHandler {
            seen: Mutex::new(Vec::new()),
        };
        let ctx = TestContext::new();
        let out = block_on(h.handle(&ExtensionEvent::SessionShutdown, &ctx)).unwrap();
        assert!(matches!(out, HandlerOutcome::Continue));
    }

    // === Message projections ==================================================

    use crate::models::{ContentBlock, Message};

    fn proj_msg(role: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: "x".into(),
                cache_control: None,
            }],
        }
    }

    fn counting_fold() -> MessageFoldFn {
        Arc::new(|state, msg| {
            let n = state.as_u64().unwrap_or(0) + u64::from(msg.role == "assistant");
            Ok(Value::from(n))
        })
    }

    #[test]
    fn projection_register_fold_refold_clear_round_trip() {
        let hub = MessageProjectionHub::new();
        hub.register("mod-a", "count", Value::from(0), counting_fold())
            .unwrap();
        // Duplicate (owner, key) is rejected — states are not migratable.
        assert!(
            hub.register("mod-a", "count", Value::from(0), counting_fold())
                .is_err()
        );
        // The same key under another mod is a different projection.
        hub.register(
            "mod-b",
            "count",
            Value::from(7),
            Arc::new(|state, _| Ok(state)),
        )
        .unwrap();

        hub.fold_message(&proj_msg("assistant"));
        hub.fold_message(&proj_msg("user"));
        hub.fold_message(&proj_msg("assistant"));
        assert_eq!(hub.state("mod-a", "count"), Some(Value::from(2)));
        assert_eq!(hub.state("mod-b", "count"), Some(Value::from(7)));

        // Refold resets to init and folds the whole transcript.
        hub.refold_all(&[
            proj_msg("assistant"),
            proj_msg("assistant"),
            proj_msg("assistant"),
        ]);
        assert_eq!(hub.state("mod-a", "count"), Some(Value::from(3)));

        // Clear drops entries and turns the hub dirty (reload signal).
        hub.clear();
        assert_eq!(hub.state("mod-a", "count"), None);
        assert!(hub.take_dirty());
        assert!(!hub.take_dirty(), "dirty is one-shot");
    }

    #[test]
    fn projection_fold_error_drops_the_projection() {
        let hub = MessageProjectionHub::new();
        // Fails from the second fold on (state-driven — the fold is `Fn`).
        let fold: MessageFoldFn = Arc::new(|state, _| {
            if state.as_u64().is_some_and(|n| n >= 1) {
                Err("script bug".to_string())
            } else {
                Ok(Value::from(1u64))
            }
        });
        hub.register("m", "flaky", Value::from(0), fold).unwrap();
        hub.fold_message(&proj_msg("user"));
        assert_eq!(hub.state("m", "flaky"), Some(Value::from(1)));
        hub.fold_message(&proj_msg("user"));
        assert_eq!(hub.state("m", "flaky"), None, "poisoned projection dropped");
        // A dropped projection is gone for good until the next mod load.
        hub.fold_message(&proj_msg("user"));
        assert_eq!(hub.state("m", "flaky"), None);
    }

    #[test]
    fn projection_key_format_and_cap_enforced() {
        let hub = MessageProjectionHub::new();
        assert!(
            hub.register("m", "bad key", Value::Null, counting_fold())
                .is_err()
        );
        assert!(hub.register("m", "", Value::Null, counting_fold()).is_err());
        let identity: MessageFoldFn = Arc::new(|state, _| Ok(state));
        for i in 0..MAX_MESSAGE_PROJECTIONS {
            hub.register("m", &format!("p{i}"), Value::Null, identity.clone())
                .unwrap();
        }
        assert!(
            hub.register("m", "overflow", Value::Null, identity)
                .is_err()
        );
    }

    // --- snapshot/commit lineage (reload thread races the engine) ----------

    #[test]
    fn stale_fold_does_not_land_on_a_re_registered_incarnation() {
        let hub = MessageProjectionHubArc::new(MessageProjectionHub::new());
        // The fold re-enters the hub mid-fold (what a concurrent reload's
        // `clear` + re-registration of the same `(owner, key)` looks like to
        // an in-flight fold).
        let hub2 = Arc::clone(&hub);
        let fold: MessageFoldFn = Arc::new(move |state, _| {
            if state.as_u64() == Some(0) {
                hub2.clear();
                hub2.register("m", "count", Value::from(100), counting_fold())
                    .unwrap();
            }
            Ok(Value::from(state.as_u64().map_or(100, |n| n + 1)))
        });
        hub.register("m", "count", Value::from(0), fold).unwrap();
        hub.fold_message(&proj_msg("user"));
        assert_eq!(
            hub.state("m", "count"),
            Some(Value::from(100)),
            "the fresh incarnation keeps its init; the stale fold is dropped"
        );
    }

    #[test]
    fn doomed_fold_does_not_drop_a_re_registered_incarnation() {
        let hub = MessageProjectionHubArc::new(MessageProjectionHub::new());
        let hub2 = Arc::clone(&hub);
        let flaky: MessageFoldFn = Arc::new(move |state, _| {
            if state.as_u64() == Some(0) {
                hub2.clear();
                hub2.register("m", "flaky", Value::from(42), counting_fold())
                    .unwrap();
                Err("script bug".to_string())
            } else {
                Ok(state)
            }
        });
        hub.register("m", "flaky", Value::from(0), flaky).unwrap();
        hub.fold_message(&proj_msg("user"));
        assert_eq!(
            hub.state("m", "flaky"),
            Some(Value::from(42)),
            "the failed fold only dooms the incarnation it snapshotted"
        );
    }

    #[test]
    fn fold_snapshotted_before_a_refold_commit_does_not_revert_it() {
        let hub = MessageProjectionHubArc::new(MessageProjectionHub::new());
        let fired = std::sync::atomic::AtomicBool::new(false);
        let hub2 = Arc::clone(&hub);
        let fold: MessageFoldFn = Arc::new(move |state, msg| {
            if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                hub2.refold_all(&[proj_msg("assistant")]);
                Ok(Value::from(999u64))
            } else {
                let n = state.as_u64().unwrap_or(0) + u64::from(msg.role == "assistant");
                Ok(Value::from(n))
            }
        });
        hub.register("m", "count", Value::from(0), fold).unwrap();
        hub.fold_message(&proj_msg("user"));
        assert_eq!(
            hub.state("m", "count"),
            Some(Value::from(1)),
            "the refold result survives; the stale fold does not overwrite it"
        );
    }

    #[test]
    fn refold_commit_does_not_clobber_a_mid_fold_clear() {
        let hub = MessageProjectionHubArc::new(MessageProjectionHub::new());
        let hub2 = Arc::clone(&hub);
        let fold: MessageFoldFn = Arc::new(move |state, _| {
            if state.as_u64() == Some(0) {
                // A reload lands while the refold's folds run outside the
                // lock: clear() empties the hub and raises the reload flag.
                hub2.clear();
            }
            Ok(state)
        });
        hub.register("m", "count", Value::from(0), fold).unwrap();
        hub.refold_all(&[proj_msg("assistant")]);
        assert_eq!(
            hub.state("m", "count"),
            None,
            "the clear() dropped the projection"
        );
        assert!(
            hub.take_dirty(),
            "the clear()'s reload signal survives the refold commit"
        );
    }

    fn guard_call_event(name: &str, command: &str) -> ExtensionEvent {
        ExtensionEvent::ToolCall(ToolCallEvent {
            id: "t1".to_string(),
            name: name.to_string(),
            input: json!({ "command": command }),
        })
    }

    #[test]
    fn guard_handler_denial_maps_to_attributed_block() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let guard: ToolGuardFn = Arc::new(|e| {
            (e.name == "exec_shell" && e.input["command"].as_str()?.contains("rm -rf"))
                .then(|| "destructive command".to_string())
        });
        let h = GuardHandler::new("guard (mod: safety)", guard);
        let out = rt
            .block_on(h.handle(
                &guard_call_event("exec_shell", "rm -rf /tmp/x"),
                &TestContext::new(),
            ))
            .unwrap();
        assert!(matches!(
            &out,
            HandlerOutcome::Block { reason }
                if reason == "guard (mod: safety): destructive command"
        ));
    }

    #[test]
    fn guard_handler_abstention_and_misroute_continue() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let guard: ToolGuardFn = Arc::new(|e| {
            (e.name == "exec_shell" && e.input["command"].as_str()?.contains("rm -rf"))
                .then(|| "destructive command".to_string())
        });
        let h = GuardHandler::new("guard (mod: safety)", guard);
        let ctx = TestContext::new();
        // Abstains on a non-matching call…
        let out = rt
            .block_on(h.handle(&guard_call_event("exec_shell", "ls"), &ctx))
            .unwrap();
        assert!(matches!(out, HandlerOutcome::Continue));
        // …denies on a matching one…
        let out = rt
            .block_on(h.handle(&guard_call_event("exec_shell", "rm -rf /"), &ctx))
            .unwrap();
        assert!(matches!(out, HandlerOutcome::Block { .. }));
        // …and abstains on a non-ToolCall event (the kind filter makes this
        // unreachable in practice; the arm must not deny what it never
        // matched).
        let out = rt
            .block_on(h.handle(&ExtensionEvent::ToolExecutionStart, &ctx))
            .unwrap();
        assert!(matches!(out, HandlerOutcome::Continue));
    }

    #[test]
    fn guard_handler_panic_fails_open() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let guard: ToolGuardFn = Arc::new(|_| panic!("guard bug"));
        let h = GuardHandler::new("guard (mod: safety)", guard);
        let out = rt
            .block_on(h.handle(&guard_call_event("exec_shell", "ls"), &TestContext::new()))
            .unwrap();
        assert!(matches!(out, HandlerOutcome::Continue));
    }

    // === Registration scope (DSH alignment) =============================

    #[test]
    fn scope_covers_matrix() {
        let g = Scope::Global;
        let explore = Scope::AgentType("explore".into());
        let general = Scope::AgentType("general".into());
        // A global registration covers every view/dispatch scope.
        assert!(g.covers(&g));
        assert!(g.covers(&explore));
        assert!(g.covers(&general));
        // An agent-type registration covers only its own scope.
        assert!(
            !explore.covers(&g),
            "scoped registrations never leak into the global view"
        );
        assert!(explore.covers(&explore));
        assert!(!explore.covers(&general));
        assert!(g.is_global());
        assert!(!explore.is_global());
    }

    #[test]
    fn scope_validation_rejects_blank_agent_type_ids() {
        assert!(Scope::Global.validate().is_ok());
        assert!(Scope::AgentType("explore".into()).validate().is_ok());
        for bad in ["", "  ", "\t"] {
            assert!(
                Scope::AgentType(bad.into()).validate().is_err(),
                "blank id {bad:?} must fail loud"
            );
        }
    }

    // === Around-dispatch (DSH tools/execute) =============================

    /// An around handler that records its visit, optionally vetoes, and
    /// otherwise stamps its label through the result.
    struct RecordingAround {
        label: &'static str,
        seen: Arc<Mutex<Vec<&'static str>>>,
        veto: bool,
    }

    impl RecordingAround {
        fn passthrough(label: &'static str, seen: &Arc<Mutex<Vec<&'static str>>>) -> Arc<Self> {
            Arc::new(Self {
                label,
                seen: Arc::clone(seen),
                veto: false,
            })
        }
    }

    #[async_trait]
    impl AroundToolHandler for RecordingAround {
        async fn call(
            &self,
            _call: ToolCallEvent,
            next: ToolCallNext<'_>,
        ) -> Result<ToolResult, ToolError> {
            self.seen.lock().unwrap().push(self.label);
            if self.veto {
                return Err(ToolError::permission_denied(format!(
                    "{}: not on my watch",
                    self.label
                )));
            }
            let mut result = next.await?;
            result.content = format!("{}|{}", self.label, result.content);
            Ok(result)
        }
    }

    fn around_call_event() -> ToolCallEvent {
        ToolCallEvent {
            id: "c1".to_string(),
            name: "echo".to_string(),
            input: json!({}),
        }
    }

    fn terminal() -> ToolCallNext<'static> {
        Box::pin(async { Ok(ToolResult::success("base")) })
    }

    #[test]
    fn around_chain_runs_outermost_first_and_stamps_through() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let handlers: Vec<Arc<dyn AroundToolHandler>> = vec![
            RecordingAround::passthrough("outer", &seen),
            RecordingAround::passthrough("inner", &seen),
        ];
        let out = rt.block_on(wrap_tool_dispatch(
            &handlers,
            around_call_event(),
            terminal(),
        ));
        let result = out.expect("pass-through chain reaches the terminal");
        assert_eq!(result.content, "outer|inner|base");
        assert_eq!(*seen.lock().unwrap(), vec!["outer", "inner"]);
    }

    #[test]
    fn around_veto_never_invokes_next() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let vetoing = Arc::new(RecordingAround {
            label: "outer",
            seen: Arc::clone(&seen),
            veto: true,
        });
        let handlers: Vec<Arc<dyn AroundToolHandler>> = vec![
            vetoing as Arc<dyn AroundToolHandler>,
            RecordingAround::passthrough("inner", &seen),
        ];
        let err = rt
            .block_on(wrap_tool_dispatch(
                &handlers,
                around_call_event(),
                terminal(),
            ))
            .expect_err("a veto must surface as an Err");
        assert!(
            err.to_string().contains("not on my watch"),
            "permission-denied veto: {err}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["outer"],
            "the inner handler + terminal never run past a veto"
        );
    }

    // === Around-turn (DSH waterfall next() over the main turn) ===========

    /// An around-turn handler that records its pre phase, optionally
    /// vetoes (returning its own `StopReason` without awaiting `next`),
    /// and otherwise records its post phase after the inner turn.
    struct RecordingAroundTurn {
        label: &'static str,
        seen: Arc<Mutex<Vec<String>>>,
        veto: bool,
    }

    impl RecordingAroundTurn {
        fn passthrough(label: &'static str, seen: &Arc<Mutex<Vec<String>>>) -> Arc<Self> {
            Arc::new(Self {
                label,
                seen: Arc::clone(seen),
                veto: false,
            })
        }
    }

    #[async_trait]
    impl AroundTurnHandler for RecordingAroundTurn {
        async fn call(
            &self,
            turn: TurnAroundEvent,
            next: TurnNext<'_>,
        ) -> anyhow::Result<StopReason> {
            self.seen
                .lock()
                .unwrap()
                .push(format!("{}:pre:{}", self.label, turn.turn_id));
            if self.veto {
                return Ok(StopReason::Error(format!("{}: vetoed", self.label)));
            }
            let reason = next.await?;
            self.seen
                .lock()
                .unwrap()
                .push(format!("{}:post", self.label));
            Ok(reason)
        }
    }

    fn turn_terminal(seen: &Arc<Mutex<Vec<String>>>) -> TurnNext<'static> {
        let seen = Arc::clone(seen);
        Box::pin(async move {
            seen.lock().unwrap().push("loop".to_string());
            Ok(StopReason::NoToolCalls)
        })
    }

    fn turn_event() -> TurnAroundEvent {
        TurnAroundEvent {
            turn_id: "t1".to_string(),
        }
    }

    /// Outermost-first pre phases, the loop once, then post phases unwind
    /// innermost-first; the inner `StopReason` passes through.
    #[test]
    fn around_turn_chain_wraps_the_loop_outermost_first() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let handlers: Vec<Arc<dyn AroundTurnHandler>> = vec![
            RecordingAroundTurn::passthrough("outer", &seen),
            RecordingAroundTurn::passthrough("inner", &seen),
        ];
        let reason = rt
            .block_on(wrap_turn_dispatch(
                &handlers,
                turn_event(),
                turn_terminal(&seen),
            ))
            .expect("pass-through chain reaches the loop");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "outer:pre:t1",
                "inner:pre:t1",
                "loop",
                "inner:post",
                "outer:post"
            ]
        );
    }

    /// A veto returns the handler's own reason; the loop never runs.
    #[test]
    fn around_turn_veto_never_runs_the_loop() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let vetoing = Arc::new(RecordingAroundTurn {
            label: "outer",
            seen: Arc::clone(&seen),
            veto: true,
        });
        let handlers: Vec<Arc<dyn AroundTurnHandler>> = vec![
            vetoing as Arc<dyn AroundTurnHandler>,
            RecordingAroundTurn::passthrough("inner", &seen),
        ];
        let reason = rt
            .block_on(wrap_turn_dispatch(
                &handlers,
                turn_event(),
                turn_terminal(&seen),
            ))
            .expect("a veto is a normal StopReason, not an Err");
        assert_eq!(reason, StopReason::Error("outer: vetoed".to_string()));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["outer:pre:t1"],
            "the inner handler + loop never run past a veto"
        );
    }

    /// No handlers: the chain IS the terminal (no-extension parity).
    #[test]
    fn around_turn_empty_chain_is_the_loop() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let handlers: Vec<Arc<dyn AroundTurnHandler>> = Vec::new();
        let reason = rt
            .block_on(wrap_turn_dispatch(
                &handlers,
                turn_event(),
                turn_terminal(&seen),
            ))
            .expect("terminal");
        assert_eq!(reason, StopReason::NoToolCalls);
        assert_eq!(*seen.lock().unwrap(), vec!["loop"]);
    }
}
