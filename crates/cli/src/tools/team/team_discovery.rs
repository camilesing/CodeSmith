//! Team discovery — utilities for reading team config and finding members.

use crate::tools::team::team_file::{TeamFile, TeamMember, read_team_file};

/// Read the team config file from disk.
pub fn read_team_config(team_name: &str) -> anyhow::Result<TeamFile> {
    read_team_file(team_name)
}

/// Add a member to the team file, rejecting duplicates by name or
/// agent_id: the in-memory `ctx.teammates` guard at call sites cannot see
/// members persisted on disk by another session, and duplicate names make
/// every name-based lookup/removal/toggle ambiguous (first match wins).
/// Persist via `with_team_file` (the locked read-modify-write).
pub fn add_member(team_file: &mut TeamFile, member: TeamMember) -> anyhow::Result<()> {
    if team_file.members.iter().any(|m| m.name == member.name) {
        anyhow::bail!("team member with name '{}' already exists", member.name);
    }
    if team_file
        .members
        .iter()
        .any(|m| m.agent_id == member.agent_id)
    {
        anyhow::bail!(
            "team member with agent_id '{}' already exists",
            member.agent_id
        );
    }
    team_file.members.push(member);
    Ok(())
}

/// Mark a member as inactive. Fails when no member carries the name — the
/// old `bool` conflated not-found with success and every call site
/// discarded it with `let _ =`, so a bad name silently "succeeded".
/// Names are unique (`add_member` rejects duplicates), so the first match
/// is the only match. Persist via `with_team_file` (the locked
/// read-modify-write).
pub fn set_member_inactive(team_file: &mut TeamFile, name: &str) -> anyhow::Result<()> {
    let Some(member) = team_file.members.iter_mut().find(|m| m.name == name) else {
        anyhow::bail!("no team member named '{name}' in team '{}'", team_file.name);
    };
    member.is_active = false;
    Ok(())
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
        add_member(&mut tf, make_member("alice", true)).expect("add");
        assert_eq!(tf.members.len(), 1);
        assert_eq!(tf.members[0].name, "alice");
    }

    #[test]
    fn set_member_inactive_marks_and_reports_unknown_name() {
        let mut tf = make_team();
        add_member(&mut tf, make_member("alice", true)).expect("add");
        set_member_inactive(&mut tf, "alice").expect("known name succeeds");
        assert!(!tf.members[0].is_active);

        let err = set_member_inactive(&mut tf, "nobody").expect_err("unknown name must fail");
        assert!(err.to_string().contains("nobody"), "got: {err}");
    }
}
