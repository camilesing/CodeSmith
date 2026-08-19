//! Workspace file walk feeding the inventory and the freshness diff.
//!
//! Uses the `ignore` crate (ripgrep's walker), so `.gitignore` rules are
//! honored even outside git trees. Hidden paths (`.git`, `.codesmith`, …)
//! are skipped, and grep_files' historical hardcoded exclusions
//! (`target/`, `node_modules/`, …) are kept as overrides for non-git
//! directories where no ignore file covers them.

use std::collections::HashSet;
use std::fs::Metadata;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use ignore::WalkBuilder;

use crate::types::Language;

/// Maximum file size that receives symbol extraction. Larger files stay
/// inventory-only, mirroring the `grep_files` 10 MB cap.
pub const MAX_EXTRACT_BYTES: u64 = 10 * 1024 * 1024;

/// Hardcoded exclusions kept consistent with `grep_files`' built-in table
/// so both surfaces see the same file universe.
const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
];

/// One walked file, normalized to a workspace-relative path.
#[derive(Debug, Clone)]
pub struct WalkEntry {
    /// Workspace-relative path with forward slashes.
    pub rel_path: String,
    pub mtime_ms: i64,
    pub size: u64,
    pub language: Option<Language>,
}

/// Walk `root` and collect every indexable file (sorted by path).
pub fn walk_workspace(root: &Path) -> Result<Vec<WalkEntry>> {
    let mut overrides = ignore::overrides::OverrideBuilder::new(root);
    for dir in DEFAULT_EXCLUDES {
        overrides
            .add(&format!("!{dir}/"))
            .with_context(|| format!("building index walk override for {dir}"))?;
    }
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .follow_links(false)
        .require_git(false)
        .overrides(overrides.build().context("building index walk overrides")?)
        .build();
    let mut out = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::debug!(%err, "index walk: skipping unreadable entry");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        out.push(WalkEntry {
            rel_path: rel.to_string_lossy().replace('\\', "/"),
            mtime_ms: mtime_ms(&meta),
            size: meta.len(),
            language: Language::from_path(rel),
        });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

/// The live-path set of a walk, for `IndexStore::delete_missing`.
pub fn live_paths(entries: &[WalkEntry]) -> HashSet<String> {
    entries.iter().map(|e| e.rel_path.clone()).collect()
}

fn mtime_ms(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walk_respects_gitignore_and_hardcoded_excludes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).expect("dirs");
        fs::create_dir_all(root.join("target/debug")).expect("dirs");
        fs::create_dir_all(root.join("node_modules/pkg")).expect("dirs");
        fs::write(root.join("src/main.rs"), "fn main() {}").expect("write");
        fs::write(root.join("src/ignored.rs"), "").expect("write");
        fs::write(root.join("target/debug/main.rs"), "").expect("write");
        fs::write(root.join("node_modules/pkg/index.js"), "").expect("write");
        fs::write(root.join(".hidden.rs"), "").expect("write");
        fs::write(root.join(".gitignore"), "src/ignored.rs\n").expect("write");

        let walked = walk_workspace(root).expect("walk");
        let paths: Vec<&str> = walked.iter().map(|e| e.rel_path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"), "{paths:?}");
        assert!(
            !paths.contains(&"src/ignored.rs"),
            "gitignore must apply: {paths:?}"
        );
        assert!(!paths.contains(&"target/debug/main.rs"), "{paths:?}");
        assert!(!paths.contains(&"node_modules/pkg/index.js"), "{paths:?}");
        assert!(
            !paths.contains(&".hidden.rs"),
            "hidden files skipped: {paths:?}"
        );
        assert!(
            !paths.contains(&".gitignore"),
            "hidden files skipped: {paths:?}"
        );

        let main = walked
            .iter()
            .find(|e| e.rel_path == "src/main.rs")
            .expect("main");
        assert_eq!(main.language, Some(Language::Rust));
        assert_eq!(main.size, 12);
    }
}
