//! Tool dispatch trait and eager metadata struct.
//!
//! The engine core holds a `Box<dyn ToolDispatcher>` per turn and invokes
//! tool execution, metadata queries, approval checks, and catalog generation
//! through this trait — without depending on the CLI's concrete
//! `ToolRegistry` or the fat `ToolContext` (which carries terminal-coupled
//! services like `shell_manager` and `lsp_manager` that cannot live in the
//! runtime crate).
//!
//! The CLI implements `ToolDispatcher` for `ToolRegistry`, delegating to its
//! inherent methods and passing its internal `ToolContext` to tools.

use std::sync::Arc;

use codesmith_agent::models::Tool;
use codesmith_tools::{ApprovalRequirement, ToolCapability, ToolError, ToolResult};
use serde_json::Value;

use crate::hooks::HookHost;

/// Eager, input-independent metadata for a registered tool.
///
/// Captured once per tool (not per call) so the engine can make approval and
/// scheduling decisions without holding a `dyn ToolSpec` (which lives in the
/// TUI crate alongside the fat `ToolContext`).
#[derive(Debug, Clone)]
pub struct ToolMetadata {
    /// Canonical tool name.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// Capability flags (e.g. `WritesFiles`, `ExecutesCode`).
    pub capabilities: Vec<ToolCapability>,
    /// Static approval requirement (input-independent baseline).
    pub approval_requirement: ApprovalRequirement,
    /// Whether the tool can be executed in parallel with others.
    pub supports_parallel: bool,
    /// Whether the tool should be excluded from the model-visible catalog
    /// (deferred loading).
    pub defer_loading: bool,
}

impl ToolMetadata {
    /// Whether the tool is read-only. Mirrors `ToolSpec::is_read_only`:
    /// `ReadOnly` capability without any `WritesFiles`/`ExecutesCode`
    /// override.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.capabilities.contains(&ToolCapability::ReadOnly)
            && !self.capabilities.contains(&ToolCapability::WritesFiles)
            && !self.capabilities.contains(&ToolCapability::ExecutesCode)
    }

    /// Whether the tool is sandboxable. Mirrors `ToolSpec::is_sandboxable`.
    #[must_use]
    pub fn is_sandboxable(&self) -> bool {
        self.capabilities.contains(&ToolCapability::Sandboxable)
    }
}

/// Per-turn tool dispatch surface.
///
/// Implemented by the CLI's `ToolRegistry`. The engine holds
/// `Option<Box<dyn ToolDispatcher>>` and routes all tool calls, metadata
/// queries, and catalog generation through this trait, keeping the
/// `ToolContext` and `ToolSpec` trait on the host side.
#[async_trait::async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// Whether a tool with `name` is registered (canonical name). The default
    /// derives it from `metadata` — the same lookup an explicit impl would do.
    fn has_tool(&self, name: &str) -> bool {
        self.metadata(name).is_some()
    }

    /// Resolve a non-canonical tool name to its registered canonical name.
    ///
    /// Runs a deterministic ladder (lowercase, hyphen→underscore,
    /// CamelCase→snake_case, suffix strip, fuzzy). Returns `None` when no
    /// resolution is found.
    fn resolve(&self, requested: &str) -> Option<String>;

    /// Eager metadata for a registered tool. Returns `None` if the tool
    /// isn't registered.
    fn metadata(&self, name: &str) -> Option<ToolMetadata>;

    /// Whether the tool+input can perform destructive work.
    fn is_destructive(&self, name: &str, input: &Value) -> bool;

    /// Whether the tool+input requires user interaction while executing.
    fn is_interactive(&self, name: &str, input: &Value) -> bool;

    /// Per-input approval requirement. Returns `None` if the tool isn't
    /// registered.
    fn approval_requirement_for(&self, name: &str, input: &Value) -> Option<ApprovalRequirement>;

    /// Out-of-workspace, untrusted path this input names — the subject of the
    /// path-access question the host may ask, and the path the engine grants
    /// once the user allows it. `None` when the tool targets no path or the
    /// path is already allowed. The default keeps dispatchers that carry no
    /// path semantics (embeds, tests) from ever raising the question.
    fn escaped_path_for(&self, _name: &str, _input: &Value) -> Option<std::path::PathBuf> {
        None
    }

    /// Trust `path` for the rest of this process, so the approved call's own
    /// path check passes. Persisting it belongs to the host — the user was
    /// asked, and only a "remember" answer reaches disk.
    fn grant_external_path(&self, path: std::path::PathBuf) {
        // A dispatcher reporting an escaped path but forgetting the grant
        // fails closed with no diagnostic; leave a trace so that gap is
        // visible in debug logs.
        tracing::debug!(
            path = %path.display(),
            "grant_external_path not overridden by dispatcher; approved path was not granted — the retried call may hit the tool's path check again"
        );
    }

    /// Validate finalized model-provided input before approval or execution.
    fn validate_input(&self, name: &str, input: &Value) -> Result<(), ToolError>;

    /// All tools as API `Tool` format, sorted by name for prefix-cache
    /// stability (#263).
    fn to_api_tools(&self) -> Vec<Tool>;

    /// Same as [`to_api_tools`](Self::to_api_tools) with optional
    /// `cache_control` on the last tool.
    fn to_api_tools_with_cache(&self, enable_cache: bool) -> Vec<Tool>;

    /// Execute a tool by name.
    ///
    /// `sandbox_override` is a serialized `SandboxPolicy`
    /// (`serde_json::Value`) applied when retrying after an elevated-sandbox
    /// approval. `None` uses the registry's default context. The host
    /// deserializes the value back into its concrete sandbox-policy type;
    /// on deserialization failure the host MUST abort the execution with an
    /// error — falling back to the default (typically less strict) policy
    /// would silently degrade sandboxing on a malformed override.
    async fn execute(
        &self,
        name: &str,
        input: Value,
        sandbox_override: Option<Value>,
    ) -> Result<ToolResult, ToolError>;

    /// Hook host for pre/post tool-call hooks, if hooks are configured.
    fn hook_host(&self) -> Option<Arc<dyn HookHost>>;

    /// Session-cwd override recorded by `exec_shell` during this turn
    /// (P2-6): the final working directory of a foreground child process
    /// whose `cd` should persist for the session. The engine folds it into
    /// `Session::cwd` post-turn. Default `None` (no capture / dispatchers
    /// without a context slot).
    fn session_cwd_override(&self) -> Option<std::path::PathBuf> {
        None
    }
}
