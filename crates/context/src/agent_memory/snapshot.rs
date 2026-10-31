use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_memory::AgentMemoryScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMemorySnapshotMode {
    #[default]
    None,
    Initialize,
    PromptUpdate,
}

impl std::str::FromStr for AgentMemorySnapshotMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "false" => Ok(Self::None),
            "initialize" | "init" => Ok(Self::Initialize),
            "prompt-update" | "prompt_update" | "update" => Ok(Self::PromptUpdate),
            other => Err(format!(
                "invalid agent memory snapshot mode '{other}', expected none, initialize, or prompt-update"
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemorySnapshot {
    pub agent_type: String,
    pub scope: AgentMemoryScope,
    pub memory_dir: PathBuf,
    pub prompt_hash: String,
    pub memory_hash: String,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMemorySnapshotStatus {
    pub snapshot_path: PathBuf,
    pub synced_path: PathBuf,
    pub snapshot: Option<AgentMemorySnapshot>,
    pub synced: Option<AgentMemorySnapshot>,
    pub prompt_changed: bool,
    pub memory_changed: bool,
}

pub fn initialize_or_update_snapshot(
    workspace: &Path,
    agent_type: &str,
    scope: AgentMemoryScope,
    memory_dir: &Path,
    prompt: &str,
) -> std::io::Result<AgentMemorySnapshot> {
    let snapshot_dir = resolve_snapshot_dir(workspace, agent_type)?;
    fs::create_dir_all(&snapshot_dir)?;
    let snapshot = build_snapshot(agent_type, scope, memory_dir, prompt)?;
    write_snapshot_atomic(&snapshot_dir, "snapshot.json", &snapshot)?;
    Ok(snapshot)
}

#[allow(dead_code)]
pub fn mark_snapshot_synced(
    workspace: &Path,
    agent_type: &str,
    snapshot: &AgentMemorySnapshot,
) -> std::io::Result<()> {
    let snapshot_dir = resolve_snapshot_dir(workspace, agent_type)?;
    fs::create_dir_all(&snapshot_dir)?;
    write_snapshot_atomic(&snapshot_dir, ".snapshot-synced.json", snapshot)
}

/// Write a snapshot file atomically: serialize to a temp file in the same
/// directory, then `rename` it into place (atomic replace on POSIX,
/// replace-existing on Windows). A crash or concurrent reader can therefore
/// never observe a truncated file that later parses as `None` — which would
/// silently disable change detection. Serialization errors propagate
/// instead of persisting a `{}` that is guaranteed to fail deserializing.
fn write_snapshot_atomic(
    snapshot_dir: &Path,
    file_name: &str,
    snapshot: &AgentMemorySnapshot,
) -> std::io::Result<()> {
    let payload = serde_json::to_string_pretty(snapshot).map_err(std::io::Error::from)?;
    let target = snapshot_dir.join(file_name);
    let tmp = snapshot_dir.join(format!("{file_name}.tmp"));
    fs::write(&tmp, payload)?;
    fs::rename(&tmp, &target)
}

pub fn load_snapshot_status(
    workspace: &Path,
    agent_type: &str,
    scope: AgentMemoryScope,
    memory_dir: &Path,
    prompt: &str,
) -> std::io::Result<AgentMemorySnapshotStatus> {
    let snapshot_dir = resolve_snapshot_dir(workspace, agent_type)?;
    let snapshot_path = snapshot_dir.join("snapshot.json");
    let synced_path = snapshot_dir.join(".snapshot-synced.json");
    let snapshot = read_snapshot(&snapshot_path);
    let synced = read_snapshot(&synced_path);
    let current = match build_snapshot(agent_type, scope, memory_dir, prompt) {
        Ok(current) => Some(current),
        Err(err) => {
            // Downgrade to "no snapshot comparison" but leave a trace — a
            // silent None here disables change detection without a signal.
            tracing::warn!(
                "agent memory: snapshot build failed for {}: {err:#}",
                memory_dir.display()
            );
            None
        }
    };
    // When either side is missing (build failure, unreadable or corrupt
    // snapshot, or simply no snapshot yet) the comparison is impossible —
    // report as changed so downstream errs on the side of syncing instead
    // of silently treating an unknown state as "in sync". With no snapshot
    // at all, `true` is also the semantically correct "needs
    // initialization" answer.
    let prompt_changed = match (&snapshot, &current) {
        (Some(old), Some(current)) => old.prompt_hash != current.prompt_hash,
        _ => true,
    };
    let memory_changed = match (&snapshot, &current) {
        (Some(old), Some(current)) => old.memory_hash != current.memory_hash,
        _ => true,
    };
    Ok(AgentMemorySnapshotStatus {
        snapshot_path,
        synced_path,
        snapshot,
        synced,
        prompt_changed,
        memory_changed,
    })
}

/// Resolve the snapshot directory for `agent_type`. Fails on an agent type
/// that cannot be sanitized into a path segment: collapsing two distinct
/// invalid types onto a shared `"unknown"` segment made agent A's hashes
/// compare against agent B's, producing arbitrary wrong drift results.
fn resolve_snapshot_dir(workspace: &Path, agent_type: &str) -> std::io::Result<PathBuf> {
    let segment = crate::agent_memory::paths::sanitize_agent_type_segment(agent_type)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    let codesmith = workspace
        .join(".codesmith")
        .join("agent-memory-snapshots")
        .join(&segment);
    // Prefer the directory that actually HOLDS snapshot data, not merely
    // exists: a later-created `.codesmith` dir (another agent type, a
    // config migration, the user) used to flip resolution away from an
    // existing `.claude` snapshot, silently resetting drift detection.
    let has_snapshot_data = |dir: &Path| {
        dir.join("snapshot.json").exists() || dir.join(".snapshot-synced.json").exists()
    };
    if has_snapshot_data(&codesmith) {
        return Ok(codesmith);
    }
    let claude = workspace
        .join(".claude")
        .join("agent-memory-snapshots")
        .join(&segment);
    if has_snapshot_data(&claude) {
        return Ok(claude);
    }
    Ok(codesmith)
}

fn build_snapshot(
    agent_type: &str,
    scope: AgentMemoryScope,
    memory_dir: &Path,
    prompt: &str,
) -> std::io::Result<AgentMemorySnapshot> {
    Ok(AgentMemorySnapshot {
        agent_type: agent_type.to_string(),
        scope,
        memory_dir: memory_dir.to_path_buf(),
        prompt_hash: hash_bytes(prompt.as_bytes()),
        memory_hash: hash_memory_dir(memory_dir)?,
        updated_at_ms: now_ms(),
    })
}

fn read_snapshot(path: &Path) -> Option<AgentMemorySnapshot> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(
                "agent memory: snapshot unreadable {}: {err}",
                path.display()
            );
            return None;
        }
    };
    match serde_json::from_str(&content) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            tracing::warn!("agent memory: snapshot corrupt {}: {err}", path.display());
            None
        }
    }
}

fn hash_memory_dir(memory_dir: &Path) -> std::io::Result<String> {
    let mut entries = Vec::new();
    if memory_dir.exists() {
        collect_md_files(memory_dir, memory_dir, &mut entries)?;
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (relative, content) in entries {
        hasher.update(relative.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.as_bytes());
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_md_files(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            // Known limitation: symlinked subdirectories are not traversed
            // (`file_type()` does not follow links — deliberate, to avoid
            // cycles and escaping the memory tree), while a symlinked `.md`
            // file IS read. Coverage is therefore conservative but
            // cycle-safe.
            collect_md_files(root, &path, entries)?;
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            // Case-insensitive so `NOTES.MD` and `File.Md` stay covered on
            // case-insensitive filesystems.
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            // An unreadable file must change the digest: hashing it as
            // absent would report "in sync" while coverage silently shrank.
            // Mix a sentinel carrying the error so the hash flips.
            match fs::read_to_string(&path) {
                Ok(content) => entries.push((relative, content)),
                Err(err) => {
                    tracing::warn!(
                        "agent memory: hashing unreadable file as sentinel {}: {err}",
                        path.display()
                    );
                    entries.push((relative, format!("<unreadable: {err}>")));
                }
            }
        }
    }
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn initializes_snapshot() {
        let workspace = tempdir().unwrap();
        let memory = tempdir().unwrap();
        std::fs::write(memory.path().join("MEMORY.md"), "hello").unwrap();
        let snapshot = initialize_or_update_snapshot(
            workspace.path(),
            "explore",
            AgentMemoryScope::Project,
            memory.path(),
            "prompt",
        )
        .unwrap();
        assert_eq!(snapshot.agent_type, "explore");
        assert!(
            workspace
                .path()
                .join(".codesmith/agent-memory-snapshots/explore/snapshot.json")
                .exists()
        );
    }

    #[test]
    fn detects_prompt_change() {
        let workspace = tempdir().unwrap();
        let memory = tempdir().unwrap();
        initialize_or_update_snapshot(
            workspace.path(),
            "review",
            AgentMemoryScope::Project,
            memory.path(),
            "old prompt",
        )
        .unwrap();
        let status = load_snapshot_status(
            workspace.path(),
            "review",
            AgentMemoryScope::Project,
            memory.path(),
            "new prompt",
        )
        .unwrap();
        assert!(status.prompt_changed);
    }

    #[test]
    fn unknown_comparison_defaults_to_changed() {
        // A corrupt snapshot must read as "changed" so the sync path errs
        // on the side of re-initializing instead of skipping the sync.
        let workspace = tempdir().unwrap();
        let memory = tempdir().unwrap();
        let snapshot_dir = workspace
            .path()
            .join(".codesmith/agent-memory-snapshots/review");
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        std::fs::write(snapshot_dir.join("snapshot.json"), "not json").unwrap();
        let status = load_snapshot_status(
            workspace.path(),
            "review",
            AgentMemoryScope::Project,
            memory.path(),
            "prompt",
        )
        .unwrap();
        assert!(status.prompt_changed);
        assert!(status.memory_changed);
    }
}
