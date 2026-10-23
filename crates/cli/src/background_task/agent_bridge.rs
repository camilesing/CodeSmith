//! SubAgent-to-registry bridge.
//!
//! Utility functions for bridging SubAgentManager into the unified registry.

use super::BackgroundTaskStatus;
use crate::tools::subagent::SubAgentStatus;

/// Extract error message from SubAgentStatus if present.
pub fn subagent_error(status: &SubAgentStatus) -> Option<String> {
    match status {
        SubAgentStatus::Failed(e) => Some(e.clone()),
        SubAgentStatus::Interrupted(e) => Some(e.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_error_returns_message_for_failed() {
        assert_eq!(
            subagent_error(&SubAgentStatus::Failed("oops".to_string())),
            Some("oops".to_string())
        );
        assert_eq!(subagent_error(&SubAgentStatus::Running), None);
    }
}
