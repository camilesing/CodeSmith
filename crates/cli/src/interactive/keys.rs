//! Mid-turn key input for the REPL.
//!
//! While a turn streams, stdin goes non-canonical so the user can keep
//! typing: this module turns the byte stream into [`TurnKey`]s and
//! [`super::render`]'s composer decides what they mean. It is deliberately
//! NOT crossterm raw mode. Only line-buffering (ICANON) and echo are
//! disabled on stdin, so output translation and ISIG stay untouched —
//! Ctrl+C still arrives as SIGINT through the existing `tokio::signal` arm,
//! and printed lines keep their newline behaviour. Reads run on a dedicated
//! thread with a 0.1s VTIME timeout so the tap can suspend promptly around
//! the blocking prompts (approval / user input / elevation) that read stdin
//! line-by-line.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - `Ctrl+Enter` only arrives in terminals that speak the kitty/CSI-u
//!   keyboard protocol; everywhere else use `Ctrl+X Ctrl+S`, which is why
//!   Claude Code documents both. A bare Enter sends `CR` and queues.
//! - A lone trailing `ESC` is reported as Escape immediately rather than
//!   waiting to see whether a sequence follows, so Escape never lags; a
//!   sequence split across two reads by the terminal would lose its prefix.
//! - Non-TTY stdin (or a termios failure) leaves the input inert; the
//!   composer is then unavailable and `/expand` remains the detail view.

/// Keys the tap reports while a turn runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnKey {
    /// Ctrl+O — toggle collapsed/expanded tool-call detail.
    ToggleDetail,
    /// Ctrl+T — toggle the task checklist.
    TaskList,
    /// Shift+Tab — walk the run-mode ring.
    CycleMode,
    /// A printable character (paste payloads included).
    Char(char),
    /// Enter — take the composer line and queue it.
    Queue,
    /// Ctrl+J — insert a newline in the composer.
    Newline,
    /// Ctrl+Enter (CSI-u terminals only) or Ctrl+X Ctrl+S — send now.
    SendNow,
    /// Escape — interrupt, or background what is running.
    Esc,
    /// Ctrl+B — send the running shell to the background.
    Background,
    /// Ctrl+X Ctrl+K — stop all background work.
    StopAll,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    /// Ctrl+K — kill to end of line.
    KillToEnd,
    /// Ctrl+U — kill to start of line.
    KillToStart,
    /// Ctrl+W — kill back to whitespace.
    KillWord,
}

/// Byte-level key decoder. Pure over the byte stream, so every sequence and
/// paste rule below is unit-tested without a terminal.
#[derive(Default)]
pub struct Decoder {
    /// An incomplete escape sequence carried over from the previous read.
    pending: Vec<u8>,
    /// Inside a bracketed paste: everything up to the terminator is text.
    pasting: Vec<u8>,
    /// Paste mode is explicit state, not inferred from `pasting` being
    /// non-empty: a feed that ends exactly at `PASTE_START` (marker split
    /// from its body across reads, realistic under tmux/SSH chunking) or an
    /// empty paste body would otherwise lose the flag, and the next feed
    /// would decode the paste body as ordinary keys — a pasted `\r`
    /// prematurely submitting the turn.
    in_paste: bool,
}

/// Bracketed-paste markers (the sequences are long enough that a paste body
/// cannot contain them by accident).
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

impl Decoder {
    /// Decode every complete key in `bytes`. An incomplete escape sequence
    /// is buffered for the next read; a lone trailing `ESC` is Escape; an
    /// open bracketed paste holds its body until the terminator arrives.
    ///
    /// One pass over the merged buffer, because a read can carry the tail of
    /// one paste, the marker of the next, and ordinary keys after it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TurnKey> {
        let mut keys = Vec::new();
        let mut in_paste = self.in_paste;
        let input = if in_paste {
            self.pasting.extend_from_slice(bytes);
            std::mem::take(&mut self.pasting)
        } else {
            self.pending.extend_from_slice(bytes);
            std::mem::take(&mut self.pending)
        };

        let mut cursor = 0usize;
        while cursor < input.len() {
            // Inside a paste everything up to the terminator is body — the
            // terminator itself is what ends the paste, so it must be
            // looked for before any sequence decoding.
            if in_paste {
                match find(&input[cursor..], PASTE_END) {
                    Some(end) => {
                        let body = &input[cursor..cursor + end];
                        keys.extend(String::from_utf8_lossy(body).chars().map(TurnKey::Char));
                        cursor += end + PASTE_END.len();
                        in_paste = false;
                    }
                    None => {
                        // Still open: hold the body for the next read instead
                        // of leaking the marker as text.
                        self.pasting = input[cursor..].to_vec();
                        self.in_paste = true;
                        return keys;
                    }
                }
                continue;
            }
            if input[cursor..].starts_with(PASTE_START) {
                cursor += PASTE_START.len();
                in_paste = true;
                continue;
            }
            match self.decode_one(&input[cursor..]) {
                Decoded::Key(key, used) => {
                    keys.push(key);
                    cursor += used;
                }
                Decoded::Incomplete => {
                    self.pending = input[cursor..].to_vec();
                    self.in_paste = false;
                    return keys;
                }
            }
        }
        self.in_paste = in_paste;
        keys
    }

    fn decode_one(&self, rest: &[u8]) -> Decoded {
        let Some(&byte) = rest.first() else {
            return Decoded::Incomplete;
        };
        match byte {
            0x1b => self.decode_escape(rest),
            // CR (Enter) queues; LF (Ctrl+J) starts a new line.
            0x0d => Decoded::Key(TurnKey::Queue, 1),
            0x0a => Decoded::Key(TurnKey::Newline, 1),
            0x7f | 0x08 => Decoded::Key(TurnKey::Backspace, 1),
            // Ctrl+letter.
            0x01..=0x1a => match byte {
                0x0f => Decoded::Key(TurnKey::ToggleDetail, 1),
                0x14 => Decoded::Key(TurnKey::TaskList, 1),
                0x02 => Decoded::Key(TurnKey::Background, 1),
                0x0b => Decoded::Key(TurnKey::KillToEnd, 1),
                0x15 => Decoded::Key(TurnKey::KillToStart, 1),
                0x17 => Decoded::Key(TurnKey::KillWord, 1),
                0x13 => Decoded::Key(TurnKey::SendNow, 1),
                0x01 => Decoded::Key(TurnKey::Home, 1),
                0x05 => Decoded::Key(TurnKey::End, 1),
                // Ctrl+X Ctrl+S is the portable "send now"; a bare Ctrl+X
                // with anything else is ignored.
                0x18 => match rest.get(1) {
                    Some(0x13) => Decoded::Key(TurnKey::SendNow, 2),
                    Some(0x0b) => Decoded::Key(TurnKey::StopAll, 2),
                    Some(_) => Decoded::Key(TurnKey::Char('\0'), 2),
                    None => Decoded::Incomplete,
                },
                _ => Decoded::Key(TurnKey::Char('\0'), 1),
            },
            _ => self.decode_utf8(rest),
        }
    }

    fn decode_escape(&self, rest: &[u8]) -> Decoded {
        match rest.get(1) {
            // A lone ESC is Escape, not the start of a sequence: waiting for
            // a byte that may never come would make the key feel stuck.
            None => Decoded::Key(TurnKey::Esc, 1),
            Some(b'[') => self.decode_csi(&rest[2..]).map_used(2),
            Some(b'O') => match rest.get(2) {
                Some(b'A') => Decoded::Key(TurnKey::Up, 3),
                Some(b'B') => Decoded::Key(TurnKey::Down, 3),
                Some(b'C') => Decoded::Key(TurnKey::Right, 3),
                Some(b'D') => Decoded::Key(TurnKey::Left, 3),
                Some(b'H') => Decoded::Key(TurnKey::Home, 3),
                Some(b'F') => Decoded::Key(TurnKey::End, 3),
                Some(_) => Decoded::Key(TurnKey::Char('\0'), 3),
                None => Decoded::Incomplete,
            },
            // Alt+<char> and other ESC-prefixed keys are not rebound
            // mid-turn; drop the ESC and let the character through without
            // spuriously firing the Escape binding (interrupt) on every
            // Alt-keypress.
            Some(_) => Decoded::Key(TurnKey::Char('\0'), 1),
        }
    }

    /// Decode a `CSI …` body. The `used` count excludes the `ESC [`.
    fn decode_csi(&self, body: &[u8]) -> Decoded {
        let Some(&first) = body.first() else {
            return Decoded::Incomplete;
        };
        match first {
            b'A' => Decoded::Key(TurnKey::Up, 1),
            b'B' => Decoded::Key(TurnKey::Down, 1),
            b'C' => Decoded::Key(TurnKey::Right, 1),
            b'D' => Decoded::Key(TurnKey::Left, 1),
            b'H' => Decoded::Key(TurnKey::Home, 1),
            b'F' => Decoded::Key(TurnKey::End, 1),
            b'Z' => Decoded::Key(TurnKey::CycleMode, 1),
            // Modified Enter from a CSI-u terminal: `CSI 13 ; 5 u` is
            // Ctrl+Enter.
            b'1' if body.starts_with(b"13;5u") => Decoded::Key(TurnKey::SendNow, 5),
            b'3' => match body.get(1) {
                Some(b'~') => Decoded::Key(TurnKey::Delete, 2),
                // `CSI 3;5~` (Ctrl+Delete) and friends can never match the
                // 2-byte form: `body.get(1)` is `;` and will never change
                // as bytes arrive. Consuming through the terminator like
                // the unknown arm — `Some(_) => Incomplete` used to stall
                // every later key behind one such keystroke.
                Some(_) => {
                    match body
                        .iter()
                        .position(|byte| byte.is_ascii_alphabetic() || *byte == b'~')
                    {
                        Some(end) => Decoded::Key(TurnKey::Char('\0'), end + 1),
                        None => Decoded::Incomplete,
                    }
                }
                // Still waiting for the final `~`.
                None => Decoded::Incomplete,
            },
            b'1' | b'4' | b'7' | b'8' => {
                // Home/End by terminfo flavour: `CSI 1~`, `CSI 4~`, `CSI 7~`,
                // `CSI 8~`. Other `1`-prefixed tilde codes are F-keys
                // (`CSI 15~`…), not Home — only the exact Home/End codes map.
                let mut index = 0usize;
                while body.get(index).is_some_and(u8::is_ascii_digit) {
                    index += 1;
                }
                match body.get(index) {
                    Some(b'~') => {
                        let code: u32 = std::str::from_utf8(&body[..index])
                            .ok()
                            .and_then(|digits| digits.parse().ok())
                            .unwrap_or(0);
                        match code {
                            // Home/End flavours; every other tilde code
                            // (2/5/6, F-key ranges, …) has no composer
                            // binding — consume it as ignored.
                            1 | 7 => Decoded::Key(TurnKey::Home, index + 1),
                            4 | 8 => Decoded::Key(TurnKey::End, index + 1),
                            _ => Decoded::Key(TurnKey::Char('\0'), index + 1),
                        }
                    }
                    // A non-`~` byte starts modifier/vt-style parameters
                    // (`CSI 1;5A` = Ctrl+Up, `CSI 13;2u` = Shift+Enter):
                    // consuming digits+one byte used to leak the tail as
                    // typed text — consume through the terminator like the
                    // unknown arm instead.
                    Some(_) => {
                        match body
                            .iter()
                            .position(|byte| byte.is_ascii_alphabetic() || *byte == b'~')
                        {
                            Some(end) => Decoded::Key(TurnKey::Char('\0'), end + 1),
                            None => Decoded::Incomplete,
                        }
                    }
                    None => Decoded::Incomplete,
                }
            }
            _ => {
                // Unknown but complete sequence (terminated by a letter or
                // `~`): consume it rather than leaking its bytes as text.
                let complete = body
                    .iter()
                    .position(|byte| byte.is_ascii_alphabetic() || *byte == b'~');
                match complete {
                    Some(end) => Decoded::Key(TurnKey::Char('\0'), end + 1),
                    None => Decoded::Incomplete,
                }
            }
        }
    }

    fn decode_utf8(&self, rest: &[u8]) -> Decoded {
        let first = rest[0];
        let len = match first {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1, // stray continuation byte: drop it
        };
        if rest.len() < len {
            return Decoded::Incomplete;
        }
        match std::str::from_utf8(&rest[..len]) {
            Ok(text) => match text.chars().next() {
                Some(c) if c != '\0' => Decoded::Key(TurnKey::Char(c), len),
                _ => Decoded::Key(TurnKey::Char('\0'), len),
            },
            // Invalid UTF-8: consume the first byte so the stream advances.
            Err(_) => Decoded::Key(TurnKey::Char('\0'), 1),
        }
    }
}

/// One decode step: a key plus how many bytes it used, "not yet", or nothing
/// worth reporting.
enum Decoded {
    Key(TurnKey, usize),
    Incomplete,
}

impl Decoded {
    /// Shift a CSI body's byte count by the two bytes of the `ESC [` prefix.
    fn map_used(self, prefix: usize) -> Self {
        match self {
            Decoded::Key(key, used) => Decoded::Key(key, used + prefix),
            Decoded::Incomplete => Decoded::Incomplete,
        }
    }
}

/// `haystack.windows` as a substring search; `[u8]` has no `find`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<TurnKey> {
        Decoder::default().feed(bytes)
    }

    #[test]
    fn decodes_plain_text_and_editing_keys() {
        assert_eq!(keys(b"hi"), vec![TurnKey::Char('h'), TurnKey::Char('i')]);
        assert_eq!(keys(b"\r"), vec![TurnKey::Queue]);
        assert_eq!(keys(b"\n"), vec![TurnKey::Newline]);
        assert_eq!(keys(b"\x7f"), vec![TurnKey::Backspace]);
        assert_eq!(
            keys(b"\x0b\x15\x17"),
            vec![TurnKey::KillToEnd, TurnKey::KillToStart, TurnKey::KillWord]
        );
    }

    #[test]
    fn decodes_utf8_across_reads() {
        // "é" is two bytes; a read that splits it must not emit half a
        // character.
        let mut decoder = Decoder::default();
        assert!(decoder.feed(&[0xc3]).is_empty());
        assert_eq!(decoder.feed(&[0xa9]), vec![TurnKey::Char('é')]);
    }

    #[test]
    fn decodes_arrows_and_terminfo_variants() {
        assert_eq!(
            keys(b"\x1b[A\x1b[B\x1b[C\x1b[D"),
            vec![TurnKey::Up, TurnKey::Down, TurnKey::Right, TurnKey::Left]
        );
        assert_eq!(keys(b"\x1b[3~"), vec![TurnKey::Delete]);
        assert_eq!(keys(b"\x1b[1~"), vec![TurnKey::Home]);
        assert_eq!(keys(b"\x1b[4~"), vec![TurnKey::End]);
        assert_eq!(keys(b"\x1bOH\x1bOF"), vec![TurnKey::Home, TurnKey::End]);
        assert_eq!(keys(b"\x1b[Z"), vec![TurnKey::CycleMode]);
    }

    #[test]
    fn a_lone_escape_is_escape_and_completes_next_read() {
        assert_eq!(keys(b"\x1b"), vec![TurnKey::Esc], "Escape must not lag");
        // A split sequence resumes: the prefix is buffered, not leaked.
        let mut decoder = Decoder::default();
        assert!(decoder.feed(b"\x1b[").is_empty());
        assert_eq!(decoder.feed(b"A"), vec![TurnKey::Up]);
    }

    #[test]
    fn send_now_has_a_portable_spelling() {
        assert_eq!(keys(b"\x13"), vec![TurnKey::SendNow]);
        assert_eq!(keys(b"\x18\x13"), vec![TurnKey::SendNow]);
        assert_eq!(keys(b"\x18\x0b"), vec![TurnKey::StopAll]);
        // Kitty-protocol Ctrl+Enter.
        assert_eq!(keys(b"\x1b[13;5u"), vec![TurnKey::SendNow]);
    }

    #[test]
    fn paste_bodies_survive_as_text() {
        let mut decoder = Decoder::default();
        let mut input = Vec::from(&b"before "[..]);
        input.extend_from_slice(PASTE_START);
        input.extend_from_slice("two\nlines".as_bytes());
        input.extend_from_slice(PASTE_END);
        input.extend_from_slice(b"after");
        let decoded = decoder.feed(&input);
        assert_eq!(
            decoded,
            "before two\nlinesafter"
                .chars()
                .map(TurnKey::Char)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn paste_split_across_reads_is_held_until_its_terminator() {
        let mut decoder = Decoder::default();
        let mut input = Vec::from(PASTE_START);
        input.extend_from_slice(b"half");
        assert!(
            decoder.feed(&input).is_empty(),
            "no text before the end marker"
        );
        assert_eq!(
            decoder.feed(PASTE_END),
            "half".chars().map(TurnKey::Char).collect::<Vec<_>>()
        );
    }

    #[test]
    fn detail_and_task_keys_keep_their_prompt_bindings() {
        assert_eq!(keys(b"\x0f"), vec![TurnKey::ToggleDetail]);
        assert_eq!(keys(b"\x14"), vec![TurnKey::TaskList]);
        assert_eq!(keys(b"\x02"), vec![TurnKey::Background]);
    }

    #[test]
    fn an_unknown_sequence_is_consumed_not_typed() {
        // `CSI 5 ~` (PageUp) is not a composer key: it must not appear as
        // text in the buffer.
        assert_eq!(keys(b"\x1b[5~"), vec![TurnKey::Char('\0')]);
    }
}
#[cfg(unix)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use tokio::sync::mpsc;

    use super::{Decoder, TurnKey};

    /// Bytes per read: a paste or a burst of keystrokes lands in one go.
    const READ_BUF: usize = 256;

    pub struct KeyTap {
        tx: mpsc::Sender<TurnKey>,
        rx: mpsc::Receiver<TurnKey>,
        stop: Arc<AtomicBool>,
        /// Saved canonical termios while the tap is armed; `None` when
        /// disarmed or when stdin is not a TTY (`disabled` latches the
        /// latter so we never retry).
        orig: Option<libc::termios>,
        disabled: bool,
        thread: Option<JoinHandle<()>>,
    }

    impl KeyTap {
        /// Take over stdin for the tap. A non-TTY stdin yields an inert tap
        /// (the turn runs without the mid-turn composer).
        pub fn start() -> Self {
            let (tx, rx) = mpsc::channel(256);
            let mut tap = Self {
                tx,
                rx,
                stop: Arc::new(AtomicBool::new(false)),
                orig: None,
                disabled: false,
                thread: None,
            };
            tap.arm();
            tap
        }

        /// Next key, or `None` once the sender side is gone.
        pub async fn recv(&mut self) -> Option<TurnKey> {
            self.rx.recv().await
        }

        /// Stop the reader thread and restore line-buffered stdin. Must run
        /// before any blocking line prompt reads stdin.
        pub fn suspend(&mut self) {
            if let Some(thread) = self.thread.take() {
                self.stop.store(true, Ordering::Relaxed);
                // The thread observes the flag within one VTIME slice.
                let _ = thread.join();
            }
            if let Some(orig) = self.orig.take() {
                // SAFETY: restoring the fd-0 state `arm` captured earlier.
                // A failed restore leaves the terminal non-canonical with
                // echo off — the user's shell looks broken after exit — so
                // never swallow the failure silently.
                if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &orig) } != 0 {
                    eprintln!(
                        "warning: failed to restore terminal mode on stdin: {}",
                        std::io::Error::last_os_error()
                    );
                }
            }
        }

        /// Re-arm the tap after a [`KeyTap::suspend`].
        pub fn resume(&mut self) {
            self.arm();
        }

        fn arm(&mut self) {
            if self.disabled || self.orig.is_some() {
                return;
            }
            // SAFETY: tcgetattr/tcsetattr touch only this process's fd-0
            // line-discipline state; the previous state is saved first.
            let mut orig: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut orig) } != 0 {
                self.disabled = true; // not a TTY — stay inert for good
                return;
            }
            let mut noncanonical = orig;
            noncanonical.c_lflag &= !(libc::ICANON | libc::ECHO);
            // IXON otherwise swallows 0x13 as XOFF: neither SendNow
            // spelling (bare Ctrl+S / Ctrl+X Ctrl+S) would reach `read`,
            // and output would freeze until Ctrl+Q — the app looks hung.
            // Restored with the saved `orig` on drop.
            noncanonical.c_iflag &= !libc::IXON;
            // VMIN=0 / VTIME=1: read(2) returns after 0.1s with no data,
            // giving the thread a chance to observe the stop flag.
            noncanonical.c_cc[libc::VMIN] = 0;
            noncanonical.c_cc[libc::VTIME] = 1;
            if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &noncanonical) } != 0 {
                self.disabled = true;
                return;
            }
            self.stop = Arc::new(AtomicBool::new(false));
            self.orig = Some(orig);
            self.thread = Some(spawn_reader(self.tx.clone(), self.stop.clone()));
        }
    }

    impl Drop for KeyTap {
        fn drop(&mut self) {
            self.suspend();
        }
    }

    fn spawn_reader(tx: mpsc::Sender<TurnKey>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let mut decoder = Decoder::default();
            let mut buf = [0u8; READ_BUF];
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // SAFETY: reading from fd 0; the tap owns stdin while armed
                // (`suspend` joins this thread first).
                let n = unsafe {
                    libc::read(
                        libc::STDIN_FILENO,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        READ_BUF,
                    )
                };
                if n < 0 {
                    // Only EINTR is retriable; anything persistent (EBADF
                    // after fd 0 was closed elsewhere, EIO on a lost tty)
                    // must break, or this loop re-calls `read` immediately
                    // forever — a 100%-CPU hot spin in a thread the owner
                    // believes is quiescent.
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if n == 0 {
                    // VTIME timeout: poll the stop flag again.
                    continue;
                }
                for key in decoder.feed(&buf[..n as usize]) {
                    // `Char('\0')` is the decoder's "consumed, no meaning"
                    // sentinel for sequences with no composer binding.
                    if key == TurnKey::Char('\0') {
                        continue;
                    }
                    // Never block on a full channel: `suspend()` joins this
                    // thread while showing a blocking prompt, and a large
                    // paste with a momentarily stalled consumer would
                    // deadlock the whole REPL. `try_send` + stop re-check
                    // keeps the join bounded by the VTIME slice.
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            return; // suspend() is joining us
                        }
                        match tx.try_send(key.clone()) {
                            Ok(()) => break,
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                return; // turn ended, receiver dropped
                            }
                        }
                    }
                }
            }
        })
    }
}

#[cfg(not(unix))]
mod imp {
    use super::TurnKey;

    /// Windows falls back to the line-based prompt only: there is no
    /// termios here, and the composer is a Unix-only affordance until the
    /// Windows console input path is built.
    pub struct KeyTap;

    impl KeyTap {
        pub fn start() -> Self {
            Self
        }
        pub async fn recv(&mut self) -> Option<TurnKey> {
            std::future::pending().await
        }
        pub fn suspend(&mut self) {}
        pub fn resume(&mut self) {}
    }
}

pub use imp::KeyTap;
