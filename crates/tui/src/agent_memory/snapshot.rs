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
    let snapshot_dir = resolve_snapshot_dir(workspace, agent_type);
    fs::create_dir_all(&snapshot_dir)?;
    let snapshot = build_snapshot(agent_type, scope, memory_dir, prompt)?;
    fs::write(
        snapshot_dir.join("snapshot.json"),
        serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".to_string()),
    )?;
    Ok(snapshot)
}

#[allow(dead_code)]
pub fn mark_snapshot_synced(
    workspace: &Path,
    agent_type: &str,
    snapshot: &AgentMemorySnapshot,
) -> std::io::Result<()> {
    let snapshot_dir = resolve_snapshot_dir(workspace, agent_type);
    fs::create_dir_all(&snapshot_dir)?;
    fs::write(
        snapshot_dir.join(".snapshot-synced.json"),
        serde_json::to_string_pretty(snapshot).unwrap_or_else(|_| "{}".to_string()),
    )
}

#[must_use]
pub fn load_snapshot_status(
    workspace: &Path,
    agent_type: &str,
    scope: AgentMemoryScope,
    memory_dir: &Path,
    prompt: &str,
) -> AgentMemorySnapshotStatus {
    let snapshot_dir = resolve_snapshot_dir(workspace, agent_type);
    let snapshot_path = snapshot_dir.join("snapshot.json");
    let synced_path = snapshot_dir.join(".snapshot-synced.json");
    let snapshot = read_snapshot(&snapshot_path);
    let synced = read_snapshot(&synced_path);
    let current = build_snapshot(agent_type, scope, memory_dir, prompt).ok();
    let prompt_changed = match (&snapshot, &current) {
        (Some(old), Some(current)) => old.prompt_hash != current.prompt_hash,
        _ => false,
    };
    let memory_changed = match (&snapshot, &current) {
        (Some(old), Some(current)) => old.memory_hash != current.memory_hash,
        _ => false,
    };
    AgentMemorySnapshotStatus {
        snapshot_path,
        synced_path,
        snapshot,
        synced,
        prompt_changed,
        memory_changed,
    }
}

fn resolve_snapshot_dir(workspace: &Path, agent_type: &str) -> PathBuf {
    let segment = crate::agent_memory::paths::sanitize_agent_type_segment(agent_type)
        .unwrap_or_else(|_| "unknown".to_string());
    let codesmith = workspace
        .join(".codesmith")
        .join("agent-memory-snapshots")
        .join(&segment);
    if codesmith.exists() {
        return codesmith;
    }
    let claude = workspace
        .join(".claude")
        .join("agent-memory-snapshots")
        .join(&segment);
    if claude.exists() {
        return claude;
    }
    codesmith
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
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
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
            collect_md_files(root, &path, entries)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let content = fs::read_to_string(&path).unwrap_or_default();
            entries.push((relative, content));
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
        );
        assert!(status.prompt_changed);
    }
}
