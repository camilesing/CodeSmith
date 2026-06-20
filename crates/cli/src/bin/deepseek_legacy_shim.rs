//! Legacy `deepseek` alias.
//!
//! Forwards argv to the `codesmith` dispatcher and prints a one-line
//! deprecation notice to stderr on each invocation. This binary exists
//! for one release cycle to give existing installs a smooth path to the
//! new name; it will be removed in v0.9.0. See `docs/REBRAND.md` for the
//! full migration story.

use std::env;
use std::process::Command;

fn main() {
    eprintln!(
        "warning: `deepseek` is deprecated; run `codesmith` instead. \
         This alias will be removed in v0.9.0."
    );
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
    // Try PATH first.
    match Command::new("codesmith").args(args).status() {
        Ok(s) => return Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    // On Windows, after an update the sibling `codesmith.exe` may be in the
    // same directory as this shim but not on PATH (#2006).
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
