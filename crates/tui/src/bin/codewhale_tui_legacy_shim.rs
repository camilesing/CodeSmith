//! Legacy `codewhale-tui` alias.
//!
//! Forwards argv to the `codesmith-tui` runtime and prints a one-line
//! deprecation notice to stderr on each invocation.

use std::env;
use std::process::Command;

fn main() {
    eprintln!(
        "warning: `codewhale-tui` is deprecated; run `codesmith-tui` (or `codesmith`) instead."
    );
    let args: Vec<String> = env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let status = match Command::new("codesmith-tui").args(&args).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: failed to spawn `codesmith-tui`: {e}. Is it on PATH? \
                 Install with `cargo install codesmith-tui` or via npm/Homebrew."
            );
            std::process::exit(127);
        }
    };
    std::process::exit(status.code().unwrap_or(1));
}
