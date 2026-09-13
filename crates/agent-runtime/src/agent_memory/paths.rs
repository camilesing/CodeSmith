use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::agent_memory::AgentMemoryScope;

/// Convert an agent type/name into a single safe path segment.
pub fn sanitize_agent_type_segment(agent_type: &str) -> Result<String, String> {
    let trimmed = agent_type.trim();
    if trimmed.is_empty() {
        return Err("agent type cannot be blank".to_string());
    }

    let segment = trimmed
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if segment.is_empty() || segment == "." || segment == ".." {
        return Err(format!("invalid agent type '{agent_type}'"));
    }
    Ok(segment)
}

/// Candidate memory directories in priority order. The first path is the
/// CodeSmith-native write target; later paths are read-compatible legacy/Claude
/// locations.
pub fn agent_memory_candidates(
    workspace: &Path,
    agent_type: &str,
    scope: AgentMemoryScope,
) -> Result<Vec<PathBuf>, String> {
    let segment = sanitize_agent_type_segment(agent_type)?;
    let mut candidates = Vec::new();
    match scope {
        AgentMemoryScope::User => {
            let home = codesmith_config::codesmith_home()
                .or_else(|_| {
                    dirs::home_dir()
                        .map(|home| home.join(".codesmith"))
                        .ok_or_else(|| anyhow::anyhow!("home directory not found"))
                })
                .map_err(|err| format!("failed to resolve CodeSmith home: {err}"))?;
            candidates.push(home.join("agent-memory").join(&segment));
            if let Some(home) = dirs::home_dir() {
                candidates.push(home.join(".claude").join("agent-memory").join(&segment));
            }
        }
        AgentMemoryScope::Project => {
            candidates.push(
                workspace
                    .join(".codesmith")
                    .join("agent-memory")
                    .join(&segment),
            );
            candidates.push(
                workspace
                    .join(".claude")
                    .join("agent-memory")
                    .join(&segment),
            );
        }
        AgentMemoryScope::Local => {
            candidates.push(
                workspace
                    .join(".codesmith")
                    .join("agent-memory-local")
                    .join(&segment),
            );
            candidates.push(
                workspace
                    .join(".claude")
                    .join("agent-memory-local")
                    .join(&segment),
            );
        }
    }
    Ok(dedup_paths(candidates))
}

/// Resolve the active memory directory. Existing compatible paths win for read
/// compatibility; otherwise the CodeSmith-native first candidate is returned.
pub fn resolve_agent_memory_dir(
    workspace: &Path,
    agent_type: &str,
    scope: AgentMemoryScope,
) -> Result<PathBuf, String> {
    let candidates = agent_memory_candidates(workspace, agent_type, scope)?;
    candidates
        .iter()
        .find(|path| path.exists())
        .cloned()
        .or_else(|| candidates.first().cloned())
        .ok_or_else(|| "no agent memory candidates resolved".to_string())
}

#[must_use]
pub fn resolve_agent_memory_entrypoint(memory_dir: &Path) -> PathBuf {
    memory_dir.join("MEMORY.md")
}

pub fn ensure_agent_memory_dir(memory_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(memory_dir)?;
    let entrypoint = resolve_agent_memory_entrypoint(memory_dir);
    if !entrypoint.exists() {
        crate::utils::write_atomic(
            &entrypoint,
            b"# Agent Memory\n\nAdd links to durable memory topic files here.\n",
        )?;
    }
    Ok(())
}

/// Resolve a caller-supplied path under `memory_dir`, rejecting traversal and
/// absolute paths outside the memory root.
pub fn scoped_path_within_memory(memory_dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("path cannot be blank".to_string());
    }
    let raw_path = Path::new(trimmed);
    if raw_path.is_absolute() {
        return Err("agent memory paths must be relative to the memory directory".to_string());
    }
    if raw_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err("agent memory paths may not contain '..' or absolute prefixes".to_string());
    }
    let base = memory_dir
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(memory_dir));
    let candidate = normalize_path(&base.join(raw_path));
    if !candidate.starts_with(&base) {
        return Err(format!(
            "path {} escapes agent memory directory {}",
            candidate.display(),
            base.display()
        ));
    }
    // Symlink-escape hardening: the lexical prefix check above is not enough
    // when a component under the memory root is itself a symlink pointing
    // outside it (e.g. a planted `topics -> /etc`). Verify against the real
    // filesystem using the canonical base: an existing candidate (read or
    // rewrite target) canonicalizes directly — a symlinked leaf resolves to
    // its target and then fails containment — while a write target that does
    // not exist yet is covered by canonicalizing its deepest existing
    // ancestor, with the not-yet-existing final component required to be a
    // single segment (no separators). Legitimate paths are unaffected: the
    // candidate is returned exactly as before; only the containment decision
    // gains the canonical re-check.
    if let Ok(canonical_base) = memory_dir.canonicalize() {
        if let Ok(canonical) = candidate.canonicalize() {
            if !canonical.starts_with(&canonical_base) {
                return Err(format!(
                    "path {} escapes agent memory directory {}",
                    canonical.display(),
                    canonical_base.display()
                ));
            }
        } else {
            // Write target not on disk yet: the final component must be a
            // single safe segment — a separator inside it would mean the
            // lexical component scan above was bypassed (e.g. a backslash
            // smuggled on a Unix host targeting a Windows-style path).
            let leaf_is_single_segment = candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.contains('/') && !name.contains('\\'));
            // Walk to the deepest existing ancestor — the parent for a
            // simple `dir/file.md` write — and canonicalize that.
            let mut existing = candidate.as_path();
            while !existing.exists() && existing.file_name().is_some() {
                match existing.parent() {
                    Some(parent) => existing = parent,
                    None => break,
                }
            }
            if !leaf_is_single_segment {
                return Err(format!(
                    "path {} is not a single safe segment within agent memory directory {}",
                    candidate.display(),
                    canonical_base.display()
                ));
            }
            if let Ok(canonical) = existing.canonicalize()
                && !canonical.starts_with(&canonical_base)
            {
                return Err(format!(
                    "path {} escapes agent memory directory {}",
                    canonical.display(),
                    canonical_base.display()
                ));
            }
        }
    }
    Ok(candidate)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing: &PathBuf| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sanitizes_agent_type() {
        assert_eq!(
            sanitize_agent_type_segment("Explore Agent").unwrap(),
            "explore-agent"
        );
        assert!(sanitize_agent_type_segment("../x").unwrap().contains('x'));
        assert!(sanitize_agent_type_segment("   ").is_err());
    }

    #[test]
    fn project_scope_prefers_codesmith_path() {
        let tmp = tempdir().unwrap();
        let dir =
            resolve_agent_memory_dir(tmp.path(), "explore", AgentMemoryScope::Project).unwrap();
        assert!(dir.ends_with(".codesmith/agent-memory/explore"));
    }

    #[test]
    fn scoped_path_rejects_parent_traversal() {
        let tmp = tempdir().unwrap();
        let err = scoped_path_within_memory(tmp.path(), "../escape.md").unwrap_err();
        assert!(err.contains(".."));
    }

    #[test]
    fn scoped_path_allows_nested_relative() {
        let tmp = tempdir().unwrap();
        let path = scoped_path_within_memory(tmp.path(), "topics/foo.md").unwrap();
        assert!(path.ends_with("topics/foo.md"));
    }

    #[test]
    #[cfg(unix)]
    fn scoped_path_rejects_symlinked_directory_escape() {
        // A symlink planted inside the memory dir pointing outside must be
        // rejected even though the lexical component check passes — the
        // canonicalized ancestor resolves outside the memory root.
        let tmp = tempdir().unwrap();
        let memory_dir = tmp.path().join("memory");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&memory_dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, memory_dir.join("topics")).unwrap();

        let err = scoped_path_within_memory(&memory_dir, "topics/secret.md").unwrap_err();
        assert!(
            err.contains("escapes"),
            "symlinked directory escape must be rejected; got: {err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn scoped_path_rejects_symlinked_leaf_escape() {
        // An existing symlinked leaf file resolves outside the memory root
        // and must be rejected by the canonical containment re-check.
        let tmp = tempdir().unwrap();
        let memory_dir = tmp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();
        let outside_file = tmp.path().join("outside-secret.md");
        fs::write(&outside_file, "secret").unwrap();
        std::os::unix::fs::symlink(&outside_file, memory_dir.join("leaked.md")).unwrap();

        let err = scoped_path_within_memory(&memory_dir, "leaked.md").unwrap_err();
        assert!(
            err.contains("escapes"),
            "symlinked leaf escape must be rejected; got: {err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn scoped_path_allows_real_files_after_canonical_check() {
        // Legitimate existing nested paths keep working under the canonical
        // containment re-check (both sides canonicalize consistently, so
        // e.g. macOS `/var` -> `/private/var` does not cause false hits).
        let tmp = tempdir().unwrap();
        let memory_dir = tmp.path().join("memory");
        fs::create_dir_all(memory_dir.join("topics")).unwrap();
        fs::write(memory_dir.join("topics/notes.md"), "x").unwrap();

        let path = scoped_path_within_memory(&memory_dir, "topics/notes.md").unwrap();
        assert!(path.ends_with("topics/notes.md"));
    }
}
