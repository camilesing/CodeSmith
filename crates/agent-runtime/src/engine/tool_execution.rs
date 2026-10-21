//! Low-level tool execution helpers for the engine turn loop.
//!
//! This module keeps the mechanics of MCP dispatch, execution locking, and
//! parallel-tool fanout out of `engine.rs`; the turn loop still owns planning,
//! approval, and how tool results are written back into session state.

use std::sync::Arc;

use super::*;

use crate::tool_dispatch::ToolDispatcher;

impl Engine {
    pub async fn execute_mcp_tool_with_pool(
        pool: Arc<AsyncMutex<McpPool>>,
        name: &str,
        input: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        let mut pool = pool.lock().await;
        let result = pool
            .call_tool(name, input)
            .await
            .map_err(|e| ToolError::execution_failed(format!("MCP tool failed: {e}")))?;
        let content = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
        Ok(ToolResult::success(content))
    }

    pub async fn execute_parallel_tool(
        &mut self,
        input: serde_json::Value,
        tool_registry: Option<&dyn ToolDispatcher>,
        tool_exec_lock: Arc<RwLock<()>>,
    ) -> Result<ToolResult, ToolError> {
        let calls = parse_parallel_tool_calls(&input)?;
        let mcp_pool = if calls.iter().any(|(tool, _)| McpPool::is_mcp_tool(tool)) {
            Some(self.ensure_mcp_pool().await?)
        } else {
            None
        };
        let Some(registry) = tool_registry else {
            return Err(ToolError::not_available(
                "tool registry unavailable for multi_tool_use.parallel",
            ));
        };

        let mut tasks = FuturesUnordered::new();
        for (tool_name, tool_input) in calls {
            if tool_name == MULTI_TOOL_PARALLEL_NAME {
                return Err(ToolError::invalid_input(
                    "multi_tool_use.parallel cannot call itself",
                ));
            }
            if McpPool::is_mcp_tool(&tool_name) {
                if !mcp_tool_is_parallel_safe(&tool_name) {
                    return Err(ToolError::invalid_input(format!(
                        "Tool '{tool_name}' is an MCP tool and cannot run in parallel. \
                         Allowed MCP tools: list_mcp_resources, list_mcp_resource_templates, \
                         mcp_read_resource, read_mcp_resource, mcp_get_prompt."
                    )));
                }
            } else {
                let Some(metadata) = registry.metadata(&tool_name) else {
                    return Err(ToolError::not_available(format!(
                        "tool '{tool_name}' is not registered"
                    )));
                };
                if !metadata.is_read_only() {
                    return Err(ToolError::invalid_input(format!(
                        "Tool '{tool_name}' is not read-only and cannot run in parallel"
                    )));
                }
                if metadata.approval_requirement != ApprovalRequirement::Auto {
                    return Err(ToolError::invalid_input(format!(
                        "Tool '{tool_name}' requires approval and cannot run in parallel"
                    )));
                }
                if !metadata.supports_parallel {
                    return Err(ToolError::invalid_input(format!(
                        "Tool '{tool_name}' does not support parallel execution"
                    )));
                }
            }

            let registry_ref = registry;
            let lock = tool_exec_lock.clone();
            let mcp_pool = mcp_pool.clone();
            tasks.push(async move {
                let result = Engine::execute_tool_with_lock(
                    lock,
                    true,
                    false,
                    tool_name.clone(),
                    tool_input.clone(),
                    Some(registry_ref),
                    mcp_pool,
                    None,
                )
                .await;
                (tool_name, result)
            });
        }

        let mut results = Vec::new();
        while let Some((tool_name, result)) = tasks.next().await {
            match result {
                Ok(output) => {
                    let mut error = None;
                    if !output.success {
                        error = Some(output.content.clone());
                    }
                    results.push(ParallelToolResultEntry {
                        tool_name,
                        success: output.success,
                        content: output.content,
                        error,
                    });
                }
                Err(err) => {
                    let message = format!("{err}");
                    results.push(ParallelToolResultEntry {
                        tool_name,
                        success: false,
                        content: format!("Error: {message}"),
                        error: Some(message),
                    });
                }
            }
        }

        ToolResult::json(&ParallelToolResult { results })
            .map_err(|e| ToolError::execution_failed(e.to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_tool_with_lock(
        lock: Arc<RwLock<()>>,
        supports_parallel: bool,
        interactive: bool,
        tool_name: String,
        tool_input: serde_json::Value,
        registry: Option<&dyn ToolDispatcher>,
        mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
        sandbox_override: Option<serde_json::Value>,
    ) -> Result<ToolResult, ToolError> {
        let started_at = std::time::Instant::now();
        let dispatch = if McpPool::is_mcp_tool(&tool_name) {
            "mcp"
        } else if registry.is_some() {
            "registry"
        } else {
            "missing"
        };
        let input_bytes = serde_json::to_string(&tool_input)
            .map(|s| s.len())
            .unwrap_or(0);
        tracing::debug!(
            target: "engine.tool_execution",
            tool = %tool_name,
            dispatch,
            interactive,
            supports_parallel,
            input_bytes,
            "tool.exec.start",
        );

        let _guard = if supports_parallel {
            ToolExecGuard::Read(lock.read().await)
        } else {
            ToolExecGuard::Write(lock.write().await)
        };

        let outcome = if McpPool::is_mcp_tool(&tool_name) {
            if let Some(pool) = mcp_pool {
                Engine::execute_mcp_tool_with_pool(pool, &tool_name, tool_input).await
            } else {
                Err(ToolError::not_available(format!(
                    "tool '{tool_name}' is not registered"
                )))
            }
        } else if let Some(registry) = registry {
            ToolDispatcher::execute(registry, &tool_name, tool_input, sandbox_override).await
        } else {
            Err(ToolError::not_available(format!(
                "tool '{tool_name}' is not registered"
            )))
        };

        let duration_ms = started_at.elapsed().as_millis() as u64;
        match &outcome {
            Ok(result) => {
                tracing::debug!(
                    target: "engine.tool_execution",
                    tool = %tool_name,
                    dispatch,
                    duration_ms,
                    success = result.success,
                    output_bytes = result.content.len(),
                    "tool.exec.end",
                );
            }
            Err(err) => {
                let kind = match err {
                    ToolError::InvalidInput { .. } => "invalid_input",
                    ToolError::MissingField { .. } => "missing_field",
                    ToolError::PathEscape { .. } => "path_escape",
                    ToolError::ExecutionFailed { .. } => "execution_failed",
                    ToolError::Timeout { .. } => "timeout",
                    ToolError::NotAvailable { .. } => "not_available",
                    ToolError::PermissionDenied { .. } => "permission_denied",
                    ToolError::Rejected { .. } => "rejected",
                };
                tracing::warn!(
                    target: "engine.tool_execution",
                    tool = %tool_name,
                    dispatch,
                    duration_ms,
                    error_kind = kind,
                    error = %err,
                    "tool.exec.end",
                );
            }
        }
        outcome
    }
}
