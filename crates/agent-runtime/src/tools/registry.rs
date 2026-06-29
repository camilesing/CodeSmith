//! Tool registry for managing and executing tools.
//!
//! The registry provides:
//! - Dynamic tool registration
//! - Tool lookup by name
//! - Conversion to API Tool format
//! - Filtering by capability
//!
//! This is the terminal-agnostic core: the `ToolRegistry` struct, its portable
//! execution/catalogue methods, and the fail-closed construction chokepoint
//! (`build_tool`). TUI-coupled concerns (the builder, plugin/override loading,
//! the MCP adapter) live in `codesmith-tui`'s shim module.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::hooks::HookHost;
use crate::models::Tool;
use crate::tool_dispatch::{ToolDispatcher, ToolMetadata};

use super::schema_sanitize;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

// === Types ===

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn ToolSpec>>,
    context: ToolContext,
    /// Memoised serialised tool catalog. Rebuilt lazily on first
    /// `to_api_tools` call after a mutation; pinned across reads so the
    /// description and schema bytes stay byte-stable for DeepSeek's KV
    /// prefix cache. Invalidated on `register` / `remove` / `clear`.
    api_cache: OnceLock<Vec<Tool>>,
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        let api_cache = OnceLock::new();
        if let Some(cached) = self.api_cache.get() {
            let _ = api_cache.set(cached.clone());
        }
        Self {
            tools: self.tools.clone(),
            context: self.context.clone(),
            api_cache,
        }
    }
}

impl ToolRegistry {
    /// Create a new empty registry with the given context.
    #[must_use]
    pub fn new(context: ToolContext) -> Self {
        Self {
            tools: HashMap::new(),
            context,
            api_cache: OnceLock::new(),
        }
    }

    /// Register a tool in the registry.
    ///
    /// Every registration flows through [`build_tool`], the single
    /// fail-closed chokepoint: a tool whose name or schema cannot be
    /// resolved to a well-formed API definition is substituted with a
    /// [`FailClosedTool`] stub so a single malformed plugin/MCP tool can
    /// never 400 the whole request or panic mid-turn. Mirrors Claude
    /// Code's `buildTool` contract.
    pub fn register(&mut self, tool: Arc<dyn ToolSpec>) {
        let tool = build_tool(tool);
        let name = tool.name().to_string();
        if self.tools.insert(name.clone(), tool).is_some() {
            tracing::warn!("Overwriting existing tool: {}", name);
        }
        self.invalidate_api_cache();
    }

    /// Register multiple tools at once.
    pub fn register_all(&mut self, tools: Vec<Arc<dyn ToolSpec>>) {
        for tool in tools {
            self.register(tool);
        }
    }

    /// Get a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolSpec>> {
        self.tools.get(name).cloned()
    }

    /// Check if a tool exists.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Get all registered tool names.
    #[must_use]
    #[allow(dead_code)]
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(std::string::String::as_str).collect()
    }

    /// Get the number of registered tools.
    #[must_use]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Check if the registry is empty.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Get all registered tools.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<dyn ToolSpec>> {
        self.tools.values().cloned().collect()
    }

    /// Execute a tool by name with the given input.
    pub async fn execute(&self, name: &str, input: Value) -> Result<String, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::not_available(format!("tool '{name}' is not registered")))?;

        let result = tool.execute(input, &self.context).await?;
        Ok(result.content)
    }

    /// Execute a tool by name, returning the full `ToolResult`.
    pub async fn execute_full(&self, name: &str, input: Value) -> Result<ToolResult, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::not_available(format!("tool '{name}' is not registered")))?;

        tool.execute(input, &self.context).await
    }

    /// Execute a tool with an optional context override.
    ///
    /// This is used for retrying tools with elevated sandbox policies.
    /// After execution, large results are routed through the workshop (#548).
    pub async fn execute_full_with_context(
        &self,
        name: &str,
        input: Value,
        context_override: Option<&ToolContext>,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::not_available(format!("tool '{name}' is not registered")))?;

        let ctx = context_override.unwrap_or(&self.context);
        let result = tool.execute(input.clone(), ctx).await?;

        // Large-output routing (#548): if the result exceeds the threshold and
        // the caller did not request `raw=true`, synthesise via the workshop.
        let raw_bypass = input.get("raw").and_then(|v| v.as_bool()).unwrap_or(false);

        if let Some(router) = ctx.large_output_router.as_ref() {
            use crate::tools::large_output_router::{LargeOutputRouter, RouteDecision};
            match router.route(name, &result, raw_bypass) {
                RouteDecision::PassThrough => {}
                RouteDecision::Synthesise {
                    estimated_tokens,
                    threshold,
                } => {
                    // Store the raw output in the workshop variable store.
                    if let Some(vars_arc) = ctx.workshop_vars.as_ref() {
                        let mut vars = vars_arc.lock().await;
                        vars.store_raw(name, &result.content);
                    }

                    // Build a terse synthesis using the same model the registry
                    // was constructed for (workshop Flash model). For now we
                    // produce a structured header + truncated preview without
                    // a live API call so the engine stays dependency-free at
                    // the registry layer. A follow-up can wire in the Flash
                    // client when the async LLM call is safe here.
                    let preview_chars = 1_200usize;
                    let preview: String = result.content.chars().take(preview_chars).collect();
                    let ellipsis = if result.content.chars().count() > preview_chars {
                        "\n… [output truncated — full text in workshop variable `last_tool_result`]"
                    } else {
                        ""
                    };
                    let synthesis = format!("{preview}{ellipsis}");
                    let wrapped = LargeOutputRouter::wrap_synthesis(
                        name,
                        &synthesis,
                        estimated_tokens,
                        threshold,
                    );
                    tracing::debug!(
                        tool = name,
                        estimated_tokens,
                        threshold,
                        "large-output routed through workshop"
                    );
                    return Ok(ToolResult::success(wrapped));
                }
            }
        }

        Ok(result)
    }

    /// Get the current tool context.
    #[must_use]
    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    /// Convert all tools to API Tool format for sending to the model.
    ///
    /// Output is sorted by tool name for **prefix-cache stability** (#263).
    /// Rust's `HashMap` uses a randomly-seeded hasher per process, so a raw
    /// `self.tools.values()` iteration emits tools in a different order on
    /// every `deepseek` launch, invalidating DeepSeek's KV prefix cache for
    /// every cross-session resume. Sorting here matches the way Claude Code
    /// stabilises its tool array (`assembleToolPool` in their reference).
    ///
    /// The serialised catalog is memoised on first call and pinned across
    /// reads so each tool's `description()` and `input_schema()` are sampled
    /// exactly once per registration. MCP adapters whose upstream description
    /// drifts on reconnect would otherwise rewrite the catalog mid-session
    /// and bust the prefix cache. The cache is invalidated on `register`,
    /// `remove`, and `clear`.
    #[must_use]
    pub fn to_api_tools(&self) -> Vec<Tool> {
        self.api_cache
            .get_or_init(|| self.build_api_tools())
            .clone()
    }

    fn build_api_tools(&self) -> Vec<Tool> {
        let mut tools: Vec<&Arc<dyn ToolSpec>> = self.tools.values().collect();
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
            .into_iter()
            .map(|tool| {
                let mut schema = tool.input_schema();
                schema_sanitize::sanitize(&mut schema);
                Tool {
                    tool_type: None,
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    input_schema: schema,
                    output_schema: Some(tool.output_schema()),
                    allowed_callers: Some(vec!["direct".to_string()]),
                    defer_loading: Some(tool.defer_loading()),
                    input_examples: None,
                    strict: None,
                    cache_control: None,
                }
            })
            .collect()
    }

    fn invalidate_api_cache(&mut self) {
        self.api_cache = OnceLock::new();
    }

    /// Convert tools to API Tool format with optional cache control on the last tool.
    #[must_use]
    pub fn to_api_tools_with_cache(&self, enable_cache: bool) -> Vec<Tool> {
        let mut tools = self.to_api_tools();
        if enable_cache && let Some(last) = tools.last_mut() {
            last.cache_control = Some(crate::models::CacheControl {
                cache_type: "ephemeral".to_string(),
            });
        }
        tools
    }

    /// Filter tools by capability.
    #[must_use]
    #[allow(dead_code)]
    pub fn filter_by_capability(&self, capability: ToolCapability) -> Vec<Arc<dyn ToolSpec>> {
        self.tools
            .values()
            .filter(|t| t.capabilities().contains(&capability))
            .cloned()
            .collect()
    }

    /// Get read-only tools.
    #[must_use]
    #[allow(dead_code)]
    pub fn read_only_tools(&self) -> Vec<Arc<dyn ToolSpec>> {
        self.tools
            .values()
            .filter(|t| t.is_read_only())
            .cloned()
            .collect()
    }

    /// Get tools that require approval.
    #[must_use]
    #[allow(dead_code)]
    pub fn approval_required_tools(&self) -> Vec<Arc<dyn ToolSpec>> {
        self.tools
            .values()
            .filter(|t| t.approval_requirement() == ApprovalRequirement::Required)
            .cloned()
            .collect()
    }

    /// Get tools that suggest approval.
    #[must_use]
    #[allow(dead_code)]
    pub fn approval_suggested_tools(&self) -> Vec<Arc<dyn ToolSpec>> {
        self.tools
            .values()
            .filter(|t| {
                matches!(
                    t.approval_requirement(),
                    ApprovalRequirement::Suggest | ApprovalRequirement::Required
                )
            })
            .cloned()
            .collect()
    }

    /// Update the context (e.g., when workspace changes).
    #[allow(dead_code)]
    pub fn set_context(&mut self, context: ToolContext) {
        self.context = context;
    }

    /// Get a mutable reference to the current context.
    #[must_use]
    #[allow(dead_code)]
    pub fn context_mut(&mut self) -> &mut ToolContext {
        &mut self.context
    }

    /// Remove a tool by name.
    #[must_use]
    #[allow(dead_code)]
    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn ToolSpec>> {
        let removed = self.tools.remove(name);
        if removed.is_some() {
            self.invalidate_api_cache();
        }
        removed
    }

    /// Resolve a non-canonical tool name to a registered canonical name.
    ///
    /// Runs a deterministic ladder against the registered tool names:
    /// 1. Lowercase exact match.
    /// 2. Hyphens/spaces → underscores (read-file → read_file).
    /// 3. CamelCase → snake_case (ReadFile → read_file).
    /// 4. Strip trailing `_tool` / `-tool` suffix (twice).
    /// 5. Fuzzy match via simple prefix/suffix similarity.
    ///
    /// Returns `None` when no resolution is found (let the caller surface
    /// "Unknown tool").
    #[must_use]
    pub fn resolve(&self, requested: &str) -> Option<&str> {
        let names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        let lower = requested.to_lowercase();

        // 1. lowercase exact
        if let Some(n) = names.iter().find(|n| n.to_lowercase() == lower) {
            return Some(n);
        }
        // 2. hyphen/space → underscore
        let snaked = lower.replace(['-', ' '], "_");
        if let Some(n) = names.iter().find(|n| **n == snaked) {
            return Some(n);
        }
        // 3. CamelCase → snake_case
        let cc = to_snake_case(requested);
        if let Some(n) = names.iter().find(|n| **n == cc) {
            return Some(n);
        }
        // 4. strip _tool/-tool/tool suffix, twice
        let mut stripped = cc.clone();
        for _ in 0..2 {
            for suf in ["_tool", "-tool", "tool"] {
                if let Some(s) = stripped.strip_suffix(suf) {
                    stripped = s.to_string();
                    break;
                }
            }
        }
        if !stripped.is_empty()
            && let Some(n) = names.iter().find(|n| **n == stripped)
        {
            return Some(n);
        }
        // 5. fuzzy: simple prefix match (at least 3 chars)
        if lower.len() >= 3 {
            for n in &names {
                if n.len() >= 3 && (n.starts_with(&lower) || lower.starts_with(n)) {
                    return Some(n);
                }
            }
        }
        None
    }

    /// Clear all tools from the registry.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.tools.clear();
        self.invalidate_api_cache();
    }

    /// Remove a tool from the registry by name. Returns `true` if the tool
    /// was present and removed, `false` if no tool with that name existed.
    pub fn remove_tool(&mut self, name: &str) -> bool {
        let existed = self.tools.remove(name).is_some();
        if existed {
            self.invalidate_api_cache();
        }
        existed
    }
}

/// Convert CamelCase to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// === Fail-closed tool construction ===

/// Maximum length for a tool name. Mirrors the API tool-name ceiling so a
/// single over-long plugin name can never reject the whole request.
pub const MAX_TOOL_NAME_LEN: usize = 64;

/// Validate a tool at the registration chokepoint and substitute a
/// fail-closed stub if it is malformed.
///
/// This is CodeSmith's analogue of Claude Code's `buildTool`. A tool whose
/// name or input schema cannot be resolved to a well-formed API definition
/// is replaced by a [`FailClosedTool`] so the model still sees a stable,
/// API-legal tool slot but any execution attempt returns a safe error.
/// Without this, a single bad plugin/MCP tool definition would either
/// panic mid-turn or trigger a 400 that disables *every* tool for the rest
/// of the session.
///
/// Validation is intentionally minimal and semantics-preserving: deep
/// schema normalisation still happens in `build_api_tools` via
/// `schema_sanitize::sanitize`. Here we only reject shapes the API cannot
/// accept at all — a non-object root schema or a name that breaks the
/// `^[a-zA-Z0-9_-]{1,64}$` contract every chat-completions backend
/// enforces for `tools[].name`.
fn build_tool(tool: Arc<dyn ToolSpec>) -> Arc<dyn ToolSpec> {
    let name = tool.name();
    if !is_valid_tool_name(name) {
        tracing::warn!(
            tool = %name,
            reason = "invalid tool name",
            "tool failed construction; substituting fail-closed stub",
        );
        return Arc::new(FailClosedTool::new(name, "invalid tool name"));
    }
    let schema = tool.input_schema();
    if !is_valid_input_schema(&schema) {
        tracing::warn!(
            tool = %name,
            reason = "non-object input schema",
            "tool failed construction; substituting fail-closed stub",
        );
        return Arc::new(FailClosedTool::new(name, "invalid input schema"));
    }
    tool
}

/// A tool name must be a non-empty ASCII identifier of letters, digits,
/// `_`, or `-`, no longer than [`MAX_TOOL_NAME_LEN`]. This matches the
/// contract every chat-completions backend enforces for `tools[].name`.
pub fn is_valid_tool_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_TOOL_NAME_LEN {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Coerce an arbitrary string into a valid tool name so a fail-closed stub
/// can still be keyed under something the API accepts. Invalid characters
/// collapse to `_`; an all-invalid or empty name falls back to
/// `fail_closed_tool`.
pub fn sanitize_tool_name(name: impl Into<String>) -> String {
    let mut s: String = name
        .into()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.len() > MAX_TOOL_NAME_LEN {
        s.truncate(MAX_TOOL_NAME_LEN);
    }
    if s.is_empty() {
        s.push_str("fail_closed_tool");
    }
    s
}

/// A tool's `input_schema` must be a JSON object — the API serialises it as
/// `{"type": "object", ...}`. A non-object root (`null`, array, string…)
/// would either be silently dropped or 400 the request.
fn is_valid_input_schema(schema: &Value) -> bool {
    schema.is_object()
}

/// Fail-closed placeholder substituted when a tool fails construction.
///
/// Keeps the (sanitised) tool name visible so the model-facing catalog
/// stays stable, advertises a permissive object schema, and refuses every
/// execution with a `NotAvailable` error describing the original failure.
/// Capabilities are conservatively `RequiresApproval` + `Network` so the
/// stub can never auto-execute in any mode — this generalises the stance
/// `McpToolAdapter::capabilities` already takes for unknown MCP tools.
pub(crate) struct FailClosedTool {
    name: String,
    reason: String,
}

impl FailClosedTool {
    pub(crate) fn new(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: sanitize_tool_name(name),
            reason: reason.into(),
        }
    }
}

#[async_trait::async_trait]
impl ToolSpec for FailClosedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Unavailable: this tool failed to initialise and is fail-closed. Do not call it."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        // Fail-closed: never auto-execute. Treat as approval-required and
        // network-capable (conservative, mirrors McpToolAdapter's stance).
        vec![ToolCapability::RequiresApproval, ToolCapability::Network]
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::not_available(format!(
            "tool '{}' is unavailable: {}",
            self.name, self.reason
        )))
    }
}

// === ToolDispatcher bridge ===
//
// `impl ToolDispatcher for ToolRegistry` delegates to the registry's
// inherent methods and passes its internal `ToolContext` to tools. Both
// sides of the impl now live in this crate, so it sits next to the type
// (it previously lived in the TUI's `runtime_traits.rs` bridge file,
// which is only legal while `ToolRegistry` was a TUI type).
#[async_trait::async_trait]
impl ToolDispatcher for ToolRegistry {
    fn has_tool(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn resolve(&self, requested: &str) -> Option<String> {
        ToolRegistry::resolve(self, requested).map(String::from)
    }

    fn metadata(&self, name: &str) -> Option<ToolMetadata> {
        let spec = self.get(name)?;
        Some(ToolMetadata {
            name: spec.name().to_string(),
            description: spec.description().to_string(),
            capabilities: spec.capabilities(),
            approval_requirement: spec.approval_requirement(),
            is_read_only: spec.is_read_only(),
            is_sandboxable: spec.is_sandboxable(),
            supports_parallel: spec.supports_parallel(),
            defer_loading: spec.defer_loading(),
        })
    }

    fn is_destructive(&self, name: &str, input: &Value) -> bool {
        self.get(name)
            .is_some_and(|spec| spec.is_destructive(input))
    }

    fn is_interactive(&self, name: &str, input: &Value) -> bool {
        self.get(name)
            .is_some_and(|spec| spec.is_interactive(input))
    }

    fn approval_requirement_for(&self, name: &str, input: &Value) -> Option<ApprovalRequirement> {
        let spec = self.get(name)?;
        Some(spec.approval_requirement_for_input(input, self.context()))
    }

    fn validate_input(&self, name: &str, input: &Value) -> Result<(), ToolError> {
        let spec = self
            .get(name)
            .ok_or_else(|| ToolError::not_available(format!("tool '{name}' is not registered")))?;
        spec.validate_input(input, self.context())
    }

    fn to_api_tools(&self) -> Vec<Tool> {
        ToolRegistry::to_api_tools(self)
    }

    fn to_api_tools_with_cache(&self, enable_cache: bool) -> Vec<Tool> {
        ToolRegistry::to_api_tools_with_cache(self, enable_cache)
    }

    async fn execute(
        &self,
        name: &str,
        input: Value,
        sandbox_override: Option<Value>,
    ) -> Result<ToolResult, ToolError> {
        let context_override = sandbox_override.and_then(|v| {
            match serde_json::from_value::<crate::sandbox::SandboxPolicy>(v) {
                Ok(policy) => Some(self.context().clone().with_elevated_sandbox_policy(policy)),
                Err(e) => {
                    tracing::warn!(error = %e, "invalid sandbox override policy JSON");
                    None
                }
            }
        });
        self.execute_full_with_context(name, input, context_override.as_ref())
            .await
    }

    fn hook_host(&self) -> Option<Arc<dyn HookHost>> {
        self.context()
            .runtime
            .hook_executor
            .clone()
            .map(|h| -> Arc<dyn HookHost> { h })
    }
}
