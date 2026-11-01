//! Shell-job bridge — background shell completions onto the drain seam
//! (round 10 TODO-70).
//!
//! `crates/cli/src/background_task` removed the `register_shell_task`
//! bridge (with the never-sent `Op::StartBackgroundShell`) and shell jobs
//! have lived entirely in the shell manager since: `/jobs` lists them, but
//! the *model* never learns a background `exec_shell` / `task_shell_start`
//! job finished. This module is the replacement bridge, aimed at the model
//! instead of the UI registry: the engine spawns one watcher per `run`
//! (alongside the bg-task poller) that polls [`HostServices::
//! list_shell_jobs`] every [`POLL_INTERVAL`] and, for every transition into
//! a terminal state, sends a [`SubAgentCompletion`] with
//! [`CompletionKind::ShellJob`] into the parent-completion channel — the
//! *same* channel the executor's pre-request drain reads, so delivery,
//! logging, and (with `idle_wake` on) turn synthesis all ride the round-10
//! wake seam. The registry keeps its no-shell stance.
//!
//! ## Known limitations
//!
//! - Poll granularity: a job that starts and finishes between two polls is
//!   detected as a "new terminal job" and still bridged (it must have
//!   started after the baseline poll), but a job already terminal at the
//!   baseline poll is treated as pre-existing history and never bridged —
//!   a resumed session does not replay old completions.
//! - Output carried is the shell manager's snapshot tails (bounded); the
//!   model reads more via `task_shell_output`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::host_services::HostServices;
use crate::subagent::{CompletionKind, SubAgentCompletion, SubAgentCompletionSender};
use crate::tools::shell_types::{ShellJobSnapshot, ShellStatus};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// One human summary line for the payload, matching the sub-agent wire
/// shape (summary first, sentinel after).
fn summarize_job(job: &ShellJobSnapshot) -> String {
    let outcome = match (&job.status, job.exit_code) {
        (ShellStatus::Completed, code) => match code {
            Some(code) => format!("completed (exit {code})"),
            None => "completed".to_string(),
        },
        (ShellStatus::Failed, Some(code)) => format!("failed (exit {code})"),
        (ShellStatus::Failed, None) => "failed".to_string(),
        (ShellStatus::Killed, _) => "killed".to_string(),
        (ShellStatus::TimedOut, _) => "timed out".to_string(),
        (ShellStatus::Running, _) => "running".to_string(),
    };
    format!(
        "Background shell job {} (`{}`) {outcome}.",
        job.job_id, job.command
    )
}

/// Build the completion for a terminal job: summary line, the
/// `<codesmith:shell_job.done>` sentinel (documented in the base prompt's
/// sentinel protocol), and bounded stdout/stderr tails so the model can
/// act without a round-trip.
#[must_use]
pub(crate) fn shell_job_completion(job: &ShellJobSnapshot) -> SubAgentCompletion {
    let sentinel = format!(
        "<codesmith:shell_job.done>{{\"job_id\":{},\"exit_code\":{}}}</codesmith:shell_job.done>",
        serde_json::to_string(&job.job_id).unwrap_or_else(|_| "\"?\"".to_string()),
        job.exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_string()),
    );
    let mut payload = format!("{}\n{sentinel}", summarize_job(job));
    let stdout = job.stdout_tail.trim_end();
    let stderr = job.stderr_tail.trim_end();
    if !stdout.is_empty() {
        payload.push_str("\nstdout (tail):\n");
        payload.push_str(stdout);
    }
    if !stderr.is_empty() {
        payload.push_str("\nstderr (tail):\n");
        payload.push_str(stderr);
    }
    SubAgentCompletion {
        agent_id: job.job_id.clone(),
        kind: CompletionKind::ShellJob,
        payload,
        context_patch: None,
    }
}

/// Diff one poll against remembered job states and emit the completions.
/// Exposed for tests. `known` is `None` on the baseline poll (jobs already
/// terminal there are pre-existing history and never bridge) and afterwards
/// maps job id → still running at last sight; the returned map replaces it
/// (ids absent from the poll were evicted by the manager's 1h cleanup and
/// are forgotten).
fn diff_poll(
    known: Option<&HashMap<String, bool>>,
    jobs: &[ShellJobSnapshot],
) -> (Vec<SubAgentCompletion>, HashMap<String, bool>) {
    let mut completions = Vec::new();
    let mut next = HashMap::new();
    for job in jobs {
        let running = matches!(job.status, ShellStatus::Running);
        let previously = known.and_then(|known| known.get(&job.id).copied());
        match previously {
            // Running → terminal: the transition we bridge.
            Some(true) if !running => completions.push(shell_job_completion(job)),
            // New id appearing already-terminal after the baseline: it
            // started (and finished) between polls — bridge it too.
            None if !running && known.is_some() => {
                completions.push(shell_job_completion(job));
            }
            _ => {}
        }
        next.insert(job.id.clone(), running);
    }
    (completions, next)
}

/// Spawn the per-engine shell-job watcher (called once from
/// `Engine::run_inner`). The first poll is the baseline: jobs already
/// terminal there are pre-existing history and never bridge.
pub(crate) fn spawn_shell_job_watcher(
    host: Arc<dyn HostServices>,
    completions: SubAgentCompletionSender,
    cancel_token: tokio_util::sync::CancellationToken,
) {
    crate::utils::spawn_supervised(
        "shell-job-watcher",
        std::panic::Location::caller(),
        async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            interval.tick().await; // first tick fires immediately = baseline
            let mut known: Option<HashMap<String, bool>> = None;
            loop {
                if cancel_token.is_cancelled() {
                    break;
                }
                let jobs = host.list_shell_jobs();
                let (completed, next) = diff_poll(known.as_ref(), &jobs);
                known = Some(next);
                for completion in completed {
                    completions.send(completion);
                }
                tokio::select! {
                    _ = cancel_token.cancelled() => break,
                    _ = interval.tick() => {}
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, status: ShellStatus, exit_code: Option<i32>) -> ShellJobSnapshot {
        ShellJobSnapshot {
            id: id.to_string(),
            job_id: id.to_string(),
            command: "cargo test".to_string(),
            cwd: std::path::PathBuf::from("/tmp"),
            status,
            exit_code,
            elapsed_ms: 100,
            stdout_tail: "ok. 3 passed\n".to_string(),
            stderr_tail: String::new(),
            stdout_len: 16,
            stderr_len: 0,
            stdin_available: false,
            stale: false,
            linked_task_id: None,
        }
    }

    #[test]
    fn bridges_running_to_terminal_once() {
        // Baseline: one running job.
        let (done, known) = diff_poll(None, &[job("j1", ShellStatus::Running, None)]);
        assert!(done.is_empty(), "running jobs bridge nothing");
        // Completes → exactly one completion.
        let (done, known) = diff_poll(Some(&known), &[job("j1", ShellStatus::Completed, Some(0))]);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].kind, CompletionKind::ShellJob);
        assert_eq!(done[0].agent_id, "j1");
        assert!(done[0].payload.contains("<codesmith:shell_job.done>"));
        assert!(done[0].payload.contains("completed"));
        assert!(done[0].payload.contains("stdout (tail):"));
        // Still terminal next poll → no repeat.
        let (done, _) = diff_poll(Some(&known), &[job("j1", ShellStatus::Completed, Some(0))]);
        assert!(done.is_empty(), "terminal state bridges once");
    }

    #[test]
    fn baseline_terminal_jobs_are_history_not_news() {
        // Baseline poll (empty known): a job already terminal never bridges,
        // even on later polls.
        let (_, known) = diff_poll(None, &[job("old", ShellStatus::Completed, Some(0))]);
        let (done, _) = diff_poll(Some(&known), &[job("old", ShellStatus::Completed, Some(0))]);
        assert!(done.is_empty());
    }

    #[test]
    fn empty_baseline_does_not_skip_fast_jobs() {
        // Baseline poll saw nothing; a job that appears terminal on the
        // next poll must still bridge (it cannot be pre-baseline history).
        let (_, known) = diff_poll(None, &[]);
        let (done, _) = diff_poll(
            Some(&known),
            &[job("fast", ShellStatus::Completed, Some(0))],
        );
        assert_eq!(done.len(), 1);
    }

    #[test]
    fn job_finishing_between_polls_still_bridges() {
        // Job never seen Running (started+finished between polls) after the
        // baseline exists: bridged as newly-terminal.
        let known: HashMap<String, bool> = [("other".to_string(), true)].into_iter().collect();
        let (done, _) = diff_poll(
            Some(&known),
            &[
                job("other", ShellStatus::Running, None),
                job("fast", ShellStatus::Failed, Some(2)),
            ],
        );
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].agent_id, "fast");
        assert!(done[0].payload.contains("failed (exit 2)"));
    }
}
