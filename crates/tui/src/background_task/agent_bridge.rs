//! SubAgent-to-registry bridge.
//!
//! Maps SubAgentStatus → BackgroundTaskStatus and provides utility
//! functions for bridging SubAgentManager into the unified registry.

use super::BackgroundTaskStatus;
use crate::tools::subagent::SubAgentStatus;

/// Map SubAgentStatus → BackgroundTaskStatus.
/// Standalone function so it can be used without the registry.
pub fn map_subagent_status(status: &SubAgentStatus) -> BackgroundTaskStatus {
    match status {
        SubAgentStatus::Running => BackgroundTaskStatus::Running,
        SubAgentStatus::Completed => BackgroundTaskStatus::Completed,
        SubAgentStatus::Interrupted(_) => BackgroundTaskStatus::Failed,
        SubAgentStatus::Failed(_) => BackgroundTaskStatus::Failed,
        SubAgentStatus::Cancelled => BackgroundTaskStatus::Cancelled,
    }
}

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
    use crate::background_task::BackgroundTaskStatus;

    #[test]
    fn map_subagent_status_running_to_running() {
        assert_eq!(
            map_subagent_status(&SubAgentStatus::Running),
            BackgroundTaskStatus::Running
        );
        assert_eq!(
            map_subagent_status(&SubAgentStatus::Completed),
            BackgroundTaskStatus::Completed
        );
        assert_eq!(
            map_subagent_status(&SubAgentStatus::Cancelled),
            BackgroundTaskStatus::Cancelled
        );
    }

    #[test]
    fn map_subagent_status_failed_and_interrupted_to_failed() {
        assert_eq!(
            map_subagent_status(&SubAgentStatus::Failed("err".to_string())),
            BackgroundTaskStatus::Failed
        );
        assert_eq!(
            map_subagent_status(&SubAgentStatus::Interrupted("sig".to_string())),
            BackgroundTaskStatus::Failed
        );
    }

    #[test]
    fn subagent_error_returns_message_for_failed() {
        assert_eq!(
            subagent_error(&SubAgentStatus::Failed("oops".to_string())),
            Some("oops".to_string())
        );
        assert_eq!(subagent_error(&SubAgentStatus::Running), None);
    }
}
