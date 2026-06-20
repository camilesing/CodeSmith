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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ScopedCodeSmithHome, lock_test_env};

    fn make_team_file(name: &str) -> TeamFile {
        TeamFile {
            name: name.to_string(),
            description: Some("test team".to_string()),
            created_at: 1234567890,
            lead_agent_id: format_lead_agent_id(name),
            lead_session_id: None,
            team_allowed_paths: None,
            members: vec![],
        }
    }

    fn make_member(name: &str, agent_id: &str, is_active: bool) -> TeamMember {
        TeamMember {
            agent_id: agent_id.to_string(),
            name: name.to_string(),
            agent_type: None,
            model: None,
            prompt: None,
            color: None,
            joined_at: 1234567890,
            cwd: "/tmp".to_string(),
            worktree_path: None,
            session_id: None,
            is_active,
        }
    }

    #[test]
    fn sanitize_name_lowercases_and_replaces_non_alphanumeric() {
        assert_eq!(sanitize_name("My Cool Team"), "my-cool-team");
        assert_eq!(sanitize_name("team-v2"), "team-v2");
        assert_eq!(sanitize_name("!@#"), "---");
        assert_eq!(sanitize_name(""), "");
    }

    #[test]
    fn format_lead_agent_id_composes_correctly() {
        assert_eq!(format_lead_agent_id("alpha"), "team-lead@alpha");
        assert_eq!(format_lead_agent_id("My Team"), "team-lead@my-team");
    }

    #[test]
    fn create_team_file_writes_config_and_inboxes_dir() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let tf = make_team_file("test-create");
        let config_path = create_team_file(&tf).expect("create");

        assert!(config_path.exists());
        let dir = team_dir("test-create").expect("dir");
        assert!(dir.join("inboxes").exists());
        assert!(team_task_dir("test-create").expect("task dir").exists());

        let read_back = read_team_file("test-create").expect("read");
        assert_eq!(read_back.name, "test-create");
        assert_eq!(read_back.lead_agent_id, "team-lead@test-create");
    }

    #[test]
    fn read_write_team_file_roundtrips() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let mut tf = make_team_file("test-roundtrip");
        tf.members.push(make_member("worker1", "w1", true));
        create_team_file(&tf).expect("create");
        write_team_file(&tf).expect("write");

        let read_back = read_team_file("test-roundtrip").expect("read");
        assert_eq!(read_back.members.len(), 1);
        assert_eq!(read_back.members[0].name, "worker1");
    }

    #[test]
    fn delete_team_directories_removes_dirs() {
        let _guard = lock_test_env();
        let _home = ScopedCodeSmithHome::new();
        let tf = make_team_file("test-delete");
        create_team_file(&tf).expect("create");
        assert!(team_dir("test-delete").expect("dir").exists());

        delete_team_directories("test-delete").expect("delete");
        assert!(!team_dir("test-delete").expect("dir").exists());
        assert!(!team_task_dir("test-delete").expect("task dir").exists());
    }

    #[test]
    fn find_member_by_name_found_and_not_found() {
        let mut tf = make_team_file("find-test");
        tf.members.push(make_member("alice", "a1", true));
        tf.members.push(make_member("bob", "b1", true));

        assert!(find_member_by_name(&tf, "alice").is_some());
        assert!(find_member_by_name(&tf, "unknown").is_none());
    }

    #[test]
    fn find_member_by_agent_id_found_and_not_found() {
        let mut tf = make_team_file("find-id-test");
        tf.members.push(make_member("alice", "a1", true));

        assert!(find_member_by_agent_id(&tf, "a1").is_some());
        assert!(find_member_by_agent_id(&tf, "unknown").is_none());
    }

    #[test]
    fn remove_member_by_name_returns_removed_and_mutates() {
        let mut tf = make_team_file("remove-test");
        tf.members.push(make_member("alice", "a1", true));
        tf.members.push(make_member("bob", "b1", true));

        let removed = remove_member_by_name(&mut tf, "alice");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name, "alice");
        assert_eq!(tf.members.len(), 1);
        assert_eq!(tf.members[0].name, "bob");
    }

    #[test]
    fn active_teammates_excludes_lead_and_inactive() {
        let mut tf = make_team_file("active-test");
        tf.members.push(make_member("team-lead", "lead1", true));
        tf.members.push(make_member("worker", "w1", true));
        tf.members.push(make_member("sleeper", "s1", false));

        let active = active_teammates(&tf);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "worker");
    }

    #[test]
    fn active_teammate_count_matches_active_teammates() {
        let mut tf = make_team_file("count-test");
        tf.members.push(make_member("team-lead", "lead1", true));
        tf.members.push(make_member("w1", "a1", true));
        tf.members.push(make_member("w2", "a2", true));

        assert_eq!(active_teammate_count(&tf), active_teammates(&tf).len());
    }
}
