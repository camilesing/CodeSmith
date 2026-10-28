//! `codes` is the shorthand entry point for the same product as `codesmith`.
//!
//! Cargo installs one executable per `[[bin]]` target and has no post-install
//! hook, so `cargo install` users cannot get a symlink; this launcher
//! re-executes the real binary that sits next to it, falling back to `PATH`
//! when someone copies it alone. On Unix `exec` replaces this process, so
//! signals and the terminal stay with the CLI rather than an intermediate
//! shell of ours.
//!
//! Known limitations: the alias carries no version of its own (`codes update`
//! runs the real binary, which updates `codesmith`), and shell completions are
//! generated for the `codesmith` command name only.

use std::path::PathBuf;
use std::process::Command;

fn codesmith_command() -> Command {
    let exe_name = format!("codesmith{}", std::env::consts::EXE_SUFFIX);
    let sibling: Option<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&exe_name)))
        .filter(|candidate| candidate.is_file());
    let program = sibling.unwrap_or_else(|| exe_name.into());
    let mut command = Command::new(program);
    command.args(std::env::args_os().skip(1));
    command
}

fn main() -> std::process::ExitCode {
    let mut command = codesmith_command();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("codes: failed to launch codesmith: {error}");
        std::process::ExitCode::FAILURE
    }

    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(error) => {
                eprintln!("codes: failed to launch codesmith: {error}");
                std::process::ExitCode::FAILURE
            }
        }
    }
}
