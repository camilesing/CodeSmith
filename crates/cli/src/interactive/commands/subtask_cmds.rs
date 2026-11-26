//! `/subtask <task>` — hand a side task to a background sub-agent.
//!
//! The child is spawned through the engine's own `Op::SpawnSubAgent`, so it
//! runs on the same host the model's `agent_open` call would use: the same
//! workspace, the same tool catalog, the same approval routing (a child's
//! path-access question lands in this session), and the same sub-agent
//! manager — which is why `/agents` lists it and `/tasks` shows its turn.
//! Progress and the final result come back as `AgentProgress`/`AgentComplete`
//! events, which the REPL prints (`◇ agent <id> done: …`).
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - **The child does not inherit this conversation.** Claude Code's
//!   `/subtask` forks the transcript so the task can be underspecified; this
//!   spawn path takes the task text alone, so the task has to stand on its
//!   own. Conversation inheritance exists — `agent_open` with
//!   `fork_context: true` — but it belongs to the model's tool surface, not
//!   to a REPL command; until a user-facing spawn can ask for it, the task
//!   must carry its own context.
//! - The child's lines appear in the transcript when the session next renders
//!   engine events, which is the start of your next turn: the REPL drains the
//!   engine channel inside a turn, so a result that arrives while it sits at
//!   the prompt waits there (nothing is lost — the event is queued).
//! - One child per `/subtask`. There is no `/subtask --wait` or batch of
//!   them; `/batch` is the many-units workflow, and `agent_eval` (the model's
//!   tool) is how a result is read inside a turn.

use colored::Colorize;

use super::ReplState;
use codesmith_agent_runtime::ops::Op;

/// `/subtask <task>`.
pub async fn run(state: &mut ReplState, arg: &str) {
    let task = arg.trim();
    if task.is_empty() {
        println!(
            "{}",
            "usage: /subtask <task> — spawns a background sub-agent on that task in this workspace \
             (the child does not inherit this conversation, so make the task self-contained)"
                .dimmed()
        );
        return;
    }

    let engine = match state.engine.as_ref() {
        Some(engine) => engine.clone(),
        None => {
            println!("{}", "✗ no engine in this session to spawn on".red());
            return;
        }
    };

    match engine
        .send(Op::SpawnSubAgent {
            prompt: task.to_string(),
        })
        .await
    {
        Ok(()) => {
            println!(
                "{}",
                format!("subtask sent: {}", task.lines().next().unwrap_or(task)).dimmed()
            );
            println!(
                "{}",
                "  runs in the background — /agents lists it, /tasks shows its turn, and its result \
                 prints here when it finishes"
                    .dimmed()
            );
        }
        Err(err) => println!("{} could not spawn the sub-agent: {err}", "✗".red()),
    }
}

#[cfg(test)]
mod tests {
    // Async tests hold the test-env mutex across awaits.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    fn test_state() -> ReplState {
        let options = crate::app_state::InteractiveOptions {
            load_project_presets: false,
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

    /// The two guards before any spawn: a bare `/subtask` prints usage, and
    /// a task without a live engine says so instead of panicking. The
    /// engine-present spawn path needs a running engine (engine integration
    /// territory), not a unit test.
    #[tokio::test]
    async fn empty_tasks_and_missing_engines_are_refused() {
        let _guard = codesmith_agent_runtime::test_support::lock_test_env();
        let mut state = test_state();
        assert!(state.engine.is_none(), "a fresh REPL state has no engine");

        run(&mut state, "   ").await; // usage branch
        run(&mut state, "summarize the todo list").await; // no-engine branch
        assert!(state.engine.is_none());
    }
}
