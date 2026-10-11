//! In-turn key tap for the REPL: Ctrl+O toggles tool-call detail while a
//! turn streams.
//!
//! This is deliberately NOT crossterm raw mode. Only line-buffering
//! (ICANON) and echo are disabled on stdin, so output translation and
//! ISIG stay untouched — Ctrl+C still arrives as SIGINT through the
//! existing `tokio::signal` arm, and printed lines keep their newline
//! behaviour. Reads run on a dedicated thread with a 0.1s VTIME timeout
//! so the tap can suspend promptly around the blocking prompts
//! (approval / user input / elevation) that read stdin line-by-line.
//!
//! Known limitations: while the tap is armed, every other key typed
//! mid-turn is read and discarded — there is no composer to buffer it.
//! Non-TTY stdin (or a termios failure) leaves the tap inert; `/expand`
//! remains the only detail view then.

/// Keys the tap reports while a turn runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnKey {
    /// Ctrl+O — toggle collapsed/expanded tool-call detail.
    ToggleDetail,
}

#[cfg(unix)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use tokio::sync::mpsc;

    use super::TurnKey;

    /// Ctrl+O as delivered by the terminal's input encoding.
    const CTRL_O: u8 = 0x0f;

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
        /// Take over stdin for the tap. A non-TTY stdin yields an inert
        /// tap (the turn runs without the Ctrl+O shortcut).
        pub fn start() -> Self {
            let (tx, rx) = mpsc::channel(16);
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

        /// Stop the reader thread and restore line-buffered stdin. Must
        /// run before any blocking line prompt reads stdin.
        pub fn suspend(&mut self) {
            if let Some(thread) = self.thread.take() {
                self.stop.store(true, Ordering::Relaxed);
                // The thread observes the flag within one VTIME slice.
                let _ = thread.join();
            }
            if let Some(orig) = self.orig.take() {
                // SAFETY: restoring the fd-0 state `arm` captured earlier.
                unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &orig) };
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
            let mut byte = [0u8; 1];
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // SAFETY: reading one byte from fd 0; the tap owns stdin
                // while armed (suspend joins this thread first).
                let n = unsafe {
                    libc::read(
                        libc::STDIN_FILENO,
                        byte.as_mut_ptr() as *mut libc::c_void,
                        1,
                    )
                };
                if n <= 0 {
                    // 0 = VTIME timeout; -1 = EINTR (e.g. SIGINT arrived).
                    continue;
                }
                if byte[0] == CTRL_O && tx.blocking_send(TurnKey::ToggleDetail).is_err() {
                    break; // turn ended, receiver dropped
                }
                // Other mid-turn keystrokes are discarded.
            }
        })
    }
}

#[cfg(not(unix))]
mod imp {
    use super::TurnKey;

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
