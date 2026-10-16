//! Desktop notifications for turn completion.
//!
//! Supports four delivery mechanisms:
//! - **OSC 9** — terminal escape sequence (`\x1b]9;…\x07`) for iTerm2,
//!   Ghostty, WezTerm, and tmux (with DCS passthrough).
//! - **Kitty** — OSC 99 protocol with ST terminator (no audible beep).
//! - **Ghostty** — OSC 777 notification protocol.
//! - **BEL** — audible bell (`\x07`) as a last-resort fallback.
//!
//! When `method = "auto"`, the resolver picks the best method for the
//! current terminal; Windows falls back to `Bel`, which is routed through
//! `MessageBeep(MB_OK)` for an audible default notification sound.

#[cfg(target_os = "windows")]
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE;

use std::io::{self, Write};
use std::time::Duration;

/// Notification delivery method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// Automatically pick the best protocol for the current terminal.
    /// See [`resolve_method`] for the canonical resolution table.
    #[default]
    Auto,
    /// OSC 9 escape: `\x1b]9;<msg>\x07`
    Osc9,
    /// Plain BEL character: `\x07`
    Bel,
    /// osascript
    MacOS,
    /// Kitty notification protocol (OSC 99) with ST terminator.
    /// Uses `ESC ] 99 ; params ST` — no audible beep, unlike BEL.
    Kitty,
    /// Ghostty notification protocol (OSC 777).
    /// Uses `ESC ] 777 ; notify ; title ; message BEL`.
    Ghostty,
}

/// Emit a Windows system beep via `MessageBeep(MB_OK)`.
///
/// Writing BEL (`\\x07`) to the terminal is silent on most Windows
/// terminals (Windows Terminal, Conhost, etc.), so we call the Win32
/// API directly to produce the standard notification sound.
#[cfg(target_os = "windows")]
fn windows_bell() {
    // MB_OK = 0x00000000 — plays the default system sound. Best-effort: a
    // failed beep is not worth surfacing to the caller, so the Result is
    // discarded.
    unsafe {
        let _ = MessageBeep(MESSAGEBOX_STYLE(0));
    }
}

/// Resolve `Auto` to a concrete method by inspecting `$TERM_PROGRAM`,
/// `$LC_TERMINAL`, and `$TERM`.
///
/// Resolution table:
/// - `iTerm.app`, `WezTerm`, `Cmux` → `Osc9`
/// - `Ghostty` → `Ghostty` (OSC 777)
/// - `kitty` → `Kitty` (OSC 99)
/// - `$LC_TERMINAL` matches OSC-9 capable → `Osc9` (Cmux that sets LC_TERMINAL)
/// - `$TERM` contains `ghostty` → `Osc9` (cmux etc.)
/// - `$TERM` contains `kitty` → `Kitty`
/// - Unix unknown → `Bel`
/// - Windows unknown → `Bel`
#[must_use]
fn resolve_method() -> Method {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    match term_program.as_str() {
        "iTerm.app" | "WezTerm" | "Cmux" => return Method::Osc9,
        "Ghostty" => return Method::Ghostty,
        "kitty" => return Method::Kitty,
        _ => {}
    }

    // LC_TERMINAL fallback for terminals (e.g. Cmux) that set
    // LC_TERMINAL instead of TERM_PROGRAM.
    let lc_terminal = std::env::var("LC_TERMINAL").unwrap_or_default();
    match lc_terminal.as_str() {
        "iTerm.app" | "Ghostty" | "WezTerm" | "Cmux" => return Method::Osc9,
        _ => {}
    }

    // Windows: use BEL so `windows_bell()` (MessageBeep) fires on turn
    // completion.  Previous behavior returned `Off` to avoid the error chime
    // (#583), but `MessageBeep(MB_OK)` plays the *default system sound* —
    // distinct from the error sound — so BEL is safe and gives Windows users
    // audible feedback when a long turn finishes.
    if cfg!(target_os = "windows") {
        return Method::Bel;
    }

    if cfg!(target_os = "macos") {
        return Method::MacOS;
    }

    // Ghostty-based terminals (cmux, etc.) may not set their own
    // TERM_PROGRAM but do set TERM=xterm-ghostty. Likewise for Kitty.
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("ghostty") {
        Method::Osc9
    } else if term.contains("kitty") {
        Method::Kitty
    } else {
        Method::Bel
    }
}

/// Wrap an escape sequence for terminal multiplexer passthrough.
///
/// tmux intercepts escape sequences; DCS passthrough tunnels them to
/// the outer terminal unmodified. Every ESC inside the payload is
/// doubled so tmux does not interpret it as DCS end.
fn wrap_for_multiplexer(seq: &str, in_tmux: bool) -> String {
    if in_tmux {
        let escaped = seq.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;{escaped}\x1b\\")
    } else {
        seq.to_string()
    }
}

/// Build the raw escape bytes for the given method and message.
///
/// When `in_tmux` is `true`, OSC sequences are wrapped in DCS passthrough
/// so tmux forwards them to the outer terminal.
#[must_use]
fn build_escape(method: Method, in_tmux: bool, msg: &str) -> Vec<u8> {
    match method {
        Method::Bel => vec![b'\x07'],
        Method::Osc9 => {
            let inner = format!("\x1b]9;{msg}\x07");
            if in_tmux {
                let escaped_inner = inner.replace('\x1b', "\x1b\x1b");
                format!("\x1bPtmux;{escaped_inner}\x1b\\").into_bytes()
            } else {
                inner.into_bytes()
            }
        }
        Method::Kitty => {
            // Kitty notification: OSC 99 ; params ST
            // ST terminator (ESC \) instead of BEL to avoid audible beep.
            let title_seq = "\x1b]99;d=0:p=title\x1b\\";
            let body_seq = format!("\x1b]99;p=body;{msg}\x1b\\");
            let focus_seq = "\x1b]99;d=1:a=focus\x1b\\";
            let combined = format!("{title_seq}{body_seq}{focus_seq}");
            wrap_for_multiplexer(&combined, in_tmux).into_bytes()
        }
        Method::Ghostty => {
            // Ghostty notification: OSC 777 ; notify ; title ; message BEL
            let seq = format!("\x1b]777;notify;codesmith;{msg}\x07");
            wrap_for_multiplexer(&seq, in_tmux).into_bytes()
        }
        // Auto and MacOS should not reach build_escape.
        Method::Auto | Method::MacOS => vec![],
    }
}

/// Emit a turn-complete notification to `sink` if the elapsed time meets or
/// exceeds `threshold`.
///
/// This variant takes a `W: Write` sink for testability.
pub fn notify_done_to<W: Write>(
    method: Method,
    in_tmux: bool,
    msg: &str,
    threshold: Duration,
    elapsed: Duration,
    sink: &mut W,
) {
    if elapsed < threshold {
        return;
    }
    let effective = match method {
        Method::Auto => resolve_method(),
        other => other,
    };

    // macOS Notification Center: handled via osascript, not terminal escapes.
    #[cfg(target_os = "macos")]
    if Method::MacOS == effective {
        macos_display_notification(msg);
        return;
    }

    let bytes = build_escape(effective, in_tmux, msg);
    if bytes.is_empty() {
        return;
    }
    // Best-effort: ignore write errors (e.g. stdout closed).
    let _ = sink.write_all(&bytes);
    let _ = sink.flush();

    // On Windows, writing BEL (`\x07`) to the terminal is silent in most
    // terminals (Windows Terminal, Conhost, etc.). Call MessageBeep to
    // produce an actual notification sound via the system audio scheme.
    #[cfg(target_os = "windows")]
    if effective == Method::Bel {
        windows_bell();
    }
}

/// Emit a turn-complete notification to **stdout** if `elapsed >= threshold`.
///
/// With `method = Auto`, selects the best protocol for the current terminal
/// (OSC 9, Kitty OSC 99, Ghostty OSC 777, or Bel). The unknown-terminal
/// fallback is platform-aware: `Bel` on every platform, with Windows routing
/// it through `MessageBeep(MB_OK)` for a default system notification sound.
/// See [`resolve_method`] for the canonical resolution table. Pass
/// `in_tmux = true` (i.e. `$TMUX` is non-empty at runtime) to wrap OSC
/// sequences in a DCS passthrough.
pub fn notify_done(
    method: Method,
    in_tmux: bool,
    msg: &str,
    threshold: Duration,
    elapsed: Duration,
) {
    notify_done_to(method, in_tmux, msg, threshold, elapsed, &mut io::stdout());
}

/// CLI-side [`NotifierHost`] — delegates to the terminal-coupled
/// [`notify_done`] free function above. Zero-state; `Arc`-shared so cloned
/// tool contexts see one logical notifier. The host resolves the delivery
/// protocol (`Method::Auto` → OSC 9 / BEL / macOS Notification Center / Kitty
/// / Ghostty) and handles tmux DCS passthrough internally.
#[derive(Default)]
pub struct NotifierHostImpl;

impl codesmith_agent_runtime::host_services::NotifierHost for NotifierHostImpl {
    fn notify_done(&self, msg: &str) {
        let in_tmux = std::env::var("TMUX")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        // Threshold = 0 so the notification always fires; the model has
        // already decided this is the moment. `elapsed = 1s` satisfies the
        // `>= threshold` gate inside [`notify_done_to`].
        notify_done(
            Method::Auto,
            in_tmux,
            msg,
            std::time::Duration::ZERO,
            std::time::Duration::from_secs(1),
        );
    }
}

/// Construct the trait-erased notifier handle for `RuntimeToolServices`.
pub fn wrap_notifier() -> std::sync::Arc<dyn codesmith_agent_runtime::host_services::NotifierHost> {
    std::sync::Arc::new(NotifierHostImpl)
}

///
/// Runs on a dedicated background thread so the caller is not blocked.
///
/// The notification includes:
/// - **Title**: "CodeSmith"
/// - **Subtitle**: First line of `msg` (when the message contains a newline,
///   e.g. the response preview from a completed turn)
/// - **Body**: Remaining lines of `msg`, or the full `msg` if single-line
/// - **Sound**: Default macOS notification sound
///
/// The message body is capped at 200 **characters** (not bytes) to keep the
/// bubble readable while correctly handling multi-byte text.
///
/// **Security**: The message is passed to `osascript` as a command-line
/// argument via `ARGV`, never embedded inline in the AppleScript source.
/// AppleScript does not treat backslash as an escape inside double-quoted
/// string literals, so the previous `\"` approach would terminate the
/// string at the `"` and leave any text between unbalanced quotes
/// evaluated as raw AppleScript code — a code-injection vector for
/// AI-generated notification text. Passing via `ARGV` avoids this
/// entirely because the message is never parsed as AppleScript syntax.
///
/// This is best-effort: if `osascript` is not available (e.g. headless SSH
/// session) the error is logged via `tracing::warn!` instead of silently
/// swallowed.
#[cfg(target_os = "macos")]
fn macos_display_notification(msg: &str) {
    let body = msg.to_string();

    // Spawn on a background thread so we don't block the caller.
    // osascript itself is fast (~50 ms), but spawning a subprocess
    // synchronously from an async context steals a tokio thread.
    let _ = std::thread::Builder::new()
        .name("osascript-notif".into())
        .spawn(move || {
            // Char-bounded truncation (not byte-bounded) so we don't slice
            // through a multi-byte sequence and emit invalid UTF-8.
            let body_str: String = body.chars().take(200).collect();

            // Build AppleScript that receives the message via ARGV
            // instead of inline string interpolation. AppleScript does
            // not treat backslash as an escape inside double-quoted
            // string literals, so `\"` would terminate the string at
            // the `"` and leave a dangling `\`. Passing the message as
            // a command-line argument avoids any injection risk.
            //
            // When the message has multiple lines, the first line
            // becomes the subtitle and the rest becomes the body —
            // this lets turn notifications show the response preview
            // in the subtitle and the duration/cost summary in the body.
            let mut args: Vec<String> = Vec::new();

            if let Some(idx) = body_str.find('\n') {
                let subtitle = body_str[..idx].trim();
                let body_text = body_str[idx + 1..].trim();
                args.extend_from_slice(&[
                    "-e".into(),
                    "on run argv".into(),
                    "-e".into(),
                    "set theBody to item 1 of argv".into(),
                    "-e".into(),
                    "set theSubtitle to item 2 of argv".into(),
                    "-e".into(),
                    "display notification theBody with title \"CodeSmith\" subtitle theSubtitle sound name \"default\"".into(),
                    "-e".into(),
                    "end run".into(),
                    "--".into(),
                    body_text.into(),
                    subtitle.into(),
                ]);
            } else {
                args.extend_from_slice(&[
                    "-e".into(),
                    "on run argv".into(),
                    "-e".into(),
                    "display notification (item 1 of argv) with title \"CodeSmith\" sound name \"default\"".into(),
                    "-e".into(),
                    "end run".into(),
                    "--".into(),
                    body_str,
                ]);
            }

            match std::process::Command::new("osascript")
                .args(&args)
                .output()
            {
                Ok(output) if !output.status.success() => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!(stderr = %stderr, "osascript notification failed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "osascript notification error");
                }
                _ => {}
            }
        });
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;

    /// Serialise all tests that mutate `TERM_PROGRAM` to prevent data races
    /// when the test harness runs them in parallel threads.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn capture(
        method: Method,
        in_tmux: bool,
        msg: &str,
        threshold_secs: u64,
        elapsed_secs: u64,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        notify_done_to(
            method,
            in_tmux,
            msg,
            Duration::from_secs(threshold_secs),
            Duration::from_secs(elapsed_secs),
            &mut buf,
        );
        buf
    }

    #[test]
    fn osc9_body_format() {
        let out = capture(Method::Osc9, false, "codesmith: done", 0, 1);
        assert_eq!(out, b"\x1b]9;codesmith: done\x07");
    }

    #[test]
    fn bel_emits_exactly_one_byte() {
        let out = capture(Method::Bel, false, "ignored", 0, 1);
        assert_eq!(out, b"\x07");
    }

    #[test]
    fn kitty_escape_uses_st_terminator() {
        let out = capture(Method::Kitty, false, "done", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("99;"), "should have kitty OSC 99");
        assert!(s.contains("\x1b\\"), "kitty uses ST terminator");
        assert!(!s.contains("\x07"), "kitty should NOT use BEL");
    }

    #[test]
    fn ghostty_escape_format() {
        let out = capture(Method::Ghostty, false, "done", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("777;notify;codesmith;done"),
            "should have ghostty seq"
        );
    }

    #[test]
    fn kitty_tmux_dcs_passthrough() {
        let out = capture(Method::Kitty, true, "hello", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;"), "should start with DCS");
        assert!(s.ends_with("\x1b\\"), "should end with ST");
    }

    #[test]
    fn ghostty_tmux_dcs_passthrough() {
        let out = capture(Method::Ghostty, true, "hello", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;"), "should start with DCS");
        assert!(s.ends_with("\x1b\\"), "should end with ST");
    }

    #[test]
    fn below_threshold_emits_nothing() {
        let out = capture(Method::Osc9, false, "msg", 30, 29);
        assert!(out.is_empty());
    }

    #[test]
    fn at_threshold_emits() {
        let out = capture(Method::Osc9, false, "msg", 30, 30);
        assert!(!out.is_empty());
    }

    #[test]
    fn tmux_dcs_passthrough_wraps_osc9() {
        let out = capture(Method::Osc9, true, "hello", 0, 1);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.starts_with("\x1bPtmux;"),
            "should start with DCS passthrough"
        );
        assert!(s.ends_with("\x1b\\"), "should end with ST");
        assert!(s.contains("hello"), "should contain message");
    }

    #[test]
    fn auto_detect_picks_osc9_for_iterm() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "iTerm.app") };
        let resolved = resolve_method();
        // Restore previous value.
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// Cmux in typical configurations does not set `TERM_PROGRAM`; it sets
    /// `LC_TERMINAL=Cmux` instead. Verify the `LC_TERMINAL` fallback probe
    /// correctly resolves to `Osc9`.
    #[test]
    fn auto_detect_picks_osc9_for_cmux_via_lc_terminal() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::set_var("LC_TERMINAL", "Cmux");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// `LC_TERMINAL` should also match other OSC-9 capable terminals in case
    /// they set it in addition to or instead of `TERM_PROGRAM`.
    #[test]
    fn auto_detect_picks_osc9_for_wezterm_via_lc_terminal() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::set_var("LC_TERMINAL", "WezTerm");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_picks_bel_for_unknown_on_unix() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // SAFETY: test-only; serialised by env_lock().
        // Clear LC_TERMINAL and TERM so the fallback probes don't
        // accidentally pick up an OSC-9 / Kitty / Ghostty capable
        // terminal from the test runner environment.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "xterm-256color");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-256color");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Bel);
    }

    /// #2166: on Windows, an unknown TERM_PROGRAM resolves to `Bel` so
    /// `windows_bell()` can route the notification through `MessageBeep`.
    #[test]
    #[cfg(target_os = "windows")]
    fn auto_detect_picks_bel_for_unknown_on_windows() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "Windows Terminal") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Bel);
    }

    /// #583: known OSC-9 terminals must still resolve to `Osc9` on
    /// Windows — the off-fallback only applies to unrecognised
    /// `TERM_PROGRAM`. The cross-platform iTerm test above is a thin
    /// proxy because iTerm itself only runs on macOS; if the WezTerm
    /// arm of the match silently disappeared, that test would still
    /// pass on the Windows runner and we'd lose the WezTerm-on-Windows
    /// compatibility guarantee. Pin it directly.
    #[test]
    #[cfg(target_os = "windows")]
    fn auto_detect_picks_osc9_for_wezterm_on_windows() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "WezTerm") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// Ghostty-based terminals (cmux, etc.) may not set
    /// `TERM_PROGRAM` but do set `TERM=xterm-ghostty`. The `$TERM`
    /// fallback should catch them.
    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_picks_osc9_for_xterm_ghostty_term_fallback() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // Simulate a Ghostty-based terminal that only sets TERM.
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-ghostty");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Osc9);
    }

    /// Ghostty now has its own protocol (OSC 777).
    #[test]
    fn auto_detect_picks_ghostty_from_term_program() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "Ghostty") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Ghostty);
    }

    #[test]
    fn auto_detect_picks_kitty_from_term_program() {
        let _lock = env_lock();
        let prev = std::env::var_os("TERM_PROGRAM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe { std::env::set_var("TERM_PROGRAM", "kitty") };
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
        }
        assert_eq!(resolved, Method::Kitty);
    }

    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_picks_kitty_from_term_fallback() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-kitty");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Kitty);
    }

    /// When neither `TERM_PROGRAM` nor `TERM` suggests a known capable
    /// terminal, the fallback on Unix is `Bel`.
    ///
    /// On macOS the `MacOS` method takes priority, so this test is
    /// excluded there.
    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn auto_detect_falls_back_to_bel_for_unrelated_term() {
        let _lock = env_lock();
        let prev_tp = std::env::var_os("TERM_PROGRAM");
        let prev_lc = std::env::var_os("LC_TERMINAL");
        let prev_term = std::env::var_os("TERM");
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
            std::env::remove_var("LC_TERMINAL");
            std::env::set_var("TERM", "xterm-256color");
        }
        let resolved = resolve_method();
        // SAFETY: test-only; serialised by env_lock().
        unsafe {
            match prev_tp {
                Some(v) => std::env::set_var("TERM_PROGRAM", v),
                None => std::env::remove_var("TERM_PROGRAM"),
            }
            match prev_lc {
                Some(v) => std::env::set_var("LC_TERMINAL", v),
                None => std::env::remove_var("LC_TERMINAL"),
            }
            match prev_term {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
        }
        assert_eq!(resolved, Method::Bel);
    }
}
