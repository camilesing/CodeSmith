//! Root-dispatch regression tests.
//!
//! These spawn the real `codesmith` binary, which is the only way to cover the
//! wiring in `run()`: the guard helpers are unit-tested next to the code, but a
//! guard that is never called would still pass those tests.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `CODESMITH_HOME` keeps config and state lookups out of the developer's real
/// `~/.codesmith`. The typo path never reads config at all — it fails before
/// `ConfigStore::load`.
fn codesmith(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_codesmith"));
    cmd.env("CODESMITH_HOME", home);
    cmd
}

fn scratch_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("create scratch CODESMITH_HOME")
}

fn read_pipe(mut pipe: Option<impl Read>) -> String {
    let mut out = String::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_string(&mut out);
    }
    out
}

/// `.output()` waits for the child forever; if a regression makes `codesmith`
/// open the interactive TUI instead of exiting, the test would hang until the
/// CI job timeout. Spawn with piped stdio instead, poll `try_wait` under a
/// deadline, and kill the child and fail with the captured stderr so a TUI
/// regression surfaces as a fast, diagnosable failure.
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> (std::process::ExitStatus, String, String) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn codesmith");
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                child.kill().expect("kill runaway codesmith");
                child.wait().expect("reap codesmith");
                let stderr = read_pipe(child.stderr.take());
                panic!(
                    "codesmith did not exit within {timeout:?} (interactive TUI regression?); stderr so far:\n{stderr}"
                );
            }
            Err(e) => panic!("failed to wait for codesmith: {e}"),
        }
    };
    let stdout = read_pipe(child.stdout.take());
    let stderr = read_pipe(child.stderr.take());
    (status, stdout, stderr)
}

#[test]
fn unknown_bare_word_fails_loud_without_opening_the_tui() {
    // Regression: before the guard, `codesmith docotr` spawned `codesmith-tui`
    // with `--prompt docotr` and opened an interactive session on the typo.
    let home = scratch_home();
    let mut cmd = codesmith(home.path());
    cmd.arg("docotr");
    let (status, stdout, stderr) = run_with_timeout(cmd, Duration::from_secs(30));

    assert_eq!(status.code(), Some(2), "usage errors exit 2");
    for token in ["unrecognized subcommand 'docotr'", "-p docotr", "--help"] {
        assert!(
            stderr.contains(token),
            "missing {token:?} in stderr:\n{stderr}"
        );
    }
    assert!(stdout.is_empty(), "stdout should stay empty:\n{stdout}");
}

#[test]
fn version_subcommand_prints_the_build_version() {
    let home = scratch_home();
    let out = codesmith(home.path())
        .arg("version")
        .output()
        .expect("spawn codesmith");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        format!("codesmith {}\n", env!("CODESMITH_BUILD_VERSION")),
        "version output must be exactly one line"
    );
    assert!(
        out.stderr.is_empty(),
        "stderr should stay empty:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn docker_subcommand_prints_the_container_quick_start() {
    let home = scratch_home();
    let out = codesmith(home.path())
        .arg("docker")
        .output()
        .expect("spawn codesmith");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for token in ["docker run", "ghcr.io/camilesing/codesmith"] {
        assert!(
            stdout.contains(token),
            "missing {token:?} in stdout:\n{stdout}"
        );
    }
    assert!(
        out.stderr.is_empty(),
        "stderr should stay empty:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
