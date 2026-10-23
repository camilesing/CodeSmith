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
//! - Pasting text that starts with `!` enters shell mode too (the prefix is
//!   read from the line, not from the keystroke), but there is no
//!   shell-history completion yet — `Up` recalls the prompt line, not the
//!   `!` command history.

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
    let shell = shell();
    let flag = if cfg!(windows) { "/C" } else { "-c" };
    let mut child = tokio::process::Command::new(&shell)
        .arg(flag)
        .arg(command)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("{shell} could not be started: {err}"))?;

    // Stream both pipes with a byte cap instead of `.output()`: a command
    // that prints gigabytes must not pin that much memory before the char
    // cap runs. Bytes beyond the cap are drained-and-counted so the child
    // never blocks on a full pipe.
    let mut stdout = child.stdout.take().expect("stdout piped above");
    let mut stderr = child.stderr.take().expect("stderr piped above");
    let stdout_task = tokio::spawn(async move {
        codesmith_agent_runtime::utils::read_capped(&mut stdout, MAX_OUTPUT_BYTES).await
    });
    let stderr_task = tokio::spawn(async move {
        codesmith_agent_runtime::utils::read_capped(&mut stderr, MAX_OUTPUT_BYTES).await
    });

    let status = child
        .wait()
        .await
        .map_err(|err| format!("{shell} failed to report its exit status: {err}"))?;
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

/// The message the model sees for a shell-mode run.
#[must_use]
pub fn transcript_message(command: &str, status: &str, output: &str) -> String {
    if output.trim().is_empty() {
        format!("I ran this shell command myself:\n\n```sh\n{command}\n```\n\n{status}, no output.")
    } else {
        format!(
            "I ran this shell command myself:\n\n```sh\n{command}\n```\n\n{status}:\n\n```\n{output}```"
        )
    }
}

#[cfg(test)]
mod tests {
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
