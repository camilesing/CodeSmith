//! Local Team Memory sync helpers.
//!
//! This is intentionally a local-only skeleton: it records a small manifest of
//! agent-memory directories for active team members so future sync code has a
//! stable hand-off point without introducing network or remote side effects.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent_memory::{
    AgentMemoryScope, resolve_agent_memory_dir, resolve_agent_memory_entrypoint,
};

use super::team_file::{TeamFile, team_dir};

#[allow(dead_code)]
const TEAM_MEMORY_SYNC_FILE: &str = "team-memory-sync.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMemoryMemberSync {
    pub agent_id: String,
    pub name: String,
    pub agent_type: String,
    pub project_memory_dir: PathBuf,
    pub project_memory_entrypoint: PathBuf,
    pub local_memory_dir: PathBuf,
    pub local_memory_entrypoint: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMemorySyncManifest {
    pub team_name: String,
    pub workspace: PathBuf,
    pub generated_at_ms: u64,
    pub members: Vec<TeamMemoryMemberSync>,
}

#[allow(dead_code)]
pub fn team_memory_sync_path(team_name: &str) -> anyhow::Result<PathBuf> {
    Ok(team_dir(team_name)?.join(TEAM_MEMORY_SYNC_FILE))
}

#[allow(dead_code)]
pub fn build_team_memory_sync_manifest(
    team_file: &TeamFile,
    workspace: &Path,
) -> TeamMemorySyncManifest {
    let members = team_file
        .members
        .iter()
        .filter(|member| member.is_active)
        .filter_map(|member| {
            let agent_type = member
                .agent_type
                .as_deref()
                .unwrap_or(member.name.as_str())
                .to_string();
            let project_memory_dir =
                resolve_agent_memory_dir(workspace, &agent_type, AgentMemoryScope::Project).ok()?;
            let local_memory_dir =
                resolve_agent_memory_dir(workspace, &agent_type, AgentMemoryScope::Local).ok()?;
            Some(TeamMemoryMemberSync {
                agent_id: member.agent_id.clone(),
                name: member.name.clone(),
                agent_type,
                project_memory_entrypoint: resolve_agent_memory_entrypoint(&project_memory_dir),
                local_memory_entrypoint: resolve_agent_memory_entrypoint(&local_memory_dir),
                project_memory_dir,
                local_memory_dir,
            })
        })
        .collect();

    TeamMemorySyncManifest {
        team_name: team_file.name.clone(),
        workspace: workspace.to_path_buf(),
        generated_at_ms: now_ms(),
        members,
    }
}

#[allow(dead_code)]
pub fn write_team_memory_sync_manifest(
    team_file: &TeamFile,
    workspace: &Path,
) -> anyhow::Result<PathBuf> {
    let manifest = build_team_memory_sync_manifest(team_file, workspace);
    let path = team_memory_sync_path(&team_file.name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(path)
}

#[allow(dead_code)]
pub fn read_team_memory_sync_manifest(
    team_name: &str,
) -> anyhow::Result<Option<TeamMemorySyncManifest>> {
    let path = team_memory_sync_path(team_name)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&content)?))
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
    use crate::test_support::{ScopedCodeSmithHome, lock_test_env};
    use crate::tools::team::{TeamMember, create_team_file, format_lead_agent_id};
    use tempfile::tempdir;

    fn member(name: &str, agent_type: &str) -> TeamMember {
        TeamMember {
            agent_id: format!("agent-{name}"),
            name: name.to_string(),
            agent_type: Some(agent_type.to_string()),
            model: None,
            prompt: None,
            color: None,
            joined_at: 1,
            cwd: "/tmp".to_string(),
            worktree_path: None,
            session_id: None,
            is_active: true,
        }
    }

    #[test]
    fn writes_local_sync_manifest() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let workspace = tempdir().unwrap();
        let team = TeamFile {
            name: "memory-team".to_string(),
            description: None,
            created_at: 1,
            lead_agent_id: format_lead_agent_id("memory-team"),
            lead_session_id: None,
            team_allowed_paths: None,
            members: vec![member("reviewer", "review")],
        };
        create_team_file(&team).unwrap();

        let path = write_team_memory_sync_manifest(&team, workspace.path()).unwrap();
        assert!(path.exists());
        let manifest = read_team_memory_sync_manifest("memory-team")
            .unwrap()
            .expect("manifest");
        assert_eq!(manifest.members.len(), 1);
        assert_eq!(manifest.members[0].agent_type, "review");
        assert!(
            manifest.members[0]
                .project_memory_entrypoint
                .ends_with("MEMORY.md")
        );
    }
}
