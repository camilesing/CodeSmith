//! MEMORY.md entrypoint loading and truncation.
//!
//! The MEMORY.md file acts as the index for the memory directory system.
//! It is loaded and truncated to fit budget limits, then composed into a
//! system prompt block that replaces the legacy `<user_memory>` block
//! when KoD is enabled.

use std::fs;
use std::path::Path;

use super::budget::{MAX_ENTRYPOINT_BYTES, MAX_ENTRYPOINT_LINES};

/// Result of loading and truncating the MEMORY.md entrypoint.
#[derive(Debug, Clone)]
pub struct EntrypointTruncation {
    /// The truncated content ready for injection.
    pub content: String,
    /// Line count of the truncated content.
    pub line_count: usize,
    /// Byte count of the truncated content.
    pub byte_count: usize,
    /// Whether content was truncated due to line limit.
    pub was_line_truncated: bool,
    /// Whether content was truncated due to byte limit.
    pub was_byte_truncated: bool,
}

/// Load the MEMORY.md entrypoint file, truncating if it exceeds limits.
///
/// Returns `None` if the file doesn't exist or is empty after trimming.
pub fn load_entrypoint(memory_dir: &Path) -> Option<EntrypointTruncation> {
    let entrypoint_path = super::paths::resolve_memory_entrypoint(memory_dir);
    let content = fs::read_to_string(&entrypoint_path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_entrypoint(trimmed))
}

/// Truncate entrypoint content to fit within line and byte budget.
fn truncate_entrypoint(content: &str) -> EntrypointTruncation {
    let mut lines: Vec<&str> = content.lines().collect();
    let was_line_truncated = lines.len() > MAX_ENTRYPOINT_LINES;
    if was_line_truncated {
        lines.truncate(MAX_ENTRYPOINT_LINES);
    }

    let mut result = lines.join("\n");
    let was_byte_truncated = result.len() > MAX_ENTRYPOINT_BYTES;
    if was_byte_truncated {
        // Find a safe char boundary near the byte limit.
        let cutoff = previous_char_boundary(&result, MAX_ENTRYPOINT_BYTES);
        let omitted = result.len() - cutoff;
        result = format!(
            "{}\n<truncated bytes={omitted} source=\"MEMORY.md\">",
            &result[..cutoff]
        );
    }

    let line_count = result.lines().count();
    let byte_count = result.len();

    EntrypointTruncation {
        content: result,
        line_count,
        byte_count,
        was_line_truncated,
        was_byte_truncated,
    }
}

pub fn previous_char_boundary(s: &str, mut idx: usize) -> usize {
    while !s.is_char_boundary(idx) && idx > 0 {
        idx -= 1;
    }
    idx
}

/// Compose the KoD knowledge block for the system prompt.
///
/// When KoD is enabled, this replaces the legacy `<user_memory>` block.
/// It loads the MEMORY.md entrypoint and wraps it with KoD-specific
/// behavioral guidance (type taxonomy, how-to-save, freshness warnings).
pub fn compose_knowledge_block(memory_dir: &Path) -> Option<String> {
    let truncation = load_entrypoint(memory_dir)?;

    let mut block = String::from("<knowledge_memory source=\"MEMORY.md\">\n");
    block.push_str(&truncation.content);

    if truncation.was_line_truncated || truncation.was_byte_truncated {
        block.push_str("\n\n[Note: MEMORY.md was truncated to fit budget limits. ");
        if truncation.was_line_truncated {
            block.push_str(&format!("Max {} lines. ", MAX_ENTRYPOINT_LINES));
        }
        if truncation.was_byte_truncated {
            block.push_str(&format!("Max {} bytes. ", MAX_ENTRYPOINT_BYTES));
        }
        block.push_str("Full content available via `read_file`.]");
    }

    block.push_str("\n</knowledge_memory>");
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_entrypoint_returns_none_for_missing() {
        let tmp = tempdir().unwrap();
        assert!(load_entrypoint(tmp.path()).is_none());
    }

    #[test]
    fn load_entrypoint_returns_none_for_empty() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("MEMORY.md");
        fs::write(&path, "   \n  \n").unwrap();
        assert!(load_entrypoint(tmp.path()).is_none());
    }

    #[test]
    fn load_entrypoint_returns_content() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("MEMORY.md");
        fs::write(&path, "- [role](user_role.md) — user profile").unwrap();
        let result = load_entrypoint(tmp.path()).unwrap();
        assert!(result.content.contains("role"));
        assert!(!result.was_line_truncated);
        assert!(!result.was_byte_truncated);
    }

    #[test]
    fn truncate_entrypoint_respects_line_limit() {
        let content = (0..300)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_entrypoint(&content);
        assert!(result.was_line_truncated);
        assert!(result.line_count <= MAX_ENTRYPOINT_LINES);
    }

    #[test]
    fn truncate_entrypoint_respects_byte_limit() {
        let content = "x".repeat(MAX_ENTRYPOINT_BYTES + 1000);
        let result = truncate_entrypoint(&content);
        assert!(result.was_byte_truncated);
        assert!(result.content.contains("<truncated bytes="));
    }

    #[test]
    fn compose_knowledge_block_wraps_in_xml() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("MEMORY.md");
        fs::write(&path, "Test content").unwrap();
        let block = compose_knowledge_block(tmp.path()).unwrap();
        assert!(block.starts_with("<knowledge_memory source=\"MEMORY.md\">"));
        assert!(block.ends_with("</knowledge_memory>"));
        assert!(block.contains("Test content"));
    }
}
