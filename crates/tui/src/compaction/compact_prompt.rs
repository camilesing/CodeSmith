//! Enhanced compaction prompt template with 9-part structured summary.
//!
//! Replaces the existing simple 3-part template (Conversation Summary /
//! Workflow Context / What to Do Next) with a more comprehensive structure
//! that preserves critical context across compaction cycles.

use std::fmt::Write;

/// Sections extracted from conversation messages for structured summary.
#[derive(Debug, Clone, Default)]
pub struct CompactSummarySections {
    /// The user's primary request and intent.
    pub primary_request: String,
    /// Key technical concepts referenced during the conversation.
    pub key_technical_concepts: Vec<String>,
    /// Files and code sections touched or discussed.
    pub files_and_code: Vec<FileEntry>,
    /// Errors encountered and their resolution status.
    pub errors: Vec<ErrorEntry>,
    /// Problem-solving approaches and solutions applied.
    pub problem_solving: Vec<String>,
    /// User messages and stated preferences.
    pub user_messages: Vec<String>,
    /// Pending tasks and TODOs identified.
    pub pending_tasks: Vec<String>,
    /// Current work status (what the assistant was doing when compacted).
    pub current_work: String,
    /// Suggested next step for continuation.
    pub next_step: String,
}

/// A file entry in the compaction summary.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Workspace-relative path.
    pub path: String,
    /// Action taken on the file.
    pub action: String, // "read", "modified", "created", "deleted"
    /// Brief summary of what was done with this file.
    pub summary: String,
}

/// An error entry in the compaction summary.
#[derive(Debug, Clone)]
pub struct ErrorEntry {
    /// Type/category of the error.
    pub error_type: String,
    /// Error message or description.
    pub message: String,
    /// Resolution status (None = unresolved).
    pub resolution: Option<String>,
}

/// Extract structured summary sections from conversation messages.
///
/// Programatically extracts the 9 sections from messages, using path
/// extraction, error marker detection, and working set heuristics.
/// The resulting sections are then formatted into a structured prompt.
pub fn extract_compact_sections(
    messages: &[crate::models::Message],
    workspace: Option<&std::path::Path>,
) -> CompactSummarySections {
    use crate::models::ContentBlock;

    let mut sections = CompactSummarySections::default();

    // Track files, errors, and user messages across all messages.
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_errors: std::collections::HashSet<String> = std::collections::HashSet::new();

    for msg in messages {
        // Extract user messages.
        if msg.role == "user" {
            for block in &msg.content {
                if let ContentBlock::Text { text, .. } = block {
                    // First user message is the primary request.
                    if sections.primary_request.is_empty() {
                        sections.primary_request = truncate_to(text, 500);
                    } else {
                        sections.user_messages.push(truncate_to(text, 200));
                    }
                    // Check for task/TODO mentions.
                    extract_tasks_from_text(text, &mut sections.pending_tasks);
                }
            }
        }

        // Extract tool calls for files and actions.
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    let tool_action = match name.as_str() {
                        "file_read" => "read",
                        "write_file" => "modified",
                        "edit_file" => "modified",
                        "apply_patch" => "modified",
                        "exec_shell" | "exec_shell_wait" => "executed",
                        "grep" => "searched",
                        "glob" => "listed",
                        "web_search" | "web_fetch" => "researched",
                        _ => "used",
                    };

                    // Extract file paths from tool input.
                    let paths =
                        crate::compaction::extract_paths_from_tool_input(input, workspace);
                    for path in paths {
                        if seen_paths.insert(path.clone()) {
                            sections.files_and_code.push(FileEntry {
                                path,
                                action: tool_action.to_string(),
                                summary: format!("[{name}]"),
                            });
                        }
                    }

                    // Extract technical concepts from tool names.
                    if !sections.key_technical_concepts.contains(&name.clone()) {
                        sections.key_technical_concepts.push(name.clone());
                    }
                }
                ContentBlock::ToolResult { content, .. } => {
                    // Check for errors in tool results.
                    extract_errors_from_text(content, &mut sections.errors, &mut seen_errors);
                    // Extract paths from tool results too.
                    let paths = crate::compaction::extract_paths_from_text(content, workspace);
                    for path in paths {
                        if seen_paths.insert(path.clone()) {
                            sections.files_and_code.push(FileEntry {
                                path,
                                action: "referenced".to_string(),
                                summary: truncate_to(content, 100),
                            });
                        }
                    }
                }
                ContentBlock::Text { text, .. } if msg.role == "assistant" => {
                    // Check for problem-solving descriptions.
                    extract_problem_solving_from_text(text, &mut sections.problem_solving);
                    // Check for errors in assistant messages.
                    extract_errors_from_text(text, &mut sections.errors, &mut seen_errors);
                    // Track current work from most recent assistant text.
                    if !text.trim().is_empty() {
                        sections.current_work = truncate_to(text, 300);
                    }
                }
                _ => {}
            }
        }
    }

    // Derive next step from current work + pending tasks.
    if let Some(task) = sections.pending_tasks.first() {
        sections.next_step = task.clone();
    } else if !sections.current_work.is_empty() {
        sections.next_step = "Continue the current task.".to_string();
    }

    sections
}

/// Format the extracted sections into a structured Markdown summary prompt.
///
/// Produces a formatted block suitable for inclusion in the compaction
/// summary system prompt.
pub fn format_compact_summary_prompt(sections: &CompactSummarySections) -> String {
    let mut output = String::new();

    if !sections.primary_request.is_empty() {
        let _ = writeln!(output, "## 1. Primary Request and Intent");
        let _ = writeln!(output, "{}", sections.primary_request);
        let _ = writeln!(output);
    }

    if !sections.key_technical_concepts.is_empty() {
        let _ = writeln!(output, "## 2. Key Technical Concepts");
        for concept in &sections.key_technical_concepts {
            let _ = writeln!(output, "- {concept}");
        }
        let _ = writeln!(output);
    }

    if !sections.files_and_code.is_empty() {
        let _ = writeln!(output, "## 3. Files and Code Sections");
        for entry in &sections.files_and_code {
            let _ = writeln!(output, "- `{}` ({}) {}", entry.path, entry.action, entry.summary);
        }
        let _ = writeln!(output);
    }

    if !sections.errors.is_empty() {
        let _ = writeln!(output, "## 4. Errors and Debugging History");
        for entry in &sections.errors {
            match &entry.resolution {
                Some(res) => {
                    let _ = writeln!(output, "- {} [{}]: resolved — {res}", entry.error_type, entry.message);
                }
                None => {
                    let _ = writeln!(output, "- {} [{}]: unresolved", entry.error_type, entry.message);
                }
            }
        }
        let _ = writeln!(output);
    }

    if !sections.problem_solving.is_empty() {
        let _ = writeln!(output, "## 5. Problem Solving and Solutions");
        for solution in &sections.problem_solving {
            let _ = writeln!(output, "- {solution}");
        }
        let _ = writeln!(output);
    }

    if !sections.user_messages.is_empty() {
        let _ = writeln!(output, "## 6. User Messages and Preferences");
        for msg in &sections.user_messages {
            let _ = writeln!(output, "- {msg}");
        }
        let _ = writeln!(output);
    }

    if !sections.pending_tasks.is_empty() {
        let _ = writeln!(output, "## 7. Pending Tasks and TODOs");
        for task in &sections.pending_tasks {
            let _ = writeln!(output, "- {task}");
        }
        let _ = writeln!(output);
    }

    if !sections.current_work.is_empty() {
        let _ = writeln!(output, "## 8. Current Work Status");
        let _ = writeln!(output, "{}", sections.current_work);
        let _ = writeln!(output);
    }

    if !sections.next_step.is_empty() {
        let _ = writeln!(output, "## 9. Next Step");
        let _ = writeln!(output, "{}", sections.next_step);
        let _ = writeln!(output);
    }

    output
}

/// Enhanced summary instruction for the LLM compaction call.
///
/// This replaces the simple `summary_instruction()` in mod.rs with a
/// more structured prompt that asks the model to produce a 9-section summary.
pub fn enhanced_summary_instruction(word_limit: usize) -> String {
    format!(
        "Create a detailed summary of the conversation above with the following sections. \
         Each section should be concise but preserve critical information needed to continue \
         the work without losing context.\n\n\
         1. **Primary Request and Intent**: What the user asked for and why.\n\
         2. **Key Technical Concepts**: Important technologies, frameworks, patterns referenced.\n\
         3. **Files and Code Sections**: Files read, modified, or created — include exact paths.\n\
         4. **Errors and Fixes**: Errors encountered, debugging steps, and whether they were resolved.\n\
         5. **Problem Solving**: Approaches tried, decisions made, trade-offs considered.\n\
         6. **User Messages**: Key user messages and stated preferences.\n\
         7. **Pending Tasks**: TODOs, unfinished work, open questions.\n\
         8. **Current Work**: What was being done when this summary was generated.\n\
         9. **Next Step**: Suggested next action to continue the task.\n\n\
         Preserve exact file paths, command names, and error messages. \
         Abbreviate repetitive tool outputs but keep unique results. \
         Keep it under {word_limit} words."
    )
}

/// Truncate text to a maximum character count.
fn truncate_to(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let end_idx = text
            .char_indices()
            .nth(max_chars)
            .map_or(text.len(), |(idx, _)| idx);
        format!("{}...", &text[..end_idx])
    }
}

/// Extract task/TODO mentions from text.
fn extract_tasks_from_text(text: &str, tasks: &mut Vec<String>) {
    let lower = text.to_lowercase();
    let markers = ["todo", "task", "need to", "must", "should", "pending"];
    for marker in markers {
        if lower.contains(marker) {
            let task = truncate_to(text, 200);
            if !tasks.contains(&task) {
                tasks.push(task);
            }
            return; // One task per text block
        }
    }
}

/// Extract error entries from text.
fn extract_errors_from_text(
    text: &str,
    errors: &mut Vec<ErrorEntry>,
    seen: &mut std::collections::HashSet<String>,
) {
    let lower = text.to_lowercase();
    let error_markers = [
        ("compilation", "error:"),
        ("runtime", "panic"),
        ("test", "test failed"),
        ("runtime", "traceback"),
        ("runtime", "stack trace"),
        ("assertion", "assertion failed"),
        ("compilation", "failed"),
    ];

    for (error_type, marker) in error_markers {
        if lower.contains(marker) {
            let key = format!("{error_type}:{marker}");
            if seen.insert(key.clone()) {
                errors.push(ErrorEntry {
                    error_type: error_type.to_string(),
                    message: truncate_to(text, 150),
                    resolution: None,
                });
            }
        }
    }
}

/// Extract problem-solving descriptions from assistant text.
fn extract_problem_solving_from_text(text: &str, solutions: &mut Vec<String>) {
    let lower = text.to_lowercase();
    let markers = ["fixed", "resolved", "solved", "workaround", "solution", "approach"];
    for marker in markers {
        if lower.contains(marker) {
            let solution = truncate_to(text, 200);
            if !solutions.contains(&solution) {
                solutions.push(solution);
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentBlock, Message};
    use serde_json::json;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_use_msg(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
            }],
        }
    }

    fn tool_result_msg(id: &str, content: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    #[test]
    fn extract_primary_request_from_first_user_message() {
        let messages = vec![
            msg("user", "Please refactor the authentication module"),
            msg("assistant", "I'll help with that."),
        ];
        let sections = extract_compact_sections(&messages, None);
        assert!(sections.primary_request.contains("refactor the authentication module"));
    }

    #[test]
    fn extract_files_from_tool_calls() {
        let messages = vec![
            tool_use_msg("t1", "file_read", json!({"path": "src/auth.rs"})),
            tool_result_msg("t1", "file content here"),
            tool_use_msg("t2", "edit_file", json!({"path": "src/auth_mod.rs"})),
            tool_result_msg("t2", "edit applied"),
        ];
        let sections = extract_compact_sections(&messages, None);
        assert!(sections.files_and_code.iter().any(|f| f.path == "src/auth.rs" && f.action == "read"));
        assert!(sections.files_and_code.iter().any(|f| f.path == "src/auth_mod.rs" && f.action == "modified"));
    }

    #[test]
    fn extract_errors_from_messages() {
        let messages = vec![
            msg("assistant", "error: compilation failed at src/main.rs"),
            msg("assistant", "I fixed the error by correcting the type"),
        ];
        let sections = extract_compact_sections(&messages, None);
        assert!(sections.errors.iter().any(|e| e.error_type == "compilation"));
    }

    #[test]
    fn extract_tasks_from_text() {
        let messages = vec![
            msg("user", "TODO: implement caching layer"),
            msg("assistant", "Working on it."),
        ];
        let sections = extract_compact_sections(&messages, None);
        assert!(sections.pending_tasks.iter().any(|t| t.contains("TODO")));
    }

    #[test]
    fn format_produces_structured_markdown() {
        let sections = CompactSummarySections {
            primary_request: "Refactor auth module".to_string(),
            key_technical_concepts: vec!["OAuth2".to_string()],
            files_and_code: vec![FileEntry {
                path: "src/auth.rs".to_string(),
                action: "modified".to_string(),
                summary: "[edit_file]".to_string(),
            }],
            errors: vec![],
            problem_solving: vec![],
            user_messages: vec!["Also add caching".to_string()],
            pending_tasks: vec!["Implement caching".to_string()],
            current_work: "Editing auth.rs".to_string(),
            next_step: "Implement caching".to_string(),
        };

        let formatted = format_compact_summary_prompt(&sections);
        assert!(formatted.contains("## 1. Primary Request and Intent"));
        assert!(formatted.contains("## 3. Files and Code Sections"));
        assert!(formatted.contains("`src/auth.rs`"));
        assert!(formatted.contains("## 7. Pending Tasks and TODOs"));
        assert!(formatted.contains("## 9. Next Step"));
    }

    #[test]
    fn enhanced_instruction_contains_all_sections() {
        let instruction = enhanced_summary_instruction(500);
        assert!(instruction.contains("Primary Request"));
        assert!(instruction.contains("Key Technical Concepts"));
        assert!(instruction.contains("Files and Code Sections"));
        assert!(instruction.contains("Errors and Fixes"));
        assert!(instruction.contains("Problem Solving"));
        assert!(instruction.contains("User Messages"));
        assert!(instruction.contains("Pending Tasks"));
        assert!(instruction.contains("Current Work"));
        assert!(instruction.contains("Next Step"));
        assert!(instruction.contains("500"));
    }

    #[test]
    fn truncate_preserves_short_text() {
        assert_eq!(truncate_to("short", 10), "short");
    }

    #[test]
    fn truncate_adds_ellipsis_for_long_text() {
        let long = "abcdefghij".repeat(10);
        let truncated = truncate_to(&long, 50);
        assert!(truncated.ends_with("..."));
        assert!(truncated.chars().count() <= 53); // 50 + "..."
    }
}