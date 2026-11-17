//! `/btw [question]` — a side question about the session.
//!
//! The question is answered from what the conversation already holds: the
//! session's messages go to the model with **no tools**, so the answer can
//! only use what the session has already read and said. The exchange is not
//! part of the conversation afterwards — the next turn does not see it — and
//! that is the whole point of the command.
//!
//! It is still recorded: the repo rule is that anything reaching a model
//! request is reconstructable from the session log, so each exchange is
//! appended to `~/.codesmith/side-questions.jsonl` with the session id. The
//! record is a side channel, not history: nothing reads it back into the
//! model's context.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - No overlay. Claude Code shows the answer in a dismissible panel with
//!   its own keys (`c` copy, `f` fork into a sub-agent, `x` clear the earlier
//!   exchanges); a line REPL prints the answer like any other block, and the
//!   earlier exchanges stay in the transcript instead of being listed above
//!   the current one. `/copy` is the way to take an answer away.
//! - No follow-up turns inside the thread (ask another `/btw`), matching the
//!   overlay's own rule.
//! - The transcript the model sees is `state.api_messages` — what the session
//!   has exchanged so far. A turn still streaming is not included.

use std::io::Write as _;

use colored::Colorize;

use super::ReplState;

/// Where side questions are recorded.
fn record_path() -> Option<std::path::PathBuf> {
    codesmith_config::codesmith_home()
        .ok()
        .map(|home| home.join("side-questions.jsonl"))
        .or_else(|| {
            dirs::home_dir().map(|home| home.join(".codesmith").join("side-questions.jsonl"))
        })
}

/// Append one exchange to the side-question record.
///
/// Failures are reported to the caller rather than swallowed: a silent
/// failure here would mean a model-visible input with no record, which is
/// exactly what the rule exists to prevent.
fn record(session_id: Option<&str>, question: &str, answer: &str) -> Result<(), String> {
    let Some(path) = record_path() else {
        return Err("home directory not resolvable".to_string());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let entry = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "session": session_id.unwrap_or("(unsaved)"),
        "question": question,
        "answer": answer,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    writeln!(file, "{entry}").map_err(|err| format!("{}: {err}", path.display()))
}

/// Run a side question: ask, print, record.
///
/// Returns `Ok(())` even when the model call fails after printing the error,
/// because the caller has nothing useful to do with the failure — but a
/// failed *record* is printed as a warning, since that is the promise that
/// must not be broken quietly.
pub async fn ask(state: &ReplState, question: &str) -> anyhow::Result<()> {
    let question = question.trim();
    if question.is_empty() {
        println!(
            "{}",
            "usage: /btw <question> — answered from this session's conversation, without tools, and not added to it"
                .dimmed()
        );
        return Ok(());
    }
    if state.api_messages.is_empty() {
        println!(
            "{}",
            "nothing to answer from yet — /btw reads the session's own conversation, so ask it after a turn"
                .dimmed()
        );
        return Ok(());
    }

    let messages = crate::interactive::btw_request_messages(state, question);
    let answer = match crate::main_root::one_shot_messages(
        &state.config,
        &state.model,
        messages,
        state.system_prompt.clone(),
    )
    .await
    {
        Ok(answer) => answer,
        Err(err) => {
            println!("{} side question failed: {err}", "✗".red());
            return Ok(());
        }
    };
    println!("{}", "btw:".bold());
    println!("{}", answer.trim());
    println!(
        "{}",
        "— side question: not added to the conversation (ask another /btw to continue)".dimmed()
    );

    if let Err(err) = record(state.current_session_id.as_deref(), question, answer.trim()) {
        println!(
            "{}",
            format!("warning: the side question could not be recorded: {err}").yellow()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Async tests hold the test-env mutex across awaits.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use codesmith_agent_runtime::test_support::lock_test_env;

    fn test_repl_state() -> ReplState {
        let options = crate::app_state::InteractiveOptions {
            model: "test-model".to_string(),
            workspace: std::env::temp_dir(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            max_subagents: 2,
            skills_dir: std::env::temp_dir(),
            memory_path: std::env::temp_dir().join("memory.md"),
            mcp_config_path: std::env::temp_dir().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        ReplState::new(options, &crate::config::Config::default())
    }

    #[test]
    fn the_record_path_lands_in_the_codesmith_home() {
        // `record_path()` resolves `codesmith_home()` from the process env;
        // other tests retarget `CODESMITH_HOME` under the env lock, so this
        // reader holds it too or the full-suite parallel run races.
        let _guard = lock_test_env();
        // Resolvable on any machine with a home directory; the command says
        // so out loud when it is not.
        if let Some(path) = record_path() {
            assert!(path.ends_with("side-questions.jsonl"));
            assert!(path.to_string_lossy().contains(".codesmith"));
        }
    }

    #[test]
    fn record_appends_one_json_line_per_exchange() {
        let _guard = lock_test_env();
        let home = tempfile::tempdir().expect("tempdir");
        let _env =
            codesmith_agent_runtime::test_support::EnvVarGuard::set("CODESMITH_HOME", home.path());

        record(Some("sess-7"), "what did we read?", "the config file")
            .expect("first exchange recorded");
        record(None, "again?", "still there").expect("second exchange recorded");

        let body =
            std::fs::read_to_string(home.path().join("side-questions.jsonl")).expect("record file");
        let lines: Vec<serde_json::Value> = body
            .lines()
            .map(|line| serde_json::from_str(line).expect("json line"))
            .collect();
        assert_eq!(lines.len(), 2, "one line per exchange:\n{body}");
        assert_eq!(lines[0]["session"], "sess-7");
        assert_eq!(lines[0]["question"], "what did we read?");
        assert_eq!(lines[0]["answer"], "the config file");
        assert_eq!(
            lines[1]["session"], "(unsaved)",
            "an unsaved session is named, not null"
        );
    }

    /// The two guards before any model call: a bare `/btw` prints usage and
    /// an empty session has nothing to answer from — neither reaches the
    /// model, so neither is a network test.
    #[tokio::test]
    async fn ask_refuses_empty_questions_and_empty_sessions() {
        let _guard = lock_test_env();
        let home = tempfile::tempdir().expect("tempdir");
        let _env =
            codesmith_agent_runtime::test_support::EnvVarGuard::set("CODESMITH_HOME", home.path());
        let state = test_repl_state();
        assert!(
            state.api_messages.is_empty(),
            "a fresh state has no conversation to answer from"
        );

        ask(&state, "   ")
            .await
            .expect("usage branch is not an error");
        ask(&state, "what happened so far?")
            .await
            .expect("empty-session branch is not an error");
        assert!(
            !home.path().join("side-questions.jsonl").exists(),
            "no exchange happened, so none was recorded"
        );
    }
}
