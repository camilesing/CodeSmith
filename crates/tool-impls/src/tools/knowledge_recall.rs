//! `knowledge_recall` tool — explicit memory retrieval by the model.
//!
//! Supplements the automatic prefetch with manual recall. The model can
//! invoke this when it needs specific memory content that wasn't surfaced
//! by the prefetch or when it wants to search memories by keyword.

use async_trait::async_trait;
use serde_json::{Value, json};

use codesmith_agent_runtime::knowledge::age::{memory_age_label, memory_freshness_text};
use codesmith_agent_runtime::knowledge::budget::{MAX_BYTES_PER_MEMORY, MAX_LINES_PER_MEMORY};
use codesmith_agent_runtime::knowledge::scan::{MemoryHeader, scan_memory_files};

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
        // `required_str` validates presence, not content: an empty query
        // makes `contains("")` true for every header, "matching" everything.
        let query = query.trim();
        if query.is_empty() {
            return Err(ToolError::invalid_input(
                "'query' must be a non-empty keyword or phrase",
            ));
        }
        let query_lower = query.to_lowercase();

        let memory_dir = context.memory_dir.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "Knowledge On Demand is not enabled — set `[memory] kod_enabled = true` and \
                 `[memory] enabled = true` in config.toml to enable directory-based memory.",
            )
        })?;

        let dir = memory_dir.clone();
        let query_text = query.to_string();
        let output = tokio::task::spawn_blocking(move || {
            let headers = scan_memory_files(&dir);
            if headers.is_empty() {
                return "No memory files found in the memory directory.".to_string();
            }

            // Filter headers by query matching filename or description.
            let matching: Vec<MemoryHeader> = headers
                .iter()
                .filter(|h| {
                    h.filename.to_lowercase().contains(&query_lower)
                        || h.description
                            .as_deref()
                            .is_some_and(|d| d.to_lowercase().contains(&query_lower))
                })
                .cloned()
                .collect();

            if matching.is_empty() {
                return format!(
                    "No memories matching '{}'. Available memories: {}",
                    query_text,
                    headers
                        .iter()
                        .map(|h| h.filename.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            // Read and format matching memories (up to 3 for manual recall).
            format_matching_memories(&matching)
        })
        .await
        .map_err(|e| {
            // A panicking scan closure is a code bug, not an I/O failure —
            // keep the payload so it is not misread as a scan error.
            if e.is_panic() {
                let payload = e.into_panic();
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic payload".to_string());
                ToolError::execution_failed(format!("memory recall task panicked: {msg}"))
            } else {
                ToolError::execution_failed(format!("memory recall task failed: {e}"))
            }
        })?;

        Ok(ToolResult::success(output.trim_end().to_string()))
    }
}

fn format_matching_memories(matching: &[MemoryHeader]) -> String {
    const MAX_RECALLED_MEMORIES: usize = 3;
    let mut output = String::new();
    for header in matching.iter().take(MAX_RECALLED_MEMORIES) {
        match std::fs::read_to_string(&header.file_path) {
            Ok(content) => {
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
                    const TRUNCATION_SUFFIX: &str = "\n[truncated]";
                    let limit = MAX_BYTES_PER_MEMORY.saturating_sub(TRUNCATION_SUFFIX.len());
                    let cutoff =
                        codesmith_agent_runtime::knowledge::entrypoint::previous_char_boundary(
                            &body_text, limit,
                        );
                    body_text = format!("{}{}", &body_text[..cutoff], TRUNCATION_SUFFIX);
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
            // A matching memory that cannot be read (permissions, file
            // removed between scan and read) must not silently vanish —
            // the model needs to know recall was degraded.
            Err(err) => {
                output.push_str(&format!(
                    "[Memory: {} — failed to read: {err}]\n\n",
                    header.filename
                ));
            }
        }
    }
    if matching.len() > MAX_RECALLED_MEMORIES {
        output.push_str(&format!(
            "[{} additional matching memories omitted]\n",
            matching.len() - MAX_RECALLED_MEMORIES
        ));
    }
    output
}
