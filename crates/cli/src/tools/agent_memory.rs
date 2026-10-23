//! Scoped Agent Memory tools.
//!
//! These tools intentionally do not reuse the generic workspace file tools: they
//! are constrained to `ToolContext.agent_memory_dir`, so memory-enabled read-only
//! sub-agents can maintain their own MEMORY.md without gaining workspace write
//! privileges.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_agent_runtime::agent_memory::paths::{
    ensure_agent_memory_dir, scoped_path_within_memory,
};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_u64, required_str,
};
use codesmith_agent_runtime::tools::diff_format::make_unified_diff;

pub struct AgentMemoryReadTool;
pub struct AgentMemoryWriteTool;
pub struct AgentMemoryEditTool;

#[async_trait]
impl ToolSpec for AgentMemoryReadTool {
    fn name(&self) -> &'static str {
        "agent_memory_read"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 file from this sub-agent's scoped memory directory. Paths are relative to the agent memory root; use MEMORY.md for the index."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path inside the agent memory directory, e.g. MEMORY.md or topics/style.md" },
                "start_line": { "type": "integer", "description": "Starting line (1-based, default 1)" },
                "max_lines": { "type": "integer", "description": "Maximum lines to return (default 200, max 500)" }
            },
            "required": ["path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let path = resolve_agent_memory_tool_path(context, required_str(&input, "path")?)?;
        let contents = tokio::fs::read_to_string(&path).await.map_err(|err| {
            ToolError::execution_failed(format!("failed to read {}: {err}", path.display()))
        })?;
        let start_line = optional_u64(&input, "start_line", 1).max(1) as usize;
        let max_lines = optional_u64(&input, "max_lines", 200).clamp(1, 500) as usize;
        let total_lines = contents.lines().count();
        let selected = contents
            .lines()
            .skip(start_line.saturating_sub(1))
            .take(max_lines)
            .collect::<Vec<_>>()
            .join("\n");
        let shown = selected.lines().count();
        let truncated = start_line.saturating_sub(1) + shown < total_lines;
        if total_lines <= 200 && contents.len() <= 16 * 1024 && start_line == 1 && max_lines >= 200
        {
            // Return `selected`, not the raw `contents`: `str::lines()`
            // normalizes CRLF and drops the trailing newline, so the two
            // branches must come from the same slicing to be byte-identical
            // for a given file.
            return Ok(ToolResult::success(selected));
        }
        Ok(ToolResult::success(format!(
            "<agent_memory_file path=\"{}\" total_lines=\"{}\" start_line=\"{}\" shown_lines=\"{}\" truncated=\"{}\">\n{}\n</agent_memory_file>",
            path.display(),
            total_lines,
            start_line,
            shown,
            truncated,
            selected
        )))
    }
}

#[async_trait]
impl ToolSpec for AgentMemoryWriteTool {
    fn name(&self) -> &'static str {
        "agent_memory_write"
    }

    fn description(&self) -> &'static str {
        "Create or overwrite a UTF-8 file inside this sub-agent's scoped memory directory. Use MEMORY.md as the index and topic .md files for details."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path inside the agent memory directory" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let memory_dir = agent_memory_dir(context)?;
        let path = resolve_agent_memory_tool_path(context, required_str(&input, "path")?)?;
        let content = required_str(&input, "content")?;
        // Directory creation + MEMORY.md seeding live with the mutating
        // tools: `agent_memory_read` advertises ReadOnly and must resolve
        // without side effects (a failed probe read used to leave a newly
        // created dir/entrypoint behind).
        let ensure_dir = memory_dir.to_path_buf();
        tokio::task::spawn_blocking(move || ensure_agent_memory_dir(&ensure_dir))
            .await
            .map_err(|err| ToolError::execution_failed(format!("dir task failed: {err}")))?
            .map_err(|err| {
                ToolError::execution_failed(format!(
                    "failed to create agent memory dir {}: {err}",
                    memory_dir.display()
                ))
            })?;
        // Only a missing file counts as "no prior content": swallowing
        // permission/IO/UTF-8 errors here would render a misleading
        // whole-file-added diff and hide the real failure.
        let prior = match tokio::fs::read_to_string(&path).await {
            Ok(prior) => prior,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                return Err(ToolError::execution_failed(format!(
                    "failed to read {}: {err}",
                    path.display()
                )));
            }
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                ToolError::execution_failed(format!("failed to create {}: {err}", parent.display()))
            })?;
        }
        // `write_atomic` fsyncs (NamedTempFile + persist) — synchronous
        // disk I/O that belongs on the blocking pool, not the async worker.
        let write_path = path.clone();
        let bytes = content.as_bytes().to_vec();
        tokio::task::spawn_blocking(move || {
            codesmith_agent_runtime::utils::write_atomic(&write_path, &bytes)
        })
        .await
        .map_err(|err| ToolError::execution_failed(format!("write task failed: {err}")))?
        .map_err(|err| {
            ToolError::execution_failed(format!("failed to write {}: {err}", path.display()))
        })?;
        let diff = make_unified_diff(&path.display().to_string(), &prior, content);
        let summary = format!(
            "Wrote {} bytes to agent memory file {}",
            content.len(),
            path.display()
        );
        let body = if diff.is_empty() {
            summary
        } else {
            format!("{diff}\n{summary}")
        };
        Ok(ToolResult::success(body))
    }
}

#[async_trait]
impl ToolSpec for AgentMemoryEditTool {
    fn name(&self) -> &'static str {
        "agent_memory_edit"
    }

    fn description(&self) -> &'static str {
        "Replace exact text in a file inside this sub-agent's scoped memory directory. Fails if the search text is missing."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path inside the agent memory directory" },
                "search": { "type": "string", "description": "Exact text to replace" },
                "replace": { "type": "string", "description": "Replacement text" }
            },
            "required": ["path", "search", "replace"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let path = resolve_agent_memory_tool_path(context, required_str(&input, "path")?)?;
        let search = required_str(&input, "search")?;
        let replace = required_str(&input, "replace")?;
        // Empty search passes `matches("")` (len+1 hits) and `replace("",
        // …)` splices between every character — catastrophic corruption of
        // the memory file.
        if search.is_empty() {
            return Err(ToolError::invalid_input("search must not be empty"));
        }
        if search == replace {
            return Err(ToolError::invalid_input("search and replace are identical"));
        }
        let contents = tokio::fs::read_to_string(&path).await.map_err(|err| {
            ToolError::execution_failed(format!("failed to read {}: {err}", path.display()))
        })?;
        let count = contents.matches(search).count();
        if count == 0 {
            return Err(ToolError::execution_failed(format!(
                "search string not found in {}",
                path.display()
            )));
        }
        let updated = contents.replace(search, replace);
        // Atomic temp-file + rename (via spawn_blocking — the helper
        // fsyncs): a crash or ENOSPC mid-write must not truncate the
        // previously intact memory file.
        let write_path = path.clone();
        let updated_bytes = updated.as_bytes().to_vec();
        tokio::task::spawn_blocking(move || {
            codesmith_agent_runtime::utils::write_atomic(&write_path, &updated_bytes)
        })
        .await
        .map_err(|err| ToolError::execution_failed(format!("write task failed: {err}")))?
        .map_err(|err| {
            ToolError::execution_failed(format!("failed to write {}: {err}", path.display()))
        })?;
        let diff = make_unified_diff(&path.display().to_string(), &contents, &updated);
        let summary = format!(
            "Replaced {count} occurrence(s) in agent memory file {}",
            path.display()
        );
        let body = if diff.is_empty() {
            summary
        } else {
            format!("{diff}\n{summary}")
        };
        Ok(ToolResult::success(body))
    }
}

fn agent_memory_dir(context: &ToolContext) -> Result<&std::path::Path, ToolError> {
    context.agent_memory_dir.as_deref().ok_or_else(|| {
        ToolError::execution_failed("agent memory is not enabled for this sub-agent")
    })
}

/// Resolve (but do not create) a scoped path inside the agent memory
/// directory. Side-effect-free by design so the read-only tool can resolve
/// without touching the filesystem — directory creation and MEMORY.md
/// seeding belong to the mutating tools and sub-agent spawn.
fn resolve_agent_memory_tool_path(
    context: &ToolContext,
    raw: &str,
) -> Result<std::path::PathBuf, ToolError> {
    let memory_dir = agent_memory_dir(context)?;
    scoped_path_within_memory(memory_dir, raw).map_err(ToolError::invalid_input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use std::path::Path;

    fn ctx(dir: &Path) -> ToolContext {
        let mut context = ToolContext::new(dir);
        context.agent_memory_dir = Some(dir.join("agent-memory"));
        context
    }

    #[tokio::test]
    async fn rejects_without_agent_memory() {
        let tmp = tempdir().unwrap();
        let tool = AgentMemoryReadTool;
        let err = tool
            .execute(json!({"path": "MEMORY.md"}), &ToolContext::new(tmp.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not enabled"));
    }

    #[tokio::test]
    async fn write_and_read_inside_memory() {
        let tmp = tempdir().unwrap();
        let context = ctx(tmp.path());
        AgentMemoryWriteTool
            .execute(json!({"path": "topic.md", "content": "hello"}), &context)
            .await
            .unwrap();
        let result = AgentMemoryReadTool
            .execute(json!({"path": "topic.md"}), &context)
            .await
            .unwrap();
        assert_eq!(result.content, "hello");
    }

    #[tokio::test]
    async fn rejects_parent_traversal() {
        let tmp = tempdir().unwrap();
        let context = ctx(tmp.path());
        let err = AgentMemoryWriteTool
            .execute(json!({"path": "../escape.md", "content": "x"}), &context)
            .await
            .unwrap_err();
        assert!(err.to_string().contains(".."));
    }
}
