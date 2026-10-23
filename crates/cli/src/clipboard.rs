//! Clipboard access for `/copy` and the `Ctrl+V` image paste.
//!
//! Platform tools rather than a clipboard crate: the copy side is `pbcopy`
//! on macOS, `wl-copy`/`xclip` on Linux, and `clip.exe` on Windows, all of
//! which either exist on the machine already or do not exist at all — and
//! `Command`-spawn failures name what to install instead of failing
//! silently.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Text only for `/copy`. An image paste needs a tool that can emit PNG
//!   (`pngpaste`, `wl-paste`, `xclip -t image/png`); when none is installed
//!   the caller is told so, because a "paste did nothing" key is worse than
//!   a named missing program.
//! - Reads and writes go through a spawned process, so they are wrapped in
//!   `spawn_blocking` by their callers: this module is synchronous by design
//!   and must not be called inline on the runtime.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The text-clipboard writer for this platform, if one is installed.
fn text_writer() -> Option<(&'static str, &'static [&'static str])> {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])]
    };
    candidates
        .iter()
        .find(|(program, _)| which(program))
        .map(|(program, args)| (*program, *args))
}

/// Put `text` on the clipboard.
///
/// # Errors
/// Names the missing program when no clipboard tool is installed.
pub fn copy_text(text: &str) -> Result<(), String> {
    let Some((program, args)) = text_writer() else {
        return Err(missing_tool_message(
            "copy to the clipboard",
            &["pbcopy", "wl-copy", "xclip", "clip"],
        ));
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|err| format!("{program} could not be started: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|err| format!("writing to {program}: {err}"))?;
        // `take` + end-of-scope drop closes the write end: the clipboard
        // tool only sees EOF (and exits) once every writer is gone, so
        // `wait()` before this drop hangs forever.
    }
    let status = child
        .wait()
        .map_err(|err| format!("waiting for {program}: {err}"))?;
    if !status.success() {
        return Err(format!("{program} exited with {status}"));
    }
    Ok(())
}

/// The clipboard-image reader for this platform, if one is installed.
///
/// Returns PNG bytes. Called by the `Ctrl+V` binding.
pub fn read_image_png() -> Result<Vec<u8>, String> {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        // `pngpaste -` writes the clipboard PNG to stdout.
        &[("pngpaste", &["-"])]
    } else if cfg!(target_os = "windows") {
        &[]
    } else {
        &[
            ("wl-paste", &["--type", "image/png"]),
            (
                "xclip",
                &["-selection", "clipboard", "-t", "image/png", "-o"],
            ),
        ]
    };
    let Some((program, args)) = candidates
        .iter()
        .find(|(program, _)| which(program))
        .map(|(program, args)| (*program, *args))
    else {
        return Err(missing_tool_message(
            "paste an image from the clipboard",
            &["pngpaste", "wl-paste", "xclip"],
        ));
    };
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map_err(|err| format!("{program} could not be started: {err}"))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "{program} returned no image — is there one on the clipboard?"
        ));
    }
    Ok(output.stdout)
}

fn missing_tool_message(action: &str, programs: &[&str]) -> String {
    format!(
        "cannot {action}: none of {} is on PATH — install one to enable it",
        programs.join(", ")
    )
}

/// Is `program` on `PATH`? Mirrors the probe the `gh` tools use.
fn which(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file() || cfg!(windows) && dir.join(format!("{program}.exe")).is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tool_names_what_to_install() {
        let message = missing_tool_message("copy to the clipboard", &["pbcopy", "xclip"]);
        assert!(message.contains("pbcopy"));
        assert!(message.contains("xclip"));
        assert!(message.contains("PATH"));
    }

    #[test]
    fn which_finds_a_shell_that_must_exist() {
        // `sh` is present on every Unix this runs on; the probe is the same
        // one the clipboard path uses.
        #[cfg(unix)]
        assert!(which("sh"));
        assert!(!which("definitely-not-a-real-program-xyz"));
    }
}
