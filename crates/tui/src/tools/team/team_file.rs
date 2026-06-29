//! Team file persistence — re-exported from `codesmith_agent_runtime::team::team_file`.
//!
//! Production logic (struct definitions, file I/O) now lives in the
//! agent-runtime crate. This module re-exports it so historical
//! `crate::tools::team::team_file` paths keep resolving, and retains the
//! test module verbatim.

#![allow(dead_code)]

pub use codesmith_agent_runtime::team::team_file::*;

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
