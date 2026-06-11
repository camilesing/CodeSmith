//! Team discovery — utilities for reading team config and finding members.

use crate::tools::team::team_file::{
    TeamFile, TeamMember,
    read_team_file, write_team_file,
    find_member_by_name, find_member_by_agent_id, remove_member_by_name,
};

/// Read the team config file from disk.
pub fn read_team_config(team_name: &str) -> anyhow::Result<TeamFile> {
    read_team_file(team_name)
}

/// Write the team config file back to disk.
pub fn write_team_config(team_file: &TeamFile) -> anyhow::Result<()> {
    write_team_file(team_file)
}

/// Find a member by name. Returns None if not found.
pub fn find_member(team_file: &TeamFile, name: &str) -> Option<TeamMember> {
    find_member_by_name(team_file, name)
}

/// Find a member by agent ID. Returns None if not found.
pub fn find_member_by_id(team_file: &TeamFile, agent_id: &str) -> Option<TeamMember> {
    find_member_by_agent_id(team_file, agent_id)
}

/// Remove a member by name from the team file, returning the removed member.
/// Caller must call `write_team_config()` to persist.
pub fn remove_member(team_file: &mut TeamFile, name: &str) -> Option<TeamMember> {
    remove_member_by_name(team_file, name)
}

/// Add a member to the team file. Caller must call `write_team_config()` to persist.
pub fn add_member(team_file: &mut TeamFile, member: TeamMember) {
    team_file.members.push(member);
}

/// Mark a member as inactive. Caller must call `write_team_config()` to persist.
pub fn set_member_inactive(team_file: &mut TeamFile, name: &str) -> bool {
    if let Some(member) = team_file.members.iter_mut().find(|m| m.name == name) {
        member.is_active = false;
        true
    } else {
        false
    }
}

/// Mark a member as active. Caller must call `write_team_config()` to persist.
pub fn set_member_active(team_file: &mut TeamFile, name: &str) -> bool {
    if let Some(member) = team_file.members.iter_mut().find(|m| m.name == name) {
        member.is_active = true;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::team::team_file::{TeamFile, TeamMember, format_lead_agent_id};

    fn make_member(name: &str, is_active: bool) -> TeamMember {
        TeamMember {
            agent_id: format!("id-{name}"),
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

    fn make_team() -> TeamFile {
        TeamFile {
            name: "disc-test".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: format_lead_agent_id("disc-test"),
            lead_session_id: None,
            team_allowed_paths: None,
            members: vec![],
        }
    }

    #[test]
    fn add_member_appends_to_members() {
        let mut tf = make_team();
        add_member(&mut tf, make_member("alice", true));
        assert_eq!(tf.members.len(), 1);
        assert_eq!(tf.members[0].name, "alice");
    }

    #[test]
    fn remove_member_removes_correct_member() {
        let mut tf = make_team();
        add_member(&mut tf, make_member("alice", true));
        add_member(&mut tf, make_member("bob", true));
        let removed = remove_member(&mut tf, "alice");
        assert!(removed.is_some());
        assert_eq!(tf.members.len(), 1);
        assert_eq!(tf.members[0].name, "bob");
    }

    #[test]
    fn set_member_inactive_and_active_toggle() {
        let mut tf = make_team();
        add_member(&mut tf, make_member("alice", true));
        assert!(set_member_inactive(&mut tf, "alice"));
        assert!(!tf.members[0].is_active);
        assert!(set_member_active(&mut tf, "alice"));
        assert!(tf.members[0].is_active);
    }

    #[test]
    fn set_member_inactive_returns_false_for_unknown() {
        let mut tf = make_team();
        assert!(!set_member_inactive(&mut tf, "nobody"));
        assert!(!set_member_active(&mut tf, "nobody"));
    }
}