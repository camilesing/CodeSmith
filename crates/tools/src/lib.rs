//! # Shared tool value types
//!
//! This crate owns the leaf value vocabulary every tool surface in the
//! workspace shares: [`ToolResult`] / [`ToolError`] / [`ToolCapability`] /
//! [`ApprovalRequirement`], the JSON input helpers, the exec-policy request
//! shape ([`ToolCall`] over `codesmith_protocol::ToolPayload`), and the
//! per-workspace index seam ([`index_api`]).
//!
//! What it deliberately does **not** own: tool registration and dispatch.
//! Those live in two places — the framework contract tier
//! (`codesmith_agent::tools::Tool` + `ToolSet`) and the production host tier
//! (`codesmith_agent_runtime::tools::spec::ToolSpec` + its `ToolRegistry` /
//! `ToolDispatcher`), bridged per turn by `ToolSpecAdapter`. This crate used
//! to carry a third, RwLock-based registry (`ToolSpec` records +
//! `ToolHandler` + `ToolCallRuntime`); it was deleted in the §E surface
//! unification because nothing outside its own tests ever registered into it
//! — the one construction site (`codesmith-core`'s `Runtime`) built it empty,
//! so its dispatch path could only ever report `ToolNotFound`.

use std::path::PathBuf;

pub mod index_api;

use codesmith_protocol::ToolPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Capabilities that a tool may have or require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCapability {
    /// Tool only reads data, never modifies state.
    ReadOnly,
    /// Tool writes to the filesystem.
    WritesFiles,
    /// Tool executes arbitrary shell commands.
    ExecutesCode,
    /// Tool makes network requests.
    Network,
    /// Tool can be run in a sandbox.
    Sandboxable,
    /// Tool requires user approval before execution.
    RequiresApproval,
    /// Plan mode's execution gate lets this tool run even though it is not
    /// `ReadOnly`.
    ///
    /// For the tools that *are* the plan session — the plan file, the checklist,
    /// the goal and mode tools — plus the ones plan mode deliberately keeps
    /// (read-only sub-agent sessions, web lookups). A tool that writes anything
    /// else (a workspace file, a shell command) must **not** carry this: plan
    /// mode promises that its turn cannot change the world, and the turn loop
    /// reads this flag to enforce that per call — live, so `exit_plan_mode`
    /// unblocks the same turn it was approved in.
    ///
    /// A session-scoped tool that misses the mark is refused loudly in a plan
    /// session (`Plan mode is active: … is not a read-only tool`), never
    /// silently allowed.
    AllowedInPlanMode,
}

/// Approval requirement for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalRequirement {
    /// Never needs approval: safe read-only operations.
    #[default]
    Auto,
    /// Suggest approval but allow user to skip.
    Suggest,
    /// Always require explicit user approval.
    Required,
}

/// Errors that can occur during tool execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolError {
    #[error("Failed to validate input: {message}")]
    InvalidInput { message: String },
    #[error("Failed to validate input: missing required field '{field}'")]
    MissingField { field: String },
    // This `Display` is the one rendering of the refusal: the model-visible
    // transcript (`engine/turn/batches.rs`), the REPL line
    // (`interactive/render.rs`), and the logs all print it verbatim. Terse on
    // purpose — the remedies (`/trust add`, copying the file in, YOLO,
    // pasting) are model guidance in `agent-runtime/src/prompts/base.md`
    // (`<workspace_boundary>`), so the refusal line stays one sentence.
    #[error("Failed to resolve path '{}': path escapes workspace.", path.display())]
    PathEscape { path: PathBuf },
    #[error("Failed to execute tool: {message}")]
    ExecutionFailed { message: String },
    #[error("Failed to execute tool: operation timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("Failed to locate tool: {message}")]
    NotAvailable { message: String },
    #[error("Failed to authorize tool execution: {message}")]
    PermissionDenied { message: String },
    /// A guard refused the call before anything ran (e.g. the file-freshness
    /// gate rejecting an edit to a file never read this session). The
    /// message carries the remedy and the model is expected to retry with
    /// corrected input; presentation layers render it as a warning rather
    /// than a failure.
    #[error("Tool call rejected: {message}")]
    Rejected { message: String },
}

impl ToolError {
    #[must_use]
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn missing_field(field: impl Into<String>) -> Self {
        Self::MissingField {
            field: field.into(),
        }
    }

    #[must_use]
    pub fn execution_failed(msg: impl Into<String>) -> Self {
        Self::ExecutionFailed {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn path_escape(path: impl Into<PathBuf>) -> Self {
        Self::PathEscape { path: path.into() }
    }

    #[must_use]
    pub fn rejected(msg: impl Into<String>) -> Self {
        Self::Rejected {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn not_available(msg: impl Into<String>) -> Self {
        Self::NotAvailable {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        Self::PermissionDenied {
            message: msg.into(),
        }
    }
}

/// Result of a tool execution.
///
/// Canonical/rendered separation: `content` is the model-visible rendering;
/// `canonical` is the structured machine-readable value of the same result
/// for program callers (host consumers, tools calling tools, mod handlers),
/// which must not re-parse the rendering. The two channels are independent —
/// rewriting the rendering does not rewrite the machine value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The output content, which may be JSON or plain text.
    pub content: String,
    /// Whether the execution was successful.
    pub success: bool,
    /// Structured machine-readable value of this result (canonical value).
    /// Populated by [`ToolResult::json`] and by producers that render prose
    /// over structured data via [`ToolResult::with_canonical`]. Held as
    /// `Arc` so event-fanout clones (the event channel and every mod
    /// handler receive a `ToolResult` clone) are O(1) instead of
    /// deep-copying the tree. Known limitation: live-process only — the
    /// transcript persists the rendered `content`, so canonical is not
    /// reconstructed on session load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical: Option<std::sync::Arc<Value>>,
    /// Optional structured metadata — auxiliary annotations about the
    /// execution (exit codes, sandbox flags, summaries), not the result's
    /// own value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl ToolResult {
    /// Create a successful result with content.
    #[must_use]
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            success: true,
            canonical: None,
            metadata: None,
        }
    }

    /// Create an error result with message.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            success: false,
            canonical: None,
            metadata: None,
        }
    }

    /// Create a successful result from JSON. The value is kept as the
    /// canonical machine-readable value; `content` renders it pretty for
    /// the model. `T` is serialized once, into a `Value`, and the pretty
    /// rendering prints that same tree. Honest baseline: versus the
    /// pre-canonical code (a single `to_string_pretty` on `T`) the CPU is
    /// unchanged — the "halves" claim only held against an intermediate
    /// revision that serialized `T` twice. The real cost of the canonical
    /// channel is retained memory: every JSON result carries the `Value`
    /// tree (typically several times the size of its rendered text) for
    /// its lifetime, event-fanout clones included, whether or not a
    /// program caller reads it (results can be large: grep output,
    /// parallel dispatcher aggregation).
    pub fn json<T: Serialize>(value: &T) -> std::result::Result<Self, serde_json::Error> {
        let canonical = serde_json::to_value(value)?;
        Ok(Self {
            content: serde_json::to_string_pretty(&canonical)?,
            success: true,
            canonical: Some(std::sync::Arc::new(canonical)),
            metadata: None,
        })
    }

    /// Add metadata to the result.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Set the canonical machine-readable value on a prose-rendered result.
    #[must_use]
    pub fn with_canonical(mut self, canonical: Value) -> Self {
        self.canonical = Some(std::sync::Arc::new(canonical));
        self
    }
}

/// Helper to extract a required string field from JSON input.
///
/// Distinguishes "field absent" from "field present but not a string":
/// conflating them makes the provided-fields hint list the very field it
/// claims is missing, sending the model into an unsatisfiable retry loop.
pub fn required_str<'a>(input: &'a Value, field: &str) -> std::result::Result<&'a str, ToolError> {
    match input.get(field) {
        Some(Value::String(s)) => Ok(s),
        None => Err(input
            .as_object()
            .map(|obj| {
                // List the fields the caller *did* supply so the model can
                // spot the mismatch without a retry.
                let provided: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
                if provided.is_empty() {
                    ToolError::missing_field(field)
                } else {
                    let hint = format!(
                        "missing required field '{field}'. Input provided: {}",
                        provided.join(", ")
                    );
                    ToolError::invalid_input(hint)
                }
            })
            .unwrap_or_else(|| ToolError::missing_field(field))),
        Some(other) => Err(ToolError::invalid_input(format!(
            "field '{field}' must be a string, got: {other}"
        ))),
    }
}

/// Helper to extract an optional string field from JSON input.
#[must_use]
pub fn optional_str<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(Value::as_str)
}

/// Helper to extract a required u64 field from JSON input.
///
/// Distinguishes "field absent" from "field present but not a non-negative
/// integer" (negative, fractional, or string values) — same rationale as
/// [`required_str`].
pub fn required_u64(input: &Value, field: &str) -> std::result::Result<u64, ToolError> {
    match input.get(field) {
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| {
            ToolError::invalid_input(format!(
                "field '{field}' must be a non-negative integer, got: {n}"
            ))
        }),
        None => Err(ToolError::missing_field(field)),
        Some(other) => Err(ToolError::invalid_input(format!(
            "field '{field}' must be a non-negative integer, got: {other}"
        ))),
    }
}

/// Helper to extract an optional u64 field with default.
///
/// Three states, not two: absent (or JSON `null`) is the default, a
/// present value of the wrong type is an error. Folding `"10"`, `7.5`, or
/// `-5` into the default made an explicitly invalid request
/// indistinguishable from an omitted one — a `limit` that never reached
/// its clamp, a flag silently flipped — with no diagnostic for the model
/// to correct.
pub fn optional_u64(
    input: &Value,
    field: &str,
    default: u64,
) -> std::result::Result<u64, ToolError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_u64().ok_or_else(|| {
            ToolError::invalid_input(format!(
                "field '{field}' must be a non-negative integer, got: {v}"
            ))
        }),
    }
}

/// Helper to extract an optional bool field with default. Same three-state
/// contract as [`optional_u64`].
pub fn optional_bool(
    input: &Value,
    field: &str,
    default: bool,
) -> std::result::Result<bool, ToolError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_bool().ok_or_else(|| {
            ToolError::invalid_input(format!("field '{field}' must be a boolean, got: {v}"))
        }),
    }
}

/// Identifies where a tool call originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallSource {
    /// Direct invocation from the model or user.
    Direct,
    /// Invocation through the JavaScript REPL environment.
    JsRepl,
}

/// A tool invocation request before it has been validated and dispatched.
///
/// Contains the tool name, its input payload, and metadata about where the
/// call originated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Name of the tool to invoke.
    pub name: String,
    /// The input payload for the tool.
    pub payload: ToolPayload,
    /// Where this call originated (direct or REPL).
    pub source: ToolCallSource,
    /// Optional raw tool-call identifier from the upstream provider.
    pub raw_tool_call_id: Option<String>,
}

impl ToolCall {
    /// Derive the execution subject for this call.
    ///
    /// For local shell payloads this returns the shell command and its
    /// working directory; for all other payloads the tool name and the
    /// provided `fallback_cwd` are returned instead. The third element
    /// of the tuple is a human-readable kind label (`"shell"` or `"tool"`).
    pub fn execution_subject(&self, fallback_cwd: &str) -> (String, String, &'static str) {
        match &self.payload {
            ToolPayload::LocalShell { params } => (
                params.command.clone(),
                params
                    .cwd
                    .clone()
                    .unwrap_or_else(|| fallback_cwd.to_string()),
                "shell",
            ),
            _ => (self.name.clone(), fallback_cwd.to_string(), "tool"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_result_json_round_trips_content() {
        let result = ToolResult::json(&json!({"ok": true})).expect("json");
        assert!(result.success);
        assert!(result.content.contains("\"ok\": true"));
    }

    /// Canonical/rendered separation: `json` keeps the structured value as
    /// `canonical` while `content` stays the pretty rendering for the model.
    #[test]
    fn tool_result_json_sets_canonical_alongside_rendered_content() {
        let result = ToolResult::json(&json!({"ok": true, "items": [1, 2]})).expect("json");
        assert_eq!(
            result.canonical,
            Some(std::sync::Arc::new(json!({"ok": true, "items": [1, 2]})))
        );
    }

    /// Prose constructors carry no canonical value; `with_canonical` attaches
    /// one without touching the rendering.
    #[test]
    fn tool_result_prose_constructors_default_to_no_canonical() {
        assert!(ToolResult::success("done").canonical.is_none());
        assert!(ToolResult::error("boom").canonical.is_none());
        let prose = ToolResult::success("Added todo #1 (pending)")
            .with_canonical(json!({"items": [{"id": 1}]}));
        assert_eq!(
            prose.canonical,
            Some(std::sync::Arc::new(json!({"items": [{"id": 1}]})))
        );
        assert_eq!(prose.content, "Added todo #1 (pending)");
    }

    /// Results serialized before the canonical channel existed (no field)
    /// deserialize with `canonical: None`, and canonical round-trips when
    /// present. Serialization keeps field order stable for old readers.
    #[test]
    fn tool_result_serde_omits_absent_canonical_and_defaults_on_read() {
        let legacy = serde_json::from_str::<ToolResult>(r#"{"content":"ok","success":true}"#)
            .expect("legacy");
        assert!(legacy.canonical.is_none());

        let with = ToolResult::json(&json!({"v": 1})).expect("json");
        let text = serde_json::to_string(&with).expect("ser");
        assert!(text.contains(r#""canonical""#), "{text}");
        let back: ToolResult = serde_json::from_str(&text).expect("de");
        assert_eq!(back.canonical, Some(std::sync::Arc::new(json!({"v": 1}))));

        let without = ToolResult::success("plain");
        let text = serde_json::to_string(&without).expect("ser");
        assert!(!text.contains("canonical"), "{text}");
    }

    #[test]
    fn helper_extractors_validate_shape() {
        let input = json!({"name": "demo", "count": 7, "enabled": true});
        assert_eq!(required_str(&input, "name").expect("name"), "demo");
        assert_eq!(optional_u64(&input, "count", 0).expect("count"), 7);
        assert!(optional_bool(&input, "enabled", false).expect("enabled"));
        // Optional fields distinguish absent (default) from present-but-wrong
        // -type (error): `"10"`, `7.5`, and `-5` used to read as the default,
        // so an invalid request was indistinguishable from an omitted field.
        assert_eq!(optional_u64(&json!({}), "count", 50).expect("absent"), 50);
        assert_eq!(
            optional_u64(&json!({"count": null}), "count", 50).expect("null"),
            50
        );
        for wrong in [
            json!({"count": "10"}),
            json!({"count": 7.5}),
            json!({"count": -5}),
        ] {
            assert!(
                matches!(
                    optional_u64(&wrong, "count", 50),
                    Err(ToolError::InvalidInput { .. })
                ),
                "{wrong}"
            );
        }
        assert!(matches!(
            optional_bool(&json!({"enabled": "yes"}), "enabled", false),
            Err(ToolError::InvalidInput { .. })
        ));
        // Present-but-wrong-type is a distinct error from absent: the
        // provided-fields hint must not list the field it claims is missing.
        assert!(matches!(
            required_u64(&input, "name"),
            Err(ToolError::InvalidInput { .. })
        ));
        let absent_with_hint =
            required_str(&json!({"count": 7}), "name").expect_err("name is absent");
        assert!(
            absent_with_hint
                .to_string()
                .contains("missing required field 'name'")
        );
        assert!(matches!(
            required_str(&json!({}), "name"),
            Err(ToolError::MissingField { .. })
        ));
        let wrong_type = required_str(&json!({"name": 42}), "name").expect_err("not a string");
        assert!(wrong_type.to_string().contains("must be a string"));
    }

    #[test]
    fn required_str_reports_provided_fields_on_missing_required_field() {
        let input = json!({"path": "src/lib.rs", "content": "new body"});
        let err = required_str(&input, "replace").expect_err("replace is missing");
        let message = err.to_string();
        assert!(message.contains("missing required field 'replace'"));
        assert!(message.contains("Input provided:"));
        assert!(message.contains("path"));
        assert!(message.contains("content"));
    }

    #[test]
    fn tool_error_display_matches_legacy_text() {
        let err = ToolError::missing_field("path");
        assert_eq!(
            err.to_string(),
            "Failed to validate input: missing required field 'path'"
        );
    }

    /// The refusal stays one line: remedies live in the base prompt
    /// (`<workspace_boundary>`), not in the error text, so the transcript
    /// shows the fact without a paragraph around it.
    #[test]
    fn path_escape_display_stays_terse() {
        let err = ToolError::PathEscape {
            path: PathBuf::from("/Users/dev/other-repo"),
        };
        assert_eq!(
            err.to_string(),
            "Failed to resolve path '/Users/dev/other-repo': path escapes workspace."
        );
    }
}
