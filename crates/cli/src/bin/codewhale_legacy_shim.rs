//! Legacy `codewhale` alias.
//!
//! Forwards argv to the `codesmith` dispatcher and prints a one-line
//! deprecation notice to stderr on each invocation.

use std::env;
use std::process::Command;

fn main() {
    eprintln!("warning: `codewhale` is deprecated; run `codesmith` instead.");
    let args: Vec<String> = env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let status = match spawn_codesmith(&args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: failed to spawn `codesmith`: {e}. Is it on PATH? \
                 Install with `cargo install codesmith-cli` or via npm/Homebrew."
            );
            std::process::exit(127);
        }
    };
    std::process::exit(status.code().unwrap_or(1));
}

fn spawn_codesmith(args: &[String]) -> std::io::Result<std::process::ExitStatus> {
    match Command::new("codesmith").args(args).status() {
        Ok(s) => return Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    #[cfg(windows)]
    {
        if let Ok(exe_path) = env::current_exe()
            && let Some(dir) = exe_path.parent()
        {
            let sibling = dir.join("codesmith.exe");
            if sibling.is_file() {
                return Command::new(sibling).args(args).status();
            }
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "codesmith not found on PATH or in sibling directory",
    ))
}
