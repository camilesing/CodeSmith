//! CLAUDE.md four-tier memory loading + `@include` expansion.
//!
//! Mirrors Claude Code's tiered trust model for memory/config files (finding
//! F1 of the extra-findings analysis): instructions are collected from four
//! trust tiers in priority order — Managed (`/etc/...`), User (`~/.codesmith/…`),
//! Project (`{cwd}/WHALE.md`/`AGENTS.md`/`CLAUDE.md`/…), Local
//! (`.claude/rules/*.md`, `.codesmith/rules/*.md`) — and `@include <path>`
//! directives are expanded inline with a hard depth cap, symlink-stable
//! deduplication, and an exclude list.
//!
//! The merged result is labelled per tier (`<!-- tier: … -->`) so the model
//! can tell which level any rule comes from when two tiers disagree; the
//! later (more specific) tier wins the last word, matching Claude Code's
//! "project overrides global" semantics.
//!
//! CodeSmith previously loaded only two tiers (Project + User global) via
//! [`crate::project_context`], picked the first matching project file, had
//! no Managed tier, no Local rules glob, and no `@include` support. This
//! module generalizes the merge from two tiers to N and is wired into
//! `project_context::load_project_context_with_parents_and_home`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::project_context::{
    GLOBAL_AGENTS_RELATIVE_PATH, GLOBAL_AGENTS_VENDOR_NEUTRAL_PATH, GLOBAL_WHALE_RELATIVE_PATH,
    GLOBAL_WHALE_VENDOR_NEUTRAL_PATH, PROJECT_CONTEXT_FILES, load_context_file,
};
use crate::workspace_trust::{canonicalize_or_keep, expand_path};

/// Process-global exclude list published by the engine at construction (from
/// `EngineConfig.memory_excludes`). Kept here — not on `EngineConfig` at read
/// time — so config-less load sites (the per-turn prompt reloader in
/// `prompts.rs`, `Session::new`) can honour excludes without threading
/// `EngineConfig` through every call. `OnceLock` keeps the publish safe
/// under the `#![deny(unsafe_code)]` crate policy.
static MEMORY_EXCLUDES: OnceLock<Vec<String>> = OnceLock::new();

/// Publish the memory exclude list for the process. Called once by
/// `Engine::new_runtime` from `EngineConfig.memory_excludes`. A second call
/// (e.g. engine rebuild in-process) is a no-op: excludes are config-time
/// data and a stale value is preferable to a race.
pub fn set_memory_excludes(excludes: Vec<String>) {
    let _ = MEMORY_EXCLUDES.set(excludes);
}

/// The effective exclude list: the published config value merged with the
/// `CODESMITH_MEMORY_EXCLUDES` env var (colon-separated), so users can also
/// override at the shell without editing config. Duplicates are dropped.
pub fn memory_excludes() -> Vec<String> {
    let mut out: Vec<String> = MEMORY_EXCLUDES
        .get()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if let Ok(env_val) = std::env::var("CODESMITH_MEMORY_EXCLUDES") {
        out.extend(
            env_val
                .split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    dedup_preserve_order(&mut out);
    out
}

fn dedup_preserve_order(v: &mut Vec<String>) {
    let mut seen: HashSet<String> = HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

/// Maximum nesting depth for `@include` expansion. A chain longer than this
/// is silently truncated to bound recursion and keep prompt assembly
/// cache-friendly. Mirrors Claude Code's `MAX_INCLUDE_DEPTH = 5`: the root
/// file plus up to five include levels load; the sixth include level is
/// dropped.
pub const MAX_INCLUDE_DEPTH: usize = 5;

/// The four memory trust tiers, in ascending specificity (later tiers
/// override earlier ones when they conflict).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryTier {
    /// Organisation-managed policy (`/etc/deepseek/CLAUDE.md`).
    Managed,
    /// User-wide preferences (`~/.codesmith/AGENTS.md`, …).
    User,
    /// Project-local instructions (`{cwd}/WHALE.md`, `AGENTS.md`, `CLAUDE.md`,
    /// …, including a parent-directory walk).
    Project,
    /// Workspace-local rule snippets (`.claude/rules/*.md`,
    /// `.codesmith/rules/*.md`).
    Local,
}

impl MemoryTier {
    /// Lowercase label emitted in the `<!-- tier: … -->` merge comment.
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}

/// If `line` is an `@include <path>` directive, return the raw target text
/// (pre-`~`/env expansion). Returns `None` for prose or empty targets. A
/// whitespace separator is required so prose like "see @include" in a
/// sentence is not treated as a directive.
fn match_include_directive(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("@include")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let target = rest.trim();
    if target.is_empty() {
        return None;
    }
    Some(target)
}

/// Resolve an `@include` target (raw text from [`match_include_directive`])
/// against `base`: `~`/env-expanded via [`workspace_trust::expand_path`],
/// then treated as relative to `base` unless already absolute.
fn resolve_include_target(target: &str, base: &Path) -> PathBuf {
    let expanded = expand_path(target);
    if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    }
}

/// Read a single memory file and recursively expand `@include` directives
/// inline at their positions, returning the file's full text (with includes
/// spliced in) tagged with its `tier`.
///
/// Bounding rules (all silent — a skipped include simply contributes nothing,
/// matching Claude Code's behaviour so a broken include never aborts the
/// whole tier):
///
/// - `depth > MAX_INCLUDE_DEPTH` → truncated.
/// - Symlink-stable dedup via [`canonicalize_or_keep`] into `processed`; a
///   file already seen in this merge is not loaded again (cycle safe).
/// - A path whose canonical form matches any entry in `excludes` is dropped.
/// - Files failing [`load_context_file`] (missing, oversized, empty) are
///   skipped.
fn process_memory_file(
    path: &Path,
    tier: MemoryTier,
    processed: &mut HashSet<PathBuf>,
    excludes: &[PathBuf],
    depth: usize,
) -> Option<(MemoryTier, String)> {
    if depth > MAX_INCLUDE_DEPTH {
        return None;
    }
    let canon = canonicalize_or_keep(path);
    if !processed.insert(canon.clone()) {
        return None;
    }
    if excludes.iter().any(|e| e == &canon) {
        return None;
    }
    let content = load_context_file(path).ok()?;
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut out = String::new();
    // `split_inclusive('\n')` keeps the trailing newline so non-directive
    // lines round-trip byte-for-byte; directive lines are dropped and
    // replaced by the included file's expanded text.
    for line in content.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        if let Some(target) = match_include_directive(body) {
            let resolved = resolve_include_target(target, &base);
            if let Some((_, inc_text)) =
                process_memory_file(&resolved, tier, processed, excludes, depth + 1)
            {
                out.push_str(&inc_text);
                if !inc_text.ends_with('\n') {
                    out.push('\n');
                }
            }
            continue;
        }
        out.push_str(body);
        out.push_str(nl);
    }
    Some((tier, out))
}

/// Managed tier: `/etc/deepseek/CLAUDE.md` (legacy) then
/// `/etc/codesmith/CLAUDE.md`, mirroring the `default_managed_config_path`
/// convention. Absent on platforms without `/etc`.
fn load_managed_tier(
    excludes: &[PathBuf],
    processed: &mut HashSet<PathBuf>,
) -> Option<(MemoryTier, String)> {
    let candidates = [
        Path::new("/etc/deepseek/CLAUDE.md"),
        Path::new("/etc/codesmith/CLAUDE.md"),
    ];
    for candidate in candidates {
        if candidate.exists() && candidate.is_file() {
            if let Some(found) =
                process_memory_file(candidate, MemoryTier::Managed, processed, excludes, 0)
            {
                return Some(found);
            }
        }
    }
    None
}

/// User tier: reuse the `project_context` global candidate list (`.codesmith`
/// → `.agents`, for both `WHALE.md` and `AGENTS.md`).
fn load_user_tier(
    home: &Path,
    excludes: &[PathBuf],
    processed: &mut HashSet<PathBuf>,
) -> Option<(MemoryTier, String)> {
    let candidates: &[&[&str]] = &[
        GLOBAL_WHALE_RELATIVE_PATH,
        GLOBAL_AGENTS_RELATIVE_PATH,
        GLOBAL_WHALE_VENDOR_NEUTRAL_PATH,
        GLOBAL_AGENTS_VENDOR_NEUTRAL_PATH,
    ];
    for candidate in candidates {
        let mut path = home.to_path_buf();
        for component in *candidate {
            path.push(component);
        }
        if path.exists() && path.is_file() {
            if let Some(found) =
                process_memory_file(&path, MemoryTier::User, processed, excludes, 0)
            {
                return Some(found);
            }
        }
    }
    None
}

/// First match of [`PROJECT_CONTEXT_FILES`] in `workspace`, then a
/// parent-directory walk (monorepo root support), reusing the existing
/// `project_context` parent-walk behaviour.
fn load_project_tier(
    workspace: &Path,
    excludes: &[PathBuf],
    processed: &mut HashSet<PathBuf>,
) -> Option<(MemoryTier, String)> {
    if let Some(found) = find_project_file(workspace) {
        if let Some(c) = process_memory_file(&found, MemoryTier::Project, processed, excludes, 0) {
            return Some(c);
        }
    }
    let mut current = workspace.parent();
    while let Some(parent) = current {
        if let Some(found) = find_project_file(parent) {
            if let Some(c) =
                process_memory_file(&found, MemoryTier::Project, processed, excludes, 0)
            {
                return Some(c);
            }
        }
        current = parent.parent();
    }
    None
}

/// First existing project-context filename in `dir`, by `PROJECT_CONTEXT_FILES`
/// priority.
fn find_project_file(dir: &Path) -> Option<PathBuf> {
    for filename in PROJECT_CONTEXT_FILES {
        let path = dir.join(filename);
        if path.exists() && path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Local tier: `*.md` rule snippets under `.claude/rules/` and
/// `.codesmith/rules/` (depth 1, sorted). Each file is read via
/// [`process_memory_file`] so `@include` works inside rules too.
fn load_local_tier(
    workspace: &Path,
    excludes: &[PathBuf],
    processed: &mut HashSet<PathBuf>,
) -> Vec<(MemoryTier, String)> {
    let dirs = [
        workspace.join(".claude").join("rules"),
        workspace.join(".codesmith").join("rules"),
    ];
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if let Ok(read_dir) = std::fs::read_dir(&dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") && path.is_file() {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    let mut out = Vec::new();
    for file in files {
        if let Some(found) = process_memory_file(&file, MemoryTier::Local, processed, excludes, 0) {
            out.push(found);
        }
    }
    out
}

/// Collect Managed → User → Project → Local tiers, expand `@include`, apply
/// symlink-stable dedup and the exclude list, and merge the results with
/// `<!-- tier: … -->` labels. Returns an empty `String` when no tier
/// resolves to non-empty content, so callers can fall back to other sources.
///
/// `excludes` is a list of raw path strings (`~`/env-expanded and
/// canonicalized internally); entries that don't resolve to a real file are
/// simply never matched.
#[must_use]
pub fn load_all_memory_tiers(workspace: &Path, home: Option<&Path>, excludes: &[String]) -> String {
    let exclude_paths: Vec<PathBuf> = excludes
        .iter()
        .map(|s| canonicalize_or_keep(&expand_path(s)))
        .collect();
    let mut processed: HashSet<PathBuf> = HashSet::new();
    let mut blocks: Vec<(MemoryTier, String)> = Vec::new();

    if let Some(managed) = load_managed_tier(&exclude_paths, &mut processed) {
        blocks.push(managed);
    }
    if let Some(home) = home {
        if let Some(user) = load_user_tier(home, &exclude_paths, &mut processed) {
            blocks.push(user);
        }
    }
    if let Some(project) = load_project_tier(workspace, &exclude_paths, &mut processed) {
        blocks.push(project);
    }
    blocks.extend(load_local_tier(workspace, &exclude_paths, &mut processed));

    if blocks.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (tier, content) in blocks {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let _ = write!(out, "<!-- tier: {} -->\n{}", tier.as_label(), trimmed);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::path_buf_push_overwrite)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    #[test]
    fn four_tiers_merge_in_priority_order_with_labels() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();

        // User tier (fake home).
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".codesmith")).unwrap();
        write(
            &home.join(".codesmith").join("AGENTS.md"),
            "# User rules\nuser-only",
        );

        // Project tier.
        write(&ws.join("AGENTS.md"), "# Project rules\nproject-only");

        // Local tier.
        let rules = ws.join(".codesmith").join("rules");
        fs::create_dir_all(&rules).unwrap();
        write(&rules.join("01.md"), "# Local rule one");
        write(&rules.join("02.md"), "# Local rule two");

        let merged = load_all_memory_tiers(ws, Some(&home), &[]);

        // Priority order: user before project before local.
        let user_pos = merged.find("user-only").unwrap();
        let project_pos = merged.find("project-only").unwrap();
        let local_pos = merged.find("Local rule two").unwrap();
        assert!(user_pos < project_pos);
        assert!(project_pos < local_pos);

        // Tier labels present.
        assert!(merged.contains("<!-- tier: user -->"));
        assert!(merged.contains("<!-- tier: project -->"));
        assert!(merged.contains("<!-- tier: local -->"));
        // Managed label absent (no /etc file).
        assert!(!merged.contains("<!-- tier: managed -->"));
    }

    #[test]
    fn include_directive_resolves_an_external_file_inline() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();
        let included = ws.join("extra.md");
        write(&included, "INCLUDED_BODY");
        write(&ws.join("AGENTS.md"), "before\n@include extra.md\nafter");

        let merged = load_all_memory_tiers(ws, None, &[]);
        let before = merged.find("before").unwrap();
        let inc = merged.find("INCLUDED_BODY").unwrap();
        let after = merged.find("after").unwrap();
        assert!(before < inc);
        assert!(inc < after);
        // The directive line itself is dropped.
        assert!(!merged.contains("@include extra.md"));
    }

    #[test]
    fn include_depth_limit_truncates_the_sixth_level() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();
        // Chain: AGENTS -> f1 -> f2 -> f3 -> f4 -> f5 -> f6 -> f7.
        // process_memory_file depths: AGENTS=0, f1=1, …, f5=5 (loads),
        // f6=6 (truncated, since 6 > MAX_INCLUDE_DEPTH), f7 unreachable.
        for i in 1..=6 {
            write(
                &ws.join(format!("f{i}.md")),
                &format!("@include f{}.md\nL{i}", i + 1),
            );
        }
        write(&ws.join("f7.md"), "L7");
        write(&ws.join("AGENTS.md"), "@include f1.md\nroot");

        let merged = load_all_memory_tiers(ws, None, &[]);

        assert!(merged.contains("root"), "root (depth 0) should load");
        // Five include levels (depth 1..=5) load.
        for i in 1..=5 {
            assert!(
                merged.contains(&format!("L{i}")),
                "include level {i} (depth {i}) should load"
            );
        }
        // The sixth include level (depth 6) is truncated.
        assert!(
            !merged.contains("L6"),
            "L6 (depth 6, beyond MAX_INCLUDE_DEPTH) must be truncated"
        );
        assert!(!merged.contains("L7"));
    }

    #[test]
    fn include_cycle_does_not_infinite_loop() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();
        write(&ws.join("a.md"), "a-start\n@include b.md\na-end");
        write(&ws.join("b.md"), "b-start\n@include a.md\nb-end");
        write(&ws.join("AGENTS.md"), "@include a.md\nroot");

        let merged = load_all_memory_tiers(ws, None, &[]);
        // a is inlined once (via the root include); the b -> a back-edge is
        // deduped, so "a-start" appears exactly once.
        assert_eq!(merged.matches("a-start").count(), 1);
        assert_eq!(merged.matches("b-start").count(), 1);
    }

    #[test]
    fn symlink_dedup_loads_a_file_once() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();
        let real = ws.join("real.md");
        write(&real, "REAL_BODY");
        // Two project candidates that both resolve to the same canonical file
        // via a symlink, plus a second include of the same path directly.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, ws.join("link.md")).unwrap();
            write(
                &ws.join("AGENTS.md"),
                "@include real.md\n@include link.md\nroot",
            );
            let merged = load_all_memory_tiers(ws, None, &[]);
            // Symlink dedup: the body appears once despite two include paths.
            assert_eq!(merged.matches("REAL_BODY").count(), 1);
        }
        #[cfg(not(unix))]
        {
            // Symlinks aren't reliably available on non-unix; just ensure no panic.
            let _ = load_all_memory_tiers(ws, None, &[]);
        }
    }

    #[test]
    fn exclude_list_drops_a_matched_path() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();
        let secret = ws.join("secret.md");
        write(&secret, "SECRET_BODY");
        write(&ws.join("AGENTS.md"), "@include secret.md\nroot");

        let excluded = vec![secret.to_string_lossy().to_string()];
        let merged = load_all_memory_tiers(ws, None, &excluded);
        assert!(!merged.contains("SECRET_BODY"));
        assert!(merged.contains("root"));
    }

    #[test]
    fn local_rules_glob_loads_multiple_files_sorted() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();
        let rules = ws.join(".claude").join("rules");
        fs::create_dir_all(&rules).unwrap();
        write(&rules.join("zebra.md"), "# Z");
        write(&rules.join("apple.md"), "# A");
        write(&rules.join("notmd.txt"), "ignore me");

        let merged = load_all_memory_tiers(ws, None, &[]);
        let a = merged.find("# A").unwrap();
        let z = merged.find("# Z").unwrap();
        assert!(a < z, "rules must be sorted: apple before zebra");
        assert!(!merged.contains("ignore me"));
    }

    #[test]
    fn managed_tier_absent_without_etc_file() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();
        write(&ws.join("AGENTS.md"), "project-only");
        let merged = load_all_memory_tiers(ws, None, &[]);
        assert!(!merged.contains("<!-- tier: managed -->"));
        assert!(merged.contains("<!-- tier: project -->"));
    }

    #[test]
    fn empty_workspace_returns_empty_string() {
        let tmp = tempdir().unwrap();
        let merged = load_all_memory_tiers(tmp.path(), None, &[]);
        assert!(merged.is_empty());
    }

    #[test]
    fn match_include_directive_extracts_targets() {
        // Directive with relative + home-anchored targets.
        assert_eq!(
            match_include_directive("@include ../other.md"),
            Some("../other.md")
        );
        assert_eq!(
            match_include_directive("  @include ~/notes.md"),
            Some("~/notes.md")
        );
        // Prose / no-separator / empty target are not directives.
        assert_eq!(match_include_directive("see @include in the docs"), None);
        assert_eq!(match_include_directive("@includex"), None);
        assert_eq!(match_include_directive("@include   "), None);
        assert_eq!(match_include_directive("plain text"), None);
    }

    #[test]
    fn resolve_include_target_relative_and_absolute() {
        let base = Path::new("/repo/sub");
        // Relative target joins onto base.
        assert_eq!(
            resolve_include_target("../other.md", base),
            Path::new("/repo/sub/../other.md")
        );
        // Absolute target is used as-is.
        assert_eq!(
            resolve_include_target("/etc/notes.md", base),
            Path::new("/etc/notes.md")
        );
    }
}
