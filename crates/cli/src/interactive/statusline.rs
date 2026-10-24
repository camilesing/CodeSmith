//! The user's statusline command (`/statusline <command>`).
//!
//! A timestamped cache sits between the prompt and the command: the status
//! line is redrawn on every prompt and the live ticker runs four times a
//! second, so the command runs at most once per [`TTL`] and the renderer
//! prints whatever the last run produced.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - The command runs through `sh -c` (or `cmd /C` on Windows) with the
//!   workspace as its working directory, the same way the model's shell tool
//!   resolves, but with **no sandbox and no approval prompt**: it is the
//!   user's own command, typed by the user, like the `!` prefix. That is why
//!   nothing else may write here.
//! - Only stdout's first line is kept, capped at [`MAX_COLUMNS`] terminal
//!   columns (CJK counts as two): the status line is one row as long as the
//!   terminal is wider than the cap, and a command that prints a paragraph
//!   would otherwise push the prompt off screen.
//! - Each run enforces its own deadline (the `timeout` argument to
//!   [`run`], killing the child on expiry); a timed-out run leaves its
//!   error text in the status line rather than a stale value.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How often the command may run.
pub const TTL: Duration = Duration::from_secs(5);

/// Cap on the segment's length, in terminal columns (CJK counts as two).
pub const MAX_COLUMNS: usize = 120;

static LAST_RUN: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether enough time has passed to run the command again.
///
/// Marks the run as started, so two callers in the same tick cannot both
/// launch it.
pub fn is_due() -> bool {
    let mut last = match LAST_RUN.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let due = last.is_none_or(|at| at.elapsed() >= TTL);
    if due {
        *last = Some(Instant::now());
    }
    due
}

/// Run the command and return the status segment.
///
/// # Errors
/// Names the failure (spawn failure, timeout, non-zero exit, empty output)
/// so the status line shows why it has nothing to say.
pub fn run(command: &str, timeout: Duration) -> Result<String, String> {
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let mut cmd = std::process::Command::new(shell);
    // Windows: cmd.exe does not follow MSVCRT quoting rules; `.arg()`
    // applies them and mangles commands with quotes/`&`/`|`/`&&`.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.arg("/C").raw_arg(command);
    }
    #[cfg(not(windows))]
    {
        cmd.arg("-c").arg(command);
    }
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("{shell} could not be started: {err}"))?;
    // Drain both pipes on reader threads while polling: a statusline
    // command that writes more than the pipe buffer blocks on write, the
    // poll loop below would never see it exit, and the run would die as a
    // bogus "timeout" instead of returning the line.
    let mut stdout_pipe = child.stdout.take().expect("stdout piped above");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped above");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout_pipe, &mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr_pipe, &mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Join so the reader threads (and their buffers) are
                    // gone before we report the timeout.
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err("statusline command timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => {
                // Kill and reap before returning: dropping a live
                // `std::process::Child` neither kills nor reaps it — the
                // process kept running and lingered as a zombie until the
                // parent exited.
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("statusline command failed: {err}"));
            }
        }
    }
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let status = child
        .wait()
        .map_err(|err| format!("statusline command failed: {err}"))?;
    if !status.success() {
        let reason = String::from_utf8_lossy(&stderr)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| {
                format!(
                    ": {}",
                    crate::localization::truncate_to_width(line, MAX_COLUMNS)
                )
            })
            .unwrap_or_default();
        return Err(format!("statusline command exited with {status}{reason}"));
    }
    let stdout = String::from_utf8_lossy(&stdout);
    let line = stdout.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Err("statusline command printed nothing".to_string());
    }
    Ok(crate::localization::truncate_to_width(line, MAX_COLUMNS))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use unicode_width::UnicodeWidthStr;

    // The commands below are POSIX shell builtins/utilities (`printf`,
    // `seq`, `true`, `sleep`); `run` supports `cmd /C` on Windows, where
    // they do not exist, so the shell-dependent tests stay on Unix.
    #[test]
    #[cfg(unix)]
    fn a_command_produces_one_capped_line() {
        let segment = run(
            "printf 'first line\\nsecond line\\n'",
            Duration::from_secs(10),
        )
        .expect("runs");
        assert_eq!(segment, "first line");

        let long = run("printf 'a%.0s' $(seq 1 400)", Duration::from_secs(10)).expect("runs");
        assert!(
            long.width() <= MAX_COLUMNS,
            "capped to the column budget, ellipsis included: {} columns",
            long.width()
        );
        assert!(long.ends_with('…'));
    }

    #[test]
    #[cfg(unix)]
    fn a_cjk_command_line_is_capped_by_columns() {
        // 200 CJK characters are 400 terminal columns; the cap bounds the row
        // in columns, so the segment must not reach twice its width.
        let segment = run("printf '中%.0s' $(seq 1 200)", Duration::from_secs(10)).expect("runs");
        assert!(
            segment.width() <= MAX_COLUMNS,
            "status segment ran to {} columns",
            segment.width()
        );
    }

    #[test]
    #[cfg(unix)]
    fn failures_name_the_reason() {
        let err = run("exit 3", Duration::from_secs(10)).expect_err("non-zero exits are errors");
        assert!(err.contains("exited"), "{err}");
        let err = run("true", Duration::from_secs(10)).expect_err("no output is an error");
        assert!(err.contains("nothing"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn a_hanging_command_times_out_and_is_killed() {
        let err = run("sleep 30", Duration::from_millis(150)).expect_err("hangs are errors");
        assert!(err.contains("timed out"), "{err}");
    }

    #[test]
    fn the_ttl_gates_repeat_runs() {
        // One call marks the run; the immediate second call is not due. The
        // static is per-process, so this asserts the gate rather than a
        // sleep-then-fire.
        assert!(is_due());
        assert!(!is_due());
    }
}
