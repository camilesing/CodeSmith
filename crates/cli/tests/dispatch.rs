//! Root-dispatch regression tests.
//!
//! These spawn the real `codesmith` binary, which is the only way to cover the
//! wiring in `run()`: the guard helpers are unit-tested next to the code, but a
//! guard that is never called would still pass those tests.

use std::path::PathBuf;
use std::process::Command;

/// `CODESMITH_HOME` keeps config and state lookups out of the developer's real
/// `~/.codesmith`. The typo path never reads config at all — it fails before
/// `ConfigStore::load`.
fn codesmith(home: &PathBuf) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_codesmith"));
    cmd.env("CODESMITH_HOME", home);
    cmd
}

fn scratch_home(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("codesmith-dispatch-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch CODESMITH_HOME");
    dir
}

#[test]
fn unknown_bare_word_fails_loud_without_opening_the_tui() {
    // Regression: before the guard, `codesmith docotr` spawned `codesmith-tui`
    // with `--prompt docotr` and opened an interactive session on the typo.
    let out = codesmith(&scratch_home("typo"))
        .arg("docotr")
        .output()
        .expect("spawn codesmith");

    assert_eq!(out.status.code(), Some(2), "usage errors exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for token in [
        "unrecognized subcommand 'docotr'",
        "codesmith -p 'docotr'",
        "codesmith run <COMMAND>",
    ] {
        assert!(
            stderr.contains(token),
            "missing {token:?} in stderr:\n{stderr}"
        );
    }
    assert!(
        out.stdout.is_empty(),
        "stdout should stay empty:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn version_subcommand_prints_the_build_version() {
    let out = codesmith(&scratch_home("version"))
        .arg("version")
        .output()
        .expect("spawn codesmith");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with(&format!("codesmith {}", env!("CARGO_PKG_VERSION"))),
        "stdout:\n{stdout}"
    );
}

#[test]
fn docker_subcommand_prints_the_container_quick_start() {
    let out = codesmith(&scratch_home("docker"))
        .arg("docker")
        .output()
        .expect("spawn codesmith");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ghcr.io/camilesing/codesmith"),
        "stdout:\n{stdout}"
    );
}
