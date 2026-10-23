//! Sandbox policy definitions for command execution restrictions.
//!
//! The portable policy data types (`SandboxPolicy`, `WritableRoot`) now live
//! in `codesmith-agent-runtime`'s `sandbox` module so they can cross the
//! `Arc<dyn HostServices>` boundary. This module re-exports them and keeps
//! the safety-level → behavior mapping, which references the CLI-local
//! command-safety types. The mapping and `SandboxPolicyBehavior` have no
//! production caller yet (#2186), so they are gated to the test build.

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use codesmith_agent_runtime::command_safety::SafetyLevel;

pub use codesmith_agent_runtime::sandbox::SandboxPolicy;
// Only the test module below consumes this re-export today; the cfg gate
// keeps the non-test bin pass from flagging it as an unused import.
#[cfg(test)]
pub use codesmith_agent_runtime::sandbox::WritableRoot;

/// Map a command safety classification to the appropriate sandbox policy (#2186).
///
/// - `Safe` / `WorkspaceSafe` → use the default sandbox policy
/// - `RequiresApproval` → user must approve before execution (handled by caller)
/// - `Dangerous` → always mapped to `Blocked`. Any YOLO-mode/trust override
///   is the caller's responsibility and must be decided BEFORE consulting
///   this mapping; this function performs no override of its own.
#[cfg(test)]
pub fn map_safety_level_to_behavior(
    level: SafetyLevel,
    default_policy: &SandboxPolicy,
) -> SandboxPolicyBehavior {
    match level {
        SafetyLevel::Safe | SafetyLevel::WorkspaceSafe => {
            SandboxPolicyBehavior::Sandboxed(default_policy.clone())
        }
        SafetyLevel::RequiresApproval => SandboxPolicyBehavior::RequiresApproval,
        SafetyLevel::Dangerous => SandboxPolicyBehavior::Blocked,
    }
}

/// Behavior decision for a sandboxed command based on safety level.
#[cfg(test)]
#[derive(Debug, Clone)]
pub enum SandboxPolicyBehavior {
    /// Execute with the given sandbox policy.
    Sandboxed(SandboxPolicy),
    /// User approval required before execution.
    RequiresApproval,
    /// Block execution entirely. Callers that implement a YOLO-mode/trust
    /// override must apply it before consulting this mapping.
    Blocked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = SandboxPolicy::default();
        assert!(matches!(policy, SandboxPolicy::WorkspaceWrite { .. }));
        assert!(!policy.has_network_access());
        assert!(policy.should_sandbox());
    }

    #[test]
    fn test_full_access_policy() {
        let policy = SandboxPolicy::DangerFullAccess;
        assert!(policy.has_full_disk_write_access());
        assert!(policy.has_network_access());
        assert!(!policy.should_sandbox());
    }

    #[test]
    fn test_read_only_policy() {
        let policy = SandboxPolicy::ReadOnly;
        assert!(!policy.has_full_disk_write_access());
        assert!(!policy.has_network_access());
        assert!(policy.should_sandbox());
    }

    #[test]
    fn test_workspace_with_network() {
        let policy = SandboxPolicy::workspace_with_network();
        assert!(policy.has_network_access());
        assert!(policy.should_sandbox());
    }

    #[test]
    #[cfg(unix)]
    fn test_writable_root_basic() {
        let root = WritableRoot::new(PathBuf::from("/project"));
        assert!(root.is_path_writable(Path::new("/project/src/main.rs")));
        assert!(!root.is_path_writable(Path::new("/other/file.txt")));
    }

    #[test]
    #[cfg(unix)]
    fn test_writable_root_with_exceptions() {
        let root = WritableRoot::with_exceptions(
            PathBuf::from("/project"),
            vec![PathBuf::from("/project/.codesmith")],
        );
        assert!(root.is_path_writable(Path::new("/project/src/main.rs")));
        assert!(!root.is_path_writable(Path::new("/project/.codesmith/config")));
    }

    #[test]
    fn test_safety_level_mapping() {
        let default = SandboxPolicy::default();

        // Safe commands get sandboxed
        assert!(matches!(
            map_safety_level_to_behavior(SafetyLevel::Safe, &default),
            SandboxPolicyBehavior::Sandboxed(_)
        ));
        assert!(matches!(
            map_safety_level_to_behavior(SafetyLevel::WorkspaceSafe, &default),
            SandboxPolicyBehavior::Sandboxed(_)
        ));

        // RequiresApproval gets RequiresApproval
        assert!(matches!(
            map_safety_level_to_behavior(SafetyLevel::RequiresApproval, &default),
            SandboxPolicyBehavior::RequiresApproval
        ));

        // Dangerous gets Blocked
        assert!(matches!(
            map_safety_level_to_behavior(SafetyLevel::Dangerous, &default),
            SandboxPolicyBehavior::Blocked
        ));
    }

    #[test]
    fn test_policy_serialization() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("/extra")],
            network_access: true,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        };

        let json = serde_json::to_string(&policy).unwrap();
        assert!(json.contains("workspace-write"));

        let parsed: SandboxPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, parsed);
    }
}
