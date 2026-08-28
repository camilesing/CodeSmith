//! Post-compaction attachment re-injection.
//!
//! After compaction removes messages, active state (plans, skills,
//! subagents) that was referenced in removed messages gets lost.
//! This module re-injects those states as user messages with
//! system-reminder blocks so the model can continue working.

use crate::models::{ContentBlock, Message};
use crate::session::RecentReadFile;
use crate::subagent::SubAgentResult;
use crate::tool_state::plan::PlanSnapshot;
use crate::tool_state::todo::TodoListSnapshot;

/// Types of attachments that can be re-injected after compaction.
#[derive(Debug, Clone)]
pub enum AttachmentType {
    /// Active plan state (from plan tool).
    Plan { summary: String },
    /// Active skill definitions.
    Skills { definitions: Vec<String> },
    /// Running subagents.
    SubAgents { agent_summaries: Vec<AgentSummary> },
    /// Recent file reads.
    ReadFiles { paths: Vec<String> },
}

/// Summary of a running subagent.
#[derive(Debug, Clone)]
pub struct AgentSummary {
    /// Agent nickname.
    pub name: String,
    /// Current status.
    pub status: String,
    /// Brief description of what the agent is doing.
    pub description: String,
}

/// Result of attachment re-injection.
#[derive(Debug)]
pub struct AttachmentReinjectResult {
    /// Messages injected into the conversation tail.
    pub injected_messages: Vec<Message>,
    /// Types of attachments that were re-injected.
    pub types_reinjected: Vec<AttachmentType>,
}

/// Build a re-injection message from attachment content (the shared
/// `<system-reminder>` wrapper used by every reinject candidate). `pub(crate)`
/// (slice 25b §E) so the framework-core `HostAgentExecutor` reinject path can
/// reuse the same wrapper as production's `Engine::reinject_compaction_attachments`.
pub(crate) fn compaction_reinject_message(content: String) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: format!("<system-reminder>\n{content}\n</system-reminder>"),
            cache_control: None,
        }],
    }
}

/// Format an active plan snapshot into a re-injectable summary body. Returns
/// `None` when the plan is empty (no explanation + no steps) so the caller
/// skips the candidate. Extracted from `Engine` (slice 25b §E) so the
/// framework-core executor reinject path formats plans identically.
pub(crate) fn format_plan_reinject_summary(snapshot: &PlanSnapshot) -> Option<String> {
    if snapshot.items.is_empty()
        && snapshot
            .explanation
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(explanation) = snapshot
        .explanation
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        lines.push(explanation.trim().to_string());
        lines.push(String::new());
    }
    for item in &snapshot.items {
        lines.push(format!("- {:?}: {}", item.status, item.step));
    }
    Some(lines.join("\n"))
}

/// Format a todo-list snapshot into a re-injectable summary body. Returns
/// `None` when there are no items so the caller skips the candidate. Extracted
/// from `Engine` (slice 25b §E) so the framework-core executor formats todos
/// identically.
pub(crate) fn format_todo_reinject_summary(snapshot: &TodoListSnapshot) -> Option<String> {
    if snapshot.items.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for item in &snapshot.items {
        lines.push(format!(
            "- #{} {:?}: {}",
            item.id, item.status, item.content
        ));
    }
    Some(lines.join("\n"))
}

/// Map live-running sub-agent snapshots into re-injectable [`AgentSummary`]s
/// (name / "running" status / id+role+objective+model+steps description).
/// Extracted from `Engine::compaction_subagent_summaries` (slice 25b §E) so
/// the framework-core executor (which holds a `SubAgentApi`, not the full
/// `HostServices`) can summarize sub-agents identically.
pub(crate) fn summarize_subagents(snapshots: &[SubAgentResult]) -> Vec<AgentSummary> {
    snapshots
        .iter()
        .map(|snapshot| {
            let name = if snapshot.name.trim().is_empty() {
                snapshot.agent_id.clone()
            } else {
                snapshot.name.clone()
            };
            let role = snapshot.assignment.role.as_deref().unwrap_or("unspecified");
            let description = format!(
                "id={}, role={}, objective={}, model={}, steps={}",
                snapshot.agent_id,
                role,
                snapshot.assignment.objective,
                snapshot.model,
                snapshot.steps_taken
            );
            AgentSummary {
                name,
                status: "running".to_string(),
                description,
            }
        })
        .collect()
}

/// Re-inject plan state after compaction.
///
/// If there's an active plan, inject its current state as a user message
/// so the model doesn't lose track of what it was planning.
pub fn reinject_plan_attachment(plan_summary: &str) -> Option<Message> {
    if plan_summary.trim().is_empty() {
        return None;
    }
    Some(compaction_reinject_message(format!(
        "Active plan resumed after context compaction:\n\n{plan_summary}"
    )))
}

/// Re-inject skill definitions after compaction.
///
/// Active skills that were loaded before compaction may have been
/// referenced in removed messages. Re-inject their definitions.
pub fn reinject_skill_attachments(skill_definitions: &[String]) -> Option<Message> {
    if skill_definitions.is_empty() {
        return None;
    }
    let content = skill_definitions
        .iter()
        .map(|def| format!("---\n{def}\n---"))
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(compaction_reinject_message(format!(
        "Active skills resumed after context compaction:\n\n{content}"
    )))
}

/// Re-inject running subagent summaries after compaction.
///
/// Subagents that were spawned before compaction may have their
/// context in removed messages. Re-inject their status summaries.
pub fn reinject_subagent_attachments(agent_summaries: &[AgentSummary]) -> Option<Message> {
    if agent_summaries.is_empty() {
        return None;
    }
    let content = agent_summaries
        .iter()
        .map(|a| format!("- **{}** ({}): {}", a.name, a.status, a.description))
        .collect::<Vec<_>>()
        .join("\n");
    Some(compaction_reinject_message(format!(
        "Running subagents resumed after context compaction:\n\n{content}"
    )))
}

/// Re-inject recent read_file outputs after compaction.
pub fn reinject_read_file_attachments(read_files: &[RecentReadFile]) -> Option<Message> {
    if read_files.is_empty() {
        return None;
    }
    let mut content = String::from(
        "Recent read_file outputs retained after context compaction. Treat these as reminders only; re-read files before making claims that require fresh contents.\n\n",
    );
    for entry in read_files.iter().rev() {
        let _ = std::fmt::Write::write_fmt(
            &mut content,
            format_args!(
                "## `{}`\nInput: `{}`\n\n```\n{}\n```\n\n",
                entry.path, entry.input, entry.output_preview
            ),
        );
    }
    Some(compaction_reinject_message(content.trim_end().to_string()))
}

/// Re-inject all available attachments after compaction.
///
/// Checks session state for active plans, skills, and subagents,
/// and re-injects their current state as user messages.
pub fn reinject_all_attachments(
    plan_summary: Option<&str>,
    skill_definitions: &[String],
    agent_summaries: &[AgentSummary],
    read_files: &[RecentReadFile],
) -> AttachmentReinjectResult {
    let mut injected_messages = Vec::new();
    let mut types_reinjected = Vec::new();

    if let Some(plan) = plan_summary
        && let Some(msg) = reinject_plan_attachment(plan)
    {
        injected_messages.push(msg);
        types_reinjected.push(AttachmentType::Plan {
            summary: plan.to_string(),
        });
    }

    if let Some(msg) = reinject_skill_attachments(skill_definitions) {
        injected_messages.push(msg);
        types_reinjected.push(AttachmentType::Skills {
            definitions: skill_definitions.to_vec(),
        });
    }

    if let Some(msg) = reinject_subagent_attachments(agent_summaries) {
        injected_messages.push(msg);
        types_reinjected.push(AttachmentType::SubAgents {
            agent_summaries: agent_summaries.to_vec(),
        });
    }

    if let Some(msg) = reinject_read_file_attachments(read_files) {
        injected_messages.push(msg);
        types_reinjected.push(AttachmentType::ReadFiles {
            paths: read_files.iter().map(|entry| entry.path.clone()).collect(),
        });
    }

    AttachmentReinjectResult {
        injected_messages,
        types_reinjected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reinject_plan_with_content() {
        let msg = reinject_plan_attachment("Step 1: Refactor auth module");
        assert!(msg.is_some());
        let text = &msg.unwrap().content[0];
        if let ContentBlock::Text { text, .. } = text {
            assert!(text.contains("Active plan resumed"));
            assert!(text.contains("Step 1: Refactor auth module"));
        }
    }

    #[test]
    fn reinject_plan_empty_returns_none() {
        assert!(reinject_plan_attachment("").is_none());
        assert!(reinject_plan_attachment("   ").is_none());
    }

    #[test]
    fn reinject_skills_with_definitions() {
        let defs = vec![
            "skill 1: debug mode".to_string(),
            "skill 2: tdd".to_string(),
        ];
        let msg = reinject_skill_attachments(&defs);
        assert!(msg.is_some());
        let text = &msg.unwrap().content[0];
        if let ContentBlock::Text { text, .. } = text {
            assert!(text.contains("Active skills resumed"));
            assert!(text.contains("skill 1"));
        }
    }

    #[test]
    fn reinject_skills_empty_returns_none() {
        assert!(reinject_skill_attachments(&[]).is_none());
    }

    #[test]
    fn reinject_subagents_with_summaries() {
        let summaries = vec![AgentSummary {
            name: "researcher".to_string(),
            status: "running".to_string(),
            description: "Searching for auth patterns".to_string(),
        }];
        let msg = reinject_subagent_attachments(&summaries);
        assert!(msg.is_some());
        let text = &msg.unwrap().content[0];
        if let ContentBlock::Text { text, .. } = text {
            assert!(text.contains("Running subagents resumed"));
            assert!(text.contains("researcher"));
        }
    }

    #[test]
    fn reinject_subagents_empty_returns_none() {
        assert!(reinject_subagent_attachments(&[]).is_none());
    }

    #[test]
    fn reinject_read_files_with_recent_outputs() {
        let read_files = vec![RecentReadFile {
            path: "src/lib.rs".to_string(),
            input: serde_json::json!({"path": "src/lib.rs", "limit": 20}),
            output_preview: "fn main() {}".to_string(),
        }];

        let msg = reinject_read_file_attachments(&read_files);
        assert!(msg.is_some());
        let text = &msg.unwrap().content[0];
        if let ContentBlock::Text { text, .. } = text {
            assert!(text.contains("Recent read_file outputs retained"));
            assert!(text.contains("re-read files before making claims"));
            assert!(text.contains("src/lib.rs"));
            assert!(text.contains("fn main() {}"));
        }
    }

    #[test]
    fn reinject_read_files_empty_returns_none() {
        assert!(reinject_read_file_attachments(&[]).is_none());
    }

    #[test]
    fn reinject_all_combines_multiple_types() {
        let result = reinject_all_attachments(
            Some("Plan: step 1"),
            &["skill: debug".to_string()],
            &[AgentSummary {
                name: "agent-1".to_string(),
                status: "running".to_string(),
                description: "working".to_string(),
            }],
            &[RecentReadFile {
                path: "src/lib.rs".to_string(),
                input: serde_json::json!({"path": "src/lib.rs"}),
                output_preview: "contents".to_string(),
            }],
        );
        assert_eq!(result.injected_messages.len(), 4);
        assert_eq!(result.types_reinjected.len(), 4);
    }

    #[test]
    fn reinject_all_with_nothing_returns_empty() {
        let result = reinject_all_attachments(None, &[], &[], &[]);
        assert!(result.injected_messages.is_empty());
        assert!(result.types_reinjected.is_empty());
    }
}
