//! Team file persistence — config.json I/O for team coordination.
//!
//! Team files live at `~/.codesmith/teams/{sanitized_name}/config.json`.
//! Each file holds the team metadata, leader identity, and member roster.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Sanitize a team name for use in directory/file paths.
/// Replaces non-alphanumeric chars with hyphens, lowercased.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Deterministic leader agent ID: "team-lead@{teamName}"
pub fn format_lead_agent_id(team_name: &str) -> String {
    format!("team-lead@{}", sanitize_name(team_name))
}

const TEAM_LEAD_NAME: &str = "team-lead";

/// Get the canonical team lead name.
pub fn team_lead_name() -> &'static str {
    TEAM_LEAD_NAME
}

/// Path to a team directory under `~/.codesmith/teams/`.
pub fn team_dir(team_name: &str) -> anyhow::Result<PathBuf> {
    let base = codesmith_config::codesmith_home()?;
    Ok(base.join("teams").join(sanitize_name(team_name)))
}

/// Path to the team config.json file.
pub fn team_config_path(team_name: &str) -> anyhow::Result<PathBuf> {
    Ok(team_dir(team_name)?.join("config.json"))
}

/// Path to the team's task directory under `~/.codesmith/tasks/`.
pub fn team_task_dir(team_name: &str) -> anyhow::Result<PathBuf> {
    let base = codesmith_config::codesmith_home()?;
    Ok(base.join("tasks").join(sanitize_name(team_name)))
}

/// An allowed path entry for team-level permission overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamAllowedPath {
    pub path: String,
    pub tool_name: String,
    pub added_by: String,
    pub added_at: i64,
}

/// A single team member record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub agent_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub joined_at: i64,
    #[serde(default)]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub is_active: bool,
}

/// The team config file stored at `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamFile {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: i64,
    pub lead_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_allowed_paths: Option<Vec<TeamAllowedPath>>,
    #[serde(default)]
    pub members: Vec<TeamMember>,
}

/// Create the team directory and write an initial config.json.
pub fn create_team_file(team_file: &TeamFile) -> anyhow::Result<PathBuf> {
    let dir = team_dir(&team_file.name)?;
    fs::create_dir_all(&dir)?;
    let config_path = dir.join("config.json");
    let json = serde_json::to_string_pretty(team_file)?;
    fs::write(&config_path, json)?;
    // Also create the inboxes subdirectory.
    fs::create_dir_all(dir.join("inboxes"))?;
    // Create the team-scoped task directory.
    let task_dir = team_task_dir(&team_file.name)?;
    fs::create_dir_all(&task_dir)?;
    Ok(config_path)
}

/// Read the team config.json from disk.
pub fn read_team_file(team_name: &str) -> anyhow::Result<TeamFile> {
    let path = team_config_path(team_name)?;
    let json = fs::read_to_string(&path)?;
    let team_file: TeamFile = serde_json::from_str(&json)?;
    Ok(team_file)
}

/// Write the team config.json back to disk (full overwrite).
pub fn write_team_file(team_file: &TeamFile) -> anyhow::Result<()> {
    let path = team_config_path(&team_file.name)?;
    let json = serde_json::to_string_pretty(team_file)?;
    fs::write(&path, json)?;
    Ok(())
}

/// Delete the entire team directory and task directory.
pub fn delete_team_directories(team_name: &str) -> anyhow::Result<()> {
    let dir = team_dir(team_name)?;
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    let task_dir = team_task_dir(team_name)?;
    if task_dir.exists() {
        fs::remove_dir_all(&task_dir)?;
    }
    Ok(())
}

/// Find a member by name in the team file. Returns cloned member.
pub fn find_member_by_name(team_file: &TeamFile, name: &str) -> Option<TeamMember> {
    team_file.members.iter().find(|m| m.name == name).cloned()
}

/// Find a member by agent_id in the team file. Returns cloned member.
pub fn find_member_by_agent_id(team_file: &TeamFile, agent_id: &str) -> Option<TeamMember> {
    team_file
        .members
        .iter()
        .find(|m| m.agent_id == agent_id)
        .cloned()
}

/// Remove a member by name from the team file, returning the removed member
/// if found. Caller must `write_team_file()` to persist.
pub fn remove_member_by_name(team_file: &mut TeamFile, name: &str) -> Option<TeamMember> {
    let idx = team_file.members.iter().position(|m| m.name == name)?;
    Some(team_file.members.remove(idx))
}

/// List active (non-lead) teammates from the team file. Returns cloned members.
pub fn active_teammates(team_file: &TeamFile) -> Vec<TeamMember> {
    team_file
        .members
        .iter()
        .filter(|m| m.name != TEAM_LEAD_NAME && m.is_active)
        .cloned()
        .collect()
}

/// Count active teammates (excluding the lead).
pub fn active_teammate_count(team_file: &TeamFile) -> usize {
    active_teammates(team_file).len()
}
