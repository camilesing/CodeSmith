//! Directory path resolution for Knowledge On Demand.
//!
//! Determines where the memory directory and MEMORY.md entrypoint live,
//! mirroring the TypeScript `paths.ts` and `resolveMemoryDir` logic.

use std::path::{Path, PathBuf};

/// Resolve the memory directory path from a memory file path.
///
/// Given the existing `memory_path` (e.g. `~/.codesmith/projects/<hash>/memory.md`),
/// the memory directory is the same parent directory with `/memory/` appended.
/// When a custom `directory_override` is provided, it takes priority; a
/// leading `~` in it is expanded like `memory_path`'s (the sibling key is
/// tilde-expanded, so `~/Notes/memory` is the natural spelling — it used
/// to resolve against the process cwd).
pub fn resolve_memory_dir(memory_path: &Path, directory_override: Option<&str>) -> PathBuf {
    if let Some(override_dir) = directory_override {
        return crate::workspace_trust::expand_path(override_dir);
    }
    if let Some(parent) = memory_path.parent() {
        parent.join("memory")
    } else {
        PathBuf::from("./memory")
    }
}

/// Resolve the MEMORY.md entrypoint path inside the memory directory.
pub fn resolve_memory_entrypoint(memory_dir: &Path) -> PathBuf {
    memory_dir.join("MEMORY.md")
}

/// Ensure the memory directory exists on disk. Creates it (and parents)
/// if needed. Only the (unwired) prefetch calls it today; the `remember`
/// tool creates the directory lazily on its first write.
pub fn ensure_memory_dir_exists(memory_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(memory_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn entrypoint_is_memory_md_in_dir() {
        let dir = Path::new("/tmp/test_memory");
        let entry = resolve_memory_entrypoint(dir);
        assert_eq!(entry, PathBuf::from("/tmp/test_memory/MEMORY.md"));
    }

    #[test]
    fn resolve_dir_from_memory_path() {
        let mp = Path::new("/home/.codesmith/projects/abc123/memory.md");
        let dir = resolve_memory_dir(mp, None);
        assert_eq!(
            dir,
            PathBuf::from("/home/.codesmith/projects/abc123/memory")
        );
    }

    #[test]
    fn resolve_dir_with_override() {
        let mp = Path::new("/home/.codesmith/projects/abc123/memory.md");
        let dir = resolve_memory_dir(mp, Some("/custom/dir"));
        assert_eq!(dir, PathBuf::from("/custom/dir"));
    }

    /// `~` in the override used to come back as the relative path `~/notes`,
    /// so memories landed in a literal `~` directory under whatever the
    /// process cwd was.
    #[test]
    fn tilde_in_the_override_expands_against_home() {
        let tmp = tempfile::tempdir().unwrap();
        let _lock = crate::test_support::lock_test_env();
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());

        let dir = resolve_memory_dir(Path::new("/x/memory.md"), Some("~/notes"));

        assert_eq!(
            dir,
            tmp.path().join("notes"),
            "not a relative `~` directory"
        );
    }

    #[test]
    fn ensure_dir_creates_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("new_memory_dir");
        assert!(!dir.exists());
        ensure_memory_dir_exists(&dir).unwrap();
        assert!(dir.exists());
    }
}
