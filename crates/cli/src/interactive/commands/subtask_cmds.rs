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
use crate::core::ops::Op;

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
