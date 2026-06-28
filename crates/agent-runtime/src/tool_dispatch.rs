//! Tool dispatch trait and eager metadata struct.
//!
//! The engine core holds a `Box<dyn ToolDispatcher>` per turn and invokes
//! tool execution, metadata queries, approval checks, and catalog generation
//! through this trait — without depending on the TUI's concrete
//! `ToolRegistry` or the fat `ToolContext` (which carries terminal-coupled
//! services like `shell_manager` and `lsp_manager` that cannot live in the
//! runtime crate).
//!
//! The TUI implements `ToolDispatcher` for `ToolRegistry`, delegating to its
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
    /// Whether the tool is read-only.
    pub is_read_only: bool,
    /// Whether the tool is sandboxable.
    pub is_sandboxable: bool,
    /// Whether the tool can be executed in parallel with others.
    pub supports_parallel: bool,
    /// Whether the tool should be excluded from the model-visible catalog
    /// (deferred loading).
    pub defer_loading: bool,
}

/// Per-turn tool dispatch surface.
///
/// Implemented by the TUI's `ToolRegistry`. The engine holds
/// `Option<Box<dyn ToolDispatcher>>` and routes all tool calls, metadata
/// queries, and catalog generation through this trait, keeping the
/// `ToolContext` and `ToolSpec` trait on the host side.
#[async_trait::async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// Whether a tool with `name` is registered (canonical name).
    fn has_tool(&self, name: &str) -> bool;

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
    /// deserializes the value back into its concrete sandbox-policy type.
    async fn execute(
        &self,
        name: &str,
        input: Value,
        sandbox_override: Option<Value>,
    ) -> Result<ToolResult, ToolError>;

    /// Hook host for pre/post tool-call hooks, if hooks are configured.
    fn hook_host(&self) -> Option<Arc<dyn HookHost>>;
}
