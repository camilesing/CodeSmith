//! Deduplication of surfaced memories against tool result file paths.
//!
//! When the model has already read, written, or edited a memory file via
//! tool calls in the current turn, the prefetch should not re-surface
//! that same content. This module filters out duplicate paths.

use std::path::PathBuf;

use super::types::SurfacedMemory;

/// Filter surfaced memories that overlap with file paths from tool results.
///
/// A memory is considered duplicate if its canonical path matches any
/// path in the tool result set. Canonicalization handles symlinks and
/// relative path differences.
pub fn filter_duplicate_attachments(
    surfaced: &[SurfacedMemory],
    tool_result_paths: &[PathBuf],
) -> Vec<SurfacedMemory> {
    // Pre-compute canonical tool result paths for efficient lookup.
    let canonical_tool_paths: Vec<PathBuf> = tool_result_paths
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();

    surfaced
        .iter()
        .filter(|mem| {
            let mem_canonical = mem.path.canonicalize().ok();
            // Keep memory if its path doesn't match any tool result path.
            // If canonicalization fails, keep it (conservative: don't filter
            // on paths we can't resolve).
            match mem_canonical {
                Some(mc) => !canonical_tool_paths.iter().any(|tp| *tp == mc),
                None => true,
            }
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surfaced_mem(path: &str) -> SurfacedMemory {
        SurfacedMemory {
            path: PathBuf::from(path),
            staleness_header: String::new(),
            content: "test content".to_string(),
            was_truncated: false,
            byte_count: 12,
        }
    }

    #[test]
    fn filters_matching_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let role_path = tmp.path().join("role.md");
        let feedback_path = tmp.path().join("feedback.md");
        std::fs::write(&role_path, "role content").unwrap();
        std::fs::write(&feedback_path, "feedback content").unwrap();

        let surfaced = vec![
            surfaced_mem(role_path.to_str().unwrap()),
            surfaced_mem(feedback_path.to_str().unwrap()),
        ];
        let tool_paths = vec![role_path.clone()];

        let result = filter_duplicate_attachments(&surfaced, &tool_paths);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, feedback_path);
    }

    #[test]
    fn keeps_all_when_no_overlap() {
        let tmp = tempfile::tempdir().unwrap();
        let role_path = tmp.path().join("role.md");
        let feedback_path = tmp.path().join("feedback.md");
        let other_path = tmp.path().join("other.rs");
        std::fs::write(&role_path, "role").unwrap();
        std::fs::write(&feedback_path, "feedback").unwrap();
        std::fs::write(&other_path, "other").unwrap();

        let surfaced = vec![
            surfaced_mem(role_path.to_str().unwrap()),
            surfaced_mem(feedback_path.to_str().unwrap()),
        ];
        let tool_paths = vec![other_path];

        let result = filter_duplicate_attachments(&surfaced, &tool_paths);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn keeps_all_when_tool_paths_empty() {
        let surfaced = vec![
            surfaced_mem("/tmp/memory/role.md"),
            surfaced_mem("/tmp/memory/feedback.md"),
        ];

        let result = filter_duplicate_attachments(&surfaced, &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn keeps_unresolvable_paths() {
        let surfaced = vec![surfaced_mem("/nonexistent/memory.md")];
        let tool_paths = vec![PathBuf::from("/nonexistent/memory.md")];

        // Both paths fail canonicalize → conservative: keep the memory.
        let result = filter_duplicate_attachments(&surfaced, &tool_paths);
        assert_eq!(result.len(), 1);
    }
}
