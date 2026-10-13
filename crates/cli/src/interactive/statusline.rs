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
//! - Each run is wrapped in a timeout by the caller; a timed-out run leaves
//!   its error text in the status line rather than a stale value.

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
/// Names the failure (spawn failure, non-zero exit, empty output) so the
/// status line shows why it has nothing to say.
pub fn run(command: &str) -> Result<String, String> {
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let flag = if cfg!(windows) { "/C" } else { "-c" };
    let output = std::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .output()
        .map_err(|err| format!("{shell} could not be started: {err}"))?;
    if !output.status.success() {
        return Err(format!("statusline command exited with {}", output.status));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Err("statusline command printed nothing".to_string());
    }
    Ok(crate::localization::truncate_to_width(line, MAX_COLUMNS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn a_command_produces_one_capped_line() {
        let segment = run("printf 'first line\\nsecond line\\n'").expect("runs");
        assert_eq!(segment, "first line");

        let long = run("printf 'a%.0s' $(seq 1 400)").expect("runs");
        assert!(
            long.width() <= MAX_COLUMNS,
            "capped to the column budget, ellipsis included: {} columns",
            long.width()
        );
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_cjk_command_line_is_capped_by_columns() {
        // 200 CJK characters are 400 terminal columns; the cap bounds the row
        // in columns, so the segment must not reach twice its width.
        let segment = run("printf '中%.0s' $(seq 1 200)").expect("runs");
        assert!(
            segment.width() <= MAX_COLUMNS,
            "status segment ran to {} columns",
            segment.width()
        );
    }

    #[test]
    fn failures_name_the_reason() {
        let err = run("exit 3").expect_err("non-zero exits are errors");
        assert!(err.contains("exited"), "{err}");
        let err = run("true").expect_err("no output is an error");
        assert!(err.contains("nothing"), "{err}");
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
