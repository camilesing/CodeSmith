//! Shell mode: a prompt line that starts with `!`.
//!
//! The command runs in the user's own shell, in the workspace, and its
//! output joins the conversation — Claude Code's `!` prefix. Two decisions
//! are deliberate and worth stating:
//!
//! - **No approval prompt and no sandbox.** The user typed the command; the
//!   approval surface exists to gate what the *model* runs. This is the same
//!   rule the sandbox documents for shell mode in Claude Code.
//! - **The output is a session message.** Whatever reaches the model must be
//!   reconstructable from the session log (repo rule), and these notes do
//!   reach it, so the transcript carries the command and its output as one
//!   user message — exactly as if the user had typed them.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Output is captured, then printed: a long-running command shows nothing
//!   until it exits. Streaming it live would fight the live/composer region
//!   for the same rows.
//! - Both streams are merged (`2>&1` semantics) and the tail is capped at
//!   [`MAX_OUTPUT_CHARS`]; a command that prints more says so rather than
//!   flooding the model's context.
//! - A command's output is captured, so an interactive program (an editor, a
//!   `[y/N]` prompt) is unsupported: it reads the terminal blind. Past
//!   [`RUN_TIMEOUT`] it is asked to leave (TERM), then killed after
//!   [`TERM_GRACE`].
//! - Pasting text that starts with `!` enters shell mode too (the prefix is
//!   read from the line, not from the keystroke). Tab completes paths and
//!   earlier `!` commands; `Up` still recalls the whole prompt line rather
//!   than `!` history on its own.

/// Cap on the output carried into the conversation.
pub const MAX_OUTPUT_CHARS: usize = 20_000;

/// What to run for a `!` line: the text after the prefix.
///
/// Returns `None` for a bare `!` (nothing to run) so the caller can print a
/// usage line instead of spawning an empty shell.
#[must_use]
pub fn command_from_line(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('!')?.trim();
    (!rest.is_empty()).then_some(rest)
}

/// The user's shell, falling back to `sh` (and `cmd` on Windows).
fn shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string())
    }
}

/// Run one shell-mode command and return `(exit description, combined
/// output)`.
///
/// The command is passed to the shell with `-c`, so pipes, redirects, and
/// aliases behave the way they do when typed at a prompt.
///
/// # Errors
/// Only when the shell itself cannot be started; a non-zero exit is a
/// result, not an error — the caller prints the status and the output.
pub async fn run(command: &str, cwd: &std::path::Path) -> Result<(String, String), String> {
    run_bounded(command, cwd, RUN_TIMEOUT, TERM_GRACE).await
}

/// [`run`] with its bounds made explicit: `run_timeout` is how long the
/// command may run, `term_grace` how long it has to leave once asked.
async fn run_bounded(
    command: &str,
    cwd: &std::path::Path,
    run_timeout: std::time::Duration,
    term_grace: std::time::Duration,
) -> Result<(String, String), String> {
    let shell = shell();
    let mut cmd = tokio::process::Command::new(&shell);
    // Windows: cmd.exe does not follow MSVCRT quoting rules, so `.arg()`
    // (which applies them) mangles commands containing quotes, `&`, `|`,
    // `%`, or `&&`/`||` chains — pass the command raw (tokio's own
    // `raw_arg`; the std `CommandExt` one is for `std::process::Command`).
    #[cfg(windows)]
    {
        cmd.arg("/C").raw_arg(command);
    }
    #[cfg(not(windows))]
    {
        cmd.arg("-c").arg(command);
    }
    // The command leads its own process group, so a timeout kill takes
    // what it spawned too (a dev server, a cargo-built binary): killing the
    // shell alone orphaned those holding the pipes, and the reads below
    // never reached EOF.
    codesmith_agent_runtime::shell_manager::tokio_own_process_group(&mut cmd);
    let mut child = cmd
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("{shell} could not be started: {err}"))?;

    // Stream both pipes with a byte cap instead of `.output()`: a command
    // that prints gigabytes must not pin that much memory before the char
    // cap runs. Bytes beyond the cap are drained-and-counted so the child
    // never blocks on a full pipe.
    let stdout = child.stdout.take().expect("stdout piped above");
    let stderr = child.stderr.take().expect("stderr piped above");
    // Announces the command's exit to the readers, which then bound their
    // wait: a job the command backgrounded (`!npm run dev &`) keeps the
    // pipes' write end, so EOF may never come.
    let (exited_tx, exited_rx) = tokio::sync::watch::channel(None);
    let stdout_task = tokio::spawn(read_until_closed(
        stdout,
        MAX_OUTPUT_BYTES,
        exited_rx.clone(),
    ));
    let stderr_task = tokio::spawn(read_until_closed(stderr, MAX_OUTPUT_BYTES, exited_rx));

    // Bound the wait: a command that never exits on its own (`!tail -f`,
    // `!npm run dev`) used to wedge the interactive session forever with
    // zero feedback. A timeout is a normal result, like a non-zero exit.
    let status = match tokio::time::timeout(run_timeout, child.wait()).await {
        Ok(status) => {
            status.map_err(|err| format!("{shell} failed to report its exit status: {err}"))?
        }
        Err(_) => {
            // Asked to leave before it is made to: TERM to the whole group
            // lets a program that waits on the user (`!git commit` in its
            // editor) release what it holds — git's index.lock, an editor
            // session. A bare kill left the lock behind for good.
            codesmith_agent_runtime::shell_manager::terminate_process_group_of_opt(child.id());
            #[cfg(not(unix))]
            let _ = child.kill().await;
            if tokio::time::timeout(term_grace, child.wait())
                .await
                .is_err()
            {
                codesmith_agent_runtime::shell_manager::kill_process_group_of_opt(child.id());
                let _ = child.wait().await;
            }
            let _ = exited_tx.send(Some(tokio::time::Instant::now() + DRAIN_TIMEOUT));
            let (stdout_kept, stdout_dropped) = stdout_task.await.unwrap_or_default();
            let (stderr_kept, stderr_dropped) = stderr_task.await.unwrap_or_default();
            let mut combined = String::from_utf8_lossy(&stdout_kept).into_owned();
            let stderr = String::from_utf8_lossy(&stderr_kept);
            if !stderr.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            let dropped = stdout_dropped + stderr_dropped;
            if dropped > 0 {
                combined.push_str(&format!(
                    "\n… [{dropped} more bytes of output not captured]\n"
                ));
            }
            return Ok((
                format!("timed out after {} s", run_timeout.as_secs()),
                cap(combined, MAX_OUTPUT_CHARS),
            ));
        }
    };
    let _ = exited_tx.send(Some(tokio::time::Instant::now() + DRAIN_TIMEOUT));
    let (stdout_kept, stdout_dropped) = stdout_task.await.unwrap_or_default();
    let (stderr_kept, stderr_dropped) = stderr_task.await.unwrap_or_default();

    let mut combined = String::from_utf8_lossy(&stdout_kept).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_kept);
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    let dropped = stdout_dropped + stderr_dropped;
    if dropped > 0 {
        combined.push_str(&format!(
            "\n… [{dropped} more bytes of output not captured]\n"
        ));
    }
    let status = match status.code() {
        Some(0) => "exit 0".to_string(),
        Some(code) => format!("exit {code}"),
        None => "terminated by a signal".to_string(),
    };
    Ok((status, cap(combined, MAX_OUTPUT_CHARS)))
}

/// Byte ceiling for captured child output: 4 bytes per kept char covers the
/// worst-case UTF-8 expansion, so the later char-level cap is never
/// loosened by the byte cap.
const MAX_OUTPUT_BYTES: usize = MAX_OUTPUT_CHARS * 4;

/// Wall-clock bound for a shell-mode command: long-running servers are a
/// timeout result, not an interactive-session wedge.
const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a command that outlived `RUN_TIMEOUT` has to leave once asked
/// (TERM) before it is killed.
const TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the output readers keep waiting once the command itself has
/// exited.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// `utils::read_capped` with a bound: once `exited` names a deadline, a read
/// that has not arrived by then ends the capture with what was read so far.
async fn read_until_closed<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    cap: usize,
    mut exited: tokio::sync::watch::Receiver<Option<tokio::time::Instant>>,
) -> (Vec<u8>, usize) {
    use tokio::io::AsyncReadExt as _;

    let (mut kept, mut dropped) = (Vec::new(), 0usize);
    let mut buf = [0u8; 8192];
    loop {
        // Copied out: a `watch::Ref` is not `Send` and must not live across the await.
        let deadline = *exited.borrow_and_update();
        let read = if let Some(deadline) = deadline {
            // Past the bound the capture ends; a ready read still counts.
            tokio::time::timeout_at(deadline, reader.read(&mut buf))
                .await
                .unwrap_or(Ok(0))
        } else {
            tokio::select! {
                read = reader.read(&mut buf) => read,
                _ = exited.changed() => continue,
            }
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..read.min(room)]);
                dropped += read.saturating_sub(room);
            }
        }
    }
    (kept, dropped)
}

/// Truncate to `max` chars, saying how much was dropped.
#[must_use]
pub fn cap(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        return text;
    }
    let kept: String = text.chars().take(max).collect();
    let dropped = text.chars().count() - max;
    format!("{kept}\n… [{dropped} more characters, truncated]\n")
}

/// Longest backtick run in `s`.
fn longest_backtick_run(s: &str) -> usize {
    s.split(|c: char| c != '`').map(str::len).max().unwrap_or(0)
}

/// The message the model sees for a shell-mode run. The fence is longer
/// than any backtick run inside the command or the output: a fixed
/// three-backtick fence is closed early by ``` inside the content (a
/// README with fenced blocks, or attacker-controlled output), letting
/// untrusted text escape into the transcript as fabricated model/system
/// content.
#[must_use]
pub fn transcript_message(command: &str, status: &str, output: &str) -> String {
    let fence = "`".repeat(
        longest_backtick_run(command)
            .max(longest_backtick_run(output))
            .max(2)
            + 1,
    );
    if output.trim().is_empty() {
        format!(
            "I ran this shell command myself:\n\n{fence}sh\n{command}\n{fence}\n\n{status}, no output."
        )
    } else {
        // Normalize a trailing newline so the last output line cannot
        // glue onto the closing fence.
        let out = if output.ends_with('\n') {
            output.to_string()
        } else {
            format!("{output}\n")
        };
        format!(
            "I ran this shell command myself:\n\n{fence}sh\n{command}\n{fence}\n\n{status}:\n\n{fence}\n{out}{fence}"
        )
    }
}

#[cfg(test)]
mod tests {
    /// Found in the review: the bound was a bare kill, so a program waiting
    /// on the user (`!git commit` in its editor) never released what it held
    /// — git's `index.lock` stayed behind. It is asked to leave first.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_command_is_asked_to_leave_before_it_is_killed() {
        let cwd = std::env::temp_dir();
        let (status, output) = run_bounded(
            "trap 'echo cleaned; exit 0' TERM; sleep 30",
            &cwd,
            std::time::Duration::from_millis(200),
            std::time::Duration::from_secs(2),
        )
        .await
        .expect("the command runs");
        assert!(status.contains("timed out"), "{status}");
        assert!(
            output.contains("cleaned"),
            "TERM reached the command before KILL: {output:?}"
        );
    }

    /// One that ignores TERM is still killed once the grace passes.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_that_ignores_term_is_killed_after_the_grace() {
        let cwd = std::env::temp_dir();
        let (status, _output) = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            run_bounded(
                r#"trap '' TERM; sh -c "trap '' TERM; exec sleep 30""#,
                &cwd,
                std::time::Duration::from_millis(200),
                std::time::Duration::from_millis(300),
            ),
        )
        .await
        .expect("the grace ends the wait")
        .expect("the command runs");
        assert!(status.contains("timed out"), "{status}");
    }

    /// Found in the review: a command that backgrounds a job left the orphan
    /// holding the pipes, and the reader joins waited for its EOF — the
    /// prompt never came back.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_backgrounded_job_does_not_hold_the_prompt() {
        let cwd = std::env::temp_dir();
        let (status, output) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run("sleep 30 & echo hi", &cwd),
        )
        .await
        .expect("the prompt comes back")
        .expect("the command runs");
        assert!(output.contains("hi"), "{output:?}");
        assert!(!status.contains("timed out"), "{status}");
    }

    use super::*;

    #[test]
    fn only_a_leading_bang_is_a_shell_line() {
        assert_eq!(command_from_line("!ls -la"), Some("ls -la"));
        assert_eq!(command_from_line("  ! git status "), Some("git status"));
        assert_eq!(command_from_line("!"), None, "a bare bang runs nothing");
        assert_eq!(command_from_line("!   "), None);
        assert_eq!(command_from_line("hello !world"), None);
        assert_eq!(command_from_line("/help"), None);
    }

    #[test]
    fn long_output_is_capped_with_a_note() {
        let text = "x".repeat(50);
        assert_eq!(cap(text.clone(), 100), text, "under the cap is untouched");
        let capped = cap(text, 10);
        assert!(capped.starts_with("xxxxxxxxxx"));
        assert!(capped.contains("40 more characters"), "{capped}");
    }

    #[test]
    fn the_transcript_says_a_command_was_run_and_by_whom() {
        let message = transcript_message("ls", "exit 0", "a\nb\n");
        assert!(message.contains("I ran this shell command myself"));
        assert!(message.contains("```sh\nls\n```"));
        assert!(message.contains("exit 0"));
        assert!(message.contains("a\nb"));
        let empty = transcript_message("true", "exit 0", "");
        assert!(empty.contains("no output"));
    }

    #[tokio::test]
    async fn a_command_reports_its_exit_status_and_output() {
        let cwd = std::env::temp_dir();
        let (status, output) = run("echo hello", &cwd).await.expect("runs");
        assert_eq!(status, "exit 0");
        assert_eq!(output.trim(), "hello");

        let (status, _) = run("exit 7", &cwd).await.expect("runs");
        assert_eq!(
            status, "exit 7",
            "a failing command is a result, not an error"
        );

        // stderr joins stdout, so a failing command's message is visible.
        let (_, output) = run("echo oops >&2; exit 1", &cwd).await.expect("runs");
        assert!(output.contains("oops"));
    }
}
