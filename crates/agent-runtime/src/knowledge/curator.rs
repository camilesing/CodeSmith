//! Memory curator — P3-8 step 3, the deterministic half of "sleep learning".
//!
//! The online loop only ever *appends* to memory (`remember` dedupes exact
//! lines, nothing prunes). Over a long life the MEMORY.md entrypoint
//! therefore collects exact-duplicate pointer lines, pointers to topic
//! files that no longer exist, and drifts over the budget that
//! `entrypoint.rs` enforces by truncating. This module is the offline
//! counterpart: a deterministic consolidation pass over the index plus a
//! validator for LLM-proposed rewrites. It never runs inside a session —
//! the caller is an explicit offline command (`codesmith memory
//! consolidate`), honoring the dual-loop separation from P3-8: the online
//! execution loop records; only the offline evolution loop rewrites.
//!
//! Deterministic passes (no LLM, no judgment):
//! 1. exact-duplicate pointer-line removal (first occurrence wins);
//! 2. stale-pointer removal (target topic file missing on disk);
//! 3. orphan topic files reported (present on disk, unreferenced — never
//!    deleted; content deletions are semantic decisions for the human);
//! 4. budget check against [`MAX_ENTRYPOINT_LINES`] / [`MAX_ENTRYPOINT_BYTES`].
//!
//! The LLM half (merge/description rewrite proposals) lives in the TUI
//! command; whatever it produces must pass [`validate_proposed_index`]
//! before anyone writes it: every existing topic file still referenced
//! exactly once, no unknown files introduced, pointer syntax intact,
//! budget respected. A validator the proposal cannot buy is the whole
//! point — 睡眠学习 without a gate is just an unattended rewrite.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::budget::{MAX_ENTRYPOINT_BYTES, MAX_ENTRYPOINT_LINES};

/// Result of the deterministic consolidation pass.
#[derive(Debug, Clone)]
pub struct CuratorReport {
    /// The MEMORY.md path the pass ran over.
    pub index_path: PathBuf,
    /// Index content after the deterministic passes (dedupe + stale
    /// removal). This is what `--apply` writes when no (valid) LLM
    /// proposal supersedes it.
    pub cleaned_index: String,
    /// Total pointer lines found in the input index.
    pub pointer_lines_total: usize,
    /// Exact-duplicate pointer lines removed.
    pub duplicate_pointers_removed: usize,
    /// Pointer lines removed because their target file is gone.
    pub stale_pointers_removed: Vec<String>,
    /// Topic files on disk that the index does not reference.
    pub orphan_topic_files: Vec<String>,
    /// Index (post-cleanup) exceeds [`MAX_ENTRYPOINT_LINES`].
    pub over_line_budget: bool,
    /// Index (post-cleanup) exceeds [`MAX_ENTRYPOINT_BYTES`].
    pub over_byte_budget: bool,
    /// The index file did not exist — nothing to curate.
    pub index_missing: bool,
}

/// A parsed pointer line: `- [label](target.md) — description` (the
/// `remember` tool's format, remember.rs `append_to_entrypoint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerLine {
    /// The line verbatim.
    pub raw: String,
    /// Link target filename including the `.md` extension.
    pub target: String,
}

/// Parse one index line as a remember-style pointer line.
/// `None` for headers, prose, blank lines, or malformed links.
pub fn parse_pointer_line(line: &str) -> Option<PointerLine> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("- [")?;
    let bracket_end = rest.find("](")?;
    let close_paren = rest.find(')')?;
    if close_paren < bracket_end {
        return None;
    }
    let target = &rest[bracket_end + 2..close_paren];
    if target.is_empty() || !target.ends_with(".md") || target.contains("..") {
        return None;
    }
    Some(PointerLine {
        raw: line.to_string(),
        target: target.to_string(),
    })
}

/// Run the deterministic consolidation pass over `<memory_dir>/MEMORY.md`.
/// Topic files are discovered from the directory (top-level `.md`,
/// excluding the index itself), matching `remember`'s flat layout.
pub fn curate_index(memory_dir: &Path) -> CuratorReport {
    let index_path = memory_dir.join("MEMORY.md");
    let mut report = CuratorReport {
        index_path: index_path.clone(),
        cleaned_index: String::new(),
        pointer_lines_total: 0,
        duplicate_pointers_removed: 0,
        stale_pointers_removed: Vec::new(),
        orphan_topic_files: Vec::new(),
        over_line_budget: false,
        over_byte_budget: false,
        index_missing: !index_path.exists(),
    };
    if report.index_missing {
        return report;
    }
    let original = fs::read_to_string(&index_path).unwrap_or_default();

    let topic_files_on_disk: HashSet<String> = fs::read_dir(memory_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.ends_with(".md") && name != "MEMORY.md")
                .collect()
        })
        .unwrap_or_default();

    let mut seen_lines: HashSet<String> = HashSet::new();
    let mut referenced: HashSet<String> = HashSet::new();
    let mut cleaned_lines: Vec<&str> = Vec::new();
    for line in original.lines() {
        match parse_pointer_line(line) {
            Some(pointer) => {
                report.pointer_lines_total += 1;
                if !seen_lines.insert(pointer.raw.clone()) {
                    report.duplicate_pointers_removed += 1;
                    continue;
                }
                if !topic_files_on_disk.contains(&pointer.target) {
                    report.stale_pointers_removed.push(pointer.raw);
                    continue;
                }
                referenced.insert(pointer.target);
                cleaned_lines.push(line);
            }
            None => cleaned_lines.push(line),
        }
    }

    let mut orphans: Vec<String> = topic_files_on_disk
        .difference(&referenced)
        .cloned()
        .collect();
    orphans.sort();
    report.orphan_topic_files = orphans;

    report.cleaned_index = if cleaned_lines.is_empty() {
        String::new()
    } else {
        let mut out = cleaned_lines.join("\n");
        out.push('\n');
        out
    };
    report.over_line_budget = report.cleaned_index.lines().count() > MAX_ENTRYPOINT_LINES;
    report.over_byte_budget = report.cleaned_index.len() > MAX_ENTRYPOINT_BYTES;
    report
}

/// Validate an LLM-proposed rewrite of the index before it may be written.
///
/// Contract (any violation rejects the proposal — the caller falls back to
/// the deterministic result):
/// * every topic file currently on disk is referenced exactly once;
/// * no pointer references a file that does not exist;
/// * every pointer line parses (remember-style syntax preserved);
/// * the result fits the entrypoint budgets.
pub fn validate_proposed_index(memory_dir: &Path, proposed: &str) -> Result<(), String> {
    let topic_files_on_disk: HashSet<String> = fs::read_dir(memory_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.ends_with(".md") && name != "MEMORY.md")
                .collect()
        })
        .unwrap_or_default();

    let mut referenced: HashSet<String> = HashSet::new();
    let mut pointer_count = 0usize;
    for line in proposed.lines() {
        if let Some(pointer) = parse_pointer_line(line) {
            pointer_count += 1;
            if !topic_files_on_disk.contains(&pointer.target) {
                return Err(format!(
                    "proposal references unknown topic file '{}'",
                    pointer.target
                ));
            }
            if !referenced.insert(pointer.target.clone()) {
                return Err(format!(
                    "proposal references '{}' more than once",
                    pointer.target
                ));
            }
        }
    }
    if pointer_count == 0 && !topic_files_on_disk.is_empty() {
        return Err("proposal drops every pointer line".to_string());
    }
    let missing: Vec<String> = topic_files_on_disk
        .difference(&referenced)
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "proposal drops pointers for existing topic files: {}",
            missing.join(", ")
        ));
    }
    if proposed.lines().count() > MAX_ENTRYPOINT_LINES {
        return Err(format!(
            "proposal exceeds the {}-line entrypoint budget",
            MAX_ENTRYPOINT_LINES
        ));
    }
    if proposed.len() > MAX_ENTRYPOINT_BYTES {
        return Err(format!(
            "proposal exceeds the {}-byte entrypoint budget",
            MAX_ENTRYPOINT_BYTES
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("rust-role.md"),
            "---\nname: rust\ndescription: Rust\n---\nbody",
        )
        .unwrap();
        fs::write(
            dir.join("editor.md"),
            "---\nname: ed\ndescription: editor\n---\nbody",
        )
        .unwrap();
        fs::write(
            dir.join("MEMORY.md"),
            "# Agent Memory\n\n\
             - [Rust role](rust-role.md) — User is a Rust developer\n\
             - [Rust role](rust-role.md) — User is a Rust developer\n\
             - [Gone](gone.md) — topic file was deleted\n\
             - [Editor](editor.md) — prefers vim\n",
        )
        .unwrap();
        tmp
    }

    #[test]
    fn parse_pointer_line_accepts_remember_format() {
        let pointer = parse_pointer_line("- [Rust role](rust-role.md) — User is a Rust developer")
            .expect("parses");
        assert_eq!(pointer.target, "rust-role.md");
        assert!(parse_pointer_line("# Agent Memory").is_none());
        assert!(parse_pointer_line("plain prose").is_none());
        assert!(parse_pointer_line("- [x](../escape.md) — traversal").is_none());
        assert!(parse_pointer_line("- [x](noext) — missing extension").is_none());
    }

    #[test]
    fn curate_removes_duplicates_and_stale_pointers() {
        let tmp = setup_dir();
        let report = curate_index(tmp.path());
        assert_eq!(report.pointer_lines_total, 4);
        assert_eq!(report.duplicate_pointers_removed, 1);
        assert_eq!(report.stale_pointers_removed.len(), 1);
        assert!(report.stale_pointers_removed[0].contains("gone.md"));
        assert!(report.cleaned_index.contains("rust-role.md"));
        assert!(report.cleaned_index.contains("editor.md"));
        assert!(!report.cleaned_index.contains("gone.md"));
        // The header line survives in place.
        assert!(report.cleaned_index.starts_with("# Agent Memory"));
        assert!(!report.over_line_budget);
        assert!(!report.over_byte_budget);
    }

    #[test]
    fn curate_reports_orphan_topic_files() {
        let tmp = setup_dir();
        fs::write(tmp.path().join("unreferenced.md"), "content").unwrap();
        let report = curate_index(tmp.path());
        assert_eq!(
            report.orphan_topic_files,
            vec!["unreferenced.md".to_string()]
        );
    }

    #[test]
    fn curate_handles_missing_index() {
        let tmp = tempfile::tempdir().unwrap();
        let report = curate_index(tmp.path());
        assert!(report.index_missing);
        assert_eq!(report.pointer_lines_total, 0);
    }

    #[test]
    fn validate_accepts_complete_proposal() {
        let tmp = setup_dir();
        let proposal = "# Agent Memory\n\n\
             - [Editor](editor.md) — prefers vim\n\
             - [Rust role](rust-role.md) — User is a Rust developer\n";
        assert!(validate_proposed_index(tmp.path(), proposal).is_ok());
    }

    #[test]
    fn validate_rejects_dropped_and_unknown_pointers() {
        let tmp = setup_dir();
        // Drops the editor pointer.
        let dropped = "- [Rust role](rust-role.md) — User is a Rust developer\n";
        let err = validate_proposed_index(tmp.path(), dropped).unwrap_err();
        assert!(err.contains("editor.md"), "got: {err}");

        // References a file that does not exist.
        let unknown = "- [Editor](editor.md) — vim\n- [Ghost](ghost.md) — nope\n";
        let err = validate_proposed_index(tmp.path(), unknown).unwrap_err();
        assert!(err.contains("unknown topic file"), "got: {err}");

        // Duplicate pointer for the same file.
        let dup = "- [Editor](editor.md) — vim\n- [Editor again](editor.md) — dup\n- [Rust role](rust-role.md) — dev\n";
        let err = validate_proposed_index(tmp.path(), dup).unwrap_err();
        assert!(err.contains("more than once"), "got: {err}");
    }

    #[test]
    fn validate_rejects_over_budget_proposals() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("only.md"), "content").unwrap();
        // One pointer + enough prose lines to cross the line budget (a
        // proposal full of duplicate pointers would be rejected by the
        // duplicate check before the budget check ever runs).
        let mut huge = String::from("- [Only](only.md) — x\n");
        for _ in 0..MAX_ENTRYPOINT_LINES {
            huge.push_str("filler prose line\n");
        }
        let err = validate_proposed_index(tmp.path(), &huge).unwrap_err();
        assert!(err.contains("line"), "got: {err}");
    }
}
