//! `knowledge_recall` tool — explicit memory retrieval by the model.
//!
//! Supplements the automatic prefetch with manual recall. The model can
//! invoke this when it needs specific memory content that wasn surfaced
//! by the prefetch or when it wants to search memories by keyword.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_agent_runtime::knowledge::age::{memory_age_label, memory_freshness_text};
use codesmith_agent_runtime::knowledge::budget::{MAX_BYTES_PER_MEMORY, MAX_LINES_PER_MEMORY};
use codesmith_agent_runtime::knowledge::scan::scan_memory_files;

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};

/// Tool that explicitly retrieves memories from the KoD directory.
pub struct KnowledgeRetrievalTool;

#[async_trait]
impl ToolSpec for KnowledgeRetrievalTool {
    fn name(&self) -> &'static str {
        "knowledge_recall"
    }

    fn description(&self) -> &'static str {
        "Search and retrieve memory files by keyword. Use this when you \
         need specific memory content that wasn't automatically surfaced, \
         or when you want to recall memories about a particular topic. \
         Returns matching memory files with their content (truncated to \
         fit budget limits)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keyword or phrase to search for in memory filenames and descriptions."
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = required_str(&input, "query")?;
        let query_lower = query.to_lowercase();

        let memory_dir = context.memory_dir.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "Knowledge On Demand is not enabled — set `[memory] kod_enabled = true` and \
                 `[memory] enabled = true` in config.toml to enable directory-based memory.",
            )
        })?;

        let headers = scan_memory_files(memory_dir);
        if headers.is_empty() {
            return Ok(ToolResult::success(
                "No memory files found in the memory directory.",
            ));
        }

        // Filter headers by query matching filename or description.
        let matching: Vec<_> = headers
            .iter()
            .filter(|h| {
                h.filename.to_lowercase().contains(&query_lower)
                    || h.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&query_lower))
            })
            .collect();

        if matching.is_empty() {
            return Ok(ToolResult::success(format!(
                "No memories matching '{}'. Available memories: {}",
                query,
                headers
                    .iter()
                    .map(|h| h.filename.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }

        // Read and format matching memories (up to 3 for manual recall).
        let mut output = String::new();
        for header in matching.iter().take(3) {
            if let Some(content) = std::fs::read_to_string(&header.file_path).ok() {
                let (_fm, body) =
                    codesmith_agent_runtime::knowledge::scan::parse_frontmatter(&content);
                let age_label = memory_age_label(header.mtime_ms);
                let freshness = memory_freshness_text(header.mtime_ms);

                // Truncate body.
                let mut lines: Vec<&str> = body.lines().collect();
                let truncated = lines.len() > MAX_LINES_PER_MEMORY;
                if truncated {
                    lines.truncate(MAX_LINES_PER_MEMORY);
                }
                let mut body_text = lines.join("\n");
                if body_text.len() > MAX_BYTES_PER_MEMORY {
                    let cutoff =
                        codesmith_agent_runtime::knowledge::entrypoint::previous_char_boundary(
                            &body_text,
                            MAX_BYTES_PER_MEMORY,
                        );
                    body_text = format!("{}[truncated]", &body_text[..cutoff]);
                }

                output.push_str(&format!(
                    "[Memory: {}, last modified {}]\n",
                    header.filename, age_label
                ));
                if !freshness.is_empty() {
                    output.push_str(&format!("{}\n", freshness));
                }
                output.push_str(&body_text);
                if truncated {
                    output.push_str("\n[truncated: content exceeds memory budget]");
                }
                output.push_str("\n\n");
            }
        }

        Ok(ToolResult::success(output.trim_end().to_string()))
    }
}
