//! Parse-gated file editing (P0-1): pre-write syntax gate plus rustfmt
//! re-normalization, shared by every file-writing tool (`write_file`,
//! `edit_file`, `apply_patch`, `fim_edit`).
//!
//! The gate is **regression-only**: a write is rejected before it touches
//! disk only when the file parsed successfully *before* the edit and the
//! new content does not. Files that were already broken — or do not exist
//! yet — are never locked by the gate; cheap models must still be able to
//! repair a broken workspace. Gated formats (first version): Rust via
//! `syn::parse_file`, TOML and JSON via their standard parsers. `Cargo.lock`
//! is exempt (machine-generated, large, and never hand-edited through the
//! write tools).
//!
//! After the gate passes, Rust files that were rustfmt-clean before the
//! edit are re-normalized so subsequent patch anchors stay stable; files
//! that were not clean are written verbatim. rustfmt failures of any kind
//! (missing binary, timeout, edition quirks) silently skip normalization —
//! the gate alone decides rejection.
//!
//! The `parse-gate` cargo feature carries the `syn` dependency. Builds
//! without it compile a pass-through [`gate`]: the switch exists so hosts
//! that never register write tools can drop the parser from their graph.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::tools::spec::ToolError;

/// Final content the caller must persist, plus an optional note for the
/// tool result when rustfmt re-normalization changed the write.
#[derive(Debug, Clone)]
pub struct GateOutcome {
    pub content: String,
    pub normalization_note: Option<String>,
}

/// Pre-write parse gate. `Err` rejects the write — the caller must not
/// touch the file. See the module docs for the regression-only contract.
///
/// `old` is the file's current content (`None` when the file does not
/// exist); `new` is the content the tool is about to write.
#[cfg(feature = "parse-gate")]
pub fn gate(path: &Path, old: Option<&str>, new: &str) -> Result<GateOutcome, ToolError> {
    use Lang::*;

    let Some(lang) = gated_language(path) else {
        return Ok(pass_through(new));
    };

    // Regression-only: compare parseability, not syntax trees.
    let old_was_valid = old.is_some_and(|old| check(lang, old).is_ok());
    if old_was_valid && let Err(failure) = check(lang, new) {
        return Err(reject(path, lang, new, failure));
    }

    let mut outcome = pass_through(new);
    if lang == Rust
        && let Some(old) = old
        && let Some(formatted) = normalize_rustfmt(old, new)
        && formatted != new
    {
        outcome.content = formatted;
        outcome.normalization_note = Some(
            "[parse-gate] this file was rustfmt-clean before the edit; the result was \
             re-normalized with rustfmt to keep future patch anchors stable"
                .to_string(),
        );
    }
    Ok(outcome)
}

/// Pass-through implementation for builds without the `parse-gate` feature:
/// the gate is inert by contract, every write is allowed verbatim.
#[cfg(not(feature = "parse-gate"))]
pub fn gate(path: &Path, old: Option<&str>, new: &str) -> Result<GateOutcome, ToolError> {
    let _ = (path, old);
    Ok(pass_through(new))
}

fn pass_through(new: &str) -> GateOutcome {
    GateOutcome {
        content: new.to_string(),
        normalization_note: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Toml,
    Json,
}

fn gated_language(path: &Path) -> Option<Lang> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("cargo.lock"))
    {
        return None;
    }
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "rs" => Some(Lang::Rust),
        "toml" => Some(Lang::Toml),
        "json" => Some(Lang::Json),
        _ => None,
    }
}

struct ParseFailure {
    line: usize,
    column: usize,
    message: String,
}

#[cfg(feature = "parse-gate")]
fn check(lang: Lang, src: &str) -> Result<(), ParseFailure> {
    match lang {
        Lang::Rust => parse_rust(src),
        Lang::Toml => parse_toml(src),
        Lang::Json => parse_json(src),
    }
}

/// proc-macro2 fallback spans carry real line/column positions only with
/// the `span-locations` feature (enabled alongside this crate's
/// `parse-gate` feature); syn reports 1-based lines, 0-based columns.
#[cfg(feature = "parse-gate")]
fn parse_rust(src: &str) -> Result<(), ParseFailure> {
    match syn::parse_file(src) {
        Ok(_) => Ok(()),
        Err(err) => {
            let start = err.span().start();
            Err(ParseFailure {
                line: start.line.max(1),
                column: start.column + 1,
                message: err.to_string(),
            })
        }
    }
}

#[cfg(feature = "parse-gate")]
fn parse_toml(src: &str) -> Result<(), ParseFailure> {
    toml::from_str::<toml::Value>(src)
        .map(|_| ())
        .map_err(|err| {
            // `span()` is a byte offset into `src`; translate to 1-based line
            // and column. Errors without a span degrade to line 1.
            let (line, column) = err
                .span()
                .map(|span| line_col_from_offset(src, span.start))
                .unwrap_or((1, 1));
            ParseFailure {
                line,
                column,
                message: err.message().to_string(),
            }
        })
}

#[cfg(feature = "parse-gate")]
fn parse_json(src: &str) -> Result<(), ParseFailure> {
    serde_json::from_str::<serde_json::Value>(src)
        .map(|_| ())
        .map_err(|err| {
            // serde_json's Display appends " at line L column C"; strip the
            // tail so the header line stays the single source of location.
            let message = err.to_string();
            let message = message
                .split_once(" at line ")
                .map_or(message.as_str(), |(head, _)| head)
                .to_string();
            ParseFailure {
                line: err.line().max(1),
                column: err.column() + 1,
                message,
            }
        })
}

fn line_col_from_offset(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut line_start = 0;
    for (idx, ch) in src.char_indices() {
        if idx >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = idx + 1;
        }
    }
    let column = src[line_start..offset.min(src.len())].chars().count() + 1;
    (line, column)
}

/// Rejection error following the tool-error house style: what was rejected,
/// where the syntax broke, the offending line, and what to do next.
#[cfg(feature = "parse-gate")]
fn reject(path: &Path, lang: Lang, new: &str, failure: ParseFailure) -> ToolError {
    let lang_name = match lang {
        Lang::Rust => "Rust",
        Lang::Toml => "TOML",
        Lang::Json => "JSON",
    };
    let snippet = new
        .lines()
        .nth(failure.line.saturating_sub(1))
        .map(truncate_line)
        .unwrap_or_default();
    ToolError::execution_failed(format!(
        "Parse gate rejected write to {}: this file parsed as valid {} before the edit, but the new content does not — line {}, column {}: {}\n{:>4}│ {}\nThe file was NOT modified. Fix the syntax error and send the edit again.",
        path.display(),
        lang_name,
        failure.line,
        failure.column,
        failure.message,
        failure.line,
        snippet
    ))
}

fn truncate_line(line: &str) -> String {
    const MAX_LINE_LEN: usize = 200;
    if line.len() <= MAX_LINE_LEN {
        line.to_string()
    } else {
        let mut cut = MAX_LINE_LEN;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &line[..cut])
    }
}

const RUSTFMT_TIMEOUT: Duration = Duration::from_secs(10);
static RUSTFMT_BIN: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Resolve the rustfmt binary once per process. `None` when it is not on
/// PATH — normalization then silently disables itself.
fn rustfmt_binary() -> Option<&'static PathBuf> {
    RUSTFMT_BIN
        .get_or_init(|| {
            Command::new("rustfmt")
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
                .then(|| PathBuf::from("rustfmt"))
        })
        .as_ref()
}

/// Re-normalize `new` when `old` was rustfmt-clean. `None` means "write
/// `new` verbatim": rustfmt unavailable, `old` not clean, or the format
/// run failed — never a rejection.
fn normalize_rustfmt(old: &str, new: &str) -> Option<String> {
    normalize_rustfmt_with(rustfmt_binary().map(PathBuf::as_path), old, new)
}

fn normalize_rustfmt_with(bin: Option<&Path>, old: &str, new: &str) -> Option<String> {
    let formatted_old = format_rustfmt_content(bin, old)?;
    if formatted_old != old {
        return None; // `old` was not rustfmt-clean; leave layout untouched
    }
    format_rustfmt_content(bin, new)
}

/// Run `rustfmt --edition 2024` over `content` via stdin and capture the
/// formatted stdout. `None` on spawn failure, non-zero exit, timeout, or
/// non-UTF-8 output.
fn format_rustfmt_content(bin: Option<&Path>, content: &str) -> Option<String> {
    let bin = bin?;
    let mut child = Command::new(bin)
        .arg("--edition")
        .arg("2024")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let payload = content.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
        // Dropping stdin closes the pipe so rustfmt can finish.
    });
    let mut stdout = Vec::new();
    if child.stdout.take()?.read_to_end(&mut stdout).is_err() {
        let _ = child.kill();
        return None;
    }
    match child.wait_timeout(RUSTFMT_TIMEOUT) {
        Ok(Some(status)) if status.success() => {
            let _ = writer.join();
            Some(String::from_utf8(stdout).ok()?)
        }
        // Non-zero exit: rustfmt refused the input (edition quirks, ...)
        Ok(Some(_)) => {
            let _ = writer.join();
            None
        }
        // Timed out or the wait itself failed: kill and abandon the writer
        // thread — it finishes (or fails) on its own once the killed
        // child's stdin closes.
        Ok(None) | Err(_) => {
            let _ = child.kill();
            None
        }
    }
}

#[cfg(all(test, feature = "parse-gate"))]
mod tests {
    use super::*;

    const GOOD_RS: &str = "fn main() {\n    let x = 1;\n}\n";
    const BROKEN_RS: &str = "fn main() {\n    let x = ;\n}\n";

    fn gate_str(path: &str, old: Option<&str>, new: &str) -> Result<GateOutcome, ToolError> {
        gate(Path::new(path), old, new)
    }

    #[test]
    fn rust_regression_is_rejected_with_line_info() {
        let err = gate_str("src/lib.rs", Some(GOOD_RS), BROKEN_RS).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Parse gate rejected write to src/lib.rs"),
            "{msg}"
        );
        assert!(msg.contains("valid Rust"), "{msg}");
        // The syn span must carry a real position (span-locations feature):
        // the error sits on line 2.
        assert!(msg.contains("line 2"), "{msg}");
        assert!(msg.contains("NOT modified"), "{msg}");
    }

    #[test]
    fn already_broken_rust_is_never_locked() {
        let worse = "fn main() {\n    let x = ; ; ;\n";
        let outcome = gate_str("src/lib.rs", Some(BROKEN_RS), worse).unwrap();
        assert_eq!(outcome.content, worse);
        assert!(outcome.normalization_note.is_none());
    }

    #[test]
    fn new_file_creation_is_allowed() {
        let outcome = gate_str("src/new.rs", None, BROKEN_RS).unwrap();
        assert_eq!(outcome.content, BROKEN_RS);
    }

    #[test]
    fn ungated_extensions_pass_through() {
        let outcome = gate_str("notes.md", Some("hello"), "}}}not markdown{{{").unwrap();
        assert_eq!(outcome.content, "}}}not markdown{{{");
    }

    #[test]
    fn cargo_lock_is_exempt() {
        let outcome = gate_str("Cargo.lock", Some(""), "not toml ][").unwrap();
        assert_eq!(outcome.content, "not toml ][");
    }

    #[test]
    fn json_regression_is_rejected() {
        let err = gate_str("data.json", Some(r#"{"a": 1}"#), r#"{"a": 1, "b": }"#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("valid JSON"), "{msg}");
        assert!(msg.contains("line 1"), "{msg}");
        // The serde_json location tail must not appear twice.
        assert!(!msg.contains(" at line 1 column"), "{msg}");
    }

    #[test]
    fn toml_regression_is_rejected() {
        let err = gate_str(
            "Cargo.toml",
            Some("[package]\nname = \"x\"\n"),
            "[package\nname = \"x\"\n",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("valid TOML"), "{msg}");
    }

    #[test]
    fn empty_rust_is_valid_and_passes() {
        // syn treats an empty file as a valid (item-less) Rust file, so the
        // gate allows truncating a valid file to empty. The old file was
        // rustfmt-clean, so the outcome is the normalized form ("\n").
        let outcome = gate_str("src/lib.rs", Some(GOOD_RS), "").unwrap();
        assert!(outcome.content.trim().is_empty());
    }

    #[test]
    fn empty_toml_is_valid_and_passes() {
        let outcome = gate_str("Cargo.toml", Some("[package]\nname = \"x\"\n"), "").unwrap();
        assert_eq!(outcome.content, "");
    }

    #[test]
    fn missing_rustfmt_disables_normalization() {
        let messy = "fn main(){let x=1;}";
        // No rustfmt binary → normalize returns None regardless of input.
        assert!(normalize_rustfmt_with(None, GOOD_RS, messy).is_none());
    }

    #[test]
    fn rustfmt_normalizes_only_clean_files() {
        let Some(bin) = test_rustfmt() else {
            return; // rustfmt unavailable in this environment
        };
        let messy = "fn main(){let x=1;}";
        let normalized = normalize_rustfmt_with(Some(bin), GOOD_RS, messy).unwrap();
        assert_ne!(normalized, messy);
        assert!(normalized.contains("fn main() {"), "{normalized}");
        // A file that was not rustfmt-clean keeps the model's layout.
        assert!(normalize_rustfmt_with(Some(bin), messy, messy).is_none());
    }

    fn test_rustfmt() -> Option<&'static Path> {
        let status = Command::new("rustfmt")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        status.success().then(|| Path::new("rustfmt"))
    }
}
