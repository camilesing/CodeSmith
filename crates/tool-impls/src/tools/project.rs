//! Project mapping tool for understanding codebase structure.

use anyhow::Result;
use async_trait::async_trait;
use codesmith_agent_runtime::utils::{is_key_file, project_tree, summarize_project};
use serde::Serialize;
use serde_json::{Value, json};

use codesmith_agent_runtime::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64,
};

pub struct ProjectMapTool;

#[derive(Debug, Serialize)]
struct ProjectMap {
    tree: String,
    summary: String,
    key_files: Vec<String>,
}

#[async_trait]
impl ToolSpec for ProjectMapTool {
    fn name(&self) -> &'static str {
        "project_map"
    }

    fn description(&self) -> &'static str {
        "Get a high-level map of the project structure, including key files and a tree view."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "description": "Maximum depth for the tree view (default: 3, clamped to 1-10)."
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let max_depth = usize::try_from(optional_u64(&input, "max_depth", 3)?)
            .map(|d| d.clamp(1, 10))
            .map_err(|_| ToolError::invalid_input("max_depth out of range"))?;
        let root = context.workspace.clone();
        let map = tokio::task::spawn_blocking(move || generate_project_map(&root, max_depth))
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))??;
        ToolResult::json(&map).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

fn generate_project_map(root: &std::path::Path, max_depth: usize) -> Result<ProjectMap, ToolError> {
    let tree = project_tree(root, max_depth);
    let summary = summarize_project(root);

    // For key_files, we can just do a quick scan since summarize_project doesn't return them directly anymore
    let mut key_files = Vec::new();
    let mut builder = ignore::WalkBuilder::new(root);
    builder.hidden(false).follow_links(false).max_depth(Some(2));
    let walker = builder.build();

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            // A partially scanned workspace must still be diagnosable:
            // log skipped entries instead of silently dropping them.
            Err(err) => {
                tracing::debug!(error = %err, "project_map: skipping unreadable entry");
                continue;
            }
        };
        if entry.file_type().is_some_and(|ft| ft.is_symlink()) {
            continue;
        }
        if is_key_file(entry.path())
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            key_files.push(rel.to_string_lossy().to_string());
        }
    }
    key_files.sort_unstable();

    Ok(ProjectMap {
        tree,
        summary,
        key_files,
    })
}
