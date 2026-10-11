//! Auto-mode permission strip/restore.
//!
//! Mirrors Claude Code's `stripDangerousPermissionsForAutoMode` /
//! `restoreDangerousPermissions` (see
//! `analysis/05-differentiators-and-comparison.md` §4). When the user
//! enters auto-accept mode — or YOLO, which implies it — catastrophic
//! commands are *stripped* from the auto-elevate set: they are denied even
//! though approval is auto-skipped, so the model can never auto-run
//! `rm -rf /`, fork bombs, shell `eval`, or pipe-remote-content-to-shell.
//!
//! The non-dangerous remainder may elevate to the active mode's baseline
//! sandbox policy, and only when the caller explicitly permits elevation
//! (trust mode / YOLO opt-in). Without that opt-in, auto-approve handles
//! *approval* but does not bypass the *sandbox* — the sandbox is a hard
//! boundary that approval-bypass never silently removes.
//!
//! This closes the security hole where `ApprovalMode::Auto` /
//! `--auto-approve` silently re-ran sandbox-denied tools with
//! `DangerFullAccess` (no sandbox at all), conflating approval-bypass with
//! sandbox-bypass.

use crate::command_safety::{self, SafetyLevel};
use crate::sandbox::SandboxPolicy;

/// Detect catastrophic commands that must be stripped from auto-approve.
///
/// Delegates to `command_safety::analyze_command` — the existing authority
/// for the manual approval path — so the auto-mode strip set stays in
/// lock-step with the manual-approval dangerous set: token-aware `rm -rf`
/// matching, shell `eval`, pipe-remote-content-to-shell, null bytes, and
/// the literal legacy patterns (`rm -rf /`, fork bomb, …). Returns the
/// human-readable reason(s) when the command is `Dangerous`, `None`
/// otherwise.
///
/// `RequiresApproval` commands (command chains, substitution, privileged
/// commands) are *not* stripped — auto-approve exists precisely to
/// auto-grant that tier. Only the `Dangerous` tier is stripped.
pub fn dangerous_command_reason(command: &str) -> Option<String> {
    let analysis = command_safety::analyze_command(command);
    if analysis.level == SafetyLevel::Dangerous {
        let reason = analysis.reasons.join("; ");
        Some(if reason.is_empty() {
            "dangerous command".to_string()
        } else {
            reason
        })
    } else {
        None
    }
}

/// Decision returned for a sandbox-denied tool in auto-approve mode.
#[derive(Debug)]
pub enum AutoElevationDecision {
    /// Strip: the command is catastrophic and must be denied even in auto
    /// mode. Carries the reason so the model sees *why* it was blocked.
    Deny { reason: String },
    /// Elevate to the caller-supplied baseline sandbox policy. The caller
    /// chooses this target (the active mode's baseline) and only receives
    /// this variant when `allow_elevation` was true, so `DangerFullAccess`
    /// here is always an explicit YOLO opt-in — never a silent default.
    ElevateTo(SandboxPolicy),
}

/// Decide what auto-approve mode does with a sandbox-denied tool.
///
/// Evaluation order (mirrors Claude's strip-then-grant):
/// 1. **Strip** — if `command` is catastrophic ([`dangerous_command_reason`]),
///    always `Deny`, regardless of `allow_elevation` or `elevation_target`.
/// 2. **Grant** — if `allow_elevation` is true, `ElevateTo(elevation_target)`.
/// 3. **Deny** — otherwise, deny with a reason explaining that auto-approve
///    does not bypass the sandbox without the trust/YOLO opt-in.
///
/// `elevation_target` is the most permissive policy auto-approve may
/// escalate to — the active mode's baseline sandbox
/// (`sandbox_policy_for_mode`). Callers pass `DangerFullAccess` *only* for
/// YOLO (whose baseline is full access by explicit opt-in).
#[must_use]
pub fn decide_auto_elevation(
    command: Option<&str>,
    allow_elevation: bool,
    elevation_target: SandboxPolicy,
) -> AutoElevationDecision {
    if let Some(cmd) = command
        && let Some(reason) = dangerous_command_reason(cmd)
    {
        return AutoElevationDecision::Deny {
            reason: format!("auto-mode stripped dangerous command: {reason}"),
        };
    }
    if allow_elevation {
        AutoElevationDecision::ElevateTo(elevation_target)
    } else {
        AutoElevationDecision::Deny {
            reason: "sandbox denied; auto-approve does not bypass the sandbox without trust mode"
                .to_string(),
        }
    }
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_catastrophic_rm_rf_root() {
        assert!(dangerous_command_reason("rm -rf /").is_some());
        assert!(dangerous_command_reason("rm -rf /*").is_some());
        assert!(dangerous_command_reason("sudo rm -rf $HOME").is_some());
    }

    #[test]
    fn strips_fork_bomb() {
        assert!(dangerous_command_reason(":(){ :|:& };:").is_some());
    }

    #[test]
    fn does_not_strip_requires_approval_tier() {
        // Command chains and substitution escalate to RequiresApproval,
        // not Dangerous — auto-approve should still grant them.
        assert!(dangerous_command_reason("cargo build && cargo test").is_none());
        assert!(dangerous_command_reason("echo $(date)").is_none());
        assert!(dangerous_command_reason("git status").is_none());
        assert!(dangerous_command_reason("ls -la").is_none());
    }

    #[test]
    fn decide_strips_dangerous_even_with_full_access_target() {
        // YOLO opt-in must still strip catastrophic commands.
        let decision =
            decide_auto_elevation(Some("rm -rf /"), true, SandboxPolicy::DangerFullAccess);
        assert!(matches!(decision, AutoElevationDecision::Deny { .. }));
        if let AutoElevationDecision::Deny { reason } = decision {
            assert!(reason.contains("stripped"));
        }
    }

    #[test]
    fn decide_elevates_when_allowed_and_not_dangerous() {
        let decision =
            decide_auto_elevation(Some("cargo build"), true, SandboxPolicy::DangerFullAccess);
        assert!(matches!(
            decision,
            AutoElevationDecision::ElevateTo(SandboxPolicy::DangerFullAccess)
        ));
    }

    #[test]
    fn decide_denies_when_not_allowed_and_not_dangerous() {
        // Agent + Auto (no trust): approval is bypassed but the sandbox is
        // not — a sandbox-denied tool is denied, not auto-elevated.
        let decision =
            decide_auto_elevation(Some("cargo build"), false, SandboxPolicy::DangerFullAccess);
        assert!(matches!(decision, AutoElevationDecision::Deny { .. }));
        if let AutoElevationDecision::Deny { reason } = decision {
            assert!(reason.contains("does not bypass the sandbox"));
        }
    }

    #[test]
    fn decide_handles_none_command() {
        // Non-shell tools (no command) still go through the grant/deny gate.
        let elevate = decide_auto_elevation(None, true, SandboxPolicy::DangerFullAccess);
        assert!(matches!(elevate, AutoElevationDecision::ElevateTo(_)));

        let deny = decide_auto_elevation(None, false, SandboxPolicy::DangerFullAccess);
        assert!(matches!(deny, AutoElevationDecision::Deny { .. }));
    }
}
