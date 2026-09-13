//! Thin JSON-RPC over stdio client for LSP servers.
//!
//! We deliberately do **not** depend on `tower-lsp` — it is a server-side
//! framework and dragging it in here would add hundreds of unnecessary
//! transitive dependencies and slow down `cargo build` for every contributor.
//! The LSP wire protocol is small enough that handling it ourselves is a
//! self-contained ~400 LOC and lets us keep total control of the spawn
//! lifecycle, timeouts, and the async surface.
//!
//! Architecture:
//!
//! - [`LspTransport`] is the trait the [`super::LspManager`] talks to. The
//!   real implementation is [`StdioLspTransport`] (forks an LSP server with
//!   `tokio::process::Command`); tests use `super::tests::FakeTransport`.
//! - [`StdioLspTransport`] runs background tokio tasks — a writer, a reader,
//!   an inbound dispatcher, and a stderr drain. Communication uses tokio mpsc
//!   channels plus a shared diagnostics cache.
//! - We parse `Content-Length`-framed JSON-RPC and route inbound messages
//!   either to a per-request response slot (for replies) or to the
//!   diagnostics cache (for `textDocument/publishDiagnostics` notifications).
//!
//! The transport is one-shot per file in MVP form: the manager spawns a
//! transport on demand for a language and reuses it. We do not implement
//! workspace sync beyond didOpen/didChange because the goal is "post-edit
//! diagnostics," not full IDE smartness.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;

use super::diagnostics::{Diagnostic, Severity};
use super::registry::Language;
use crate::utils::spawn_supervised;

/// How long [`StdioLspTransport::spawn`] waits for the server's `initialize`
/// response before falling back to sending `initialized` anyway. Generous
/// enough for slow first starts (indexing servers, cold binaries) while
/// keeping startup bounded — a wedged server cannot hang us forever.
const INIT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Trait the LSP manager talks to. A real LSP server speaks this via stdio;
/// tests use an in-process fake.
#[async_trait]
pub trait LspTransport: Send + Sync {
    /// Notify the server that a file was opened or its contents updated, then
    /// wait up to `wait` for a `publishDiagnostics` notification for that
    /// file. Returns the diagnostics list (possibly empty). Implementations
    /// must NOT block past `wait`.
    async fn diagnostics_for(
        &self,
        path: &Path,
        text: &str,
        wait: Duration,
    ) -> Result<Vec<Diagnostic>>;

    /// Best-effort shutdown. Called via `LspManager::shutdown_all`.
    #[allow(dead_code)]
    async fn shutdown(&self);
}

/// Stdio-backed transport. Spawns the LSP server as a child process and
/// pipes JSON-RPC over stdin/stdout. Stderr is drained by a background task
/// and logged at `debug` — without the drain the OS pipe buffer (~64 KiB)
/// fills once the server writes enough output, and the child deadlocks
/// mid-request.
pub struct StdioLspTransport {
    /// JoinHandle for the running server. Held so the child stays alive for
    /// the transport's lifetime; consumed during `shutdown`.
    #[allow(dead_code)]
    child: AsyncMutex<Option<Child>>,
    /// Outgoing message sender to the writer task.
    tx_outbound: mpsc::Sender<Vec<u8>>,
    /// Latest diagnostics per canonical file path, tagged with the global
    /// publish counter at store time. Maintained by the dispatcher task.
    diagnostics_cache: Arc<DiagnosticsCache>,
    /// Watch on the global publish counter; bumped by the dispatcher on
    /// every `publishDiagnostics` it routes into the cache. Each
    /// `diagnostics_for` call clones its own receiver, so concurrent calls
    /// (different files, same transport) no longer serialize on a shared
    /// receiver mutex.
    diagnostics_version: watch::Receiver<u64>,
    /// Monotonic request id counter. Reserved for future LSP request/reply
    /// methods (workspace symbol queries, etc.).
    #[allow(dead_code)]
    next_id: AsyncMutex<i64>,
    /// Language id passed in `textDocument/didOpen` (e.g. "rust").
    language_id: &'static str,
    /// Track which files we have opened so the second touch sends
    /// `didChange` instead of `didOpen`.
    opened: AsyncMutex<HashMap<PathBuf, i64>>,
}

impl StdioLspTransport {
    /// Spawn `command args…` and run the LSP `initialize` handshake. Returns
    /// `Err` immediately if the binary is not on PATH or the server rejects
    /// `initialize`. If the `initialize` response does not arrive within
    /// [`INIT_RESPONSE_TIMEOUT`], logs a warning and sends `initialized`
    /// anyway as a compatibility fallback (many servers tolerate it).
    pub async fn spawn(
        command: &str,
        args: &[String],
        language: Language,
        workspace: PathBuf,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn LSP server `{command}`"))?;

        let stdin = child
            .stdin
            .take()
            .context("LSP child has no stdin handle")?;
        let stdout = child
            .stdout
            .take()
            .context("LSP child has no stdout handle")?;
        let stderr = child
            .stderr
            .take()
            .context("LSP child has no stderr handle")?;

        let (tx_outbound, rx_outbound) = mpsc::channel::<Vec<u8>>(64);
        let (tx_inbound, rx_inbound) = mpsc::channel::<Value>(64);
        let diagnostics_cache = Arc::new(DiagnosticsCache::default());
        let (version_tx, diagnostics_version) = watch::channel(0u64);

        // Writer task: drain outbound channel, frame with Content-Length, write to stdin.
        spawn_supervised(
            "lsp-writer",
            std::panic::Location::caller(),
            writer_task(stdin, rx_outbound),
        );
        // Reader task: parse Content-Length frames from stdout, push to inbound queue.
        spawn_supervised(
            "lsp-reader",
            std::panic::Location::caller(),
            reader_task(stdout, tx_inbound),
        );
        // Stderr drain: the pipe is never read otherwise, and a full OS pipe
        // buffer (~64 KiB) blocks the server's writes until it deadlocks.
        spawn_supervised(
            "lsp-stderr",
            std::panic::Location::caller(),
            stderr_drain_task(stderr, command.to_string()),
        );
        // Inbound dispatcher: routes notifications into the diagnostics cache
        // (bumping the publish counter) and replies into a pending-request
        // slot keyed by request id.
        let pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(AsyncMutex::new(HashMap::new()));
        spawn_supervised(
            "lsp-dispatcher",
            std::panic::Location::caller(),
            dispatcher_task(rx_inbound, diagnostics_cache.clone(), version_tx, pending.clone()),
        );

        // Register the reply slot for `initialize` (id 1) BEFORE sending so a
        // fast server cannot beat us to the dispatcher.
        let (init_tx, init_rx) = oneshot::channel::<Value>();
        pending.lock().await.insert(1, init_tx);

        // Send `initialize` (we synthesize id=1) and then — per the LSP spec,
        // in this order — `initialized` only after the response arrives.
        let init_payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": uri_from_path(&workspace),
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": { "relatedInformation": false }
                    }
                },
                "workspaceFolders": [{
                    "uri": uri_from_path(&workspace),
                    "name": "workspace"
                }]
            }
        });
        send_message(&tx_outbound, &init_payload).await?;

        // Wait for the `initialize` response (bounded) before queueing
        // `initialized`. On timeout we deliberately fall back to sending it
        // anyway: most servers buffer notifications until they are ready,
        // and dropping the transport entirely would disable diagnostics for
        // a server that is merely slow to answer.
        match timeout(INIT_RESPONSE_TIMEOUT, init_rx).await {
            Ok(Ok(response)) => {
                if let Some(error) = response.get("error") {
                    let message = error
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error");
                    return Err(anyhow!(
                        "LSP server `{command}` rejected initialize: {message}"
                    ));
                }
                tracing::debug!(server = command, "lsp: initialize acknowledged");
            }
            Ok(Err(_)) => {
                // The oneshot sender was dropped: the dispatcher exited
                // (server died / stdout closed) without replying.
                tracing::warn!(
                    server = command,
                    "lsp: dispatcher closed before initialize response; sending initialized anyway"
                );
            }
            Err(_) => {
                // Drop the stale slot so a very late response finds nothing.
                pending.lock().await.remove(&1);
                tracing::warn!(
                    server = command,
                    timeout_secs = INIT_RESPONSE_TIMEOUT.as_secs(),
                    "lsp: initialize response timed out; sending initialized anyway"
                );
            }
        }

        let initialized = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        send_message(&tx_outbound, &initialized).await?;

        Ok(Self {
            child: AsyncMutex::new(Some(child)),
            tx_outbound,
            diagnostics_cache,
            diagnostics_version,
            next_id: AsyncMutex::new(2),
            language_id: language.language_id(),
            opened: AsyncMutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl LspTransport for StdioLspTransport {
    async fn diagnostics_for(
        &self,
        path: &Path,
        text: &str,
        wait: Duration,
    ) -> Result<Vec<Diagnostic>> {
        let path_buf = path.to_path_buf();
        let uri = uri_from_path(&path_buf);
        // Cache keys use the canonical path — the same form `uri_from_path`
        // sends and the server echoes back — so a canonicalized publish
        // matches even when the caller passed a symlinked path.
        let cache_key = canonicalize_best_effort(&path_buf);

        // Either send didOpen (first time) or didChange (subsequent edits).
        let mut opened = self.opened.lock().await;
        let is_new = !opened.contains_key(&path_buf);
        let new_version = opened.get(&path_buf).copied().unwrap_or(0) + 1;
        opened.insert(path_buf.clone(), new_version);
        drop(opened);

        let payload = if is_new {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri.clone(),
                        "languageId": self.language_id,
                        "version": new_version,
                        "text": text
                    }
                }
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": {
                        "uri": uri.clone(),
                        "version": new_version
                    },
                    "contentChanges": [{ "text": text }]
                }
            })
        };
        // Capture the publish counter BEFORE sending so a publish racing
        // with our didOpen/didChange still counts as fresh for this call.
        // The clone carries its own "seen" marker, so `changed()` below only
        // fires for publishes after this point.
        let mut version_rx = self.diagnostics_version.clone();
        let start_version = *version_rx.borrow();
        send_message(&self.tx_outbound, &payload).await?;

        // Wait for the first `publishDiagnostics` for this file that lands
        // after our request (version > `start_version`). Stale cache entries
        // from earlier edits are ignored while waiting, so repeat edits never
        // surface pre-edit diagnostics. Servers typically publish within a
        // few hundred ms; for initial cold-start (rust-analyzer) it can be
        // many seconds — but the manager guards us with a separate timeout.
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some((items, version)) = self.diagnostics_cache.get(&cache_key)
                && version > start_version
            {
                return Ok(items);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;
            // `changed()` resolves on the next publish for ANY file; loop
            // back to re-check this file's cache entry.
            match timeout(remaining, version_rx.changed()).await {
                Ok(Ok(())) => continue,
                // Timed out, or the dispatcher dropped the watch sender.
                Ok(Err(_)) | Err(_) => break,
            }
        }
        // No fresh publish within the window: report "no diagnostics this
        // turn" (same contract as before the cache refactor).
        Ok(Vec::new())
    }

    async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        if let Some(mut c) = child.take() {
            let _ = c.start_kill();
            let _ = c.wait().await;
        }
    }
}

/// Latest-diagnostics cache shared between the dispatcher (sole writer) and
/// `diagnostics_for` callers (readers). Replaces the previous single
/// `mpsc::Receiver` behind an async mutex, which serialized every concurrent
/// `diagnostics_for` call on the same transport — a call waiting out its
/// timeout for file A also blocked the call for file B.
#[derive(Default)]
struct DiagnosticsCache {
    /// Canonical path -> (latest diagnostics, publish counter when stored).
    inner: std::sync::RwLock<HashMap<PathBuf, (Vec<Diagnostic>, u64)>>,
}

impl DiagnosticsCache {
    fn get(&self, path: &Path) -> Option<(Vec<Diagnostic>, u64)> {
        self.inner
            .read()
            .expect("lsp diagnostics cache lock poisoned")
            .get(path)
            .cloned()
    }

    fn store(&self, path: PathBuf, items: Vec<Diagnostic>, version: u64) {
        self.inner
            .write()
            .expect("lsp diagnostics cache lock poisoned")
            .insert(path, (items, version));
    }
}

/// Send a JSON value as one Content-Length-framed JSON-RPC message.
async fn send_message(tx: &mpsc::Sender<Vec<u8>>, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value).context("serialize LSP message")?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(&body);
    tx.send(frame)
        .await
        .map_err(|_| anyhow!("LSP outbound channel closed"))?;
    Ok(())
}

/// Background task that drains the outbound queue and writes each frame to
/// the LSP server's stdin. Exits cleanly when the channel closes.
async fn writer_task(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(frame) = rx.recv().await {
        if stdin.write_all(&frame).await.is_err() {
            break;
        }
        if stdin.flush().await.is_err() {
            break;
        }
    }
}

/// Background task that drains the LSP server's stderr line by line and
/// forwards it to the log. Without it the OS pipe buffer (~64 KiB) fills and
/// the server blocks on its next stderr write — a guaranteed deadlock for
/// chatty servers. Exits on EOF (server exited).
async fn stderr_drain_task(stderr: tokio::process::ChildStderr, server: String) {
    let mut lines = BufReader::new(stderr).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::debug!(server = %server, "lsp stderr: {line}");
            }
            Ok(None) => return, // EOF
            Err(err) => {
                tracing::debug!(server = %server, ?err, "lsp stderr read failed");
                return;
            }
        }
    }
}

/// Background task that parses `Content-Length`-framed JSON-RPC frames from
/// the LSP server's stdout. Pushes each parsed JSON value to `tx`. Exits
/// when stdout closes or a frame is malformed (we choose to fail closed
/// rather than risk hanging).
async fn reader_task(mut stdout: tokio::process::ChildStdout, tx: mpsc::Sender<Value>) {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 4096];
    loop {
        let n = match stdout.read(&mut tmp).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        buf.extend_from_slice(&tmp[..n]);
        // Try to parse as many frames as we can from the accumulated buffer.
        while let Some((header_end, content_length)) = parse_header(&buf) {
            if buf.len() < header_end + content_length {
                break; // need more bytes
            }
            let body = &buf[header_end..header_end + content_length];
            let parsed = serde_json::from_slice::<Value>(body).ok();
            // Drop the consumed bytes regardless of parse result so a bad frame
            // does not stall the loop.
            buf.drain(..header_end + content_length);
            if let Some(value) = parsed
                && tx.send(value).await.is_err()
            {
                return;
            }
        }
    }
}

/// Parse a JSON-RPC header block. Returns `Some((header_end, content_length))`
/// where `header_end` is the byte offset of the first body byte. The header
/// terminator is `\r\n\r\n`. We require a `Content-Length` header.
fn parse_header(buf: &[u8]) -> Option<(usize, usize)> {
    let term = b"\r\n\r\n";
    let pos = buf.windows(term.len()).position(|window| window == term)?;
    let header = std::str::from_utf8(&buf[..pos]).ok()?;
    let mut content_length: Option<usize> = None;
    for line in header.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse::<usize>().ok();
        }
    }
    content_length.map(|cl| (pos + term.len(), cl))
}

/// Background task that consumes inbound JSON values, classifies them as
/// notifications/responses, and routes accordingly: diagnostics
/// notifications go into the shared cache (bumping the publish counter),
/// responses complete the matching pending-request slot.
async fn dispatcher_task(
    mut rx: mpsc::Receiver<Value>,
    cache: Arc<DiagnosticsCache>,
    version_tx: watch::Sender<u64>,
    pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>>,
) {
    let mut version: u64 = 0;
    while let Some(value) = rx.recv().await {
        // Notifications have a `method` and no `id`.
        let method = value.get("method").and_then(|v| v.as_str());
        if method == Some("textDocument/publishDiagnostics") {
            if let Some((path, diags)) = parse_publish_diagnostics(&value) {
                version += 1;
                cache.store(path, diags, version);
                // `send` fails only once every receiver is gone (transport
                // dropped) — nothing useful to do then.
                let _ = version_tx.send(version);
            }
            continue;
        }
        // Replies have an `id` and a `result` or `error`.
        if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
            let mut map = pending.lock().await;
            if let Some(slot) = map.remove(&id) {
                let _ = slot.send(value);
            }
        }
    }
}

/// Decode a `textDocument/publishDiagnostics` notification. A malformed
/// `diagnostics` ENTRY only drops that entry (logged); the rest of the
/// notification is still returned. Returns `None` only when the envelope
/// itself (params/uri/diagnostics array) is unusable.
fn parse_publish_diagnostics(value: &Value) -> Option<(PathBuf, Vec<Diagnostic>)> {
    let params = value.get("params")?;
    let uri = params.get("uri")?.as_str()?;
    let path = path_from_uri(uri)?;
    let raw = params.get("diagnostics")?.as_array()?;
    let mut out = Vec::with_capacity(raw.len());
    for d in raw {
        let (line, column) = match (
            d.get("range")
                .and_then(|range| range.get("start"))
                .and_then(|start| start.get("line"))
                .and_then(|v| v.as_u64()),
            d.get("range")
                .and_then(|range| range.get("start"))
                .and_then(|start| start.get("character"))
                .and_then(|v| v.as_u64()),
        ) {
            (Some(line), Some(column)) => (line as u32 + 1, column as u32 + 1),
            _ => {
                tracing::debug!(uri, entry = ?d, "lsp: skipping malformed diagnostic entry");
                continue;
            }
        };
        let severity = Severity::from_lsp(d.get("severity").and_then(|v| v.as_i64()))
            .unwrap_or(Severity::Error);
        let message = d
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(Diagnostic {
            line,
            column,
            severity,
            message,
        });
    }
    Some((path, out))
}

/// Canonicalize `path` for use as a URI source / cache key, falling back to
/// the input when the file does not exist yet (or the filesystem is odd).
/// The server echoes back the canonical URI we send, so cache lookups must
/// use the same canonical form.
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Convert a filesystem path to a `file://` URI with the path component
/// percent-encoded per RFC 3986: `/` and the unreserved characters
/// (ALPHA / DIGIT / `-._~`) stay literal, everything else (spaces, `#`,
/// `?`, `%`, non-ASCII, …) is encoded as UTF-8 `%XX` with uppercase hex.
/// Raw paths break diagnostics matching because `#`/`?` truncate the URI
/// and spaces are rejected by stricter servers. Best-effort — we do not
/// support Windows drive letters perfectly.
fn uri_from_path(path: &Path) -> String {
    let canonical = canonicalize_best_effort(path);
    let encoded = percent_encode_path(&canonical.to_string_lossy());
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{}", encoded.trim_start_matches('/'))
    }
}

/// Percent-encode a URI path component (see [`uri_from_path`]).
fn percent_encode_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(path.len());
    for &byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Inverse of [`percent_encode_path`]. Invalid escapes (a `%` not followed
/// by two hex digits) are kept literal as a best effort.
fn percent_decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = if bytes[i] == b'%' && i + 2 < bytes.len() {
            match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(hi), Some(lo)) => Some(hi * 16 + lo),
                _ => None,
            }
        } else {
            None
        };
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Value of a single hex digit, or `None` if not hex.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Inverse of [`uri_from_path`]. Returns `None` when the URI is not a
/// `file://` URI; percent-escapes in the path are decoded.
fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode_path(stripped)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsp_header() {
        let frame = b"Content-Length: 5\r\n\r\nhello";
        let (end, len) = parse_header(frame).expect("header parses");
        assert_eq!(end, 21);
        assert_eq!(len, 5);
    }

    #[test]
    fn parse_header_returns_none_when_truncated() {
        let frame = b"Content-Length: 5\r\nMissingTerm";
        assert!(parse_header(frame).is_none());
    }

    #[test]
    fn parses_publish_diagnostics_payload() {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/foo.rs",
                "diagnostics": [
                    {
                        "range": {
                            "start": { "line": 11, "character": 7 },
                            "end":   { "line": 11, "character": 8 }
                        },
                        "severity": 1,
                        "message": "missing semicolon"
                    }
                ]
            }
        });
        let (path, diags) = parse_publish_diagnostics(&payload).expect("parses");
        assert_eq!(path, PathBuf::from("/tmp/foo.rs"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 12);
        assert_eq!(diags[0].column, 8);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].message, "missing semicolon");
    }

    #[test]
    fn malformed_diagnostic_entries_are_skipped_not_fatal() {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/foo.rs",
                "diagnostics": [
                    // No range at all.
                    { "severity": 1, "message": "rangeless" },
                    // Range present but `character` missing.
                    {
                        "range": { "start": { "line": 4 } },
                        "severity": 1,
                        "message": "half a start"
                    },
                    // Fully valid entry.
                    {
                        "range": {
                            "start": { "line": 2, "character": 3 },
                            "end":   { "line": 2, "character": 4 }
                        },
                        "severity": 1,
                        "message": "valid"
                    }
                ]
            }
        });
        let (path, diags) = parse_publish_diagnostics(&payload).expect("parses");
        assert_eq!(path, PathBuf::from("/tmp/foo.rs"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "valid");
        assert_eq!(diags[0].line, 3);
        assert_eq!(diags[0].column, 4);
    }

    #[test]
    fn percent_encoding_keeps_unreserved_chars_and_slash() {
        assert_eq!(percent_encode_path("/aB0-._~/z.rs"), "/aB0-._~/z.rs");
    }

    #[test]
    fn percent_encoding_escapes_space_hash_question_percent() {
        assert_eq!(percent_encode_path("a b.rs"), "a%20b.rs");
        assert_eq!(percent_encode_path("a#b?c"), "a%23b%3Fc");
        assert_eq!(percent_encode_path("100%"), "100%25");
    }

    #[test]
    fn percent_encoding_escapes_utf8_bytes_with_uppercase_hex() {
        // 项目 = E9 A1 B9 E7 9B AE
        assert_eq!(percent_encode_path("项目"), "%E9%A1%B9%E7%9B%AE");
    }

    #[test]
    fn percent_decoding_inverts_encoding_for_tricky_paths() {
        for s in [
            "/tmp/my file.rs",
            "/tmp/a#b.rs",
            "/tmp/100%.rs",
            "/tmp/what?x=1.rs",
            "/tmp/项目/foo.rs",
            "/plain/path.rs",
        ] {
            assert_eq!(percent_decode_path(&percent_encode_path(s)), s, "case {s}");
        }
    }

    #[test]
    fn percent_decoding_tolerates_invalid_escapes() {
        assert_eq!(percent_decode_path("100%"), "100%");
        assert_eq!(percent_decode_path("%zz"), "%zz");
        assert_eq!(percent_decode_path("%e4%b8"), "\u{fffd}"); // truncated UTF-8
    }

    #[test]
    fn path_from_uri_decodes_percent_escapes() {
        assert_eq!(
            path_from_uri("file:///tmp/my%20file.rs"),
            Some(PathBuf::from("/tmp/my file.rs"))
        );
        assert_eq!(
            path_from_uri("file:///tmp/%E9%A1%B9%E7%9B%AE/foo.rs"),
            Some(PathBuf::from("/tmp/项目/foo.rs"))
        );
    }

    #[test]
    fn path_from_uri_rejects_non_file_scheme() {
        assert!(path_from_uri("http://example.com/foo.rs").is_none());
    }

    #[test]
    fn uri_from_path_percent_encodes_and_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("my file #1%.rs");
        std::fs::write(&path, b"fn main() {}").expect("write file");
        let canonical = path.canonicalize().expect("canonicalize");
        let uri = uri_from_path(&path);
        // Exact wire form: only unreserved chars and `/` survive literal.
        assert_eq!(
            uri,
            format!("file://{}", percent_encode_path(&canonical.to_string_lossy()))
        );
        assert!(uri.contains("%20"), "space encoded in {uri}");
        assert!(uri.contains("%23"), "hash encoded in {uri}");
        assert!(uri.contains("%25"), "percent encoded in {uri}");
        assert_eq!(path_from_uri(&uri), Some(canonical));
    }

    #[tokio::test]
    async fn dispatcher_completes_pending_request_by_id() {
        let (tx_inbound, rx_inbound) = mpsc::channel::<Value>(8);
        let cache = Arc::new(DiagnosticsCache::default());
        let (version_tx, _version_rx) = watch::channel(0u64);
        let pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(AsyncMutex::new(HashMap::new()));

        let task = tokio::spawn(dispatcher_task(
            rx_inbound,
            cache,
            version_tx,
            pending.clone(),
        ));

        // Register a slot for request id 7 (as `spawn` does for id 1).
        let (slot_tx, slot_rx) = oneshot::channel::<Value>();
        pending.lock().await.insert(7, slot_tx);

        tx_inbound
            .send(json!({"jsonrpc": "2.0", "id": 7, "result": {"capabilities": {}}}))
            .await
            .expect("send inbound");
        let reply = timeout(Duration::from_secs(2), slot_rx)
            .await
            .expect("slot completed")
            .expect("slot not dropped");
        assert_eq!(reply["id"], json!(7));
        assert!(reply.get("result").is_some());

        drop(tx_inbound);
        let _ = task.await;
    }

    #[tokio::test]
    async fn dispatcher_caches_publishes_and_bumps_version() {
        let (tx_inbound, rx_inbound) = mpsc::channel::<Value>(8);
        let cache = Arc::new(DiagnosticsCache::default());
        let (version_tx, mut version_rx) = watch::channel(0u64);
        let pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(AsyncMutex::new(HashMap::new()));

        let task = tokio::spawn(dispatcher_task(rx_inbound, cache.clone(), version_tx, pending));

        let notification = |message: &str| {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {
                    "uri": "file:///tmp/my%20file.rs",
                    "diagnostics": [
                        {
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end":   { "line": 0, "character": 1 }
                            },
                            "severity": 1,
                            "message": message
                        }
                    ]
                }
            })
        };

        tx_inbound.send(notification("first")).await.expect("send 1");
        timeout(Duration::from_secs(2), version_rx.changed())
            .await
            .expect("version bumped after first publish")
            .expect("watch sender alive");
        let (items, version) = cache
            .get(&PathBuf::from("/tmp/my file.rs"))
            .expect("cached after first publish");
        assert_eq!(version, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].message, "first");

        // Second publish replaces the entry and bumps the version tag.
        tx_inbound.send(notification("second")).await.expect("send 2");
        timeout(Duration::from_secs(2), version_rx.changed())
            .await
            .expect("version bumped after second publish")
            .expect("watch sender alive");
        let (items, version) = cache
            .get(&PathBuf::from("/tmp/my file.rs"))
            .expect("cached after second publish");
        assert_eq!(version, 2);
        assert_eq!(items[0].message, "second");

        drop(tx_inbound);
        let _ = task.await;
    }

    /// End-to-end smoke test against a real rust-analyzer: exercises the
    /// initialize -> response -> initialized ordering and the cache-based
    /// `diagnostics_for` path. Ignored by default because it needs
    /// `rust-analyzer` on PATH and several seconds of startup; run manually
    /// with `cargo test -p codesmith-tui --bin codesmith-tui lsp -- --ignored`.
    #[tokio::test]
    #[ignore = "requires rust-analyzer on PATH; run with -- --ignored"]
    async fn real_rust_analyzer_handshake_and_diagnostics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("demo");
        std::fs::create_dir_all(project.join("src")).expect("create src");
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write Cargo.toml");
        const SOURCE: &str = "fn main() { let x: i32 = \"oops\"; }\n";
        let main_rs = project.join("src/main.rs");
        std::fs::write(&main_rs, SOURCE).expect("write main.rs");

        // `spawn` only succeeds once the initialize/initialized handshake
        // completes (bounded by INIT_RESPONSE_TIMEOUT).
        let transport =
            StdioLspTransport::spawn("rust-analyzer", &[], Language::Rust, project.clone())
                .await
                .expect("rust-analyzer spawned and initialized");

        // rust-analyzer may publish an empty batch before analysis finishes;
        // retry within a bounded window until real diagnostics land.
        let mut diags = Vec::new();
        for _ in 0..10 {
            diags = transport
                .diagnostics_for(&main_rs, SOURCE, Duration::from_secs(15))
                .await
                .expect("diagnostics_for");
            if !diags.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        transport.shutdown().await;

        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("i32") || d.message.to_lowercase().contains("expected")),
            "expected a type-mismatch diagnostic, got {diags:?}"
        );
    }
}
