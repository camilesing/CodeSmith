//! Directory scanning and frontmatter parsing for memory files.
//!
//! Scans the memory directory for `.md` files, parses their YAML-style
//! frontmatter (`---` delimited) to extract name, description, and type,
//! and collects `MemoryHeader` structs for relevance ranking.

use std::fs;
use std::path::{Path, PathBuf};

use super::budget::MAX_MEMORY_FILES;
use super::types::MemoryType;

/// Header metadata extracted from a memory file's frontmatter.
#[derive(Debug, Clone)]
pub struct MemoryHeader {
    /// Relative filename from the memory directory root.
    pub filename: String,
    /// Absolute path to the file on disk.
    pub file_path: PathBuf,
    /// File modification time in milliseconds since epoch.
    pub mtime_ms: i64,
    /// Description from frontmatter (used for relevance ranking).
    pub description: Option<String>,
    /// Memory type from frontmatter.
    pub memory_type: Option<MemoryType>,
}

/// Parsed frontmatter fields from a `---` delimited block.
#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub memory_type: Option<MemoryType>,
}

/// Scan the memory directory for all `.md` files and parse their frontmatter.
///
/// Returns headers sorted by modification time (most recent first).
/// Skips `MEMORY.md` entrypoint — it is handled separately by `entrypoint.rs`.
/// Respects `MAX_MEMORY_FILES` limit — older files are dropped if exceeded.
pub fn scan_memory_files(memory_dir: &Path) -> Vec<MemoryHeader> {
    let entries = collect_md_files(memory_dir);
    let mut headers: Vec<MemoryHeader> = entries
        .into_iter()
        .filter_map(|path| build_header(&path, memory_dir))
        .filter(|h| h.filename != "MEMORY.md")
        .collect();

    // Sort by mtime descending (most recent first).
    headers.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms));

    // Enforce MAX_MEMORY_FILES limit — drop oldest.
    if headers.len() > MAX_MEMORY_FILES {
        headers.truncate(MAX_MEMORY_FILES);
    }

    headers
}

/// Parse frontmatter from a file's content.
///
/// Frontmatter is a YAML-like block delimited by `---` at the start of the file:
/// ```markdown
/// ---
/// name: user role
/// description: User is a Rust developer
/// type: user
/// ---
/// Content follows...
/// ```
///
/// Uses a simple line-based parser for the three known fields (name, description, type).
/// Returns `(Frontmatter, body_text)`.
pub fn parse_frontmatter(content: &str) -> (Frontmatter, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (Frontmatter::default(), content);
    }

    // Find the closing --- delimiter.
    let after_first = &trimmed[3..];
    let rest = after_first.trim_start_matches('\n');
    let closing_delim = "\n---";
    let closing_pos = rest.find(closing_delim).or_else(|| {
        // Handle case where closing --- is at end of file without trailing newline.
        if rest.ends_with("---") {
            Some(rest.len() - 3)
        } else {
            None
        }
    });

    if closing_pos.is_none() {
        return (Frontmatter::default(), content);
    }

    let closing_pos = closing_pos.unwrap();
    let frontmatter_text = &rest[..closing_pos];
    // Skip the closing delimiter ("\n---" = 4 chars) and any leading newlines.
    let body_start = closing_pos + closing_delim.len();
    let body = rest[body_start..].trim_start_matches('\n');

    let fm = parse_frontmatter_lines(frontmatter_text);
    (fm, body)
}

fn parse_frontmatter_lines(text: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => fm.name = Some(value.to_string()),
                "description" => fm.description = Some(value.to_string()),
                "type" => fm.memory_type = MemoryType::from_str_loose(value),
                _ => {} // ignore unknown fields
            }
        }
    }
    fm
}

fn collect_md_files(memory_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !memory_dir.exists() {
        return files;
    }
    // Read directory entries (flat, no recursion — memory files live at top level).
    if let Ok(entries) = fs::read_dir(memory_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                files.push(path);
            }
        }
    }
    files
}

fn build_header(path: &Path, memory_dir: &Path) -> Option<MemoryHeader> {
    let mtime = fs::metadata(path).ok()?.modified().ok()?;
    let mtime_ms = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;

    let content = fs::read_to_string(path).ok()?;
    let (fm, _body) = parse_frontmatter(&content);

    let filename = path
        .strip_prefix(memory_dir)
        .ok()?
        .to_string_lossy()
        .to_string();

    Some(MemoryHeader {
        filename,
        file_path: path.to_path_buf(),
        mtime_ms,
        description: fm.description,
        memory_type: fm.memory_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "\
---
name: user role
description: User is a Rust developer
type: user
---

Content about preferences.";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("user role"));
        assert_eq!(fm.description.as_deref(), Some("User is a Rust developer"));
        assert_eq!(fm.memory_type, Some(MemoryType::User));
        assert!(body.starts_with("Content about preferences."));
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let content = "Just regular markdown content.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.name.is_none());
        assert!(fm.description.is_none());
        assert!(fm.memory_type.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_frontmatter_partial_fields() {
        let content = "\
---
description: Only a description
---
Body text.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.name.is_none());
        assert_eq!(fm.description.as_deref(), Some("Only a description"));
        assert!(fm.memory_type.is_none());
        assert!(body.starts_with("Body text."));
    }

    #[test]
    fn parse_frontmatter_unknown_type_defaults_none() {
        let content = "\
---
type: unknown_type
---
Body.";
        let (fm, _) = parse_frontmatter(content);
        assert!(fm.memory_type.is_none());
    }

    #[test]
    fn scan_memory_files_finds_md_files() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();

        // Create MEMORY.md (should be filtered out).
        fs::write(dir.join("MEMORY.md"), "entrypoint").unwrap();
        // Create memory files.
        fs::write(
            dir.join("user_role.md"),
            "---\nname: role\ntype: user\n---\nI am a dev.",
        )
        .unwrap();
        fs::write(dir.join("feedback.md"), "---\ntype: feedback\n---\nDo X.").unwrap();

        let headers = scan_memory_files(dir);
        assert_eq!(headers.len(), 2);
        // MEMORY.md should be excluded.
        assert!(headers.iter().all(|h| h.filename != "MEMORY.md"));
    }

    #[test]
    fn scan_memory_files_empty_dir() {
        let tmp = tempdir().unwrap();
        let headers = scan_memory_files(tmp.path());
        assert!(headers.is_empty());
    }

    #[test]
    fn scan_memory_files_nonexistent_dir() {
        let headers = scan_memory_files(Path::new("/nonexistent/path"));
        assert!(headers.is_empty());
    }
}