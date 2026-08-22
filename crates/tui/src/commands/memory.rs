//! `/memory` slash command — inspect and edit the user memory file.
//!
//! When the user-memory feature is opted-in (`[memory] enabled = true` in
//! config or `DEEPSEEK_MEMORY=on` in the environment), `/memory` shows
//! the current memory file path and contents inline. Subcommands let the
//! user clear or open the file:
//!
//! - `/memory` — show path + content
//! - `/memory show` — alias for the no-arg form
//! - `/memory clear` — replace the file contents with an empty marker
//! - `/memory extract --dry-run` — build a memory-extraction worker prompt without writing
//! - `/memory path` — show only the resolved path
//! - `/memory help` — show command-specific help and the resolved path
//!
//! Editor integration (`/memory edit`) is intentionally minimal: the
//! command prints a copy-pasteable shell line to open the file in the
//! user's `$VISUAL` / `$EDITOR`, since the in-process external editor
//! plumbing requires terminal teardown that the slash-command handler
//! doesn't have access to.

use std::fs;
use std::path::{Path, PathBuf};

use super::CommandResult;
use crate::agent_memory::{
    AgentMemoryScope, agent_memory_candidates, load_snapshot_status, resolve_agent_memory_dir,
    resolve_agent_memory_entrypoint,
};
use crate::models::ContentBlock;
use crate::prompts::{MemoryExtractionMessage, build_memory_extraction_prompt};
use crate::tui::app::{App, AppAction};

const MEMORY_USAGE: &str =
    "/memory [show|path|clear|edit|extract --dry-run|help|agent <type> [scope]]";
const AGENT_MEMORY_SCOPES: &[AgentMemoryScope] = &[
    AgentMemoryScope::User,
    AgentMemoryScope::Project,
    AgentMemoryScope::Local,
];

fn memory_help(path: &Path) -> String {
    format!(
        "Inspect or manage persistent memory.

\
         Usage: {MEMORY_USAGE}

\
         Current user-memory path: {}

\
         Subcommands:
\
           /memory                    Show the resolved user-memory path and contents
\
           /memory show               Alias for the no-arg form
\
           /memory path               Print just the resolved user-memory path
\
           /memory clear              Replace the user-memory file contents with an empty marker
\
          /memory edit               Print the editor command for the user-memory file
\
          /memory extract --dry-run  Build a memory-extraction prompt from recent messages without writing
\
          /memory agent <type>       Show agent memory directories for a sub-agent type
\
           /memory agent <type> <scope> Show MEMORY.md and snapshot status for user/project/local scope
\
           /memory help               Show this help

\
         Quick capture: type `# foo` in the composer to append a timestamped
\
         bullet without firing a turn.",
        path.display()
    )
}

fn show_user_memory(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => format!(
            "{}\n(empty — add via `# foo` from the composer or have the model use the `remember` tool)",
            path.display()
        ),
        Ok(text) => format!("{}\n\n{}", path.display(), text.trim_end()),
        Err(_) => format!(
            "{}\n(file does not exist yet — add via `# foo` from the composer to create it)",
            path.display()
        ),
    }
}

const MEMORY_EXTRACT_DEFAULT_MESSAGES: usize = 24;
const MEMORY_EXTRACT_MAX_MESSAGES: usize = 80;
const MEMORY_EXTRACT_PREVIEW_MAX_CHARS: usize = 16_000;

fn parse_extract_args(args: &str) -> Result<usize, String> {
    let mut max_messages = MEMORY_EXTRACT_DEFAULT_MESSAGES;
    let mut saw_dry_run = false;
    let mut parts = args.split_whitespace().peekable();

    while let Some(part) = parts.next() {
        match part {
            "--dry-run" => saw_dry_run = true,
            "--messages" | "--max-messages" => {
                let Some(value) = parts.next() else {
                    return Err(format!("missing value for `{part}`"));
                };
                max_messages = value.parse::<usize>().map_err(|_| {
                    format!("invalid value `{value}` for `{part}`; expected a positive integer")
                })?;
            }
            value if value.starts_with("--messages=") => {
                let raw = value.trim_start_matches("--messages=");
                max_messages = raw.parse::<usize>().map_err(|_| {
                    format!("invalid value `{raw}` for `--messages`; expected a positive integer")
                })?;
            }
            value if value.starts_with("--max-messages=") => {
                let raw = value.trim_start_matches("--max-messages=");
                max_messages = raw.parse::<usize>().map_err(|_| {
                    format!(
                        "invalid value `{raw}` for `--max-messages`; expected a positive integer"
                    )
                })?;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    if !saw_dry_run {
        return Err("`/memory extract` currently supports only `--dry-run`".to_string());
    }
    if max_messages == 0 {
        return Err("message count must be greater than zero".to_string());
    }

    Ok(max_messages.min(MEMORY_EXTRACT_MAX_MESSAGES))
}

fn content_block_for_memory_extract(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text { text, .. } => Some(text.clone()),
        ContentBlock::Thinking { thinking } => Some(format!(
            "[thinking omitted: {} chars]",
            thinking.chars().count()
        )),
        ContentBlock::ToolUse { name, input, .. } => Some(format!("[tool_use: {name} {input}]")),
        ContentBlock::Image { source } => Some(format!("[attached {}]", source.summary())),
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            let prefix = if is_error.unwrap_or(false) {
                "[tool_result error]"
            } else {
                "[tool_result]"
            };
            Some(format!("{prefix}\n{content}"))
        }
        ContentBlock::ServerToolUse { name, input, .. } => {
            Some(format!("[server_tool_use: {name} {input}]"))
        }
        ContentBlock::ToolSearchToolResult { content, .. } => {
            Some(format!("[tool_search_tool_result: {content}]"))
        }
        ContentBlock::CodeExecutionToolResult { content, .. } => {
            Some(format!("[code_execution_tool_result: {content}]"))
        }
    }
}

fn recent_messages_for_memory_extract(app: &App) -> Vec<MemoryExtractionMessage> {
    app.api_messages
        .iter()
        .filter_map(|message| {
            let content = message
                .content
                .iter()
                .filter_map(content_block_for_memory_extract)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            (!content.trim().is_empty()).then(|| MemoryExtractionMessage {
                role: message.role.clone(),
                content,
            })
        })
        .collect()
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let mut iter = text.chars();
    let truncated: String = iter.by_ref().take(max_chars).collect();
    if iter.next().is_some() {
        format!("{truncated}\n\n[…truncated for display]")
    } else {
        truncated
    }
}

fn memory_extract(app: &App, args: &str) -> CommandResult {
    let max_messages = match parse_extract_args(args) {
        Ok(max_messages) => max_messages,
        Err(err) => {
            return CommandResult::error(format!(
                "{err}. Usage: /memory extract --dry-run [--messages N]"
            ));
        }
    };

    let messages = recent_messages_for_memory_extract(app);
    if messages.is_empty() {
        return CommandResult::message(
            "No conversation messages are available for memory extraction.".to_string(),
        );
    }

    let existing_memory = fs::read_to_string(&app.memory_path).ok();
    let prompt = build_memory_extraction_prompt(
        &messages,
        existing_memory.as_deref(),
        max_messages.min(messages.len()),
    );
    let preview = truncate_preview(&prompt.user_prompt, MEMORY_EXTRACT_PREVIEW_MAX_CHARS);
    let worker_request = format!(
        "{}\n\n{}",
        prompt.system_prompt.trim_end(),
        prompt.user_prompt.trim_start()
    );

    CommandResult {
        message: Some(format!(
            "Memory extraction dry-run prepared from {} recent message(s). No files were written.\n\nSystem prompt:\n```text\n{}\n```\n\nUser prompt preview:\n```text\n{}\n```\n\nTo run this extraction in-chat, send the prepared worker prompt below.",
            max_messages.min(messages.len()),
            prompt.system_prompt.trim_end(),
            preview.trim_end()
        )),
        action: Some(AppAction::SendMessage(worker_request)),
        is_error: false,
    }
}

fn show_agent_memory(app: &App, args: &str) -> CommandResult {
    let mut parts = args.split_whitespace();
    let Some(agent_type) = parts.next() else {
        return CommandResult::error(
            "missing agent type. Usage: /memory agent <type> [user|project|local]".to_string(),
        );
    };
    let scope = match parts.next() {
        Some(raw) => match raw.parse::<AgentMemoryScope>() {
            Ok(scope) => Some(scope),
            Err(err) => return CommandResult::error(err),
        },
        None => None,
    };

    if let Some(extra) = parts.next() {
        return CommandResult::error(format!(
            "unexpected argument `{extra}`. Usage: /memory agent <type> [user|project|local]"
        ));
    }

    match scope {
        Some(scope) => show_agent_memory_scope(&app.workspace, agent_type, scope),
        None => show_agent_memory_overview(&app.workspace, agent_type),
    }
}

fn show_agent_memory_overview(workspace: &Path, agent_type: &str) -> CommandResult {
    let mut out = format!("Agent memory for `{agent_type}`\n");
    for scope in AGENT_MEMORY_SCOPES {
        match resolve_agent_memory_dir(workspace, agent_type, *scope) {
            Ok(dir) => {
                let entrypoint = resolve_agent_memory_entrypoint(&dir);
                let status = if entrypoint.exists() {
                    "MEMORY.md present"
                } else if dir.exists() {
                    "directory present, MEMORY.md missing"
                } else {
                    "not created"
                };
                out.push_str(&format!(
                    "\n- {}: {} ({status})",
                    scope.as_str(),
                    dir.display()
                ));
            }
            Err(err) => out.push_str(&format!("\n- {}: {err}", scope.as_str())),
        }
    }
    out.push_str("\n\nUse `/memory agent <type> <scope>` to inspect a specific MEMORY.md and snapshot status.");
    CommandResult::message(out)
}

fn show_agent_memory_scope(
    workspace: &Path,
    agent_type: &str,
    scope: AgentMemoryScope,
) -> CommandResult {
    let dir = match resolve_agent_memory_dir(workspace, agent_type, scope) {
        Ok(dir) => dir,
        Err(err) => return CommandResult::error(err),
    };
    let entrypoint = resolve_agent_memory_entrypoint(&dir);
    let body = match fs::read_to_string(&entrypoint) {
        Ok(text) if text.trim().is_empty() => "(MEMORY.md is empty)".to_string(),
        Ok(text) => text.trim_end().to_string(),
        Err(_) => "(MEMORY.md does not exist yet)".to_string(),
    };
    let status = load_snapshot_status(workspace, agent_type, scope, &dir, &body);
    let candidates = agent_memory_candidates(workspace, agent_type, scope)
        .unwrap_or_else(|_| vec![PathBuf::from(&dir)]);
    let candidate_lines = candidates
        .iter()
        .map(|candidate| format!("  - {}", candidate.display()))
        .collect::<Vec<_>>()
        .join("\n");
    let snapshot_summary = match status.snapshot.as_ref() {
        Some(snapshot) => format!(
            "present (prompt_changed={}, memory_changed={}, updated_at_ms={})",
            status.prompt_changed, status.memory_changed, snapshot.updated_at_ms
        ),
        None => "missing".to_string(),
    };
    let synced_summary = if status.synced.is_some() {
        "present"
    } else {
        "missing"
    };

    CommandResult::message(format!(
        "Agent memory `{agent_type}` scope `{scope}`\n\n\
         Directory: {}\n\
         Entrypoint: {}\n\
         Snapshot: {} at {}\n\
         Synced marker: {} at {}\n\n\
         Candidate directories:\n{}\n\n\
         MEMORY.md:\n{}",
        dir.display(),
        entrypoint.display(),
        snapshot_summary,
        status.snapshot_path.display(),
        synced_summary,
        status.synced_path.display(),
        candidate_lines,
        body
    ))
}

pub fn memory(app: &mut App, arg: Option<&str>) -> CommandResult {
    let path = app.memory_path.clone();
    let sub = arg.unwrap_or("show").trim();

    if let Some(rest) = sub.strip_prefix("agent") {
        return show_agent_memory(app, rest.trim());
    }

    if let Some(rest) = sub.strip_prefix("extract") {
        return memory_extract(app, rest.trim());
    }

    if !app.use_memory {
        return CommandResult::error(
            "user memory is disabled. Enable with `[memory] enabled = true` in `~/.codesmith/config.toml` or `DEEPSEEK_MEMORY=on` in your environment, then restart the TUI. Agent memory can still be inspected with `/memory agent <type> [scope]`.",
        );
    }

    match sub {
        "" | "show" => CommandResult::message(show_user_memory(&path)),
        "path" => CommandResult::message(path.display().to_string()),
        "clear" => match fs::write(&path, "") {
            Ok(()) => CommandResult::message(format!("memory cleared: {}", path.display())),
            Err(err) => CommandResult::error(format!("failed to clear {}: {err}", path.display())),
        },
        "edit" => CommandResult::message(format!(
            "to edit your memory file, run:\n\n  ${{VISUAL:-${{EDITOR:-vi}}}} {}",
            path.display()
        )),
        "help" => CommandResult::message(memory_help(&path)),
        _ => CommandResult::error(format!(
            "unknown subcommand `{sub}`. Try `/memory help`.\n\n{}",
            memory_help(&path)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::app::{App, TuiOptions};
    use tempfile::TempDir;

    fn create_test_app_with_memory(tmpdir: &TempDir, use_memory: bool) -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: tmpdir.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            use_memory,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn memory_help_lists_subcommands_and_resolved_path() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let result = memory(&mut app, Some("help"));
        let msg = result.message.expect("help should return text");
        assert!(msg.contains(
            "Usage: /memory [show|path|clear|edit|extract --dry-run|help|agent <type> [scope]]"
        ));
        assert!(msg.contains("/memory edit"));
        assert!(msg.contains("/memory extract --dry-run"));
        assert!(msg.contains("/memory agent <type>"));
        assert!(msg.contains(app.memory_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn memory_unknown_subcommand_points_to_help() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let result = memory(&mut app, Some("wat"));
        let msg = result
            .message
            .expect("unknown subcommand should return text");
        assert!(msg.contains("Try `/memory help`"));
        assert!(msg.contains("/memory clear"));
    }

    #[test]
    fn memory_disabled_returns_enablement_hint() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, false);
        let result = memory(&mut app, None);
        let msg = result.message.expect("disabled memory should return text");
        assert!(msg.contains("user memory is disabled"));
        assert!(msg.contains("DEEPSEEK_MEMORY=on"));
        assert!(msg.contains("/memory agent <type>"));
    }

    #[test]
    fn memory_agent_overview_works_when_user_memory_disabled() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, false);
        let result = memory(&mut app, Some("agent explore"));
        let msg = result.message.expect("agent overview should return text");
        assert!(msg.contains("Agent memory for `explore`"));
        assert!(msg.contains("project"));
        assert!(msg.contains("agent-memory/explore"));
    }

    #[test]
    fn memory_agent_scope_shows_entrypoint() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let dir = tmpdir
            .path()
            .join(".codesmith")
            .join("agent-memory")
            .join("review");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("MEMORY.md"), "# Review notes").unwrap();

        let result = memory(&mut app, Some("agent review project"));
        let msg = result.message.expect("agent scope should return text");
        assert!(msg.contains("Agent memory `review` scope `project`"));
        assert!(msg.contains("# Review notes"));
        assert!(msg.contains("Snapshot:"));
    }

    #[test]
    fn memory_extract_dry_run_builds_worker_prompt_without_writing() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Please remember that I prefer terse status updates.".to_string(),
                cache_control: None,
            }],
        });
        app.api_messages.push(crate::models::Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "Understood.".to_string(),
                cache_control: None,
            }],
        });

        let result = memory(&mut app, Some("extract --dry-run --messages 1"));
        assert!(!result.is_error);
        let msg = result.message.expect("dry-run should explain prompt");
        assert!(msg.contains("Memory extraction dry-run prepared"));
        assert!(msg.contains("No files were written"));
        assert!(msg.contains("Memory Extraction Protocol"));
        assert!(!app.memory_path.exists());
        match result.action {
            Some(AppAction::SendMessage(prompt)) => {
                assert!(prompt.contains("Memory Extraction Protocol"));
                assert!(prompt.contains("Understood."));
                assert!(!prompt.contains("terse status updates"));
            }
            other => panic!("expected SendMessage action, got {other:?}"),
        }
    }

    #[test]
    fn memory_extract_requires_dry_run() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let result = memory(&mut app, Some("extract"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("supports only `--dry-run`")
        );
    }
}
