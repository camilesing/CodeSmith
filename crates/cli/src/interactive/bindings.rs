//! Prompt-time key bindings for the line REPL.
//!
//! rustyline hands a key handler only the editing state ([`EventContext`]:
//! the line, the cursor, the edit mode) and takes back a built-in `Cmd`. It
//! cannot print, and it cannot touch [`ReplState`](super::state::ReplState).
//! Every binding here therefore does two things at most:
//!
//! 1. records what the REPL should do in the shared [`Bindings`] cell, and
//! 2. returns the editing command the key implies.
//!
//! The REPL loop drains the cell once `readline` returns. To make that
//! possible for keys whose whole point is to *print* something (the task
//! list, the shortcut panel, the tool-detail replay), the handler returns
//! `Cmd::Interrupt`: `readline` ends with `ReadlineError::Interrupted`, the
//! loop applies what was drained, and re-prompts with the draft it kept. The
//! drafted line is never lost — the click of a key that discards the buffer
//! (Ctrl+C, Esc Esc) is covered because the hint hook snapshots the line into
//! the same cell on every keystroke, and every handler snapshots it too.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - An action key re-renders the prompt: the terminal transcript cannot be
//!   rewritten, so the panel a key prints appears above a fresh prompt line
//!   rather than replacing the one being edited. With text in the line, the
//!   text comes back with the cursor at its end (rustyline's
//!   `readline_with_initial`), not at its previous offset.
//! - `Shift+Tab` is the exception, because nothing has to be printed: it
//!   stays inside `readline` and asks for a repaint, and the status row (the
//!   prompt's first row) shows the mode it moved to in place — see
//!   [`Bindings::repainted_prompt`]. The mode itself is applied to the REPL
//!   when `readline` returns, which is still before the next message is sent.
//! - `jj` → Escape (Claude Code's `vimInsertModeRemaps`) is not implemented:
//!   rustyline 14 exposes no `Cmd` that switches vi input mode, so a handler
//!   cannot synthesize the Escape. Press Escape.
//! - Shift+Enter only reaches the binding in terminals that report the
//!   modifier; most send a bare CR, which submits. `Ctrl+J` and a trailing
//!   `\` work everywhere.
//! - Alt/Option keys (`Alt+P`, `Alt+T`) need the terminal to send Option as
//!   Meta. macOS terminals do not by default (see `/terminal-setup`).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustyline::Cmd;
use rustyline::ConditionalEventHandler;
use rustyline::EditMode;
use rustyline::EventContext;
use rustyline::InputMode;
use rustyline::Movement;

use super::status;
use crate::app_state::AppMode;

/// Esc Esc window (Claude Code's double press).
pub const ESC_WINDOW: Duration = Duration::from_millis(1500);
/// Ctrl+D's exit confirmation window.
pub const EXIT_WINDOW: Duration = Duration::from_millis(800);
/// Ctrl+X Ctrl+K's stop-everything confirmation window.
pub const STOP_ALL_WINDOW: Duration = Duration::from_secs(3);

/// The shared cell between the editor's key handlers and the REPL loop.
pub type Shared = Arc<Mutex<Bindings>>;

/// What a key press asked the REPL to do, or to say.
///
/// The `*Hint` variants carry no state change: they exist so a key whose
/// meaning is "explain the next press" can print from the loop, which is the
/// only place with a terminal to print to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Toggle collapsed/expanded tool-call detail (`Ctrl+O`).
    ToggleDetail,
    /// Toggle the task checklist (`Ctrl+T`).
    ToggleTaskList,
    /// The draft was stashed (`Ctrl+S` on a non-empty line).
    Stashed,
    /// `Ctrl+S` on an empty line with nothing stashed.
    StashHint,
    /// The draft was cleared and kept for recall (Esc Esc with text).
    DraftCleared,
    /// `Ctrl+D` on an empty line: the first press only explains itself.
    ExitHint,
    /// `Ctrl+X Ctrl+K`'s first press.
    StopAllHint,
    /// `Ctrl+X Ctrl+K`'s confirmation: cancel every background task.
    StopBackground,
    /// Switch to the next configured model (`Alt+P`).
    CycleModel,
    /// Toggle extended thinking (`Alt+T`).
    ToggleThinking,
    /// Edit the prompt in `$VISUAL`/`$EDITOR` (`Ctrl+G`, `Ctrl+X Ctrl+E`).
    EditInEditor,
    /// Print the shortcut panel (`?` on an empty line).
    Help,
    /// `Up` on an empty prompt with messages still queued from the last
    /// turn: take them back into the prompt for editing.
    TakeBackQueue,
}

/// The editing effect a key implies, named so the whole table is testable
/// without constructing rustyline's `Cmd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Let rustyline's own binding run.
    FallThrough,
    /// Consume the key without further effect.
    Consumed,
    /// Insert text at the cursor.
    Insert(String),
    /// Kill the whole line, storing the text for `Ctrl+Y`.
    KillWholeLine,
    /// Delete the character after the cursor.
    DeleteForward,
    /// Start a new line (multiline input).
    Newline,
    /// End the `readline` call so the loop can act on the drained actions.
    LeaveReadline,
    /// Redraw the prompt in place, keeping the line and the cursor where
    /// they are. The status row carries the state the key just changed
    /// (`Shift+Tab`'s run mode).
    Repaint,
    /// End the session.
    Exit,
}

/// Which key produced the callback. One handler type serves them all; the
/// binding table in [`super::build_editor`] maps keys to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    /// `Shift+Tab` — the run-mode ring.
    ShiftTab,
    /// `Esc` / `Ctrl+[`
    Esc,
    /// `Ctrl+D`
    CtrlD,
    /// `Ctrl+S`
    CtrlS,
    /// `Ctrl+O`
    CtrlO,
    /// `Ctrl+T`
    CtrlT,
    /// `Ctrl+G`
    CtrlG,
    /// `Ctrl+X Ctrl+K`
    StopAll,
    /// `Alt+P`
    AltP,
    /// `Alt+T`
    AltT,
    /// `Ctrl+J`, and Enter with a modifier that reports one
    Newline,
    /// `Up`
    RecallDraft,
    /// `?`
    Question,
}

/// The editing state a decision reads — a plain copy of what
/// [`EventContext`] exposes that the table actually uses.
#[derive(Debug, Clone, Copy)]
pub struct EditState<'a> {
    pub line: &'a str,
    /// True in vi *insert* mode, where `Esc` belongs to rustyline (it leaves
    /// insert mode) and must not be shadowed.
    pub vi_insert: bool,
}

impl EditState<'_> {
    fn empty(&self) -> bool {
        self.line.is_empty()
    }
}

/// The state shared between the editor and the REPL loop.
#[derive(Default)]
pub struct Bindings {
    pending: Vec<Action>,
    /// The line as of the last keystroke or hint refresh. A key that
    /// discards the buffer has no other way to know what was in it.
    line: String,
    /// `Ctrl+S` stash: text only — the cursor offset comes back with the
    /// text's end (see the module note).
    stash: Option<String>,
    /// The draft dropped by Ctrl+C or Esc Esc, recallable with `Up`.
    cleared_draft: Option<String>,
    last_esc: Option<Instant>,
    last_ctrl_d: Option<Instant>,
    last_stop_all: Option<Instant>,
    /// Mirrors of the REPL's display flags, so the hint can report them
    /// without the helper holding `ReplState`.
    pub tasks_shown: bool,
    /// How many messages are queued from the last turn (`Up` takes them
    /// back), mirrored for the same reason.
    pub queue_pending: usize,
    pub tool_detail: bool,
    /// The status row rustyline is showing as the prompt's first row, the
    /// mode it was rendered for, and whether `coordinator` is in the ring.
    /// `Shift+Tab` advances `prompt_mode`; [`Self::repainted_prompt`]
    /// rebuilds the row from these three, so a relabel needs no terminal.
    prompt_row: String,
    prompt_mode: Option<AppMode>,
    /// The mode the REPL last applied — what the composed row still shows.
    applied_mode: Option<AppMode>,
    /// Whether `coordinator` is one of the ring's stops this session.
    coordinator_in_ring: bool,
}

impl Bindings {
    pub fn shared() -> Shared {
        Arc::new(Mutex::new(Self::default()))
    }

    /// Lock, ignoring poisoning: a panic in one key handler must not wedge
    /// the prompt for the rest of the session.
    pub fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Self> {
        shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Everything the key handlers asked for since the last drain.
    pub fn drain(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.pending)
    }

    fn push(&mut self, action: Action) {
        // Two presses of the same action key inside one window collapse: the
        // user wants the panel once, not twice.
        if self.pending.last() != Some(&action) {
            self.pending.push(action);
        }
    }

    /// Record the line as of this keystroke or refresh.
    pub fn note_line(&mut self, line: &str) {
        self.line.clear();
        self.line.push_str(line);
    }

    /// The most recently seen line text.
    #[must_use]
    pub fn snapshot(&self) -> &str {
        &self.line
    }

    #[cfg(test)]
    pub fn has_stash(&self) -> bool {
        self.stash.is_some()
    }

    #[cfg(test)]
    pub fn has_cleared_draft(&self) -> bool {
        self.cleared_draft.is_some()
    }

    fn take_cleared_draft(&mut self) -> Option<String> {
        self.cleared_draft.take()
    }

    /// The drafts `Up` can recall on an empty prompt.
    fn recall_draft(&mut self) -> Option<String> {
        self.take_cleared_draft()
    }

    /// Keep a dropped draft for `Up`.
    pub fn keep_draft(&mut self, draft: String) {
        if !draft.trim().is_empty() {
            self.cleared_draft = Some(draft);
        }
    }

    /// Sync the mirrors the hint reports.
    pub fn set_display(&mut self, tool_detail: bool, tasks_shown: bool, queue_pending: usize) {
        self.tool_detail = tool_detail;
        self.tasks_shown = tasks_shown;
        self.queue_pending = queue_pending;
    }

    /// Record the prompt's status row and the mode it was composed for. The
    /// REPL calls this before every `readline`; `Shift+Tab` then advances
    /// the mode here and asks for a repaint.
    ///
    /// The row is stored *unpadded*: [`Self::padded_prompt_row`] and
    /// [`Self::repainted_prompt`] both pad it to the widest label the ring
    /// can show, which is what keeps a relabelled row the same width as the
    /// one the layout was computed from.
    pub fn set_prompt_row(&mut self, row: &str, mode: AppMode, coordinator_in_ring: bool) {
        self.prompt_row.clear();
        self.prompt_row.push_str(row);
        self.prompt_mode = Some(mode);
        self.applied_mode = Some(mode);
        self.coordinator_in_ring = coordinator_in_ring;
    }

    /// The status row as the prompt carries it: padded to the widest label
    /// the ring can show, so every mode a `Shift+Tab` reaches occupies the
    /// same width — and the same number of rows where the row wraps.
    #[must_use]
    pub fn padded_prompt_row(&self) -> String {
        let Some(mode) = self.prompt_mode else {
            return self.prompt_row.clone();
        };
        format!("{}{}", self.prompt_row, " ".repeat(self.pad_for(mode)))
    }

    /// Columns this cell pads a row rendered for `mode` with.
    fn pad_for(&self, mode: AppMode) -> usize {
        status::widest_mode_label_columns(self.coordinator_in_ring)
            .saturating_sub(status::mode_label_columns(mode))
    }

    /// `Shift+Tab`: move the mode the prompt shows to the ring's next stop.
    /// The handler cannot reach [`ReplState`](super::state::ReplState), so
    /// the loop applies it when `readline` returns — before the next message
    /// is sent, which is what the mode has to cover.
    fn cycle_prompt_mode(&mut self) {
        if let Some(mode) = self.prompt_mode {
            self.prompt_mode = Some(mode.cycle_next(self.coordinator_in_ring));
        }
    }

    /// The mode the user cycled to, if it is not the one already applied.
    /// Taken by the loop when `readline` returns; what was taken counts as
    /// applied, so the same mode is never handed out twice.
    pub fn take_prompt_mode(&mut self) -> Option<AppMode> {
        let pending = self
            .prompt_mode
            .filter(|mode| Some(*mode) != self.applied_mode);
        self.applied_mode = self.prompt_mode;
        pending
    }

    /// The prompt as rustyline should draw it right now, or `None` when
    /// nothing changed or the prompt is not the one this cell recorded.
    ///
    /// The relabelled row is padded back to the width of the widest label
    /// the ring can show, so every variant of the prompt has the same row
    /// count — the layout rustyline computed from the composed prompt (and
    /// the cursor it tracks) stays valid through the relabel.
    #[must_use]
    pub fn repainted_prompt(&self, prompt: &str) -> Option<String> {
        let mode = self.prompt_mode?;
        let applied = self.applied_mode?;
        if mode == applied || self.prompt_row.is_empty() || !prompt.starts_with(&self.prompt_row) {
            return None;
        }
        // Not our prompt (rustyline's reverse-search prompt is one row and
        // says something else).
        let (_, rest) = prompt.split_once('\n')?;
        // Guard the relabel: a first-occurrence `replacen` silently no-ops
        // when the segment is absent (row-composition drift) or when a
        // user-controlled segment happens to contain the mode text. A
        // padded-but-stale row would change the width this function exists
        // to preserve, so report failure instead.
        if !self.prompt_row.contains(&status::mode_segment(applied)) {
            return None;
        }
        let row = self.prompt_row.replacen(
            &status::mode_segment(applied),
            &status::mode_segment(mode),
            1,
        );
        if row == self.prompt_row {
            return None;
        }
        Some(format!("{row}{}\n{rest}", " ".repeat(self.pad_for(mode))))
    }

    /// The live status hint: what the prompt can do right now that has no
    /// other visible affordance. Empty when there is nothing to say.
    #[must_use]
    pub fn status_hint(&self, line: &str, todo_summary: Option<&str>) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.tasks_shown
            && let Some(summary) = todo_summary
        {
            parts.push(summary.to_string());
        }
        if line.is_empty() {
            if self.stash.is_some() {
                parts.push("Ctrl+S restores the stashed prompt".to_string());
            }
            if self.cleared_draft.is_some() {
                parts.push("Up recalls the cleared draft".to_string());
            }
            if self.queue_pending > 0 {
                parts.push(format!(
                    "{} queued message(s) — Up takes them back",
                    self.queue_pending
                ));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(format!("  · {}", parts.join(" · ")))
        }
    }
}

/// One conditional handler for every binding.
pub struct KeyHandler {
    bind: Bind,
    bindings: Shared,
}

impl KeyHandler {
    pub fn new(bind: Bind, bindings: Shared) -> Self {
        Self { bind, bindings }
    }
}

impl ConditionalEventHandler for KeyHandler {
    fn handle(
        &self,
        _evt: &rustyline::Event,
        _n: rustyline::RepeatCount,
        _positive: bool,
        ctx: &EventContext,
    ) -> Option<Cmd> {
        let mut bindings = Bindings::lock(&self.bindings);
        bindings.note_line(ctx.line());
        // In vi insert mode a bare `Esc` is how you leave insert mode; only
        // emacs-style editing can spare it.
        let state = EditState {
            line: ctx.line(),
            vi_insert: ctx.mode() == EditMode::Vi && ctx.input_mode() == InputMode::Insert,
        };
        to_cmd(decide(self.bind, state, &mut bindings, Instant::now()))
    }
}

fn within(last: Option<Instant>, window: Duration, now: Instant) -> bool {
    last.is_some_and(|at| now.duration_since(at) <= window)
}

/// An action key: record it, then end `readline` so the loop can act (and
/// print). The drafted line comes back through the prefill.
fn act(action: Action, bindings: &mut Bindings) -> Edit {
    bindings.push(action);
    Edit::LeaveReadline
}

/// The decision table. Pure over `(key, editing state, shared cell, now)`,
/// so every rule below is unit-tested without a terminal.
fn decide(bind: Bind, state: EditState<'_>, bindings: &mut Bindings, now: Instant) -> Edit {
    match bind {
        // The mode ring is the one action that changes nothing but what the
        // status row already shows: no panel, no re-prompt, no lost draft.
        Bind::ShiftTab => {
            bindings.cycle_prompt_mode();
            Edit::Repaint
        }
        Bind::Esc => {
            if state.vi_insert {
                return Edit::FallThrough;
            }
            let double = within(bindings.last_esc, ESC_WINDOW, now);
            bindings.last_esc = Some(now);
            if state.empty() {
                // Claude Code opens its rewind menu here. Rewind lands with
                // `/rewind`; until then an empty-line double Esc is a no-op
                // rather than a dead end that prints "unknown command".
                Edit::Consumed
            } else if double {
                bindings.keep_draft(state.line.to_string());
                bindings.push(Action::DraftCleared);
                Edit::KillWholeLine
            } else {
                Edit::Consumed
            }
        }
        Bind::CtrlD => {
            if !state.empty() {
                // Claude Code deletes the character after the cursor when
                // there is text, and exits on an empty prompt.
                return Edit::DeleteForward;
            }
            let double = within(bindings.last_ctrl_d, EXIT_WINDOW, now);
            bindings.last_ctrl_d = Some(now);
            if double {
                Edit::Exit
            } else {
                act(Action::ExitHint, bindings)
            }
        }
        Bind::CtrlS => {
            if state.empty() {
                match bindings.stash.take() {
                    Some(text) => Edit::Insert(text),
                    None => act(Action::StashHint, bindings),
                }
            } else {
                bindings.stash = Some(state.line.to_string());
                bindings.push(Action::Stashed);
                // The text lands in rustyline's kill ring, so Ctrl+Y brings
                // it back even without the stash.
                Edit::KillWholeLine
            }
        }
        Bind::CtrlO => act(Action::ToggleDetail, bindings),
        Bind::CtrlT => act(Action::ToggleTaskList, bindings),
        Bind::CtrlG => act(Action::EditInEditor, bindings),
        Bind::AltP => act(Action::CycleModel, bindings),
        Bind::AltT => act(Action::ToggleThinking, bindings),
        Bind::StopAll => {
            let double = within(bindings.last_stop_all, STOP_ALL_WINDOW, now);
            bindings.last_stop_all = Some(now);
            if double {
                act(Action::StopBackground, bindings)
            } else {
                act(Action::StopAllHint, bindings)
            }
        }
        Bind::Question => {
            if state.empty() {
                act(Action::Help, bindings)
            } else {
                // Claude Code inserts the character once there is text.
                Edit::FallThrough
            }
        }
        Bind::Newline => Edit::Newline,
        Bind::RecallDraft => {
            if !state.empty() {
                // With text in the line, Up belongs to the cursor/history.
                return Edit::FallThrough;
            }
            if let Some(draft) = bindings.recall_draft() {
                return Edit::Insert(draft);
            }
            if bindings.queue_pending > 0 {
                // Claude Code's take-back: queued messages come back into
                // the prompt for editing instead of being sent.
                return act(Action::TakeBackQueue, bindings);
            }
            Edit::FallThrough
        }
    }
}

/// The rustyline command for an [`Edit`].
fn to_cmd(edit: Edit) -> Option<Cmd> {
    match edit {
        Edit::FallThrough => None,
        Edit::Consumed => Some(Cmd::Noop),
        Edit::Insert(text) => Some(Cmd::Insert(1, text)),
        Edit::KillWholeLine => Some(Cmd::Kill(Movement::WholeLine)),
        // Known limitation: rustyline 14 has no kill-ring-free forward
        // delete (no `Cmd::Delete`), so Ctrl+D maps to single-char Kill
        // exactly like the library's own backspace binding — which resets
        // the kill ring, clobbering an earlier Ctrl+K/U/W kill. Upgrading
        // to a rustyline with `Cmd::Delete(Movement)` is the fix.
        Edit::DeleteForward => Some(Cmd::Kill(Movement::ForwardChar(1))),
        Edit::Newline => Some(Cmd::Newline),
        // `Cmd::Interrupt` is rustyline's "abandon this readline call"
        // (`Err(ReadlineError::Interrupted)`), which the REPL loop already
        // handles; it checks the drained actions first, so an action never
        // reads as Ctrl+C.
        Edit::LeaveReadline => Some(Cmd::Interrupt),
        Edit::Repaint => Some(Cmd::Repaint),
        Edit::Exit => Some(Cmd::EndOfFile),
    }
}

/// The shortcut panel printed by `?` on an empty prompt and by
/// `/keybindings`. Kept here because this module owns the key table.
#[must_use]
pub fn help_panel() -> String {
    [
        (
            "Ctrl+C",
            "interrupt · clear the draft (Up recalls it) · twice exits",
        ),
        (
            "Ctrl+D",
            "delete the character after the cursor · twice exits",
        ),
        ("Ctrl+S", "stash the draft · press again on an empty prompt"),
        ("Ctrl+O", "toggle tool-call detail"),
        ("Ctrl+T", "toggle the task checklist"),
        ("Ctrl+G", "edit the prompt in $VISUAL/$EDITOR"),
        ("Ctrl+X Ctrl+K", "stop all background work (press twice)"),
        ("Ctrl+J", "insert a newline (works in every terminal)"),
        ("\\ + Enter", "continue the prompt on the next line"),
        ("Esc, Esc", "clear the draft · on an empty prompt: rewind"),
        ("Esc", "in vim insert mode: leave insert mode"),
        ("Alt/⌥+P", "switch model"),
        ("Alt/⌥+T", "toggle extended thinking"),
        ("Shift+Tab", "cycle the run mode (agent · plan · yolo)"),
        ("Tab", "complete a command, argument, or @path"),
        ("Up / Down", "history · Ctrl+P / Ctrl+N do the same"),
        ("Ctrl+R", "reverse history search"),
        ("Ctrl+L", "redraw the screen"),
        ("Ctrl+Z", "suspend to the shell (fg returns)"),
        ("Ctrl+A / Ctrl+E", "start / end of line"),
        ("Ctrl+K / Ctrl+U", "kill to end / start of line"),
        ("Ctrl+W", "kill back to whitespace"),
        ("Ctrl+Y / Alt+Y", "paste killed text / cycle the kill ring"),
        ("Alt+B / Alt+F", "back / forward one word"),
        ("Alt+D", "kill to end of word"),
        ("Ctrl+_", "undo the last edit"),
        ("?", "this panel (on an empty prompt)"),
    ]
    .iter()
    .map(|(key, what)| format!("{key:<16} {what}"))
    .collect::<Vec<_>>()
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(line: &str) -> EditState<'_> {
        EditState {
            line,
            vi_insert: false,
        }
    }

    fn vi_state(line: &str) -> EditState<'_> {
        EditState {
            line,
            vi_insert: true,
        }
    }

    #[test]
    fn esc_esc_keeps_the_draft_and_clears_the_line() {
        let mut b = Bindings::default();
        let now = Instant::now();
        assert_eq!(
            decide(Bind::Esc, state("half a thought"), &mut b, now),
            Edit::Consumed,
            "a single Esc only arms the window"
        );
        assert!(b.drain().is_empty());
        assert_eq!(
            decide(
                Bind::Esc,
                state("half a thought"),
                &mut b,
                now + Duration::from_millis(500)
            ),
            Edit::KillWholeLine
        );
        assert_eq!(b.drain(), vec![Action::DraftCleared]);
        assert!(b.has_cleared_draft());
    }

    #[test]
    fn esc_esc_outside_the_window_does_not_clear() {
        let mut b = Bindings::default();
        let now = Instant::now();
        decide(Bind::Esc, state("text"), &mut b, now);
        assert_eq!(
            decide(
                Bind::Esc,
                state("text"),
                &mut b,
                now + ESC_WINDOW + Duration::from_millis(1)
            ),
            Edit::Consumed
        );
        assert!(!b.has_cleared_draft());
        assert!(b.drain().is_empty());
    }

    #[test]
    fn esc_in_vi_insert_mode_is_left_to_rustyline() {
        let mut b = Bindings::default();
        assert_eq!(
            decide(Bind::Esc, vi_state("text"), &mut b, Instant::now()),
            Edit::FallThrough
        );
        assert!(!b.has_cleared_draft());
    }

    #[test]
    fn ctrl_d_deletes_forward_with_text_and_confirms_on_an_empty_line() {
        let mut b = Bindings::default();
        let now = Instant::now();
        assert_eq!(
            decide(Bind::CtrlD, state("text"), &mut b, now),
            Edit::DeleteForward
        );
        assert_eq!(
            decide(Bind::CtrlD, state(""), &mut b, now),
            Edit::LeaveReadline
        );
        assert_eq!(b.drain(), vec![Action::ExitHint]);
        assert_eq!(
            decide(Bind::CtrlD, state(""), &mut b, now + EXIT_WINDOW),
            Edit::Exit,
            "the second press inside the window exits"
        );
    }

    #[test]
    fn ctrl_s_stashes_and_restores() {
        let mut b = Bindings::default();
        let now = Instant::now();
        assert_eq!(
            decide(Bind::CtrlS, state("draft"), &mut b, now),
            Edit::KillWholeLine
        );
        assert_eq!(b.drain(), vec![Action::Stashed]);
        assert!(b.has_stash());
        assert_eq!(
            decide(Bind::CtrlS, state(""), &mut b, now),
            Edit::Insert("draft".to_string())
        );
        assert!(!b.has_stash(), "restoring consumes the stash");
        assert_eq!(
            decide(Bind::CtrlS, state(""), &mut b, now),
            Edit::LeaveReadline
        );
        assert_eq!(b.drain(), vec![Action::StashHint]);
    }

    #[test]
    fn up_recalls_a_cleared_draft_before_history() {
        let mut b = Bindings::default();
        assert_eq!(
            decide(Bind::RecallDraft, state(""), &mut b, Instant::now()),
            Edit::FallThrough,
            "no draft: Up is history"
        );
        b.keep_draft("recall me".to_string());
        assert_eq!(
            decide(Bind::RecallDraft, state(""), &mut b, Instant::now()),
            Edit::Insert("recall me".to_string())
        );
        assert_eq!(
            decide(Bind::RecallDraft, state(""), &mut b, Instant::now()),
            Edit::FallThrough,
            "the draft is recalled once"
        );
        // A blank draft is not worth a slot.
        b.keep_draft("   ".to_string());
        assert!(!b.has_cleared_draft());
        assert_eq!(
            decide(Bind::RecallDraft, state("typed"), &mut b, Instant::now()),
            Edit::FallThrough,
            "with text in the line Up moves the cursor, not the draft"
        );
    }

    #[test]
    fn action_keys_leave_readline_and_record_once() {
        for (bind, action) in [
            (Bind::CtrlO, Action::ToggleDetail),
            (Bind::CtrlT, Action::ToggleTaskList),
            (Bind::CtrlG, Action::EditInEditor),
            (Bind::AltP, Action::CycleModel),
            (Bind::AltT, Action::ToggleThinking),
        ] {
            let mut b = Bindings::default();
            assert_eq!(
                decide(bind, state(""), &mut b, Instant::now()),
                Edit::LeaveReadline,
                "{bind:?} must hand control back so the loop can act"
            );
            assert_eq!(b.drain(), vec![action.clone()], "{bind:?}");
        }
    }

    /// A real status row (ANSI and all), as the REPL composes it for the prompt.
    fn status_row(mode: AppMode) -> String {
        status::render_status_line(
            status::RunState::Idle,
            Some("middle"),
            None,
            mode,
            0,
            "GLM5.3",
            crate::app_state::ReasoningEffort::Auto,
            "0.10.0",
        )
    }

    /// Visible columns of a row: CSI sequences are skipped, everything else
    /// counts its terminal width.
    fn visible_columns(text: &str) -> usize {
        use unicode_width::UnicodeWidthChar;

        let mut width = 0;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            width += c.width().unwrap_or(0);
        }
        width
    }

    #[test]
    fn shift_tab_repaints_the_status_row_without_leaving_readline() {
        let mut b = Bindings::default();
        b.set_prompt_row(&status_row(AppMode::Agent), AppMode::Agent, false);
        assert_eq!(
            b.take_prompt_mode(),
            None,
            "a fresh prompt has nothing to apply"
        );
        assert_eq!(
            decide(
                Bind::ShiftTab,
                state("half a thought"),
                &mut b,
                Instant::now()
            ),
            Edit::Repaint,
            "the draft stays in the line: no interrupt, no re-prompt"
        );
        assert!(b.drain().is_empty(), "nothing for the loop to print");
        assert_eq!(b.take_prompt_mode(), Some(AppMode::Plan));
        assert_eq!(b.take_prompt_mode(), None, "the ring is applied once");
    }

    #[test]
    fn shift_tab_walks_the_ring_and_skips_coordinator_unless_enabled() {
        let mut b = Bindings::default();
        b.set_prompt_row(&status_row(AppMode::Agent), AppMode::Agent, false);
        for expected in [AppMode::Plan, AppMode::Yolo, AppMode::Agent] {
            assert_eq!(
                decide(Bind::ShiftTab, state(""), &mut b, Instant::now()),
                Edit::Repaint
            );
            assert_eq!(b.take_prompt_mode(), Some(expected));
        }
        let mut b = Bindings::default();
        b.set_prompt_row(&status_row(AppMode::Yolo), AppMode::Yolo, true);
        assert_eq!(
            decide(Bind::ShiftTab, state(""), &mut b, Instant::now()),
            Edit::Repaint
        );
        assert_eq!(b.take_prompt_mode(), Some(AppMode::Coordinator));
    }

    #[test]
    fn repainting_the_status_row_keeps_the_prompt_width() {
        // The layout and the cursor rustyline tracks come from the composed
        // prompt string: a relabel that changed the row's width (or its row
        // count on a wrapping terminal) would move the cursor off the input.
        for coordinator_in_ring in [false, true] {
            for from in [AppMode::Agent, AppMode::Plan, AppMode::Yolo] {
                let mut b = Bindings::default();
                b.set_prompt_row(&status_row(from), from, coordinator_in_ring);
                // What the REPL hands rustyline, padding included.
                let prompt = format!("{}\n❯ typed", b.padded_prompt_row());
                let composed_row = prompt.split_once('\n').unwrap().0.to_string();
                let composed = visible_columns(&composed_row);
                let mut mode = from;
                for step in 0..4 {
                    mode = mode.cycle_next(coordinator_in_ring);
                    assert_eq!(
                        decide(Bind::ShiftTab, state("typed"), &mut b, Instant::now()),
                        Edit::Repaint
                    );
                    match b.repainted_prompt(&prompt) {
                        Some(repainted) => {
                            let head = repainted.split_once('\n').unwrap().0;
                            assert_eq!(
                                visible_columns(head),
                                composed,
                                "{from:?} → {mode:?} changed the prompt row's width\n\
                                 composed:  {}\n\
                                 repainted: {}",
                                composed_row.replace(' ', "·"),
                                head.replace(' ', "·")
                            );
                            assert!(
                                head.contains(&status::mode_segment(mode)),
                                "{from:?} → {mode:?}: the row still says {head:?}"
                            );
                            assert!(repainted.ends_with("\n❯ typed"), "the line is untouched");
                        }
                        // Cycling back to the mode the prompt was composed
                        // for needs no repaint: the prompt already says it.
                        None => assert_eq!(
                            mode, from,
                            "{from:?} step {step} skipped a repaint it needed"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn a_prompt_this_cell_does_not_own_is_left_alone() {
        let mut b = Bindings::default();
        b.set_prompt_row(&status_row(AppMode::Agent), AppMode::Agent, false);
        decide(Bind::ShiftTab, state(""), &mut b, Instant::now());
        // rustyline's own reverse-search prompt: one row, not ours.
        assert_eq!(b.repainted_prompt("(reverse-i-search)`foo': "), None);
        let ours = format!("{}\n❯ x", b.padded_prompt_row());
        assert!(
            b.repainted_prompt(&ours).is_some(),
            "our prompt still repaints"
        );
        // Nothing recorded yet (the editor exists before the first prompt).
        let fresh = Bindings::default();
        assert_eq!(fresh.repainted_prompt("❯ "), None);
    }

    #[test]
    fn question_opens_the_panel_only_on_an_empty_prompt() {
        let mut b = Bindings::default();
        assert_eq!(
            decide(Bind::Question, state("why?"), &mut b, Instant::now()),
            Edit::FallThrough,
            "with text it must insert the character"
        );
        assert_eq!(
            decide(Bind::Question, state(""), &mut b, Instant::now()),
            Edit::LeaveReadline
        );
        assert_eq!(b.drain(), vec![Action::Help]);
    }

    #[test]
    fn stop_all_needs_two_presses_within_the_window() {
        let mut b = Bindings::default();
        let now = Instant::now();
        assert_eq!(
            decide(Bind::StopAll, state(""), &mut b, now),
            Edit::LeaveReadline
        );
        assert_eq!(b.drain(), vec![Action::StopAllHint]);
        assert_eq!(
            decide(Bind::StopAll, state(""), &mut b, now + STOP_ALL_WINDOW),
            Edit::LeaveReadline
        );
        assert_eq!(b.drain(), vec![Action::StopBackground]);
        // Outside the window it re-arms instead of stopping.
        assert_eq!(
            decide(
                Bind::StopAll,
                state(""),
                &mut b,
                now + STOP_ALL_WINDOW + STOP_ALL_WINDOW + Duration::from_millis(1)
            ),
            Edit::LeaveReadline
        );
        assert_eq!(b.drain(), vec![Action::StopAllHint]);
    }

    #[test]
    fn newline_and_repeat_press_coalescing() {
        let mut b = Bindings::default();
        assert_eq!(
            decide(Bind::Newline, state("a\\"), &mut b, Instant::now()),
            Edit::Newline
        );
        b.push(Action::ToggleDetail);
        b.push(Action::ToggleDetail);
        assert_eq!(
            b.drain(),
            vec![Action::ToggleDetail],
            "two presses of one panel key show it once"
        );
    }

    #[test]
    fn status_hint_reports_what_can_be_recalled() {
        let mut b = Bindings::default();
        assert!(b.status_hint("", None).is_none());
        b.stash = Some("draft".to_string());
        assert_eq!(
            b.status_hint("", None).as_deref(),
            Some("  · Ctrl+S restores the stashed prompt")
        );
        b.keep_draft("cleared".to_string());
        assert_eq!(
            b.status_hint("", None).as_deref(),
            Some("  · Ctrl+S restores the stashed prompt · Up recalls the cleared draft")
        );
        assert!(
            b.status_hint("typing", None).is_none(),
            "stash/draft hints are for an empty prompt"
        );
        b.tasks_shown = true;
        assert_eq!(
            b.status_hint("typing", Some("tasks 1/3 done")).as_deref(),
            Some("  · tasks 1/3 done")
        );
    }

    #[test]
    fn help_panel_names_every_binding_this_module_owns() {
        let panel = help_panel();
        for key in [
            "Ctrl+S",
            "Ctrl+T",
            "Ctrl+O",
            "Ctrl+G",
            "Ctrl+X Ctrl+K",
            "Esc, Esc",
            "Shift+Tab",
        ] {
            assert!(panel.contains(key), "{key} missing from the panel");
        }
    }
}
