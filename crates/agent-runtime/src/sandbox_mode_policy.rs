//! Mode-coupled default sandbox policy — takes `AppMode`, so it stayed
//! with the engine when the sandbox plane moved to `codesmith-sandbox`
//! (round 9). Everything else about sandboxing is mode-free and lives
//! there.

use std::path::Path;

use codesmith_sandbox::SandboxPolicy;

use crate::mode::AppMode;

/// Pick the sandbox policy that gates shell commands for a given UI mode.
///
/// - **Plan** (#1077): `ReadOnly` — no writes, no network. The previous
///   `WorkspaceWrite` policy let `python -c "open('f','w').write('x')"` mutate
///   files inside the workspace because it whitelisted the workspace as
///   writable. Plan mode is investigation only; if the user wants to change
///   files they should switch to Agent.
/// - **Coordinator**: `ReadOnly` — same rationale as Plan; the coordinator
///   cannot directly execute or write, so no sandbox writes needed.
/// - **Agent**: `WorkspaceWrite` with workspace as writable root and network
///   on. Approval flow gates risky individual commands; the sandbox handles
///   the rest. Network is allowed because cargo / npm / curl-style commands
///   are normal during agent work and DNS-deny breaks them silently.
/// - **YOLO**: `DangerFullAccess` — explicit no-guardrails contract.
pub fn sandbox_policy_for_mode(mode: AppMode, workspace: &Path) -> SandboxPolicy {
    match mode {
        AppMode::Plan | AppMode::Coordinator => SandboxPolicy::ReadOnly,
        AppMode::Agent => SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![workspace.to_path_buf()],
            network_access: true,
            exclude_tmpdir: true,
            exclude_slash_tmp: true,
        },
        AppMode::Yolo => SandboxPolicy::DangerFullAccess,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_and_coordinator_are_readonly() {
        for mode in [AppMode::Plan, AppMode::Coordinator] {
            assert!(matches!(
                sandbox_policy_for_mode(mode, Path::new("/repo")),
                SandboxPolicy::ReadOnly
            ));
        }
    }

    #[test]
    fn agent_is_workspace_write() {
        let policy = sandbox_policy_for_mode(AppMode::Agent, Path::new("/repo"));
        match policy {
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                network_access,
                ..
            } => {
                assert_eq!(writable_roots, vec![std::path::PathBuf::from("/repo")]);
                assert!(network_access);
            }
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }
    }

    #[test]
    fn yolo_is_danger_full_access() {
        assert!(matches!(
            sandbox_policy_for_mode(AppMode::Yolo, Path::new("/repo")),
            SandboxPolicy::DangerFullAccess
        ));
    }
}
