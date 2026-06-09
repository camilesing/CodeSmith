//! SubAgent-to-registry bridge.
//!
//! Maps SubAgentStatus → BackgroundTaskStatus and provides utility
//! functions for bridging SubAgentManager into the unified registry.

use crate::tools::subagent::SubAgentStatus;
use super::BackgroundTaskStatus;

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