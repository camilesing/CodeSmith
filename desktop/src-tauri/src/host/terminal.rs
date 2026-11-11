//! The sidebar terminal tabs (`client-ui-sidebar-terminal` over the
//! `terminal/*` remote): session-owned interactive shells the bridge itself
//! spawns on a PTY. They are the user's terminals, not the model's — the
//! config's `allow_shell` gates the model's shell tool and does not apply
//! here.
//!
//! The wire contract is upstream's `terminal-controller` (the shapes are
//! pinned by its generated `typert.remote-client`): `environment`, `shells`,
//! `list`, `create`, `rename`, `close` unary; `retain` (one `retained` frame,
//! then the stream parks) and `attach` duplex — input rides the mux uplink
//! (`write`/`resize` commands numbered consecutively from 1 per attachment),
//! output starts with a whole-screen `snapshot` (a vt100 emulator mirroring
//! the PTY, serialized as terminal escape codes the way upstream's headless
//! xterm SerializeAddon does) and continues as `output` frames with
//! contiguous sequences, plus `state` frames and per-command `ack` frames
//! (`refusal: read-only` for a non-controller attachment,
//! `not-running` after exit). The newest attachment holds exclusive input
//! control; older ones keep receiving output. Per-follower output queues are
//! byte-capped and a slow consumer's stream fails — reconnecting re-snapshots
//! and recovers, upstream's own recovery model.
//!
//! Known limitations: terminals live and die with the bridge process (no
//! reattach across a desktop restart, like upstream's host process); there is
//! no unattended idle reclamation (upstream's 2h timeout), so an abandoned
//! terminal runs until its shell exits or the bridge does; the reconnect
//! snapshot carries the visible screen with full formatting and the
//! scrolled-off history as plain text (vt100's viewport cannot walk its
//! scrollback, so the host keeps its own stripped-text ring — no colors past
//! the visible screen, and the ring may duplicate lines still visible);
//! close signals the shell's process group with TERM then KILL but a
//! grandchild that holds the PTY open only loses its output stream, not its
//! life; `shellPath` accepts any existing executable path, not only
//! discovered shells (this host has no sandboxed subprocess provider to
//! restrict — the page-origin gate is the boundary).
//! `attach` carries no `inputWindow`: the mux uplink inbox is frame-counted,
//! not byte-counted, and the wire types omit the field for such carriers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{
    Child, ChildKiller, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};
use vt100::Parser;

use super::files;
use super::mux::UplinkFrame;
use super::open_in_app::is_secret_env;
use super::{rpc_error, rpc_ok, stream_failure, HostState};

/// Upstream's deployment defaults (`TerminalController.Config`), mirrored.
const MAX_TERMINALS: usize = 8;
const MAX_COLS: u16 = 500;
const MAX_ROWS: u16 = 200;
const SCROLLBACK_ROWS: usize = 1_000;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_BUFFERED_BYTES: usize = 2 * 1024 * 1024;
const CLOSE_GRACE: Duration = Duration::from_millis(1_000);
/// How long `close` waits for the reader thread to drain after the kill
/// before it finishes the followers regardless (a stray grandchild can hold
/// the PTY open past the shell's own death).
const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_millis(2_000);
/// Shell candidates the new-terminal selector probes, upstream's list.
const SHELL_CANDIDATES: [&str; 6] = ["zsh", "bash", "fish", "pwsh", "powershell", "cmd"];

/// One session's terminals: identities are scoped to the session and closed
/// identities never come back (upstream's `closedIds`).
#[derive(Default)]
struct Owner {
    terminals: HashMap<String, Arc<Terminal>>,
    closed: HashSet<String>,
}

/// The bridge's terminal registry: in-memory only, so terminals die with the
/// process (see the module limitations).
#[derive(Default)]
pub struct Registry {
    owners: Mutex<HashMap<String, Owner>>,
}

/// The mutable half of one terminal: the mirrored screen, the output
/// sequence, and the detachable followers.
struct Shared {
    parser: Parser,
    /// The plain-text line history feeding the snapshot above the visible
    /// screen (vt100's viewport cannot walk its scrollback beyond one
    /// screen, so the host keeps its own ring).
    history: TextRing,
    sequence: u64,
    followers: Vec<Follower>,
    controller: Option<u64>,
    next_follower: u64,
    closing: bool,
    info: Info,
}

/// A plain-text line ring over the decoded output stream: escape sequences
/// stripped, `\n` terminating a line, backspace editing the tail. Full-screen
/// applications address the real PTY size, so their cursor moves leave
/// approximations here — the visible screen below the history stays the
/// authoritative rendering.
struct TextRing {
    lines: VecDeque<String>,
    current: String,
    state: StripState,
}

#[derive(Clone, Copy, PartialEq)]
enum StripState {
    Ground,
    Esc,
    Csi,
    /// String sequences (OSC/DCS/PM/APC) ending at BEL or ST.
    StringSeq,
}

impl TextRing {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            current: String::new(),
            state: StripState::Ground,
        }
    }

    fn feed(&mut self, text: &str) {
        for ch in text.chars() {
            match self.state {
                StripState::Ground => match ch {
                    '\x1b' => self.state = StripState::Esc,
                    '\n' => {
                        self.lines.push_back(std::mem::take(&mut self.current));
                        while self.lines.len() > SCROLLBACK_ROWS {
                            self.lines.pop_front();
                        }
                    }
                    // A carriage return restarts the line visually; without
                    // column tracking the approximation keeps the text (the
                    // common case — shells echo the line, then terminate it).
                    '\r' => {}
                    '\x08' => {
                        self.current.pop();
                    }
                    ch if (ch as u32) < 0x20 => {}
                    ch => self.current.push(ch),
                },
                StripState::Esc => match ch {
                    '[' => self.state = StripState::Csi,
                    ']' | 'P' | 'X' | '^' | '_' => self.state = StripState::StringSeq,
                    // Two-byte escapes (M, 7, 8, charset selection, …) end here.
                    _ => self.state = StripState::Ground,
                },
                StripState::Csi => {
                    if ('@'..='~').contains(&ch) {
                        self.state = StripState::Ground;
                    }
                }
                StripState::StringSeq => match ch {
                    '\x07' => self.state = StripState::Ground,
                    '\x1b' => self.state = StripState::Esc,
                    _ => {}
                },
            }
        }
    }
}

/// The terminal's client-visible metadata (`WebTerminalInfo`, minus the
/// immutable identity fields the `Terminal` itself carries).
struct Info {
    title: String,
    cols: u16,
    rows: u16,
    state: &'static str,
    exit_code: Option<i32>,
    error: Option<String>,
}

/// One attach stream's bounded output queue — upstream's `TerminalFollower`.
struct Follower {
    id: u64,
    queue: VecDeque<Value>,
    bytes: usize,
    wake: Arc<Notify>,
    closed: bool,
    finished: bool,
    failure: Option<String>,
}

impl Follower {
    fn new(id: u64) -> Self {
        Self {
            id,
            queue: VecDeque::new(),
            bytes: 0,
            wake: Arc::new(Notify::new()),
            closed: false,
            finished: false,
            failure: None,
        }
    }

    fn active(&self) -> bool {
        !self.closed && !self.finished
    }

    /// Queue one frame, or fail this follower when its byte cap is exceeded
    /// (upstream: a slow consumer fails; reconnecting recovers the screen).
    fn push(&mut self, frame: Value) {
        if !self.active() {
            return;
        }
        let bytes = frame.to_string().len();
        if self.bytes + bytes > MAX_BUFFERED_BYTES {
            self.failure = Some(
                "Terminal output consumer exceeded its buffer; reconnect to recover the current screen"
                    .to_string(),
            );
            self.closed = true;
            self.queue.clear();
            self.bytes = 0;
            self.wake.notify_one();
            return;
        }
        self.bytes += bytes;
        self.queue.push_back(frame);
        self.wake.notify_one();
    }

    /// End the stream after every queued frame is delivered.
    fn finish(&mut self) {
        self.finished = true;
        self.wake.notify_one();
    }
}

enum Fetched {
    Frames(Vec<Value>),
    Failure(String),
    Finished,
    Missing,
}

/// A command the writer thread applies to the PTY in order.
enum WriterCommand {
    Write(String),
    Resize { rows: u16, cols: u16 },
}

/// One live terminal: identity, the PTY plumbing threads, and the shared
/// screen state. The reader thread owns the `Child` (it publishes the exit
/// state); the writer thread owns the master (it applies resizes).
struct Terminal {
    shell: Value,
    cwd: String,
    shared: Mutex<Shared>,
    writer_tx: std_mpsc::Sender<WriterCommand>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    #[cfg(unix)]
    process_group: Option<i32>,
    /// Signals once the reader thread has drained the PTY and published the
    /// exit state (close waits on it).
    drained: Notify,
}

impl Terminal {
    /// The client-visible `WebTerminalInfo` for this terminal.
    fn info_json(&self, id: &str, shared: &Shared) -> Value {
        json!({
            "id": id,
            "title": shared.info.title,
            "shell": self.shell,
            "cwd": self.cwd,
            "cols": shared.info.cols,
            "rows": shared.info.rows,
            "state": shared.info.state,
            "exitCode": shared.info.exit_code,
            "error": shared
                .info
                .error
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        })
    }

    fn writable(shared: &Shared, follower: u64) -> bool {
        !shared.closing && shared.info.state == "running" && shared.controller == Some(follower)
    }

    /// Push a `state` frame to every follower (its `writable` is per
    /// follower — only the controller writes).
    fn broadcast_state(&self, id: &str, shared: &mut Shared) {
        let info = self.info_json(id, shared);
        let writables: Vec<bool> = shared
            .followers
            .iter()
            .map(|f| Self::writable(shared, f.id))
            .collect();
        for (follower, writable) in shared.followers.iter_mut().zip(writables) {
            follower.push(json!({ "type": "state", "info": info, "writable": writable }));
        }
    }

    /// Feed one decoded output chunk to the mirrored screen and broadcast it
    /// (the reader thread's hot path; sequences stay contiguous).
    fn emit_output(&self, data: &str) {
        if data.is_empty() {
            return;
        }
        let mut shared = self.shared.lock().unwrap();
        shared.parser.process(data.as_bytes());
        shared.history.feed(data);
        shared.sequence += 1;
        let frame = json!({ "type": "output", "sequence": shared.sequence, "data": data });
        for follower in &mut shared.followers {
            follower.push(frame.clone());
        }
    }

    /// Publish the process outcome: `exited` with its code, or `failed` with
    /// the error. The final state frame reaches every follower; the attach
    /// streams stay open (upstream keeps them — the client shows the exit and
    /// decides), only `close` finishes them.
    fn publish_exit(&self, id: &str, outcome: std::io::Result<i32>) {
        let mut shared = self.shared.lock().unwrap();
        match outcome {
            Ok(code) => {
                shared.info.state = "exited";
                shared.info.exit_code = Some(code);
            }
            Err(err) => {
                shared.info.state = "failed";
                shared.info.error = Some(err.to_string());
            }
        }
        self.broadcast_state(id, &mut shared);
    }

    /// The reconnect snapshot: the plain-text line history, then the visible
    /// screen with formatting (vt100 emits replayable escape codes — the
    /// same role upstream's headless xterm serializer plays). The history
    /// may duplicate lines still on the visible screen; the screen below it
    /// is the authoritative rendering.
    fn snapshot(shared: &mut Shared) -> String {
        let mut out = String::new();
        for line in &shared.history.lines {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str(&String::from_utf8_lossy(
            &shared.parser.screen().contents_formatted(),
        ));
        out
    }

    /// Take every queued frame for one follower. `Failure` drops the queue
    /// (a failed follower already cleared it), `Finished` once the queue
    /// drained — a closed follower with nothing left ends its stream the
    /// same way (upstream's `read` loop breaks on `closed`).
    fn take_frames(&self, follower: u64) -> Fetched {
        let mut shared = self.shared.lock().unwrap();
        let Some(f) = shared.followers.iter_mut().find(|f| f.id == follower) else {
            return Fetched::Missing;
        };
        if let Some(failure) = f.failure.take() {
            return Fetched::Failure(failure);
        }
        let frames: Vec<Value> = f.queue.drain(..).collect();
        f.bytes = 0;
        if frames.is_empty() && (f.finished || f.closed) {
            return Fetched::Finished;
        }
        Fetched::Frames(frames)
    }

    /// Fail one follower's stream (an input-sequence gap or an oversized
    /// command — upstream fails the attachment without stopping the shell).
    fn fail_follower(&self, follower: u64, message: &str) {
        let mut shared = self.shared.lock().unwrap();
        if let Some(f) = shared.followers.iter_mut().find(|f| f.id == follower) {
            f.failure = Some(message.to_string());
            f.closed = true;
            f.queue.clear();
            f.bytes = 0;
            f.wake.notify_one();
        }
    }

    /// End every follower's stream after its queued frames drain (the
    /// `close` path).
    fn finish_followers(&self) {
        let mut shared = self.shared.lock().unwrap();
        for follower in &mut shared.followers {
            follower.finish();
        }
    }

    /// Whether the shell process has left the running state.
    fn exited(&self) -> bool {
        self.shared.lock().unwrap().info.state != "running"
    }

    /// Apply one uplink input command on behalf of `follower`: the
    /// controller writes and resizes; anyone else (or anything, once the
    /// shell is gone) gets the matching refusal. `Ok` never fails the stream
    /// — bad sequence numbers and oversized writes do (the `Err` message).
    fn apply_input(
        &self,
        id: &str,
        follower: u64,
        sequence: &mut u64,
        value: &Value,
    ) -> Result<(), String> {
        let given = value.get("sequence").and_then(Value::as_u64);
        if given != Some(*sequence + 1) {
            return Err("Terminal input sequence has a gap".to_string());
        }
        match value.get("type").and_then(Value::as_str) {
            Some("write") => {
                let Some(data) = value.get("data").and_then(Value::as_str) else {
                    return Err("Terminal write carries no data".to_string());
                };
                if data.len() > MAX_INPUT_BYTES {
                    return Err("Terminal input exceeds the configured limit".to_string());
                }
                *sequence += 1;
                let mut shared = self.shared.lock().unwrap();
                if shared.closing || shared.info.state != "running" {
                    self.ack(&mut shared, follower, *sequence, Some("not-running"));
                } else if shared.controller != Some(follower) {
                    self.ack(&mut shared, follower, *sequence, Some("read-only"));
                } else {
                    let _ = self.writer_tx.send(WriterCommand::Write(data.to_string()));
                    self.ack(&mut shared, follower, *sequence, None);
                }
                Ok(())
            }
            Some("resize") => {
                let cols = value.get("cols").and_then(Value::as_u64);
                let rows = value.get("rows").and_then(Value::as_u64);
                let (Some(cols), Some(rows)) = (cols, rows) else {
                    return Err("Terminal resize carries no dimensions".to_string());
                };
                if !(2..=MAX_COLS as u64).contains(&cols) || !(1..=MAX_ROWS as u64).contains(&rows)
                {
                    return Err("Terminal dimensions exceed the configured limits".to_string());
                }
                *sequence += 1;
                let (cols, rows) = (cols as u16, rows as u16);
                let mut shared = self.shared.lock().unwrap();
                if shared.closing || shared.info.state != "running" {
                    self.ack(&mut shared, follower, *sequence, Some("not-running"));
                } else if shared.controller != Some(follower) {
                    self.ack(&mut shared, follower, *sequence, Some("read-only"));
                } else {
                    let _ = self.writer_tx.send(WriterCommand::Resize { rows, cols });
                    shared.parser.set_size(rows, cols);
                    shared.info.cols = cols;
                    shared.info.rows = rows;
                    // The state frame precedes the acknowledgement, upstream's
                    // enqueue order (resize, then broadcastState, then ack).
                    self.broadcast_state(id, &mut shared);
                    self.ack(&mut shared, follower, *sequence, None);
                }
                Ok(())
            }
            _ => Err("Unknown terminal input command".to_string()),
        }
    }

    /// Acknowledge one input command, with the refusal when it was not
    /// applied. The sequence is the command's own number (the caller's
    /// per-attachment counter, which it just bumped).
    fn ack(&self, shared: &mut Shared, follower: u64, sequence: u64, refusal: Option<&str>) {
        if let Some(f) = shared.followers.iter_mut().find(|f| f.id == follower) {
            let mut frame = json!({ "type": "ack", "sequence": sequence });
            if let Some(refusal) = refusal {
                frame["refusal"] = json!(refusal);
            }
            f.push(frame);
        }
    }

    /// Detach one follower: remove it, and if it held input control, hand
    /// the remaining attachments a read-only state frame (a new attachment
    /// takes control back).
    fn detach(&self, id: &str, follower: u64) {
        let mut shared = self.shared.lock().unwrap();
        let was_controller = shared.controller == Some(follower);
        shared.controller = shared.controller.filter(|current| *current != follower);
        shared.followers.retain(|f| f.id != follower);
        if was_controller {
            self.broadcast_state(id, &mut shared);
        }
    }
}

/// Split the longest valid UTF-8 prefix of a PTY chunk — multibyte sequences
/// can split across reads (upstream decodes with a streaming TextDecoder).
fn split_utf8_prefix(buf: &[u8]) -> (&str, &[u8]) {
    match std::str::from_utf8(buf) {
        Ok(text) => (text, &[]),
        Err(err) => {
            let valid = err.valid_up_to();
            let text = std::str::from_utf8(&buf[..valid]).unwrap_or("");
            (text, &buf[valid..])
        }
    }
}

/// The reader thread: pumps the PTY into the mirrored screen until EOF, then
/// waits on the child (it owns the `Child`) and publishes the exit state.
/// A read error (EIO once the child dies) is the EOF.
fn reader_loop(
    id: String,
    terminal: Arc<Terminal>,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
) {
    std::thread::spawn(move || {
        let mut carry: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    carry.extend_from_slice(&buf[..read]);
                    let (text, rest) = split_utf8_prefix(&carry);
                    terminal.emit_output(text);
                    carry = rest.to_vec();
                }
            }
        }
        let tail = String::from_utf8_lossy(&carry).into_owned();
        terminal.emit_output(&tail);
        let outcome = child.wait().map(|status| status.exit_code() as i32);
        terminal.publish_exit(&id, outcome);
        terminal.drained.notify_one();
    });
}

/// The writer thread: owns the PTY master (resize needs it) and applies the
/// queued writes and resizes in order; a write error (EIO once the child
/// dies) is ignored — the reader thread owns the exit.
fn writer_loop(
    mut writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn MasterPty + Send>,
    receiver: std_mpsc::Receiver<WriterCommand>,
) {
    std::thread::spawn(move || {
        for command in receiver {
            match command {
                WriterCommand::Write(data) => {
                    let _ = writer.write_all(data.as_bytes());
                    let _ = writer.flush();
                }
                WriterCommand::Resize { rows, cols } => {
                    let _ = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
        }
    });
}

/// One discovered shell (`TerminalShell`): a profile for the new-terminal
/// selector, with the interactive flags upstream's `profile()` derives.
#[derive(Debug)]
struct ShellProfile {
    path: String,
    name: String,
    args: Vec<String>,
}

impl ShellProfile {
    fn json(&self) -> Value {
        json!({ "path": self.path, "args": self.args, "name": self.name })
    }
}

fn executable_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

fn shell_kind(path: &str) -> String {
    let name = executable_name(path).to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn shell_profile(path: &str) -> ShellProfile {
    let kind = shell_kind(path);
    let args = match kind.as_str() {
        "cmd" => Vec::new(),
        "pwsh" | "powershell" => vec!["-NoLogo".to_string()],
        _ => vec!["-i".to_string()],
    };
    ShellProfile {
        path: path.to_string(),
        name: executable_name(path),
        args,
    }
}

/// The environment's default shell: `$SHELL`, else the user's login shell's
/// conventional home, else the platform fallback (upstream's
/// `terminalEnvironment()` default).
fn default_shell() -> ShellProfile {
    #[cfg(unix)]
    let fallback = "/bin/sh";
    #[cfg(windows)]
    let fallback = "cmd.exe";
    let path = std::env::var("SHELL").ok().filter(|s| !s.is_empty());
    let path = match path {
        Some(path) => path,
        None => fallback.to_string(),
    };
    shell_profile(&path)
}

/// Discovered shells for the selector: the default first, then the
/// candidates that resolve, deduplicated by kind (upstream's
/// `discoverShells`).
fn discover_shells() -> Vec<ShellProfile> {
    let mut shells: Vec<ShellProfile> = vec![default_shell()];
    for candidate in SHELL_CANDIDATES {
        let Ok(path) = which::which(candidate) else {
            continue;
        };
        let profile = shell_profile(&path.to_string_lossy());
        if shells
            .iter()
            .any(|s| shell_kind(&s.path) == shell_kind(&profile.path))
        {
            continue;
        }
        shells.push(profile);
    }
    shells
}

/// A terminal failure with its client-visible error code.
struct TermError {
    code: &'static str,
    message: String,
}

impl TermError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new("terminal/unavailable", message)
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("gateway/arguments-invalid", message)
    }
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `terminal/environment`: the session's directory and the mirrored limits.
pub async fn environment(host: &HostState, args: &Value) -> Value {
    let Some(session) = str_arg(args, "agentId") else {
        return rpc_error(
            "gateway/arguments-invalid",
            "terminal/environment: no session",
        );
    };
    let cwd = files::workspace_root(host, &session)
        .await
        .unwrap_or_else(|_| host.cwd.to_string_lossy().into_owned());
    rpc_ok(json!({
        "cwd": cwd,
        "maxInputBytes": MAX_INPUT_BYTES,
        "maxBufferedBytes": MAX_BUFFERED_BYTES,
        "maxCols": MAX_COLS,
        "maxRows": MAX_ROWS,
        "scrollback": SCROLLBACK_ROWS,
    }))
}

/// `terminal/shells`: the verified profiles for the new-terminal selector.
pub async fn shells(_host: &HostState, args: &Value) -> Value {
    if str_arg(args, "agentId").is_none() {
        return rpc_error("gateway/arguments-invalid", "terminal/shells: no session");
    }
    rpc_ok(json!(discover_shells()
        .iter()
        .map(ShellProfile::json)
        .collect::<Vec<_>>()))
}

/// `terminal/list`: the session's retained terminals (exited ones included —
/// their screens stay viewable until closed, upstream's behavior).
pub async fn list(host: &HostState, args: &Value) -> Value {
    let Some(session) = str_arg(args, "sessionId").or_else(|| str_arg(args, "agentId")) else {
        return rpc_error("gateway/arguments-invalid", "terminal/list: no session");
    };
    let owners = host.terminals.owners.lock().unwrap();
    let rows = owners
        .get(&session)
        .map(|owner| {
            owner
                .terminals
                .iter()
                .map(|(id, terminal)| {
                    let shared = terminal.shared.lock().unwrap();
                    terminal.info_json(id, &shared)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    rpc_ok(json!(rows))
}

/// `terminal/create`: allocate a user shell once for a caller-generated
/// identity; idempotent while the identity stays open, impossible once
/// closed (upstream's `closedIds` contract).
pub async fn create(host: &HostState, args: &Value) -> Value {
    match create_inner(host, args).await {
        Ok(info) => rpc_ok(info),
        Err(err) => rpc_error(err.code, err.message),
    }
}

async fn create_inner(host: &HostState, args: &Value) -> Result<Value, TermError> {
    let Some(session) = str_arg(args, "agentId") else {
        return Err(TermError::invalid("terminal/create: no session"));
    };
    let Some(request) = args.get("request") else {
        return Err(TermError::invalid("terminal/create: no request"));
    };
    let Some(id) = str_arg(request, "id") else {
        return Err(TermError::invalid("terminal/create: no terminal identity"));
    };
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(TermError::invalid(
            "terminal/create: invalid terminal identity",
        ));
    }
    let cols = request.get("cols").and_then(Value::as_u64);
    let rows = request.get("rows").and_then(Value::as_u64);
    let (Some(cols), Some(rows)) = (cols, rows) else {
        return Err(TermError::invalid("terminal/create: no dimensions"));
    };
    if !(2..=MAX_COLS as u64).contains(&cols) || !(1..=MAX_ROWS as u64).contains(&rows) {
        return Err(TermError::invalid(
            "terminal/create: dimensions exceed the configured limits",
        ));
    }
    let (cols, rows) = (cols as u16, rows as u16);

    let cwd = files::workspace_root(host, &session)
        .await
        .map_err(|err| TermError::new("workspace-file/not-found", err.message))?;

    let mut owners = host.terminals.owners.lock().unwrap();
    let owner = owners.entry(session.clone()).or_default();
    if owner.closed.contains(&id) {
        return Err(TermError::unavailable(
            "Terminal was closed in this Session",
        ));
    }
    if let Some(existing) = owner.terminals.get(&id) {
        let shared = existing.shared.lock().unwrap();
        return Ok(existing.info_json(&id, &shared));
    }
    if owner.terminals.len() >= MAX_TERMINALS {
        return Err(TermError::new(
            "terminal/limit-reached",
            "Session terminal limit reached",
        ));
    }

    // Shell resolution: a discovered path, or any existing executable (see
    // the module limitations — this host restricts nothing the user's own
    // shell could already do).
    let shell = match str_arg(request, "shellPath") {
        Some(path) => {
            let known = discover_shells().iter().any(|shell| shell.path == path);
            if !known && !(std::path::Path::new(&path).is_file()) {
                return Err(TermError::invalid(
                    "terminal/create: selected shell is not available on this host",
                ));
            }
            shell_profile(&path)
        }
        None => default_shell(),
    };

    let terminal = spawn_terminal(&session, &shell, &cwd, id.clone(), cols, rows)
        .map_err(|err| TermError::new("gateway/internal", format!("terminal/create: {err}")))?;
    let info = {
        let shared = terminal.shared.lock().unwrap();
        terminal.info_json(&id, &shared)
    };
    owner.terminals.insert(id, terminal);
    Ok(info)
}

fn spawn_terminal(
    session: &str,
    shell: &ShellProfile,
    cwd: &str,
    id: String,
    cols: u16,
    rows: u16,
) -> anyhow::Result<Arc<Terminal>> {
    let pty = NativePtySystem::default().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut command = CommandBuilder::new(&shell.path);
    for arg in &shell.args {
        command.arg(arg);
    }
    command.cwd(cwd);
    command.env_clear();
    for (name, value) in std::env::vars_os() {
        if !is_secret_env(&name.to_string_lossy()) {
            command.env(name, value);
        }
    }
    command.env("TERM", "xterm-256color");
    command.env("CODESMITH_SESSION_ID", session);
    let child = pty.slave.spawn_command(command)?;
    // Drop the slave handle so the child's exit is the PTY's last reference.
    drop(pty.slave);
    let reader = pty.master.try_clone_reader()?;
    let writer = pty.master.take_writer()?;
    #[cfg(unix)]
    let process_group = pty.master.process_group_leader();
    let (writer_tx, writer_rx) = std_mpsc::channel::<WriterCommand>();
    writer_loop(writer, pty.master, writer_rx);
    let killer = child.clone_killer();
    let terminal = Arc::new(Terminal {
        shell: shell.json(),
        cwd: cwd.to_string(),
        shared: Mutex::new(Shared {
            parser: Parser::new(rows, cols, SCROLLBACK_ROWS),
            history: TextRing::new(),
            sequence: 0,
            followers: Vec::new(),
            controller: None,
            next_follower: 0,
            closing: false,
            info: Info {
                title: shell.name.clone(),
                cols,
                rows,
                state: "running",
                exit_code: None,
                error: None,
            },
        }),
        writer_tx,
        killer: Mutex::new(killer),
        #[cfg(unix)]
        process_group,
        drained: Notify::new(),
    });
    reader_loop(id, terminal.clone(), reader, child);
    Ok(terminal)
}

/// `terminal/rename`: publish a display title to every attached view.
pub async fn rename(host: &HostState, args: &Value) -> Value {
    let (session, id, title) = match (
        str_arg(args, "agentId"),
        str_arg(args, "id"),
        str_arg(args, "title"),
    ) {
        (Some(session), Some(id), Some(title)) => (session, id, title),
        _ => {
            return rpc_error(
                "gateway/arguments-invalid",
                "terminal/rename: agentId, id and title are required",
            )
        }
    };
    let title = title.trim().to_string();
    if title.is_empty() || title.chars().count() > 120 {
        return rpc_error(
            "gateway/arguments-invalid",
            "Terminal title must contain 1-120 characters",
        );
    }
    let owners = host.terminals.owners.lock().unwrap();
    let Some(owner) = owners.get(&session) else {
        return rpc_error(
            "terminal/unavailable",
            "Terminal no longer exists in this Session",
        );
    };
    let Some(terminal) = owner.terminals.get(&id) else {
        return rpc_error(
            "terminal/unavailable",
            "Terminal no longer exists in this Session",
        );
    };
    let mut shared = terminal.shared.lock().unwrap();
    shared.info.title = title;
    terminal.broadcast_state(&id, &mut shared);
    rpc_ok(Value::Null)
}

/// `terminal/close`: close the identity to future creation and kill its
/// process; repeated closes succeed (upstream's contract — a failure keeps
/// the shell alive for a retry).
pub async fn close(host: &HostState, args: &Value) -> Value {
    let (session, id) = match (str_arg(args, "agentId"), str_arg(args, "id")) {
        (Some(session), Some(id)) => (session, id),
        _ => {
            return rpc_error(
                "gateway/arguments-invalid",
                "terminal/close: agentId and id are required",
            )
        }
    };
    let terminal = {
        let mut owners = host.terminals.owners.lock().unwrap();
        let Some(owner) = owners.get_mut(&session) else {
            return rpc_ok(Value::Null);
        };
        owner.closed.insert(id.clone());
        match owner.terminals.remove(&id) {
            Some(terminal) => terminal,
            None => return rpc_ok(Value::Null),
        }
    };
    match close_terminal(&id, &terminal).await {
        Ok(()) => rpc_ok(Value::Null),
        Err(err) => {
            // The shell stays out of the registry but alive (the reader
            // thread still owns the child); report the failure for a retry.
            rpc_error("gateway/internal", format!("terminal/close: {err}"))
        }
    }
}

async fn close_terminal(id: &str, terminal: &Terminal) -> anyhow::Result<()> {
    {
        let mut shared = terminal.shared.lock().unwrap();
        shared.closing = true;
        terminal.broadcast_state(id, &mut shared);
    }
    // TERM to the process group, then KILL; the reader thread publishes the
    // exit state and drains whatever the shell printed while dying.
    #[cfg(unix)]
    if let Some(pgid) = terminal.process_group {
        // SAFETY: a plain kill(2) to the shell's own process group.
        unsafe { libc::kill(pgid, libc::SIGTERM) };
    }
    let exited = tokio::time::timeout(CLOSE_GRACE, async {
        while !terminal.exited() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok();
    if !exited {
        terminal.killer.lock().unwrap().kill()?;
    }
    let _ = tokio::time::timeout(CLOSE_DRAIN_TIMEOUT, terminal.drained.notified()).await;
    terminal.finish_followers();
    Ok(())
}

/// `terminal/retain`: acknowledge the window hold, then park for the
/// stream's lifetime (this host reclaims nothing unattended, so the hold is
/// a no-op beyond the acknowledgement).
pub async fn retain(host: &HostState, args: &Value, tx: &mpsc::Sender<Value>) -> Result<(), Value> {
    let (session, id) = match (str_arg(args, "sessionId"), str_arg(args, "id")) {
        (Some(session), Some(id)) => (session, id),
        _ => {
            return Err(stream_failure(
                "gateway/arguments-invalid",
                "terminal/retain: sessionId and id are required",
            ))
        }
    };
    let present = {
        let owners = host.terminals.owners.lock().unwrap();
        owners
            .get(&session)
            .is_some_and(|owner| owner.terminals.contains_key(&id))
    };
    if !present {
        return Err(stream_failure(
            "terminal/unavailable",
            "Terminal is closing or unavailable",
        ));
    }
    if tx.send(json!({ "type": "retained" })).await.is_err() {
        return Ok(());
    }
    tx.closed().await;
    Ok(())
}

/// `terminal/attach`: the duplex screen stream — the snapshot baseline, the
/// ordered output, and the input acknowledgements (see the module doc).
pub async fn attach(
    host: &HostState,
    args: &Value,
    tx: &mpsc::Sender<Value>,
    mut uplink: mpsc::Receiver<UplinkFrame>,
) -> Result<(), Value> {
    let (session, id) = match (str_arg(args, "agentId"), str_arg(args, "id")) {
        (Some(session), Some(id)) => (session, id),
        _ => {
            return Err(stream_failure(
                "gateway/arguments-invalid",
                "terminal/attach: agentId and id are required",
            ))
        }
    };
    let terminal = {
        let owners = host.terminals.owners.lock().unwrap();
        let terminal = owners
            .get(&session)
            .and_then(|owner| owner.terminals.get(&id))
            .cloned();
        match terminal {
            Some(terminal) => terminal,
            None => {
                return Err(stream_failure(
                    "terminal/unavailable",
                    "Terminal no longer exists in this Session",
                ))
            }
        }
    };

    // Register the follower and take over input control; the previous
    // controller keeps its output but goes read-only. Upstream's order: take
    // control, broadcast to the OLD followers, then snapshot and register.
    let (follower, wake, snapshot) = {
        let mut shared = terminal.shared.lock().unwrap();
        let follower = Follower::new(shared.next_follower);
        shared.next_follower += 1;
        shared.controller = Some(follower.id);
        terminal.broadcast_state(&id, &mut shared);
        let screen = Terminal::snapshot(&mut shared);
        let info = terminal.info_json(&id, &shared);
        let writable = Terminal::writable(&shared, follower.id);
        let snapshot = json!({
            "type": "snapshot",
            "sequence": shared.sequence,
            "screen": screen,
            "info": info,
            "writable": writable,
        });
        let wake = follower.wake.clone();
        let follower_id = follower.id;
        shared.followers.push(follower);
        (follower_id, wake, snapshot)
    };
    if tx.send(snapshot).await.is_err() {
        terminal.detach(&id, follower);
        return Ok(());
    }

    let mut input_sequence: u64 = 0;
    let mut uplink_done = false;
    let outcome: Result<(), Value> = loop {
        // Deliver everything already queued; an empty queue falls through to
        // the select (nothing to spin on).
        match terminal.take_frames(follower) {
            Fetched::Frames(frames) if !frames.is_empty() => {
                let mut gone = false;
                for frame in frames {
                    if tx.send(frame).await.is_err() {
                        gone = true;
                        break;
                    }
                }
                if gone {
                    break Ok(());
                }
                continue;
            }
            Fetched::Failure(message) => {
                break Err(stream_failure("gateway/internal", message));
            }
            Fetched::Finished | Fetched::Missing => break Ok(()),
            Fetched::Frames(_) => {}
        }
        let uplink_ref = &mut uplink;
        tokio::select! {
            _ = wake.notified() => {}
            up = uplink_ref.recv(), if !uplink_done => {
                match up {
                    Some(UplinkFrame::Item(value)) => {
                        if let Err(message) =
                            terminal.apply_input(&id, follower, &mut input_sequence, &value)
                        {
                            terminal.fail_follower(follower, &message);
                        }
                    }
                    // Half-close or dropped carrier: output continues.
                    Some(UplinkFrame::End) | None => uplink_done = true,
                }
            }
            _ = tx.closed() => break Ok(()),
        }
    };
    terminal.detach(&id, follower);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_for_screen(rows: u16, cols: u16) -> Shared {
        Shared {
            parser: Parser::new(rows, cols, SCROLLBACK_ROWS),
            history: TextRing::new(),
            sequence: 0,
            followers: Vec::new(),
            controller: None,
            next_follower: 0,
            closing: false,
            info: Info {
                title: "test".to_string(),
                cols,
                rows,
                state: "running",
                exit_code: None,
                error: None,
            },
        }
    }

    /// The reconnect snapshot carries the scrollback history (as text)
    /// above the formatted visible screen — every line the shell printed,
    /// in order, survives a re-attach.
    #[test]
    fn snapshot_carries_history_above_the_formatted_screen() {
        let mut shared = shared_for_screen(10, 40);
        let stream: String = (0..60)
            .map(|index| format!("line-{index:02}\r\n"))
            .collect();
        shared.parser.process(stream.as_bytes());
        shared.history.feed(&stream);
        let snapshot = Terminal::snapshot(&mut shared);
        for index in 0..60 {
            let line = format!("line-{index:02}");
            assert!(
                snapshot.contains(&line),
                "snapshot is missing {line}:\n{snapshot}"
            );
        }
        // The history comes oldest-first.
        let first = snapshot.find("line-00").unwrap();
        let last = snapshot.find("line-59").unwrap();
        assert!(first < last);
        // Formatting survives for the visible screen (vt100 serializes SGR
        // escapes into it).
        let mut colored = shared_for_screen(10, 40);
        colored.parser.process(b"\x1b[31mred\x1b[0m\r\n");
        let snapshot = Terminal::snapshot(&mut colored);
        assert!(snapshot.contains("red"), "{snapshot}");
    }

    /// The history ring strips escape sequences (CSI, OSC) across chunk
    /// boundaries, turns `\n` into lines, and edits the tail on backspace.
    #[test]
    fn text_ring_strips_sequences_and_splits_lines() {
        let mut ring = TextRing::new();
        ring.feed("one \x1b[32mgreen\x1b[0m\r\n");
        ring.feed("\x1b]0;title\x07two\x1b]0;");
        ring.feed("t2\x07\r\nthree");
        assert_eq!(
            ring.lines,
            VecDeque::from(["one green".to_string(), "two".to_string()])
        );
        assert_eq!(ring.current, "three");
        // Backspace edits the tail; other control bytes drop.
        ring.feed("\x08x\x00y");
        assert_eq!(ring.current, "threxy");
        // The ring is capped at the scrollback depth.
        let mut capped = TextRing::new();
        for _ in 0..(SCROLLBACK_ROWS + 50) {
            capped.feed("row\n");
        }
        assert_eq!(capped.lines.len(), SCROLLBACK_ROWS);
    }

    #[test]
    fn utf8_prefix_split_survives_multibyte_boundaries() {
        let bytes = "héllo".as_bytes(); // é is two bytes
        let split = 2; // 'h' plus the incomplete lead byte of é
        let (text, rest) = split_utf8_prefix(&bytes[..split]);
        assert_eq!(text, "h");
        // The carry retains the incomplete lead byte for the next chunk.
        assert_eq!(rest, &bytes[split - 1..split]);
        let mut carry = rest.to_vec();
        carry.extend_from_slice(&bytes[split..]);
        let (text, rest) = split_utf8_prefix(&carry);
        assert_eq!(text, "éllo");
        assert!(rest.is_empty());
    }

    /// Upstream's `profile()` derives the interactive flags from the shell
    /// kind; ours must agree so a discovered shell behaves the same.
    #[test]
    fn shell_profiles_mirror_upstream_args() {
        assert_eq!(shell_profile("/bin/bash").args, vec!["-i".to_string()]);
        assert_eq!(shell_profile("/bin/zsh").name, "zsh");
        assert_eq!(
            shell_profile("/opt/homebrew/bin/fish").args,
            vec!["-i".to_string()]
        );
        assert_eq!(
            shell_profile("C:\\System32\\cmd.exe").args,
            Vec::<String>::new()
        );
        assert_eq!(
            shell_profile("/usr/local/bin/pwsh").args,
            vec!["-NoLogo".to_string()]
        );
        // The kind comparison ignores case and the .exe suffix, so a PATH
        // hit of `bash` dedupes against `/bin/bash`.
        assert_eq!(shell_kind("/bin/BASH.EXE"), "bash");
    }

    /// Discovery lists the environment default first and never repeats a
    /// shell kind (upstream's `discoverShells` contract).
    #[test]
    fn discovered_shells_lead_with_the_default_without_kind_duplicates() {
        let shells = discover_shells();
        assert!(!shells.is_empty());
        assert_eq!(shells[0].path, default_shell().path);
        let mut kinds: Vec<String> = shells.iter().map(|s| shell_kind(&s.path)).collect();
        let total = kinds.len();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), total, "{shells:?}");
    }
}
