//! `RhaiMod` — a script mod as an [`Extension`](codesmith_agent::extension::Extension).
//!
//! Construction = compile + run the top-level script once against an
//! `Engine` whose natives capture `on` / `register_tool` / `register_command`
//! registrations into a shared cell; [`RhaiMod::configure`] then replays the
//! captured registrations against the runner's `ExtensionApi`. The `Engine`
//! and the compiled `AST` are shared (via `Arc`) with the emitted
//! `ScriptHandler` / `ScriptToolDefinition` / `ScriptCommandDefinition`
//! adapters — every later `FnPtr` call goes through
//! [`FnPtr::call(&Engine, &AST, args)`](rhai::FnPtr::call), so closures stay
//! resolvable after the load-time script run completes.
//!
//! Event payloads are hand-mapped to Rhai object maps (the plan's
//! `#{content, success, is_error}` flattening for `ToolResult` included);
//! `serde_json::Value` fields (`ToolCall.input`, provider `messages`) go
//! through `rhai::serde::to_dynamic` / `from_dynamic`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codesmith_agent::extension::{
    AgentStartEvent, Extension, ExtensionApi, ExtensionContext, ExtensionError, ExtensionEvent,
    ExtensionEventKind, ExtensionMetadata, ExtensionMode, InputEvent, ToolCallEvent,
    ToolResultEvent,
};
use codesmith_agent::provider::ProviderId;
use codesmith_tools::{ToolError, ToolResult};
use rhai::Dynamic;

use super::kv::ModKvStore;
use super::mod_manifest::DiscoveredMod;

/// Script-side operation budget per call (plan §三.4). Generous for hook
/// logic, instant-death for runaway loops.
pub(crate) const MAX_OPERATIONS: u64 = 200_000;
/// Script-side call depth cap.
pub(crate) const MAX_CALL_LEVELS: usize = 64;
/// String/array/map size caps — bound worst-case per-op allocations while
/// leaving room for prompt-scale payloads (input transforms).
pub(crate) const MAX_STRING_SIZE: usize = 8 * 1024 * 1024;
pub(crate) const MAX_ARRAY_SIZE: usize = 100_000;
pub(crate) const MAX_MAP_SIZE: usize = 100_000;

/// Shared per-mod script runtime: the `Engine` (post-construction every Rhai
/// API we use takes `&self`) + the compiled `AST` (anonymous closures live
/// in its function library, so it must outlive every `FnPtr` call).
pub(crate) struct ScriptRuntime {
    pub(crate) engine: Arc<rhai::Engine>,
    pub(crate) ast: Arc<rhai::AST>,
}

/// One registration captured from the load-time script run (`on` /
/// `register_tool` / `register_command`). Public for host-side
/// introspection (`/mods list` contribution counts).
#[derive(Clone)]
pub enum ScriptRegistration {
    Handler {
        kind: ExtensionEventKind,
        callback: rhai::FnPtr,
    },
    Tool {
        name: String,
        description: String,
        schema: serde_json::Value,
        callback: rhai::FnPtr,
    },
    Command {
        name: String,
        description: String,
        callback: rhai::FnPtr,
    },
    /// Route B — a named system-prompt section captured by the
    /// `register_prompt_section(id, text)` native. Replayed at
    /// `configure` via `ExtensionApi::register_prompt_section`.
    PromptSection { id: String, text: String },
    /// Route A — a declarative provider alias captured by the
    /// `register_provider(spec)` native. Replayed at `configure` via
    /// `ExtensionApi::register_provider_alias`.
    Provider {
        id: String,
        target: ProviderId,
        base_url: Option<String>,
        default_model: Option<String>,
        http_headers: Option<std::collections::HashMap<String, String>>,
    },
    /// A session-log fold captured by the `register_message_projection(key,
    /// init, fold)` native. Replayed at `configure` via
    /// `ExtensionApi::register_message_projection` (the closure adapter
    /// converts JSON state / `Message` ↔ Rhai `Dynamic`).
    MessageProjection {
        key: String,
        init: serde_json::Value,
        fold: rhai::FnPtr,
    },
}

type RegistrationCell = Arc<Mutex<Vec<ScriptRegistration>>>;

// === Event-name mapping (24 kinds, kebab-case) =============================

/// Parse a script-side event name (kebab-case) into its
/// [`ExtensionEventKind`]. `None` for unknown names (surfaced as a script
/// error at registration time).
pub(crate) fn event_kind_from_name(name: &str) -> Option<ExtensionEventKind> {
    Some(match name {
        "project-trust" => ExtensionEventKind::ProjectTrust,
        "session-start" => ExtensionEventKind::SessionStart,
        "resources-discover" => ExtensionEventKind::ResourcesDiscover,
        "input" => ExtensionEventKind::Input,
        "before-agent-start" => ExtensionEventKind::BeforeAgentStart,
        "agent-start" => ExtensionEventKind::AgentStart,
        "turn-start" => ExtensionEventKind::TurnStart,
        "before-provider-headers" => ExtensionEventKind::BeforeProviderHeaders,
        "before-provider-request" => ExtensionEventKind::BeforeProviderRequest,
        "after-provider-response" => ExtensionEventKind::AfterProviderResponse,
        "tool-execution-start" => ExtensionEventKind::ToolExecutionStart,
        "assistant-stream" => ExtensionEventKind::AssistantStream,
        "tool-call" => ExtensionEventKind::ToolCall,
        "tool-execution-update" => ExtensionEventKind::ToolExecutionUpdate,
        "tool-result" => ExtensionEventKind::ToolResult,
        "tool-execution-end" => ExtensionEventKind::ToolExecutionEnd,
        "turn-end" => ExtensionEventKind::TurnEnd,
        "agent-end" => ExtensionEventKind::AgentEnd,
        "agent-settled" => ExtensionEventKind::AgentSettled,
        "session-before-switch" => ExtensionEventKind::SessionBeforeSwitch,
        "session-before-fork" => ExtensionEventKind::SessionBeforeFork,
        "session-shutdown" => ExtensionEventKind::SessionShutdown,
        "session-before-compact" => ExtensionEventKind::SessionBeforeCompact,
        "session-compact" => ExtensionEventKind::SessionCompact,
        "tools-change" => ExtensionEventKind::ToolsChange,
        _ => return None,
    })
}

/// The kebab-case script-side name for a kind (the inverse of
/// [`event_kind_from_name`]); used to stamp a `kind` field on payloads.
pub(crate) fn event_name_from_kind(kind: ExtensionEventKind) -> &'static str {
    match kind {
        ExtensionEventKind::AssistantStream => "assistant-stream",
        ExtensionEventKind::ProjectTrust => "project-trust",
        ExtensionEventKind::SessionStart => "session-start",
        ExtensionEventKind::ResourcesDiscover => "resources-discover",
        ExtensionEventKind::Input => "input",
        ExtensionEventKind::BeforeAgentStart => "before-agent-start",
        ExtensionEventKind::AgentStart => "agent-start",
        ExtensionEventKind::TurnStart => "turn-start",
        ExtensionEventKind::BeforeProviderHeaders => "before-provider-headers",
        ExtensionEventKind::BeforeProviderRequest => "before-provider-request",
        ExtensionEventKind::AfterProviderResponse => "after-provider-response",
        ExtensionEventKind::ToolExecutionStart => "tool-execution-start",
        ExtensionEventKind::ToolCall => "tool-call",
        ExtensionEventKind::ToolExecutionUpdate => "tool-execution-update",
        ExtensionEventKind::ToolResult => "tool-result",
        ExtensionEventKind::ToolExecutionEnd => "tool-execution-end",
        ExtensionEventKind::TurnEnd => "turn-end",
        ExtensionEventKind::AgentEnd => "agent-end",
        ExtensionEventKind::AgentSettled => "agent-settled",
        ExtensionEventKind::SessionBeforeSwitch => "session-before-switch",
        ExtensionEventKind::SessionBeforeFork => "session-before-fork",
        ExtensionEventKind::SessionShutdown => "session-shutdown",
        ExtensionEventKind::SessionBeforeCompact => "session-before-compact",
        ExtensionEventKind::SessionCompact => "session-compact",
        ExtensionEventKind::ToolsChange => "tools-change",
        // `ExtensionEventKind` is `#[non_exhaustive]`; the wildcard is
        // unreachable while this crate tracks every variant (the 24-entry
        // round-trip test guards that).
        _ => "unknown-event",
    }
}

// === Event payload mapping ==================================================

fn session_reason(r: codesmith_agent::extension::SessionReason) -> &'static str {
    match r {
        codesmith_agent::extension::SessionReason::Startup => "startup",
        codesmith_agent::extension::SessionReason::Reload => "reload",
        codesmith_agent::extension::SessionReason::New => "new",
        codesmith_agent::extension::SessionReason::Resume => "resume",
        codesmith_agent::extension::SessionReason::Fork => "fork",
    }
}

fn turn_end_reason(r: codesmith_agent::extension::TurnEndReason) -> &'static str {
    match r {
        codesmith_agent::extension::TurnEndReason::NoToolCalls => "no-tool-calls",
        codesmith_agent::extension::TurnEndReason::MaxSteps => "max-steps",
        codesmith_agent::extension::TurnEndReason::Interrupted => "interrupted",
        codesmith_agent::extension::TurnEndReason::Error => "error",
    }
}

fn trust_reason(r: codesmith_agent::extension::TrustReason) -> &'static str {
    match r {
        codesmith_agent::extension::TrustReason::FirstLoad => "first-load",
        codesmith_agent::extension::TrustReason::Trusted => "trusted",
        codesmith_agent::extension::TrustReason::Untrusted => "untrusted",
    }
}

fn discover_reason(r: codesmith_agent::extension::DiscoverReason) -> &'static str {
    match r {
        codesmith_agent::extension::DiscoverReason::Startup => "startup",
        codesmith_agent::extension::DiscoverReason::Manual => "manual",
        codesmith_agent::extension::DiscoverReason::Reload => "reload",
    }
}

fn mode_string(m: ExtensionMode) -> &'static str {
    match m {
        ExtensionMode::Tui => "tui",
        ExtensionMode::Rpc => "rpc",
        ExtensionMode::Json => "json",
        ExtensionMode::Print => "print",
    }
}

/// Build a Rhai object map from `(key, value)` entries.
fn dynamic_map(entries: Vec<(&str, Dynamic)>) -> Dynamic {
    let mut m = rhai::Map::new();
    for (k, v) in entries {
        m.insert(k.into(), v);
    }
    Dynamic::from(m)
}

/// `serde_json::Value` → Rhai dynamic (`rhai::serde::to_dynamic`); `UNIT`
/// on conversion failure (defensive — all `Value`s convert).
pub(crate) fn json_to_dynamic(value: &serde_json::Value) -> Dynamic {
    rhai::serde::to_dynamic(value).unwrap_or(Dynamic::UNIT)
}

/// Rhai dynamic → `serde_json::Value` (`rhai::serde::from_dynamic`).
pub(crate) fn dynamic_to_json(value: &Dynamic) -> Result<serde_json::Value, String> {
    rhai::serde::from_dynamic::<serde_json::Value>(value)
        .map_err(|e| format!("convert Rhai value to JSON: {e}"))
}

/// The handler-side context map: `#{cwd, mode, idle, generation}`.
pub(crate) fn ctx_to_dynamic(ctx: &dyn ExtensionContext) -> Dynamic {
    dynamic_map(vec![
        ("cwd", Dynamic::from(ctx.cwd().display().to_string())),
        ("mode", Dynamic::from(mode_string(ctx.mode()))),
        ("idle", Dynamic::from(ctx.is_idle())),
        ("generation", Dynamic::from(ctx.generation() as i64)),
    ])
}

/// Convert an event into the script-side payload map. Every payload carries
/// a `kind` field (the kebab-case event name) plus its variant fields;
/// `ToolResult` flattens to `#{content, success, is_error}` (plan §三.3).
pub(crate) fn event_to_dynamic(event: &ExtensionEvent) -> Dynamic {
    let kind = event_name_from_kind(event.kind());
    let mut entries: Vec<(&str, Dynamic)> = match event {
        ExtensionEvent::ProjectTrust { reason } => {
            vec![("reason", Dynamic::from(trust_reason(*reason)))]
        }
        ExtensionEvent::SessionStart { reason } => {
            vec![("reason", Dynamic::from(session_reason(*reason)))]
        }
        ExtensionEvent::ResourcesDiscover { reason } => {
            vec![("reason", Dynamic::from(discover_reason(*reason)))]
        }
        ExtensionEvent::Input(e) => vec![("text", Dynamic::from(e.text.clone()))],
        ExtensionEvent::AssistantStream(e) => {
            vec![("text", Dynamic::from(e.text.clone()))]
        }
        ExtensionEvent::BeforeAgentStart(e) => vec![
            (
                "system_prompt",
                e.system_prompt
                    .clone()
                    .map(Dynamic::from)
                    .unwrap_or(Dynamic::UNIT),
            ),
            (
                "inject_message",
                e.inject_message
                    .clone()
                    .map(Dynamic::from)
                    .unwrap_or(Dynamic::UNIT),
            ),
        ],
        ExtensionEvent::AgentStart => vec![],
        ExtensionEvent::TurnStart { turn_id } => {
            vec![("turn_id", Dynamic::from(turn_id.clone()))]
        }
        ExtensionEvent::BeforeProviderHeaders => vec![],
        ExtensionEvent::BeforeProviderRequest(e) => {
            vec![("messages", json_to_dynamic(&e.messages))]
        }
        ExtensionEvent::AfterProviderResponse(e) => {
            vec![("response", json_to_dynamic(&e.response))]
        }
        ExtensionEvent::ToolExecutionStart => vec![],
        ExtensionEvent::ToolCall(e) => vec![
            ("id", Dynamic::from(e.id.clone())),
            ("name", Dynamic::from(e.name.clone())),
            ("input", json_to_dynamic(&e.input)),
        ],
        ExtensionEvent::ToolExecutionUpdate(e) => vec![
            ("id", Dynamic::from(e.id.clone())),
            ("name", Dynamic::from(e.name.clone())),
            ("message", Dynamic::from(e.message.clone())),
        ],
        ExtensionEvent::ToolResult(e) => {
            // Flatten `Result<ToolResult, ToolError>` → `#{content, success, is_error}`.
            let (content, success, is_error) = match &e.result {
                Ok(r) => (r.content.clone(), r.success, false),
                Err(err) => (err.to_string(), false, true),
            };
            vec![
                ("content", Dynamic::from(content)),
                ("success", Dynamic::from(success)),
                ("is_error", Dynamic::from(is_error)),
                ("id", Dynamic::from(e.id.clone())),
                ("name", Dynamic::from(e.name.clone())),
            ]
        }
        ExtensionEvent::ToolExecutionEnd => vec![],
        ExtensionEvent::TurnEnd { turn_id, reason } => vec![
            ("turn_id", Dynamic::from(turn_id.clone())),
            ("reason", Dynamic::from(turn_end_reason(*reason))),
        ],
        ExtensionEvent::AgentEnd => vec![],
        ExtensionEvent::AgentSettled => vec![],
        ExtensionEvent::SessionBeforeSwitch => vec![],
        ExtensionEvent::SessionBeforeFork => vec![],
        ExtensionEvent::SessionShutdown => vec![],
        ExtensionEvent::SessionBeforeCompact => vec![],
        ExtensionEvent::SessionCompact => vec![],
        ExtensionEvent::ToolsChange { added, removed } => vec![
            (
                "added",
                Dynamic::from(added.iter().cloned().map(Dynamic::from).collect::<Vec<_>>()),
            ),
            (
                "removed",
                Dynamic::from(
                    removed
                        .iter()
                        .cloned()
                        .map(Dynamic::from)
                        .collect::<Vec<_>>(),
                ),
            ),
        ],
        // `ExtensionEvent` is `#[non_exhaustive]` — wildcard required; the
        // kind round-trip test keeps the explicit arms in lockstep.
        _ => vec![],
    };
    entries.insert(0, ("kind", Dynamic::from(kind)));
    dynamic_map(entries)
}

// === Control-value markers ==================================================

/// Reserved marker key. Scripts never read these maps — they only return
/// them — so the `$` prefix just keeps the key out of the author's namespace
/// conventions.
pub(crate) const CONTROL_KEY: &str = "$control";

fn control_value(control: &str, value: Dynamic) -> Dynamic {
    let mut m = rhai::Map::new();
    m.insert(CONTROL_KEY.into(), Dynamic::from(control.to_string()));
    m.insert("value".into(), value);
    Dynamic::from(m)
}

/// Extract `(control, value)` when `ret` is a control marker.
pub(crate) fn split_control(ret: &Dynamic) -> Option<(String, Dynamic)> {
    let map = ret.clone().try_cast::<rhai::Map>()?;
    let control = map
        .get(CONTROL_KEY)
        .and_then(|d| d.clone().try_cast::<String>())?;
    let value = map.get("value").cloned().unwrap_or(Dynamic::UNIT);
    Some((control, value))
}

// === Transform merging ======================================================

fn map_field_string(map: &rhai::Map, key: &str) -> Option<String> {
    map.get(key).and_then(|d| d.clone().try_cast::<String>())
}

fn map_field_bool(map: &rhai::Map, key: &str) -> Option<bool> {
    map.get(key).and_then(|d| d.clone().try_cast::<bool>())
}

/// Merge a `transform(#{...})` field map into the running event (plan §三.3:
/// "按事件种类合并可变字段后继续链"). Only the four transform-capable kinds
/// have actionable fields; a transform at any other seam (or with no
/// recognized fields) degrades to `Continue`, mirroring the runner's
/// "越权 seam 忽略" semantics.
pub(crate) fn merge_transform(
    event: &ExtensionEvent,
    fields: &Dynamic,
) -> codesmith_agent::extension::HandlerOutcome {
    use codesmith_agent::extension::HandlerOutcome;
    let Some(map) = fields.clone().try_cast::<rhai::Map>() else {
        tracing::warn!(target: "codesmith_mods", "transform() argument must be an object map");
        return HandlerOutcome::Continue;
    };
    match event {
        ExtensionEvent::Input(_) => match map_field_string(&map, "text") {
            Some(text) => HandlerOutcome::Transform(ExtensionEvent::Input(InputEvent { text })),
            None => {
                tracing::warn!(target: "codesmith_mods", "input transform requires a string `text` field");
                HandlerOutcome::Continue
            }
        },
        ExtensionEvent::BeforeAgentStart(cur) => {
            let mut next = AgentStartEvent {
                system_prompt: cur.system_prompt.clone(),
                inject_message: cur.inject_message.clone(),
            };
            let mut changed = false;
            if let Some(d) = map.get("system_prompt") {
                // string → Some(value), unit → None (clear), absent → keep.
                next.system_prompt = d.clone().try_cast::<String>();
                changed = true;
            }
            if let Some(d) = map.get("inject_message") {
                next.inject_message = d.clone().try_cast::<String>();
                changed = true;
            }
            if changed {
                HandlerOutcome::Transform(ExtensionEvent::BeforeAgentStart(next))
            } else {
                tracing::warn!(target: "codesmith_mods", "before-agent-start transform recognized no fields (need system_prompt / inject_message)");
                HandlerOutcome::Continue
            }
        }
        ExtensionEvent::BeforeProviderRequest(_) => {
            if let Some(d) = map.get("messages") {
                match dynamic_to_json(d) {
                    Ok(messages) => {
                        HandlerOutcome::Transform(ExtensionEvent::BeforeProviderRequest(
                            codesmith_agent::extension::BeforeProviderRequestEvent { messages },
                        ))
                    }
                    Err(e) => {
                        tracing::warn!(target: "codesmith_mods", "provider-request transform: {e}");
                        HandlerOutcome::Continue
                    }
                }
            } else {
                tracing::warn!(target: "codesmith_mods", "before-provider-request transform requires a `messages` field");
                HandlerOutcome::Continue
            }
        }
        ExtensionEvent::ToolResult(cur) => {
            let (mut content, mut success) = match &cur.result {
                Ok(r) => (r.content.clone(), r.success),
                Err(err) => (err.to_string(), false),
            };
            let mut changed = false;
            if let Some(c) = map_field_string(&map, "content") {
                content = c;
                changed = true;
            }
            if let Some(s) = map_field_bool(&map, "success") {
                success = s;
                changed = true;
            }
            if !changed {
                tracing::warn!(target: "codesmith_mods", "tool-result transform recognized no fields (need content / success)");
                return HandlerOutcome::Continue;
            }
            let is_error = map_field_bool(&map, "is_error").unwrap_or(!success);
            let result = if is_error {
                Err(ToolError::execution_failed(content))
            } else {
                Ok(ToolResult {
                    content,
                    success,
                    metadata: None,
                })
            };
            HandlerOutcome::Transform(ExtensionEvent::ToolResult(ToolResultEvent {
                id: cur.id.clone(),
                name: cur.name.clone(),
                result,
            }))
        }
        ExtensionEvent::ToolCall(cur) => {
            // Route B (pipeline refinement): the pre-execute rewrite —
            // `transform(#{ input: {...} })` replaces the call's input; the
            // host applies it before approval + execution.
            if let Some(d) = map.get("input") {
                match dynamic_to_json(d) {
                    Ok(input) => {
                        HandlerOutcome::Transform(ExtensionEvent::ToolCall(ToolCallEvent {
                            id: cur.id.clone(),
                            name: cur.name.clone(),
                            input,
                        }))
                    }
                    Err(e) => {
                        tracing::warn!(target: "codesmith_mods", "tool-call transform: {e}");
                        HandlerOutcome::Continue
                    }
                }
            } else {
                tracing::warn!(
                    target: "codesmith_mods",
                    "tool-call transform requires an `input` field"
                );
                HandlerOutcome::Continue
            }
        }
        // Observe-only / non-transformable seams: ignore the transform.
        _ => HandlerOutcome::Continue,
    }
}

// === Engine construction + natives ==========================================

fn script_error(msg: impl Into<String>) -> Box<rhai::EvalAltResult> {
    rhai::EvalAltResult::ErrorRuntime(Dynamic::from(msg.into()), rhai::Position::NONE).into()
}

/// Tool names must survive the host registry's fail-closed chokepoint
/// (`^[a-zA-Z0-9_-]{1,64}$`) — reject at capture so a broken mod fails its
/// load instead of silently swapping its tool for a `FailClosedTool`.
fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Register the mod-facing native functions on `engine`, capturing
/// registrations into `cell` and baking `kv` in for `mod_state_*`. The
/// message-projection hub (with the mod's own id) is baked into
/// `projection_state` so reads are mod-namespaced without a ctx round-trip.
fn register_natives(
    engine: &mut rhai::Engine,
    kv: &ModKvStore,
    cell: &RegistrationCell,
    mod_id: &str,
    hub: codesmith_agent::extension::MessageProjectionHubArc,
) {
    // on(event, callback)
    let cell_on = Arc::clone(cell);
    engine.register_fn(
        "on",
        move |event: &str, callback: rhai::FnPtr| -> Result<(), Box<rhai::EvalAltResult>> {
            match event_kind_from_name(event) {
                Some(kind) => {
                    cell_on
                        .lock()
                        .expect("mod registration cell poisoned")
                        .push(ScriptRegistration::Handler { kind, callback });
                    Ok(())
                }
                None => Err(script_error(format!(
                    "on(): unknown event name '{event}' (see docs/MODS.md for the 24 event names)"
                ))),
            }
        },
    );

    // register_tool(spec_map, callback)
    let cell_tool = Arc::clone(cell);
    engine.register_fn(
        "register_tool",
        move |spec: rhai::Map, callback: rhai::FnPtr| -> Result<(), Box<rhai::EvalAltResult>> {
            let name = spec
                .get("name")
                .and_then(|d| d.clone().try_cast::<String>())
                .ok_or_else(|| {
                    script_error("register_tool(): spec must include a string `name`")
                })?;
            if !valid_tool_name(&name) {
                return Err(script_error(format!(
                    "register_tool(): tool name {name:?} must match [a-zA-Z0-9_-] and be 1-64 chars"
                )));
            }
            let description = spec
                .get("description")
                .and_then(|d| d.clone().try_cast::<String>())
                .ok_or_else(|| {
                    script_error("register_tool(): spec must include a string `description`")
                })?;
            let schema = match spec.get("schema") {
                Some(d) => dynamic_to_json(d).map_err(script_error)?,
                None => serde_json::json!({"type": "object"}),
            };
            cell_tool
                .lock()
                .expect("mod registration cell poisoned")
                .push(ScriptRegistration::Tool {
                    name,
                    description,
                    schema,
                    callback,
                });
            Ok(())
        },
    );

    // register_command(name, description, callback)
    let cell_cmd = Arc::clone(cell);
    engine.register_fn(
        "register_command",
        move |name: &str, description: &str, callback: rhai::FnPtr| -> Result<(), Box<rhai::EvalAltResult>> {
            if !valid_tool_name(name) {
                return Err(script_error(format!(
                    "register_command(): command name {name:?} must match [a-zA-Z0-9_-] and be 1-64 chars"
                )));
            }
            cell_cmd
                .lock()
                .expect("mod registration cell poisoned")
                .push(ScriptRegistration::Command {
                    name: name.to_string(),
                    description: description.to_string(),
                    callback,
                });
            Ok(())
        },
    );

    // register_prompt_section(id, text) — route B: append a named,
    // append-only section to the base system prompt. Sections register at
    // mod load and are session-stable (prefix-cache discipline); an
    // explicit before-agent-start whole-prompt replacement still wins.
    // Validated here so a bad section fails the mod's load.
    let cell_section = Arc::clone(cell);
    engine.register_fn(
        "register_prompt_section",
        move |id: &str, text: String| -> Result<(), Box<rhai::EvalAltResult>> {
            if let Err(e) = crate::runner::validate_prompt_section(id, &text) {
                return Err(script_error(e.to_string()));
            }
            cell_section
                .lock()
                .expect("mod registration cell poisoned")
                .push(ScriptRegistration::PromptSection {
                    id: id.to_string(),
                    text,
                });
            Ok(())
        },
    );

    // register_provider(spec_map) — route A: declarative provider alias.
    // `#{ id: "my-gw", kind: "openai", base_url: "...", default_model: "...",
    //    headers: #{ "X-Gateway": "acme" } }`.
    // Script mods cannot implement `LlmClient` (no async/net by design), so
    // a mod's provider is an alias onto a builtin factory with config
    // overrides. Validation fails loud at capture: unknown `kind`, an `id`
    // that shadows a builtin, or non-string header values fail the mod's
    // load instead of surfacing at the first client build. `kind` must name
    // a builtin (aliasing another mod's custom provider is not supported in
    // v1); `api_key` is deliberately not accepted — secrets live in config,
    // not scripts.
    let cell_prov = Arc::clone(cell);
    engine.register_fn(
        "register_provider",
        move |spec: rhai::Map| -> Result<(), Box<rhai::EvalAltResult>> {
            let id = spec
                .get("id")
                .and_then(|d| d.clone().try_cast::<String>())
                .ok_or_else(|| {
                    script_error("register_provider(): spec must include a string `id`")
                })?;
            if !valid_tool_name(&id) {
                return Err(script_error(format!(
                    "register_provider(): provider id {id:?} must match [a-zA-Z0-9_-] and be 1-64 chars"
                )));
            }
            if matches!(ProviderId::from(id.as_str()), ProviderId::Builtin(_)) {
                return Err(script_error(format!(
                    "register_provider(): id {id:?} shadows a builtin provider kind; pick another id"
                )));
            }
            let kind = spec
                .get("kind")
                .and_then(|d| d.clone().try_cast::<String>())
                .ok_or_else(|| {
                    script_error(
                        "register_provider(): spec must include a string `kind` naming the \
                         builtin provider to alias (e.g. \"openai\")",
                    )
                })?;
            let target = ProviderId::from(kind.as_str());
            if !matches!(target, ProviderId::Builtin(_)) {
                return Err(script_error(format!(
                    "register_provider(): kind {kind:?} is not a builtin provider (aliases must \
                     target a builtin; see docs/MODS.md)"
                )));
            }
            let base_url = spec
                .get("base_url")
                .and_then(|d| d.clone().try_cast::<String>());
            let default_model = spec
                .get("default_model")
                .and_then(|d| d.clone().try_cast::<String>());
            let http_headers = match spec.get("headers").and_then(|d| d.clone().try_cast::<rhai::Map>())
            {
                None => None,
                Some(map) => {
                    let mut headers = std::collections::HashMap::new();
                    for (key, value) in map {
                        let value = value.try_cast::<String>().ok_or_else(|| {
                            script_error(format!(
                                "register_provider(): headers[{:?}] must be a string",
                                key
                            ))
                        })?;
                        headers.insert(key.to_string(), value);
                    }
                    Some(headers)
                }
            };
            cell_prov
                .lock()
                .expect("mod registration cell poisoned")
                .push(ScriptRegistration::Provider {
                    id,
                    target,
                    base_url,
                    default_model,
                    http_headers,
                });
            Ok(())
        },
    );

    // mod_state_get / mod_state_set — per-mod persistent KV.
    let kv_get = kv.clone();
    engine.register_fn("mod_state_get", move |key: &str| -> Dynamic {
        match kv_get.get(key) {
            Some(v) => json_to_dynamic(&v),
            None => Dynamic::UNIT, // Rhai `??` default operator applies to unit
        }
    });
    let kv_set = kv.clone();
    engine.register_fn(
        "mod_state_set",
        move |key: &str, value: Dynamic| -> Result<(), Box<rhai::EvalAltResult>> {
            let json = dynamic_to_json(&value).map_err(script_error)?;
            kv_set.set(key, json);
            Ok(())
        },
    );

    // mod_log / now_ms
    engine.register_fn("mod_log", |msg: &str| {
        tracing::info!(target: "codesmith_mods", "{msg}");
    });
    engine.register_fn("now_ms", || -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    });

    // register_message_projection(key, init, fold) — a session-log fold the
    // host maintains: every transcript mutation folds through `fold(state,
    // message)`, and the state is rebuilt from the log on session reload
    // (event-sourcing projection). `state`/`init` are plain values (JSON
    // round-trip), `message` is the wire-format message map. A duplicate key
    // within this mod fails the load (pick a new key — states are not
    // migratable); a fold error at runtime drops the projection (logged),
    // never the turn.
    let cell_proj = Arc::clone(cell);
    engine.register_fn(
        "register_message_projection",
        move |key: &str,
              init: Dynamic,
              fold: rhai::FnPtr|
              -> Result<(), Box<rhai::EvalAltResult>> {
            if key.is_empty()
                || key.len() > 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(script_error(format!(
                    "register_message_projection(): key {key:?} must match [a-zA-Z0-9_-] and be 1-64 chars"
                )));
            }
            let init = dynamic_to_json(&init).map_err(script_error)?;
            let mut cell = cell_proj.lock().expect("mod registration cell poisoned");
            if cell.iter().any(|reg| {
                matches!(reg, ScriptRegistration::MessageProjection { key: k, .. } if k == key)
            }) {
                return Err(script_error(format!(
                    "register_message_projection(): key {key:?} already registered in this mod — pick a new key"
                )));
            }
            cell.push(ScriptRegistration::MessageProjection {
                key: key.to_string(),
                init,
                fold,
            });
            Ok(())
        },
    );

    // projection_state(key) — read this mod's projection state (the value
    // as of the last fold). Missing key is a script error (fail loud: read
    // what you registered).
    let hub_read = Arc::clone(&hub);
    let reader_mod = mod_id.to_string();
    engine.register_fn(
        "projection_state",
        move |key: &str| -> Result<Dynamic, Box<rhai::EvalAltResult>> {
            hub_read
                .state(&reader_mod, key)
                .map(|v| json_to_dynamic(&v))
                .ok_or_else(|| {
                    script_error(format!(
                        "projection_state(): no projection {key:?} registered by this mod"
                    ))
                })
        },
    );

    // Control-value constructors.
    engine.register_fn("proceed", || control_value("proceed", Dynamic::UNIT));
    engine.register_fn("block", |reason: &str| {
        control_value("block", Dynamic::from(reason.to_string()))
    });
    engine.register_fn("cancel", |reason: &str| {
        control_value("cancel", Dynamic::from(reason.to_string()))
    });
    engine.register_fn("transform", |fields: rhai::Map| {
        control_value("transform", Dynamic::from(fields))
    });
    engine.register_fn("ok", |value: Dynamic| control_value("ok", value));
    engine.register_fn("err", |msg: &str| {
        control_value("err", Dynamic::from(msg.to_string()))
    });
    engine.register_fn("message", |msg: &str| {
        control_value("message", Dynamic::from(msg.to_string()))
    });
    engine.register_fn("send", |msg: &str| {
        control_value("send", Dynamic::from(msg.to_string()))
    });
}

// === RhaiMod ================================================================

/// A script mod: implements the same `Extension` contract as compiled-in /
/// dylib extensions, so `ExtensionRunner::load(&rhai_mod)` needs zero runner
/// changes.
pub struct RhaiMod {
    metadata: ExtensionMetadata,
    pub(crate) runtime: Arc<ScriptRuntime>,
    pub(crate) registrations: Vec<ScriptRegistration>,
}

impl RhaiMod {
    /// Compile + run the mod's entry script once, capturing registrations.
    /// `kv` is the mod's persistent store (baked into the engine natives);
    /// `hub` is the host's message-projection hub (baked into
    /// `projection_state` reads — same `Arc` the engine folds through).
    pub fn load(
        discovered: &DiscoveredMod,
        kv: ModKvStore,
        hub: codesmith_agent::extension::MessageProjectionHubArc,
    ) -> Result<Self, ExtensionError> {
        let source = std::fs::read_to_string(&discovered.entry_path).map_err(|e| {
            ExtensionError::Load(format!(
                "read mod entry {}: {e}",
                discovered.entry_path.display()
            ))
        })?;

        let mut engine = rhai::Engine::new();
        engine
            .set_max_operations(MAX_OPERATIONS)
            .set_max_call_levels(MAX_CALL_LEVELS)
            .set_max_string_size(MAX_STRING_SIZE)
            .set_max_array_size(MAX_ARRAY_SIZE)
            .set_max_map_size(MAX_MAP_SIZE);
        // No file-module resolver is installed, and no fs/net/process
        // natives are registered: absence is the sandbox (plan §三.4).

        let cell: RegistrationCell = Arc::new(Mutex::new(Vec::new()));
        register_natives(&mut engine, &kv, &cell, &discovered.id, hub);

        let ast = engine
            .compile(&source)
            .map_err(|e| ExtensionError::Load(format!("compile {}: {e}", discovered.id)))?;
        engine
            .run_ast(&ast)
            .map_err(|e| ExtensionError::Load(format!("run {}: {e}", discovered.id)))?;

        let registrations =
            std::mem::take(&mut *cell.lock().expect("mod registration cell poisoned"));
        Ok(Self {
            metadata: ExtensionMetadata::from_strings(
                discovered.id.clone(),
                discovered.name.clone(),
                discovered.version.clone(),
            ),
            runtime: Arc::new(ScriptRuntime {
                engine: Arc::new(engine),
                ast: Arc::new(ast),
            }),
            registrations,
        })
    }

    /// The captured registrations (for introspection / tests).
    pub fn registrations(&self) -> &[ScriptRegistration] {
        &self.registrations
    }
}

/// Manual `Debug` (the struct holds an `Engine`/`AST` without `Debug`);
/// mirrors `ExtensionRunner`'s manual-`Debug` convention.
impl std::fmt::Debug for RhaiMod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhaiMod")
            .field("id", &self.metadata.id)
            .field("version", &self.metadata.version)
            .field("registrations", &self.registrations.len())
            .finish()
    }
}

#[async_trait]
impl Extension for RhaiMod {
    fn metadata(&self) -> &ExtensionMetadata {
        &self.metadata
    }

    /// Replay the captured registrations against the runner's api — the
    /// exact same surface a Rust extension authors against.
    async fn configure(&self, api: &dyn ExtensionApi) -> Result<(), ExtensionError> {
        for reg in &self.registrations {
            match reg {
                ScriptRegistration::Handler { kind, callback } => {
                    api.on_variant(
                        *kind,
                        Arc::new(super::adapters::ScriptHandler {
                            mod_id: self.metadata.id.to_string(),
                            runtime: Arc::clone(&self.runtime),
                            callback: callback.clone(),
                        }),
                    )?;
                }
                ScriptRegistration::Tool {
                    name,
                    description,
                    schema,
                    callback,
                } => {
                    api.register_tool(Box::new(super::adapters::ScriptToolDefinition {
                        mod_id: self.metadata.id.to_string(),
                        runtime: Arc::clone(&self.runtime),
                        name: name.clone(),
                        description: description.clone(),
                        schema: schema.clone(),
                        callback: callback.clone(),
                    }))?;
                }
                ScriptRegistration::Command {
                    name,
                    description,
                    callback,
                } => {
                    api.register_command(Box::new(super::adapters::ScriptCommandDefinition {
                        mod_id: self.metadata.id.to_string(),
                        runtime: Arc::clone(&self.runtime),
                        name: name.clone(),
                        description: description.clone(),
                        callback: callback.clone(),
                    }))?;
                }
                ScriptRegistration::PromptSection { id, text } => {
                    api.register_prompt_section(id.clone(), text.clone())?;
                }
                ScriptRegistration::Provider {
                    id,
                    target,
                    base_url,
                    default_model,
                    http_headers,
                } => {
                    api.register_provider_alias(codesmith_agent::provider::ProviderAlias {
                        id: id.clone(),
                        target: target.clone(),
                        base_url: base_url.clone(),
                        default_model: default_model.clone(),
                        http_headers: http_headers.clone(),
                    })?;
                }
                ScriptRegistration::MessageProjection { key, init, fold } => {
                    let runtime = Arc::clone(&self.runtime);
                    let callback = fold.clone();
                    let fold_fn: codesmith_agent::extension::MessageFoldFn =
                        Arc::new(move |state, message| {
                            let st = json_to_dynamic(&state);
                            let msg = serde_json::to_value(message)
                                .map(|v| json_to_dynamic(&v))
                                .map_err(|e| e.to_string())?;
                            let out = callback
                                .call::<Dynamic>(&runtime.engine, &runtime.ast, (st, msg))
                                .map_err(|e| e.to_string())?;
                            dynamic_to_json(&out)
                        });
                    api.register_message_projection(
                        self.metadata.id.to_string(),
                        key.clone(),
                        init.clone(),
                        fold_fn,
                    )?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use codesmith_agent::extension::ExtensionApi as _;
    use tempfile::TempDir;

    /// Build a `DiscoveredMod` for an inline script (manifest fields inline).
    fn discovered_for(dir: &TempDir, id: &str, script: &str) -> DiscoveredMod {
        let mod_dir = dir.path().join(id);
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("mod.rhai"), script).unwrap();
        DiscoveredMod {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".to_string(),
            description: None,
            entry_path: mod_dir.join("mod.rhai"),
            dir: mod_dir,
            global: true,
        }
    }

    fn kv_for(dir: &TempDir, id: &str) -> ModKvStore {
        ModKvStore::new(dir.path().join(format!("global-{id}.json")))
    }

    /// Fresh message-projection hub for `RhaiMod::load` test calls.
    fn hub() -> codesmith_agent::extension::MessageProjectionHubArc {
        codesmith_agent::extension::MessageProjectionHubArc::new(
            codesmith_agent::extension::MessageProjectionHub::new(),
        )
    }

    #[test]
    fn load_captures_handler_tool_and_command_registrations() {
        let dir = TempDir::new().unwrap();
        let script = r#"
            on("tool-call", |e| { proceed() });
            on("input", |e, ctx| { proceed() });
            register_tool(#{
                name: "team_ci_status",
                description: "查询团队 CI 状态",
                schema: #{ type: "object", properties: #{}, additionalProperties: false },
            }, |input, ctx| { ok("green") });
            register_command("ci", "显示 CI 状态", |args, ctx| { message("green") });
        "#;
        let m = RhaiMod::load(
            &discovered_for(&dir, "all-kinds", script),
            kv_for(&dir, "all-kinds"),
            hub(),
        )
        .expect("load");
        assert_eq!(m.metadata.id, "all-kinds");
        assert_eq!(m.metadata.version, "0.1.0");
        assert_eq!(
            m.registrations.len(),
            4,
            "{:?}",
            m.registrations
                .iter()
                .map(|r| match r {
                    ScriptRegistration::Handler { kind, .. } => format!("h:{:?}", kind),
                    ScriptRegistration::Tool { name, .. } => format!("t:{name}"),
                    ScriptRegistration::Command { name, .. } => format!("c:{name}"),
                    ScriptRegistration::Provider { id, .. } => format!("p:{id}"),
                    ScriptRegistration::PromptSection { id, .. } => format!("s:{id}"),
                    ScriptRegistration::MessageProjection { key, .. } => format!("m:{key}"),
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn load_rejects_unknown_event_name() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "bad-event",
                r#"on("no-such-event", |e| { proceed() });"#,
            ),
            kv_for(&dir, "bad-event"),
            hub(),
        );
        assert!(matches!(m, Err(ExtensionError::Load(_))), "got {m:?}");
        let msg = m.unwrap_err().to_string();
        assert!(msg.contains("no-such-event"), "{msg}");
    }

    #[test]
    fn load_rejects_invalid_tool_name() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "bad-tool",
                r#"register_tool(#{name: "bad name!", description: "x"}, |i| ok(1));"#,
            ),
            kv_for(&dir, "bad-tool"),
            hub(),
        );
        assert!(matches!(m, Err(ExtensionError::Load(_))), "got {m:?}");
    }

    // === Route A — register_provider(spec) ================================

    #[test]
    fn load_captures_provider_registration() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "gw-alias",
                r#"
                    register_provider(#{
                        id: "acme-gw",
                        kind: "openai",
                        base_url: "https://gw.example.test/v1",
                        default_model: "acme-large",
                        headers: #{ "X-Gateway": "acme" },
                    });
                "#,
            ),
            kv_for(&dir, "gw-alias"),
            hub(),
        )
        .expect("load");
        let (id, target, base_url, default_model, http_headers) = match m.registrations().first() {
            Some(ScriptRegistration::Provider {
                id,
                target,
                base_url,
                default_model,
                http_headers,
            }) => (
                id.clone(),
                target.clone(),
                base_url.clone(),
                default_model.clone(),
                http_headers.clone(),
            ),
            _ => panic!("expected a Provider registration"),
        };
        assert_eq!(id, "acme-gw");
        assert!(matches!(target, ProviderId::Builtin(_)));
        assert_eq!(base_url.as_deref(), Some("https://gw.example.test/v1"));
        assert_eq!(default_model.as_deref(), Some("acme-large"));
        assert_eq!(
            http_headers.expect("headers captured").get("X-Gateway"),
            Some(&"acme".to_string())
        );
    }

    #[test]
    fn register_provider_rejects_unknown_kind() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "bad-kind",
                r#"register_provider(#{id: "x", kind: "nope"});"#,
            ),
            kv_for(&dir, "bad-kind"),
            hub(),
        );
        let msg = m.unwrap_err().to_string();
        assert!(msg.contains("not a builtin provider"), "{msg}");
    }

    #[test]
    fn register_provider_rejects_builtin_shadowing_id() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "shadow",
                r#"register_provider(#{id: "deepseek", kind: "openai"});"#,
            ),
            kv_for(&dir, "shadow"),
            hub(),
        );
        let msg = m.unwrap_err().to_string();
        assert!(msg.contains("shadows a builtin provider kind"), "{msg}");
    }

    #[test]
    fn load_captures_prompt_section() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "sec-mod",
                r#"register_prompt_section("style", "Be terse.");"#,
            ),
            kv_for(&dir, "sec-mod"),
            hub(),
        )
        .expect("load");
        assert!(matches!(
            m.registrations().first(),
            Some(ScriptRegistration::PromptSection { id, text })
                if id == "style" && text == "Be terse."
        ));
    }

    #[test]
    fn prompt_section_bad_id_fails_load() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "bad-sec",
                r#"register_prompt_section("bad id!", "x");"#,
            ),
            kv_for(&dir, "bad-sec"),
            hub(),
        );
        let msg = m.unwrap_err().to_string();
        assert!(msg.contains("must match"), "{msg}");
    }

    #[test]
    fn register_provider_rejects_missing_fields() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(&dir, "no-id", r#"register_provider(#{kind: "openai"});"#),
            kv_for(&dir, "no-id"),
            hub(),
        );
        let msg = m.unwrap_err().to_string();
        assert!(msg.contains("must include a string `id`"), "{msg}");
    }

    /// Route A end-to-end: script alias → runner load → `bind_core` flush →
    /// `shared_providers().build` resolves the alias and the target
    /// factory sees the overridden `default_model`.
    #[test]
    fn script_provider_alias_builds_through_shared_registry() {
        use codesmith_agent::llm_client::LlmClient;
        use codesmith_agent::llm_client::{LlmClientHandle, RetryConfig};
        use codesmith_agent::models::MessageRequest;
        use codesmith_agent::provider::{ProviderConfig, ProviderFactory, ProviderId as Pid};

        struct EchoClient {
            model: String,
        }
        impl LlmClient for EchoClient {
            fn provider_name(&self) -> &'static str {
                "echo"
            }
            fn model(&self) -> &str {
                &self.model
            }
            fn create_message(
                &self,
                _r: MessageRequest,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = anyhow::Result<codesmith_agent::models::MessageResponse>,
                        > + Send
                        + '_,
                >,
            > {
                Box::pin(async { Err(anyhow::anyhow!("echo mock")) })
            }
            fn create_message_stream(
                &self,
                _r: MessageRequest,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = anyhow::Result<codesmith_agent::llm_client::StreamEventBox>,
                        > + Send
                        + '_,
                >,
            > {
                Box::pin(async { Err(anyhow::anyhow!("echo mock")) })
            }
        }
        struct EchoFactory;
        impl ProviderFactory for EchoFactory {
            fn id(&self) -> Pid {
                Pid::from("openai")
            }
            fn build(&self, cfg: &ProviderConfig) -> anyhow::Result<LlmClientHandle> {
                Ok(Arc::new(EchoClient {
                    model: cfg.default_model.clone(),
                }))
            }
        }

        struct TestCmdCtx;
        #[async_trait]
        impl codesmith_agent::extension::ExtensionContext for TestCmdCtx {
            fn cwd(&self) -> &std::path::Path {
                std::path::Path::new(".")
            }
            fn mode(&self) -> codesmith_agent::extension::ExtensionMode {
                codesmith_agent::extension::ExtensionMode::Tui
            }
            fn is_idle(&self) -> bool {
                true
            }
            fn signal(&self) -> tokio_util::sync::CancellationToken {
                tokio_util::sync::CancellationToken::new()
            }
            fn generation(&self) -> u64 {
                1
            }
        }
        impl codesmith_agent::extension::ExtensionCommandContext for TestCmdCtx {}

        let dir = TempDir::new().unwrap();
        let rhai_mod = RhaiMod::load(
            &discovered_for(
                &dir,
                "e2e-gw",
                r#"
                    register_provider(#{
                        id: "acme-gw",
                        kind: "openai",
                        default_model: "acme-large",
                    });
                "#,
            ),
            kv_for(&dir, "e2e-gw"),
            hub(),
        )
        .expect("load mod");

        let runner = crate::ExtensionRunner::new();
        let shared = runner.shared_providers();
        // Seed the builtin target; hold the guard (drop = unregister).
        let _target = shared.register(Arc::new(EchoFactory));
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&rhai_mod)).expect("configure");
        runner.bind_core(Arc::new(TestCmdCtx));

        let client = shared
            .build(&ProviderConfig {
                provider: Pid::from("acme-gw"),
                api_key: "k".into(),
                base_url: "https://example.test/v1".into(),
                default_model: "unused".into(),
                retry: RetryConfig::disabled(),
                http_headers: std::collections::HashMap::new(),
                on_retry: None,
            })
            .expect("alias builds through the target factory");
        assert_eq!(
            client.model(),
            "acme-large",
            "default_model override applied"
        );
    }

    #[test]
    fn load_missing_entry_is_load_error() {
        let dir = TempDir::new().unwrap();
        let d = discovered_for(&dir, "gone", "// never written");
        std::fs::remove_file(d.entry_path.clone()).unwrap();
        let m = RhaiMod::load(&d, kv_for(&dir, "gone"), hub());
        assert!(matches!(m, Err(ExtensionError::Load(_))), "got {m:?}");
        // Entry path must have been the default mod.rhai.
        d.entry_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
    }

    #[test]
    fn infinite_loop_hits_operation_cap() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(&dir, "looper", "let x = 0; while true { x += 1; }"),
            kv_for(&dir, "looper"),
            hub(),
        );
        assert!(
            matches!(m, Err(ExtensionError::Load(ref e)) if e.contains("operations") || e.contains("Terminated")),
            "expected operation-cap failure, got {m:?}"
        );
    }

    #[test]
    fn compile_error_is_load_error() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(&dir, "broken", "fn ( {"),
            kv_for(&dir, "broken"),
            hub(),
        );
        assert!(matches!(m, Err(ExtensionError::Load(_))), "got {m:?}");
    }

    #[test]
    fn kv_natives_round_trip_through_script() {
        let dir = TempDir::new().unwrap();
        // Top-level set runs at load; a tool reads it back at call time.
        let script = r#"
            mod_state_set("ci", "green");
            mod_state_set("count", 42);
            register_tool(#{name: "ci_read", description: "read ci"}, |i| {
                ok(mod_state_get("ci") ?? "unknown")
            });
        "#;
        let m = RhaiMod::load(
            &discovered_for(&dir, "kv-mod", script),
            kv_for(&dir, "kv-mod"),
            hub(),
        )
        .expect("load");
        assert_eq!(m.registrations.len(), 1);
    }

    #[test]
    fn missing_kv_read_is_unit_for_nullish_default() {
        // `?? "fallback"` must work when the key was never set: run a script
        // that stores the resolved default via mod_state_set, then assert on
        // the persisted file.
        let dir = TempDir::new().unwrap();
        let script = r#"
            let v = mod_state_get("never-set") ?? "fallback";
            mod_state_set("resolved", v);
        "#;
        let kv = kv_for(&dir, "nullish");
        RhaiMod::load(&discovered_for(&dir, "nullish", script), kv.clone(), hub()).expect("load");
        assert_eq!(kv.get("resolved"), Some(serde_json::json!("fallback")));
    }

    #[test]
    fn event_kind_name_round_trips_all_24() {
        let kinds = [
            ExtensionEventKind::ProjectTrust,
            ExtensionEventKind::SessionStart,
            ExtensionEventKind::ResourcesDiscover,
            ExtensionEventKind::Input,
            ExtensionEventKind::BeforeAgentStart,
            ExtensionEventKind::AgentStart,
            ExtensionEventKind::TurnStart,
            ExtensionEventKind::BeforeProviderHeaders,
            ExtensionEventKind::BeforeProviderRequest,
            ExtensionEventKind::AfterProviderResponse,
            ExtensionEventKind::ToolExecutionStart,
            ExtensionEventKind::ToolCall,
            ExtensionEventKind::ToolExecutionUpdate,
            ExtensionEventKind::ToolResult,
            ExtensionEventKind::ToolExecutionEnd,
            ExtensionEventKind::TurnEnd,
            ExtensionEventKind::AgentEnd,
            ExtensionEventKind::AgentSettled,
            ExtensionEventKind::SessionBeforeSwitch,
            ExtensionEventKind::SessionBeforeFork,
            ExtensionEventKind::SessionShutdown,
            ExtensionEventKind::SessionBeforeCompact,
            ExtensionEventKind::SessionCompact,
            ExtensionEventKind::ToolsChange,
        ];
        assert_eq!(kinds.len(), 24);
        for kind in kinds {
            let name = event_name_from_kind(kind);
            assert_eq!(event_kind_from_name(name), Some(kind), "round-trip {name}");
        }
        assert_eq!(event_kind_from_name("bogus"), None);
    }

    #[test]
    fn tools_change_payload_carries_diff_arrays() {
        let event = ExtensionEvent::ToolsChange {
            added: vec!["call_count".into()],
            removed: vec![],
        };
        let payload = event_to_dynamic(&event);
        let map = payload.try_cast::<rhai::Map>().expect("payload map");
        assert_eq!(
            map.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("tools-change".into())
        );
        let added = map
            .get("added")
            .and_then(|d| d.clone().try_cast::<Vec<Dynamic>>())
            .expect("added array");
        assert_eq!(
            added
                .iter()
                .map(|d| d.clone().try_cast::<String>())
                .collect::<Vec<_>>(),
            vec![Some("call_count".into())]
        );
        let removed = map
            .get("removed")
            .and_then(|d| d.clone().try_cast::<Vec<Dynamic>>())
            .expect("removed array");
        assert!(removed.is_empty());
    }

    #[test]
    fn merge_transform_tool_call_rewrites_input() {
        let event = ExtensionEvent::ToolCall(ToolCallEvent {
            id: "c1".into(),
            name: "exec_shell".into(),
            input: serde_json::json!({"command": "rm -rf /tmp"}),
        });
        let mut inner = rhai::Map::new();
        inner.insert(
            "command".into(),
            Dynamic::from("rm -rf /tmp --dry-run".to_string()),
        );
        let mut fields = rhai::Map::new();
        fields.insert("input".into(), Dynamic::from(inner));
        let out = merge_transform(&event, &Dynamic::from(fields));
        match out {
            codesmith_agent::extension::HandlerOutcome::Transform(ExtensionEvent::ToolCall(tc)) => {
                assert_eq!(tc.id, "c1");
                assert_eq!(tc.name, "exec_shell");
                assert_eq!(
                    tc.input,
                    serde_json::json!({"command": "rm -rf /tmp --dry-run"})
                );
            }
            other => panic!("expected a ToolCall transform, got {other:?}"),
        }
    }

    #[test]
    fn merge_transform_tool_call_without_input_warns_and_continues() {
        let event = ExtensionEvent::ToolCall(ToolCallEvent {
            id: "c1".into(),
            name: "exec_shell".into(),
            input: serde_json::json!({}),
        });
        let mut fields = rhai::Map::new();
        fields.insert("id".into(), Dynamic::from("nope".to_string()));
        let out = merge_transform(&event, &Dynamic::from(fields));
        assert!(matches!(
            out,
            codesmith_agent::extension::HandlerOutcome::Continue
        ));
    }

    #[test]
    fn merge_transform_tool_result_flattens_back() {
        let event = ExtensionEvent::ToolResult(ToolResultEvent {
            id: "c1".into(),
            name: "echo".into(),
            result: Ok(ToolResult::success("original")),
        });
        let mut fields = rhai::Map::new();
        fields.insert("content".into(), Dynamic::from("rewritten"));
        let out = merge_transform(&event, &Dynamic::from(fields));
        match out {
            codesmith_agent::extension::HandlerOutcome::Transform(ExtensionEvent::ToolResult(
                tr,
            )) => {
                let r = tr.result.expect("ok arm");
                assert_eq!(r.content, "rewritten");
                assert!(r.success);
            }
            other => panic!("expected Transform, got {other:?}"),
        }
    }

    #[test]
    fn merge_transform_ignores_non_transformable_kind() {
        let fields = rhai::Map::new();
        let out = merge_transform(&ExtensionEvent::SessionShutdown, &Dynamic::from(fields));
        assert!(matches!(
            out,
            codesmith_agent::extension::HandlerOutcome::Continue
        ));
    }

    #[test]
    fn message_projection_registers_folds_and_reads() {
        // Mirror the production wiring: the hub baked into the mod's natives
        // is the SAME instance the runner flushes into at bind_core.
        let runner = crate::ExtensionRunner::new();
        let hub = runner.message_projection_hub();
        let dir = TempDir::new().unwrap();
        let rhai_mod = RhaiMod::load(
            &discovered_for(
                &dir,
                "proj-mod",
                r#"
                    register_message_projection("counts", #{ users: 0 }, |state, m| {
                        if m.role == "user" { state.users = state.users + 1; }
                        state
                    });
                    on("session-shutdown", |e| {
                        let s = projection_state("counts");
                        mod_state_set("seen_users", s.users);
                    });
                "#,
            ),
            kv_for(&dir, "proj-mod"),
            hub.clone(),
        )
        .expect("load mod");
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(runner.load(&rhai_mod)).expect("configure");
        struct Ctx;
        #[async_trait]
        impl codesmith_agent::extension::ExtensionContext for Ctx {
            fn cwd(&self) -> &std::path::Path {
                std::path::Path::new(".")
            }
            fn mode(&self) -> codesmith_agent::extension::ExtensionMode {
                codesmith_agent::extension::ExtensionMode::Tui
            }
            fn is_idle(&self) -> bool {
                true
            }
            fn signal(&self) -> tokio_util::sync::CancellationToken {
                tokio_util::sync::CancellationToken::new()
            }
            fn generation(&self) -> u64 {
                1
            }
        }
        impl codesmith_agent::extension::ExtensionCommandContext for Ctx {}
        runner.bind_core(Arc::new(Ctx));

        let msg = |role: &str| codesmith_agent::models::Message {
            role: role.to_string(),
            content: vec![codesmith_agent::models::ContentBlock::Text {
                text: "x".into(),
                cache_control: None,
            }],
        };
        hub.fold_message(&msg("user"));
        hub.fold_message(&msg("assistant"));
        hub.fold_message(&msg("user"));
        assert_eq!(
            hub.state("proj-mod", "counts"),
            Some(serde_json::json!({ "users": 2 }))
        );

        // The mod reads its own projection through the baked-in native.
        let _ = rt.block_on(runner.emit(ExtensionEvent::SessionShutdown));
        assert_eq!(
            kv_for(&dir, "proj-mod").get("seen_users"),
            Some(serde_json::json!(2))
        );
    }

    #[test]
    fn message_projection_duplicate_key_fails_load() {
        let dir = TempDir::new().unwrap();
        let m = RhaiMod::load(
            &discovered_for(
                &dir,
                "dup-proj",
                r#"
                    register_message_projection("k", 0, |s, m| s);
                    register_message_projection("k", 0, |s, m| s);
                "#,
            ),
            kv_for(&dir, "dup-proj"),
            hub(),
        );
        assert!(matches!(m, Err(ExtensionError::Load(_))), "got {m:?}");
    }
}
