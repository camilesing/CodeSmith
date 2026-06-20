//! Plan file management — persistence, slug generation, directory resolution.
//!
//! Plans are stored at `~/.codesmith/plans/{slug}.md`. The slug is a short
//! identifier derived from a UUID v4 (e.g. `plan_a3f2b1c4`), providing
//! uniqueness without requiring the `rand` crate. A future iteration may
//! switch to word-pair slugs (adjective-noun) for readability once `rand`
//! is added as a workspace dependency.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Prefix for plan slugs derived from UUIDs.
const PLAN_SLUG_PREFIX: &str = "plan_";

/// Resolve the plans directory: `~/.codesmith/plans/`.
///
/// Creates the directory if it doesn't exist.
pub fn plans_dir() -> Result<PathBuf> {
    let dir = codesmith_config::codesmith_home()?.join("plans");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create plans directory at {}", dir.display()))?;
    Ok(dir)
}

/// Generate a unique plan slug.
///
/// Uses the first 8 hex characters of a UUID v4, prefixed with `plan_`.
/// Example: `plan_a3f2b1c4`. Checks for collision with existing plan files
/// and retries up to 10 times if a collision is found.
pub fn generate_plan_slug() -> Result<String> {
    let dir = plans_dir()?;
    for _ in 0..10 {
        let uuid = uuid::Uuid::new_v4();
        let hex = uuid.to_string().replace('-', "");
        let slug = format!("{PLAN_SLUG_PREFIX}{hex}");
        let path = dir.join(format!("{slug}.md"));
        if !path.exists() {
            return Ok(slug);
        }
    }
    // Fallback: use full UUID to guarantee uniqueness
    let uuid = uuid::Uuid::new_v4();
    Ok(format!("{PLAN_SLUG_PREFIX}{uuid}"))
}

/// Resolve the plan file path for a given slug.
pub fn plan_file_path(slug: &str) -> Result<PathBuf> {
    Ok(plans_dir()?.join(format!("{slug}.md")))
}

/// Write plan content to the file for the given slug.
///
/// Creates the plans directory if it doesn't exist.
pub fn write_plan_file(slug: &str, content: &str) -> Result<PathBuf> {
    let path = plan_file_path(slug)?;
    fs::write(&path, content)
        .with_context(|| format!("failed to write plan file at {}", path.display()))?;
    Ok(path)
}

/// Read plan content from the file for the given slug.
///
/// Returns `Ok(None)` if the plan file does not exist.
pub fn read_plan_file(slug: &str) -> Result<Option<String>> {
    let path = plan_file_path(slug)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read plan file at {}", path.display()))?;
    Ok(Some(content))
}

/// Delete the plan file for the given slug.
pub fn delete_plan_file(slug: &str) -> Result<()> {
    let path = plan_file_path(slug)?;
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to delete plan file at {}", path.display()))?;
    }
    Ok(())
}

/// Find the most recent plan file by modification time.
///
/// Used for session recovery: when resuming a session, we check whether
/// a plan file still exists on disk and offer to continue plan mode.
/// Returns `(slug, content)` if a plan file is found, or `None` otherwise.
pub fn find_recent_plan() -> Result<Option<(String, String)>> {
    let dir = plans_dir()?;
    if !dir.exists() {
        return Ok(None);
    }

    let mut most_recent: Option<(String, std::time::SystemTime, String)> = None;
    for entry in fs::read_dir(&dir).with_context(|| "failed to read plans directory")? {
        let entry = entry.with_context(|| "failed to read plans directory entry")?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.ends_with(".md") || !name_str.starts_with(PLAN_SLUG_PREFIX) {
            continue;
        }

        let mtime = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", name_str))?
            .modified()
            .ok();

        let slug = name_str.trim_end_matches(".md");
        let path = entry.path();
        let content = fs::read_to_string(&path).ok();

        if let (Some(mtime), Some(content)) = (mtime, content) {
            match &most_recent {
                Some((_, prev_mtime, _)) if mtime <= *prev_mtime => {}
                _ => most_recent = Some((slug.to_string(), mtime, content)),
            }
        }
    }

    Ok(most_recent.map(|(slug, _, content)| (slug, content)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ScopedCodeSmithHome, lock_test_env};

    #[test]
    fn generate_plan_slug_starts_with_prefix() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let slug = generate_plan_slug().expect("slug");
        assert!(slug.starts_with("plan_"));
    }

    #[test]
    fn generate_plan_slug_is_unique_on_consecutive_calls() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let s1 = generate_plan_slug().expect("slug1");
        let s2 = generate_plan_slug().expect("slug2");
        assert_ne!(s1, s2);
    }

    #[test]
    fn write_plan_file_creates_and_reads_back() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let slug = generate_plan_slug().expect("slug");
        write_plan_file(&slug, "# My plan\nStep 1").expect("write");
        let content = read_plan_file(&slug).expect("read");
        assert_eq!(content, Some("# My plan\nStep 1".to_string()));
    }

    #[test]
    fn read_plan_file_returns_none_for_missing() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let result = read_plan_file("plan_nonexistent").expect("read");
        assert_eq!(result, None);
    }

    #[test]
    fn delete_plan_file_removes_file() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let slug = generate_plan_slug().expect("slug");
        write_plan_file(&slug, "content").expect("write");
        delete_plan_file(&slug).expect("delete");
        assert_eq!(read_plan_file(&slug).expect("read"), None);
    }

    #[test]
    fn plans_dir_creates_directory() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let dir = plans_dir().expect("dir");
        assert!(dir.exists());
    }
}
