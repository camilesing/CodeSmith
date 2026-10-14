//! Bounded, cached workspace index behind `@path` completion.
//!
//! rustyline calls completion synchronously on the render thread, so the walk
//! is capped on both ends: at most `limit` candidate paths are kept and at
//! most [`MAX_VISITED`] entries are visited. A rebuild happens at most once
//! per [`TTL`], and only when the user actually asks for a path completion.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - The index is a snapshot. A file created after the last build is
//!   completed only once the TTL expires and the walk runs again.
//! - Symlinked directories are not followed (the walker's default), and
//!   `.gitignore` is honoured through the `ignore` crate, so ignored trees
//!   (build output, `target/`, `node_modules/`) never appear as candidates.
//!   Hidden files are skipped too (the walker's `standard_filters`), so a
//!   dotfile path is typed out rather than completed.
//! - Matching is a plain prefix test against the relative path; there is no
//!   fuzzy matching and no per-directory drill-down cache.
//! - Paths are completed relative to the workspace root, the way the `@`
//!   mentions and shell commands they feed are resolved.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;

/// How long a built index is reused before the next completion rebuilds it.
const TTL: Duration = Duration::from_secs(10);
/// Hard cap on entries visited in one walk, so a pathological tree cannot
/// stall the render thread even when the candidate limit is never reached
/// (a deep tree of ignored-then-reincluded files, say).
const MAX_VISITED: usize = 50_000;

/// One completable path, relative to the workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `/`-separated relative path, as typed in a prompt.
    pub path: String,
    pub is_dir: bool,
}

/// A cached workspace path listing for `@mention` completion.
pub struct PathIndex {
    root: PathBuf,
    /// Maximum directory depth walked; `0` means unlimited
    /// (`Settings::mention_walk_depth`).
    depth: usize,
    /// Maximum candidates kept (`Settings::mention_menu_limit`).
    limit: usize,
    built: Option<Instant>,
    entries: Vec<Entry>,
}

impl PathIndex {
    pub fn new(root: PathBuf, depth: usize, limit: usize) -> Self {
        Self {
            root,
            depth,
            // `depth == 0` means unlimited, but `limit == 0` would break
            // the walk on its first tick and silently disable ALL
            // @-completion (a `mention_menu_limit = 0` in config); a zero
            // or misconfigured value degrades to the smallest workable
            // menu instead of turning the feature off.
            limit: limit.max(1),
            built: None,
            entries: Vec::new(),
        }
    }

    /// Candidates whose relative path starts with `partial`, sorted. The
    /// index rebuilds when it is missing or older than [`TTL`].
    ///
    /// A directory candidate is returned as-is; the caller appends the `/`
    /// so the next completion continues one level deeper.
    pub fn complete(&mut self, partial: &str) -> Vec<Entry> {
        self.ensure_fresh();
        let mut hits: Vec<Entry> = self
            .entries
            .iter()
            .filter(|entry| starts_with_ci(&entry.path, partial))
            .cloned()
            .collect();
        hits.truncate(self.limit);
        hits
    }

    fn ensure_fresh(&mut self) {
        match self.built {
            Some(at) if at.elapsed() < TTL => {}
            _ => self.rebuild(),
        }
    }

    fn rebuild(&mut self) {
        self.entries = walk(&self.root, self.depth, self.limit);
        self.built = Some(Instant::now());
    }
}

/// Walk `root` for completable paths. Kept free so the walk can be tested
/// against a temporary directory without a `PathIndex`.
fn walk(root: &Path, depth: usize, _limit: usize) -> Vec<Entry> {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true) // .gitignore, .ignore, hidden files
        // Honour `.gitignore` even when the workspace is not a git checkout
        // (the default only reads it inside a repository).
        .require_git(false)
        .follow_links(false);
    if depth > 0 {
        builder.max_depth(Some(depth));
    }

    let mut entries = Vec::new();
    // Walk cap is deliberately larger than the menu `limit`: truncating the
    // walk at the menu limit kept whichever paths readdir happened to yield
    // first (pre-sort), so matching files beyond the cap were never indexed
    // and prefix queries could return zero hits in any real repository.
    // `complete()` applies the per-query menu cap itself.
    const MAX_INDEXED: usize = 5_000;
    for (visited, result) in builder.build().enumerate() {
        if visited >= MAX_VISITED || entries.len() >= MAX_INDEXED {
            break;
        }
        let Ok(entry) = result else { continue };
        if entry.depth() == 0 {
            continue; // the root itself is not a candidate
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(path) = relative.to_str().map(|p| p.replace('\\', "/")) else {
            continue; // non-UTF-8 names cannot be completed in the prompt
        };
        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        entries.push(Entry { path, is_dir });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

/// Case-insensitive prefix test. The plain byte `starts_with` returned
/// zero hits for `Src/` or `readme` on case-insensitive filesystems (the
/// default on macOS and Windows) even though the shell/@-mention
/// resolution feeding completion finds the file; the on-disk casing is
/// still what gets completed, so there is no ambiguity cost.
fn starts_with_ci(path: &str, partial: &str) -> bool {
    let mut path_chars = path.chars().flat_map(char::to_lowercase);
    partial
        .chars()
        .flat_map(char::to_lowercase)
        .all(|c| path_chars.next() == Some(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_match_is_case_insensitive() {
        assert!(starts_with_ci("README.md", "readme"));
        assert!(starts_with_ci("src/main.rs", "Src/"));
        assert!(!starts_with_ci("src/main.rs", "main"));
    }

    #[test]
    fn zero_limit_degrades_to_one_instead_of_disabling_completion() {
        let dir = fixture();
        let mut index = PathIndex::new(dir.path().to_path_buf(), 6, 0);
        let hits = index.complete("src/");
        assert!(!hits.is_empty(), "limit 0 must not disable completion");
    }

    /// A workspace with a gitignored `target/`, a nested source file, and a
    /// directory that should be offered with its `/` continuation.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp workspace");
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/nested/deep.rs"), "").unwrap();
        std::fs::write(root.join("target/debug/artifact"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        dir
    }

    #[test]
    fn completes_paths_relative_to_the_root_and_honours_gitignore() {
        let dir = fixture();
        let entries = walk(dir.path(), 6, 128);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(
            paths.contains(&"src/main.rs"),
            "source file is completable: {paths:?}"
        );
        assert!(
            paths.contains(&"src/nested/deep.rs"),
            "nested source is completable: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("target/")),
            "gitignored build output stays out: {paths:?}"
        );
    }

    #[test]
    fn depth_caps_the_walk_and_dirs_are_flagged() {
        let dir = fixture();
        let entries = walk(dir.path(), 1, 128);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "src"], "depth 1 lists children only");
        assert!(entries.iter().any(|e| e.is_dir && e.path == "src"));

        let entries = walk(dir.path(), 2, 128);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"));
        assert!(
            !paths.contains(&"src/nested/deep.rs"),
            "depth 2 stops there"
        );
    }

    #[test]
    fn prefix_filters_and_limit_truncates() {
        let dir = fixture();
        let mut index = PathIndex::new(dir.path().to_path_buf(), 6, 128);
        let hits = index.complete("src/");
        let paths: Vec<&str> = hits.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            ["src/main.rs", "src/nested", "src/nested/deep.rs"],
            "a flat index lists everything under the prefix"
        );
        assert!(index.complete("src/main.rsx").is_empty());

        let mut capped = PathIndex::new(dir.path().to_path_buf(), 6, 2);
        assert_eq!(capped.complete("").len(), 2, "limit truncates candidates");
    }
}
