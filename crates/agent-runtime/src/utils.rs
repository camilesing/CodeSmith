//! Utility helpers shared across the `DeepSeek` CLI.

pub use codesmith_context::project_docs::{is_key_file, project_tree, summarize_project};
pub use codesmith_sandbox::sanitization::{
    PROMPT_FRAMING_TAGS, defuse_closing_tag, defuse_framing_tags as defuse_prompt_framing_tags,
    escape_prompt_attr,
};
pub const GITHUB_REPO: &str = "camilesing/CodeSmith";

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::models::{ContentBlock, ImageSource, Message};
use anyhow::{Context, Result};
use serde_json::Value;

// === Home Directory Resolution ===

/// Read up to `max` bytes from an async reader into memory, then drain and
/// count the rest. Beyond-the-cap bytes are still consumed so the writing
/// child never blocks on a full pipe, but memory stays bounded at `max`.
/// Returns `(kept, dropped)`.
pub async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> (Vec<u8>, usize) {
    use tokio::io::AsyncReadExt;
    let mut kept = Vec::new();
    let mut dropped = 0usize;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let take = max.saturating_sub(kept.len()).min(n);
                kept.extend_from_slice(&buf[..take]);
                dropped += n - take;
            }
        }
    }
    (kept, dropped)
}

/// Like [`read_capped`], but keeps the **last** `max` bytes — for log-style
/// output whose actionable detail sits at the end (compiler/test failures).
/// Memory peaks at 2×`max` (double-buffer swap), and each byte is copied at
/// most twice. Returns `(kept_tail, dropped)`.
pub async fn read_capped_tail<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> (Vec<u8>, usize) {
    use tokio::io::AsyncReadExt;
    let mut previous: Vec<u8> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut dropped = 0usize;
    let mut buf = vec![0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        if current.len() + n > max {
            dropped += previous.len();
            previous = std::mem::take(&mut current);
        }
        let take = n.min(max - current.len());
        current.extend_from_slice(&buf[..take]);
        dropped += n - take;
    }
    let mut kept = std::mem::take(&mut previous);
    kept.extend_from_slice(&current);
    if kept.len() > max {
        dropped += kept.len() - max;
        kept = kept[kept.len() - max..].to_vec();
    }
    (kept, dropped)
}

/// Resolve the effective user home directory.
///
/// Checks `HOME` (Unix), `USERPROFILE` (Windows), and the
/// `HOMEDRIVE`+`HOMEPATH` pair (Windows) before falling back to
/// [`dirs::home_dir`]. Empty values are treated as unset.
///
/// This was relocated from the TUI `config` module so that runtime
/// modules (e.g. `mcp`) can resolve the home directory without depending
/// on the TUI's `config` module; the CLI re-exports it as
/// `crate::config::effective_home_dir` for backwards compatibility.
pub fn effective_home_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }

    if let Some(path) = std::env::var_os("USERPROFILE") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }

    #[cfg(windows)]
    {
        if let (Some(drive), Some(homepath)) =
            (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
        {
            let mut path = PathBuf::from(drive);
            path.push(homepath);
            if !path.as_os_str().is_empty() {
                return Some(path);
            }
        }
    }

    dirs::home_dir()
}

// === Project Mapping Helpers ===

/// Quarantine a state file that failed to parse: best-effort copy to
/// `<name>.corrupt-<unix-millis>` before the caller substitutes a default.
/// The default-then-rewrite pattern otherwise permanently destroys the
/// prior state — the only copy of the user's data. Failures are ignored
/// (preservation must never block the degradation path).
pub fn preserve_corrupt_state_file(path: &Path) {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let backup = path.with_file_name(format!("{name}.corrupt-{ts}"));
    let _ = std::fs::copy(path, &backup);
}

pub use codesmith_session::primitives::write_atomic;

/// Returns true if `component` is safe to join as a single path component:
/// non-empty, no path separators, no `..`/`.`, and only `[A-Za-z0-9._-]`
/// (plus non-ASCII word chars are REJECTED — keep it strict). Prevents
/// traversal via ids/names that reach `Path::join`.
///
/// The whitelist is deliberately conservative: ASCII alphanumerics, `_`,
/// `-`, and `.` in a non-leading, non-trailing position. A leading dot
/// rejects `.`, `..`, and dotfiles; a trailing dot is stripped by Windows
/// APIs (`foo..` aliases `foo.`); separators, control characters,
/// whitespace, and non-ASCII are all outside the whitelist and rejected.
#[must_use]
pub fn is_safe_path_component(component: &str) -> bool {
    if component.is_empty() {
        return false;
    }
    // Leading/trailing dot gate first — this alone rejects "", ".", "..",
    // ".hidden", and "foo.." before the per-character scan runs.
    let bytes = component.as_bytes();
    if bytes[0] == b'.' || bytes[bytes.len() - 1] == b'.' {
        return false;
    }
    // Windows reserved device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9,
    // including spellings like `CON.txt`) are pure ASCII alphanumerics, so
    // they pass the whitelist below yet alias a device when the component
    // is joined into a path on Windows. Reject by stem, case-insensitively.
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = component.split('.').next().unwrap_or("");
    if RESERVED.contains(&stem.to_ascii_uppercase().as_str()) {
        return false;
    }
    // Strict per-character whitelist. `char` iteration keeps multi-byte
    // sequences intact so non-ASCII like 'é' fails cleanly instead of
    // slipping through as a lucky byte sequence.
    component
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

/// Bail-shaped companion to [`is_safe_path_component`] for call sites that
/// join the id into a path and fail loud on a bad one — one shared guard
/// and message instead of per-caller copies that drift.
pub fn require_safe_path_component(id: &str) -> anyhow::Result<()> {
    if !is_safe_path_component(id) {
        anyhow::bail!("invalid id {id:?}: must be a single safe path component");
    }
    Ok(())
}

/// Open or create a file for appending at `path`, optionally syncing after
/// every write. Use this for append-only logs like `audit.log`.
///
/// The returned `BufWriter<fs::File>` wraps the append handle. Call
/// `.flush()` followed by `.get_ref().sync_all()` after each batch.
///
/// On Unix the file is created owner-only (0600) regardless of umask —
/// append logs record details of sensitive operations — and a
/// pre-existing file is tightened to 0600 if it was more permissive.
/// Known limitation: the open still follows a pre-planted symlink (the
/// callers write under the owner-only `~/.codesmith`, which is the
/// containment boundary).
pub fn open_append(path: &Path) -> std::io::Result<std::io::BufWriter<std::fs::File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(path)?.permissions();
        if perms.mode() & 0o777 != 0o600 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(std::io::BufWriter::new(file))
}

/// Flush a `BufWriter` wrapping a `File`, then `fsync` the underlying file.
pub fn flush_and_sync(writer: &mut std::io::BufWriter<std::fs::File>) -> std::io::Result<()> {
    writer.flush()?;
    writer.get_ref().sync_all()
}

/// Open an http/https URL in the system's default browser.
///
/// Dispatches to the platform-appropriate opener:
/// - macOS: `open`
/// - Linux: `xdg-open`
/// - Windows: `explorer` (NOT `cmd /C start` — cmd.exe metacharacter
///   semantics (`&`, `|`, `%VAR%`) are not containable through argv)
/// - Other: returns an error.
///
/// Only `http`/`https` URLs are dispatched, with no whitespace or control
/// characters: a URL that ever derives from untrusted content (LLM output,
/// markdown links, server responses) must not become a command-execution
/// or arbitrary-handler (`file://`, `ssh://`, …) vector. The opener is
/// waited on so it does not linger as a zombie.
pub fn open_url(url: &str) -> Result<()> {
    let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        anyhow::bail!("refusing to open non-http(s) URL: {url}");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        anyhow::bail!("refusing to open URL containing whitespace/control characters");
    }

    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer");

    command.arg(url);
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| child.wait().map(|_| ()))
        .map_err(|e| anyhow::anyhow!("failed to launch browser command: {e}"))
}

/// Spawn a tokio task with panic supervision.
///
/// Wraps the future in `AssertUnwindSafe` + `catch_unwind`. On panic:
/// 1. Logs the panic with the task name and caller location via `tracing::error!`.
/// 2. Writes a crash dump to `~/.codesmith/crashes/<timestamp>-<name>.log`.
///
/// The returned `JoinHandle` resolves to `()` — the panic is caught and
/// handled internally so the parent process stays alive.
pub fn spawn_supervised<F>(
    name: &'static str,
    location: &'static std::panic::Location<'static>,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        use futures_util::FutureExt;
        let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;
        if let Err(panic_info) = result {
            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            tracing::error!(
                target: "panic",
                "Task '{name}' panicked at {}: {msg}",
                location,
            );
            // Write crash dump (best-effort)
            let _ = write_panic_dump(name, location, &msg);
        }
    })
}

/// Write a panic dump file to `~/.codesmith/crashes/`.
///
/// Creates the directory if needed and writes a timestamped log
/// with the task name, caller location, and panic message.
/// Best-effort — failures are silently ignored.
fn write_panic_dump(
    name: &str,
    location: &std::panic::Location<'_>,
    message: &str,
) -> std::io::Result<()> {
    let home = codesmith_config::codesmith_home().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("CodeSmith home directory not found: {err}"),
        )
    })?;
    let crash_dir = home.join("crashes");
    let _ = std::fs::create_dir_all(&crash_dir);
    write_panic_dump_to(&crash_dir, name, location, message)
}

fn write_panic_dump_to(
    crash_dir: &Path,
    name: &str,
    location: &std::panic::Location<'_>,
    message: &str,
) -> std::io::Result<()> {
    use chrono::Utc;
    std::fs::create_dir_all(crash_dir)?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let filename = format!("{timestamp}-{name}.log");
    let path = crash_dir.join(&filename);
    let contents =
        format!("Task: {name}\nLocation: {location}\nTimestamp: {timestamp}\nPanic: {message}\n");
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Fire-and-forget `spawn_blocking` with panic dump protection.
///
/// In contrast to `spawn_supervised` (which wraps `tokio::spawn` for async
/// tasks), this helper wraps `tokio::task::spawn_blocking`.  Use it when a
/// CPU-bound or blocking-I/O task must run off the async runtime and its
/// completion is *not* awaited — for example a post-turn disk snapshot or a
/// file-tree build polled later via a shared data structure.  If the closure
/// panics, a crash dump is written to `~/.codesmith/crashes/` and the panic
/// is logged at ERROR level rather than being silently swallowed.
#[track_caller]
pub fn spawn_blocking_supervised<F>(name: &'static str, f: F) -> tokio::task::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    let location = std::panic::Location::caller();
    tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        if let Err(panic_info) = result {
            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            tracing::error!(
                target: "panic",
                "Blocking task '{name}' panicked at {location}: {msg}",
            );
            let _ = write_panic_dump(name, location, &msg);
        }
    })
}

#[allow(dead_code)]
pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create directory: {}", path.display()))
}

/// Render JSON with pretty formatting, falling back to a compact string on error.
#[must_use]
#[allow(dead_code)]
pub fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Truncate a string to a maximum length, adding an ellipsis if truncated.
///
/// Uses char boundaries to avoid panicking on multi-byte UTF-8 characters.
#[must_use]
pub fn truncate_with_ellipsis(s: &str, max_len: usize, ellipsis: &str) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    if max_len <= ellipsis.len() {
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max_len)
            .last()
            .unwrap_or(0);
        return s[..end].to_string();
    }
    let budget = max_len - ellipsis.len();
    // Find the last char boundary that fits within the byte budget.
    let safe_end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= budget)
        .last()
        .unwrap_or(0);
    format!("{}{}", &s[..safe_end], ellipsis)
}

/// Percent-encode a string for use in URL query parameters.
///
/// Encodes all characters except unreserved characters (A-Z, a-z, 0-9, `-`, `_`, `.`, `~`).
/// Spaces are encoded as `+`.
#[must_use]
pub fn url_encode(input: &str) -> String {
    let mut encoded = String::new();
    for ch in input.bytes() {
        match ch {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(ch as char)
            }
            b' ' => encoded.push('+'),
            // Only ever two uppercase hex digits — push them directly
            // instead of allocating a fresh `String` per encoded byte.
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                encoded.push('%');
                encoded.push(HEX[(ch >> 4) as usize] as char);
                encoded.push(HEX[(ch & 0xF) as usize] as char);
            }
        }
    }
    encoded
}

// === Prompt-Framing Escape Helpers ===
// (moved to `codesmith_sandbox::sanitization` in the round-9 split; the
// re-exports at the top of this file keep `crate::utils::…` paths working)

/// Render a path for **user-facing display** with the home directory
/// contracted to `~`. Use this in the REPL, doctor/setup stdout, and any
/// other place a viewer might see the output (screenshot, video,
/// pasted-into-issue help). On macOS/Linux the absolute path
/// `/Users/<name>/...` or `/home/<name>/...` reveals the OS account name,
/// which is often the same as a public handle — undesirable for users
/// who share their terminal.
///
/// **Do not use** this for paths that get persisted (sessions, audit log)
/// or sent to the LLM provider — those want full fidelity so they
/// resolve correctly across processes.
#[must_use]
pub fn display_path(path: &Path) -> String {
    display_path_with_home(path, effective_home_dir().as_deref())
}

/// Like [`display_path`] but takes an explicit home directory instead of
/// reading `$HOME` / `dirs::home_dir()`.  Used in tests and anywhere the
/// caller already has the home path available.
///
/// The home-relative suffix is rejoined with the platform separator
/// (`\` on Windows, `/` elsewhere) by walking the path's components, so
/// inputs that carried foreign separators don't leak through.
#[must_use]
pub fn display_path_with_home(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.display().to_string();
    };
    if let Ok(rest) = path.strip_prefix(home) {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        let sep = std::path::MAIN_SEPARATOR_STR;
        let mut out = String::from("~");
        for component in rest.components() {
            out.push_str(sep);
            out.push_str(&component.as_os_str().to_string_lossy());
        }
        return out;
    }
    path.display().to_string()
}

/// Estimate the total character count across message content blocks.
#[must_use]
pub fn estimate_message_chars(messages: &[Message]) -> usize {
    let mut total = 0;
    for msg in messages {
        for block in &msg.content {
            match block {
                ContentBlock::Text { text, .. } => total += text.len(),
                ContentBlock::Thinking { thinking } => total += thinking.len(),
                ContentBlock::ToolUse { input, .. } => total += input.to_string().len(),
                ContentBlock::ToolResult { content, .. } => total += content.len(),
                ContentBlock::Image { source } => total += image_source_chars(source),
                ContentBlock::ServerToolUse { .. }
                | ContentBlock::ToolSearchToolResult { .. }
                | ContentBlock::CodeExecutionToolResult { .. } => {}
            }
        }
    }
    total
}

/// Serialized char cost of an image block: base64 payloads dominate, file
/// references stay path-sized.
fn image_source_chars(source: &ImageSource) -> usize {
    match source {
        ImageSource::Base64 { data, .. } => data.len(),
        ImageSource::File { path, media_type } => {
            path.display().to_string().len() + media_type.as_deref().map_or(0, str::len)
        }
    }
}

// === Hashing ===

pub use codesmith_session::primitives::sha256_hex;

#[cfg(test)]
mod prompt_escape_tests {
    use super::{defuse_closing_tag, escape_prompt_attr};

    // ── escape_prompt_attr ─────────────────────────────────────────────

    #[test]
    fn escape_attr_passes_plain_text_through() {
        assert_eq!(escape_prompt_attr("MEMORY.md"), "MEMORY.md");
        assert_eq!(escape_prompt_attr("/tmp/a/b.md"), "/tmp/a/b.md");
        assert_eq!(escape_prompt_attr(""), "");
    }

    #[test]
    fn escape_attr_escapes_all_metacharacters() {
        assert_eq!(escape_prompt_attr("a&b"), "a&amp;b");
        assert_eq!(escape_prompt_attr("a\"b"), "a&quot;b");
        assert_eq!(escape_prompt_attr("a<b"), "a&lt;b");
        assert_eq!(escape_prompt_attr("a>b"), "a&gt;b");
        assert_eq!(escape_prompt_attr("a'b"), "a&#39;b");
    }

    #[test]
    fn escape_attr_blocks_early_attribute_termination() {
        // A value trying to close the attribute and open injected markup.
        let escaped = escape_prompt_attr("x\"><injected attr=\"1");
        assert_eq!(
            escaped, "x&quot;&gt;&lt;injected attr=&quot;1",
            "no raw `\"` or `<>` may survive"
        );
    }

    #[test]
    fn escape_attr_does_not_double_escape_entities() {
        // Pre-existing entity-looking text is escaped once, as raw text.
        assert_eq!(escape_prompt_attr("&amp;"), "&amp;amp;");
    }

    // ── defuse_closing_tag ─────────────────────────────────────────────

    #[test]
    fn defuse_neutralizes_exact_closing_tag() {
        assert_eq!(
            defuse_closing_tag("body with </knowledge_memory> inside", "knowledge_memory"),
            "body with &lt;/knowledge_memory> inside"
        );
    }

    #[test]
    fn defuse_neutralizes_extended_tag_names() {
        // `</knowledge_memoryfoo>` does not close `<knowledge_memory>` in
        // strict XML, but the model may read it as a close — defuse the
        // `</{tag}` prefix so neither variant can.
        assert_eq!(
            defuse_closing_tag("x</knowledge_memoryfoo>", "knowledge_memory"),
            "x&lt;/knowledge_memoryfoo>"
        );
        // Spaced / attribute-suffixed closers share the same prefix.
        assert_eq!(
            defuse_closing_tag("</knowledge_memory >", "knowledge_memory"),
            "&lt;/knowledge_memory >"
        );
    }

    #[test]
    fn defuse_leaves_other_tags_and_plain_text_alone() {
        assert_eq!(
            defuse_closing_tag(
                "has </other> and <knowledge_memory> tags",
                "knowledge_memory"
            ),
            "has </other> and <knowledge_memory> tags"
        );
        assert_eq!(
            defuse_closing_tag("plain text", "knowledge_memory"),
            "plain text"
        );
        assert_eq!(defuse_closing_tag("", "knowledge_memory"), "");
    }

    #[test]
    fn defuse_matches_case_insensitively() {
        // Matching is ASCII-case-insensitive: any casing of the closing
        // tag is defused, not just the exact-case sequence.
        assert_eq!(
            defuse_closing_tag("</Knowledge_Memory>", "knowledge_memory"),
            "&lt;/Knowledge_Memory>"
        );
        assert_eq!(
            defuse_closing_tag("</knowledge_memory>", "knowledge_memory"),
            "&lt;/knowledge_memory>"
        );
        // Other tags remain untouched.
        assert_eq!(
            defuse_closing_tag("</other>", "knowledge_memory"),
            "</other>"
        );
    }

    #[test]
    fn defuse_empty_tag_returns_input_unchanged() {
        assert_eq!(
            defuse_closing_tag("</knowledge_memory>", ""),
            "</knowledge_memory>"
        );
    }

    #[test]
    fn defuse_is_idempotent() {
        let once = defuse_closing_tag("a</knowledge_memory>b", "knowledge_memory");
        assert_eq!(
            defuse_closing_tag(&once, "knowledge_memory"),
            once,
            "escaped form must not be re-escaped"
        );
    }
}

#[cfg(test)]
mod sha256_hex_tests {
    use super::sha256_hex;

    /// Known vector: SHA-256 of the empty byte string.
    #[test]
    fn empty_input_yields_known_digest() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// Known vector: SHA-256 of `b"abc"` (FIPS 180-2 / NIST test case).
    #[test]
    fn abc_input_yields_known_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

// Tests use `display_path_with_home` so they never mutate the global `HOME`
// env var.  Mutating `HOME` via `std::env::set_var` is not thread-safe; Cargo
// runs tests in parallel by default and CI runners are multi-core, so any test
// that stomps `HOME` will race with tests that *read* it.  Using the injected
// helper avoids the race entirely and makes the tests portable to Windows
// without additional platform scaffolding.
#[cfg(test)]
mod tests {
    use super::display_path_with_home;
    use std::path::PathBuf;

    fn home(s: &str) -> Option<PathBuf> {
        Some(PathBuf::from(s))
    }

    #[test]
    fn display_path_contracts_home_prefix() {
        let h = home("/Users/alice");
        assert_eq!(
            display_path_with_home(&PathBuf::from("/Users/alice/projects/foo"), h.as_deref()),
            format!(
                "~{}projects{}foo",
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            ),
        );
    }

    #[test]
    fn display_path_returns_bare_tilde_for_home_itself() {
        let h = home("/Users/alice");
        assert_eq!(
            display_path_with_home(&PathBuf::from("/Users/alice"), h.as_deref()),
            "~"
        );
    }

    #[test]
    fn display_path_leaves_unrelated_paths_alone() {
        let h = home("/Users/alice");
        // Different user — must not get rewritten or share the tilde.
        assert_eq!(
            display_path_with_home(&PathBuf::from("/Users/bob/Code"), h.as_deref()),
            "/Users/bob/Code".to_string()
        );
        // System path must stay absolute.
        assert_eq!(
            display_path_with_home(&PathBuf::from("/etc/hosts"), h.as_deref()),
            "/etc/hosts"
        );
    }

    #[test]
    fn display_path_does_not_match_username_prefix() {
        // Regression guard: a directory named like the user's home
        // *prefix* but not under it must not get rewritten.
        let h = home("/Users/alice");
        assert_eq!(
            display_path_with_home(&PathBuf::from("/Users/alice2/work"), h.as_deref()),
            "/Users/alice2/work"
        );
    }

    #[test]
    fn display_path_with_no_home_returns_full_path() {
        assert_eq!(
            display_path_with_home(&PathBuf::from("/some/path"), None),
            "/some/path"
        );
    }
}

#[cfg(test)]
mod atomic_write_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn write_atomic_writes_content() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("test.json");
        let content = b"hello atomic world";

        write_atomic(&path, content).expect("write_atomic");
        assert!(path.exists());
        let read = fs::read_to_string(&path).expect("read");
        assert_eq!(read.as_bytes(), content);
    }

    #[test]
    fn write_atomic_replaces_existing_file() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("existing.json");
        fs::write(&path, b"old content").expect("write old");
        write_atomic(&path, b"new content").expect("write_atomic");
        let read = fs::read_to_string(&path).expect("read");
        assert_eq!(read, "new content");
    }

    #[test]
    fn write_atomic_no_temp_left_behind_on_success() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("clean.json");
        write_atomic(&path, b"clean").expect("write_atomic");
        // List files in dir — there should be no .tmp files left
        let entries: Vec<_> = fs::read_dir(tmp.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .collect();
        let tmp_files: Vec<_> = entries
            .iter()
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with('.')))
            .collect();
        assert!(
            tmp_files.is_empty(),
            "temp files left behind: {tmp_files:?}"
        );
    }

    #[test]
    fn flush_and_sync_writes_and_syncs() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("append.log");
        {
            let mut writer = open_append(&path).expect("open_append");
            writeln!(writer, "line 1").expect("write");
            flush_and_sync(&mut writer).expect("flush_and_sync");
            writeln!(writer, "line 2").expect("write");
            flush_and_sync(&mut writer).expect("flush_and_sync");
        }
        let content = fs::read_to_string(&path).expect("read");
        assert_eq!(content, "line 1\nline 2\n");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_existing_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("existing-mode.json");
        fs::write(&path, b"old").expect("write old");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod 0644");
        write_atomic(&path, b"new").expect("write_atomic");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o644,
            "repeated writes must not drift permissions"
        );
    }
}

#[cfg(test)]
mod safe_path_component_tests {
    use super::is_safe_path_component;

    #[test]
    fn accepts_plain_alphanumeric() {
        assert!(is_safe_path_component("abc123"));
    }

    #[test]
    fn accepts_inner_dots_underscores_dashes() {
        assert!(is_safe_path_component("a-b_c.d"));
        assert!(is_safe_path_component("2024-01-02T03.04.05Z"));
    }

    #[test]
    fn rejects_empty() {
        assert!(!is_safe_path_component(""));
    }

    #[test]
    fn rejects_dot_and_dotdot() {
        assert!(!is_safe_path_component("."));
        assert!(!is_safe_path_component(".."));
    }

    #[test]
    fn rejects_windows_reserved_device_names() {
        // Reserved names alias devices when joined into a path on Windows,
        // in any casing and with any extension (`CON.txt` → stem `CON`).
        for name in ["CON", "con", "NUL", "PRN", "AUX", "COM1", "LPT9"] {
            assert!(!is_safe_path_component(name), "{name} must be rejected");
        }
        assert!(!is_safe_path_component("COM3.log"));
        // Near-misses that are ordinary names stay accepted.
        assert!(is_safe_path_component("CONSOLE"));
        assert!(is_safe_path_component("console-log"));
    }

    #[test]
    fn rejects_traversal_and_separators() {
        assert!(!is_safe_path_component("../foo"));
        assert!(!is_safe_path_component("a/b"));
        // Backslash is a separator on Windows and a smuggle vector elsewhere.
        assert!(!is_safe_path_component(r"a\b"));
        assert!(!is_safe_path_component("a\\b"));
    }

    #[test]
    fn rejects_leading_and_trailing_dots() {
        assert!(!is_safe_path_component(".hidden"));
        // Trailing dots are stripped by Windows APIs — reject outright.
        assert!(!is_safe_path_component("foo.."));
    }

    #[test]
    fn rejects_non_ascii() {
        assert!(!is_safe_path_component("café"));
    }

    #[test]
    fn rejects_control_chars_and_whitespace() {
        assert!(!is_safe_path_component("a b"));
        assert!(!is_safe_path_component("a\tb"));
        assert!(!is_safe_path_component("a\nb"));
        assert!(!is_safe_path_component("a\0b"));
    }
}

#[cfg(test)]
mod spawn_supervised_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A spawned task that panics does not propagate the panic to the
    /// parent task — `spawn_supervised` catches it. Verified in isolation
    /// from the on-disk crash-dump path so the test is portable across
    /// macOS / Linux / Windows (where `dirs::home_dir()` reads
    /// `USERPROFILE`, not `HOME`, so env-mutation tricks don't redirect
    /// the dump on Windows).
    #[tokio::test]
    async fn panicking_task_does_not_propagate_to_parent() {
        let parent_alive = Arc::new(AtomicBool::new(false));
        let parent_alive_clone = parent_alive.clone();

        let handle = spawn_supervised(
            "panic-test-fixture",
            std::panic::Location::caller(),
            async move {
                parent_alive_clone.store(true, Ordering::SeqCst);
                panic!("deliberate panic for catch-unwind test");
            },
        );

        let result = handle.await;
        assert!(
            result.is_ok(),
            "spawn_supervised must convert panic to a normal completion"
        );
        assert!(
            parent_alive.load(Ordering::SeqCst),
            "fixture task must have run before panicking"
        );
    }

    #[tokio::test]
    async fn panicking_blocking_task_does_not_propagate_to_parent() {
        let parent_alive = Arc::new(AtomicBool::new(false));
        let parent_alive_clone = parent_alive.clone();

        let handle = spawn_blocking_supervised("blocking-panic-test-fixture", move || {
            parent_alive_clone.store(true, Ordering::SeqCst);
            panic!("deliberate panic for spawn_blocking catch-unwind test");
        });

        let result = handle.await;
        assert!(
            result.is_ok(),
            "spawn_blocking_supervised must convert panic to a normal completion"
        );
        assert!(
            parent_alive.load(Ordering::SeqCst),
            "fixture blocking task must have run before panicking"
        );
    }

    /// `write_panic_dump_to` writes a properly-formatted crash log into
    /// the supplied directory. Tested separately from `spawn_supervised`
    /// because env-mutation redirection of `dirs::home_dir()` doesn't
    /// work on Windows.
    #[test]
    fn write_panic_dump_writes_named_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let crash_dir = tmp.path().join("crashes");
        let location = std::panic::Location::caller();
        write_panic_dump_to(&crash_dir, "panic-fixture", location, "boom").expect("write dump");

        let entries: Vec<_> = std::fs::read_dir(&crash_dir)
            .expect("crashes dir exists")
            .flatten()
            .collect();
        assert_eq!(entries.len(), 1, "exactly one crash dump expected");
        let dump = std::fs::read_to_string(entries[0].path()).expect("read dump");
        assert!(
            dump.contains("panic-fixture"),
            "dump must include the task name; got: {dump}"
        );
        assert!(
            dump.contains("boom"),
            "dump must include the panic message; got: {dump}"
        );
    }
}

#[cfg(test)]
mod project_mapping_tests {
    use super::{project_tree, summarize_project};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn project_tree_sorts_siblings_alphabetically() {
        // Cross-platform readdir doesn't guarantee alphabetical order — on
        // ext4 with htree it's hash order, on APFS it's roughly insertion
        // order, on ZFS it's storage-class dependent. The system prompt
        // embeds this string in the cached prefix when a workspace has no
        // AGENTS.md / CLAUDE.md, so the function has to be byte-stable
        // across runs regardless of host filesystem.
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        // Create files in a deliberately scrambled order to make the
        // hosting filesystem's pre-sort (if any) less likely to mask a
        // missing sort in our code.
        fs::write(root.join("zebra.txt"), "z").expect("write zebra");
        fs::write(root.join("apple.txt"), "a").expect("write apple");
        fs::write(root.join("mango.txt"), "m").expect("write mango");

        let tree = project_tree(root, 1);
        let lines: Vec<&str> = tree.lines().collect();
        let apple_pos = lines
            .iter()
            .position(|l| l.contains("apple.txt"))
            .expect("apple line");
        let mango_pos = lines
            .iter()
            .position(|l| l.contains("mango.txt"))
            .expect("mango line");
        let zebra_pos = lines
            .iter()
            .position(|l| l.contains("zebra.txt"))
            .expect("zebra line");

        assert!(apple_pos < mango_pos);
        assert!(mango_pos < zebra_pos);
    }

    #[test]
    fn project_tree_keeps_directory_before_its_children() {
        // Sorting siblings by full path is enough to preserve tree shape:
        // `"src" < "src/lib.rs"` because the shorter string compares less.
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).expect("mkdir src");
        fs::write(src.join("lib.rs"), "lib").expect("write lib");
        fs::write(src.join("main.rs"), "main").expect("write main");

        let tree = project_tree(root, 2);
        let src_pos = tree.find("DIR: src").expect("src dir line");
        let lib_pos = tree.find("FILE: lib.rs").expect("lib file line");
        let main_pos = tree.find("FILE: main.rs").expect("main file line");

        assert!(src_pos < lib_pos, "directory must precede its children");
        assert!(lib_pos < main_pos, "siblings sorted by name");
    }

    #[test]
    fn project_tree_is_byte_stable_across_calls() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("z.txt"), "z").expect("write");
        fs::write(root.join("a.txt"), "a").expect("write");

        assert_eq!(project_tree(root, 1), project_tree(root, 1));
    }

    #[test]
    #[cfg(unix)]
    fn project_mapping_does_not_follow_symlinked_key_files() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&root).expect("mkdir workspace");
        fs::create_dir_all(&outside).expect("mkdir outside");
        let outside_file = outside.join("Cargo.toml");
        fs::write(&outside_file, "[package]\nname = \"outside\"\n").expect("write outside");
        std::os::unix::fs::symlink(&outside_file, root.join("Cargo.toml")).expect("symlink");

        assert_eq!(summarize_project(&root), "Unknown project type");
        assert!(!project_tree(&root, 1).contains("Cargo.toml"));
    }

    #[test]
    fn summarize_project_sorts_key_files_in_fallback() {
        // When `summarize_project` can't classify a project type it falls
        // back to listing the discovered key files. That joined list must
        // be deterministic so the system prompt that embeds it doesn't
        // drift between runs on filesystems that emit readdir in a
        // non-alphabetical order.
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        // Use key files that don't trigger any of the type detectors
        // (Cargo.toml / package.json / requirements.txt) so the function
        // hits the `Project with key files: …` branch.
        fs::write(root.join("Makefile"), "all:").expect("write makefile");
        fs::write(root.join("README.md"), "# x").expect("write readme");

        let summary = summarize_project(root);
        assert!(
            summary.starts_with("Project with key files: "),
            "expected fallback branch; got: {summary}"
        );
        let suffix = summary
            .strip_prefix("Project with key files: ")
            .expect("prefix");
        assert_eq!(suffix, "Makefile, README.md");
    }

    // ===================================================================
    // open_url tests
    // ===================================================================

    #[test]
    #[ignore = "spawns the real system browser opener; run explicitly"]
    fn open_url_does_not_panic_on_valid_url() {
        // We can't open a browser in CI, but we can verify the function
        // doesn't panic and returns a Result (either Ok or Err with a
        // meaningful message).
        let result = super::open_url("https://example.com");
        match result {
            Ok(()) => {} // browser opened — fine
            Err(e) => {
                let msg = e.to_string();
                // The error must contain something about "browser" or
                // "unsupported" — not a random panic message.
                assert!(
                    msg.contains("browser")
                        || msg.contains("unsupported")
                        || msg.contains("failed"),
                    "unexpected error message: {msg}"
                );
            }
        }
    }

    #[test]
    fn open_url_rejects_empty_url_gracefully() {
        // An empty URL should fail with a clear error, not panic.
        let result = super::open_url("");
        match result {
            Ok(()) => {} // some openers might accept empty string
            Err(e) => {
                let msg = e.to_string();
                assert!(!msg.is_empty(), "error message must not be empty");
            }
        }
    }

    #[test]
    fn open_url_rejects_non_http_schemes_and_malformed_urls() {
        // A URL derived from untrusted content must never reach the system
        // opener unless it is a plain http(s) URL.
        for url in [
            "file:///etc/passwd",
            "ssh://user@host",
            "javascript:alert(1)",
            "https://exa mple.com",
            "https://example.com/\u{0000}",
        ] {
            let err =
                super::open_url(url).expect_err(format!("URL '{url}' must be rejected").as_str());
            assert!(
                format!("{err:#}").contains("refusing"),
                "unexpected error for '{url}': {err:#}"
            );
        }
    }
}
