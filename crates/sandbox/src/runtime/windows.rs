//! Windows sandbox helper contract.
//!
//! Current status: CodeSmith does not advertise an in-process Windows
//! sandbox. Future Windows support must run commands through a dedicated
//! helper that provides process-tree containment with a Job Object and
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
//!
//! The first Windows helper slice is process containment only. It must not
//! claim read-only filesystem isolation, workspace-write enforcement, network
//! blocking, registry isolation, or AppContainer-level isolation until those
//! guarantees are implemented and tested separately.
//!
//! # Known limitation
//!
//! **Windows sandbox policy is not enforced.** `is_available()` is `false`,
//! so a policy that demands confinement (read-only, workspace-write) is
//! decided `SANDBOX_UNAVAILABLE` by `shell_manager::decide_sandbox`; with
//! the default fail-closed posture such commands are refused (diagnosable,
//! naming the alternatives) rather than silently run unconfined. Escape
//! hatches on Windows today: disable the sandbox in config
//! (`[sandbox] enabled = false`), set `fail_if_unavailable = false`
//! explicitly (degraded, commands run unconfined), or run in an unconfined
//! mode (e.g. YOLO).

use std::path::Path;

use super::SandboxPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsSandboxKind {
    ProcessContainment,
}

impl std::fmt::Display for WindowsSandboxKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowsSandboxKind::ProcessContainment => write!(f, "process-containment"),
        }
    }
}

pub fn is_available() -> bool {
    false
}

pub fn select_best_kind(_policy: &SandboxPolicy, _cwd: &Path) -> WindowsSandboxKind {
    WindowsSandboxKind::ProcessContainment
}

pub fn detect_denial(exit_code: i32, stderr: &str) -> bool {
    if exit_code == 0 {
        return false;
    }

    let patterns = [
        "Access is denied",
        "access denied",
        "STATUS_ACCESS_DENIED",
        "privilege",
        "AppContainer",
        "sandbox",
    ];

    patterns.iter().any(|p| stderr.contains(p))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_sandbox_is_not_advertised_until_helper_exists() {
        assert!(!is_available());
        assert_eq!(
            select_best_kind(&SandboxPolicy::default(), Path::new(".")),
            WindowsSandboxKind::ProcessContainment
        );
    }
}
