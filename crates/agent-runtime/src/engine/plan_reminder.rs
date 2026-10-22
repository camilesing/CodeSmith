//! Plan-mode runtime reminder: the plan contract restated at the request
//! seam, so its rules sit in recent context instead of only in the pinned
//! (cache-stable) mode prompt.
//!
//! Why this exists. `prompts/modes/plan.md` is part of the frozen system
//! prefix — correct for cache stability, but a rule stated once, far back,
//! reads as advice: a plan-mode investigation that spanned three modules of
//! one monorepo swept them serially and never opened a read-only sub-agent,
//! with the fan-out rule in the system prompt the whole time. The reference
//! harness (ZCode) delivers the same rules as a per-request reminder —
//! full text on entry, sparsely after — and this module mirrors that shape
//! on CodeSmith's own seam: a user-role `<codesmith:runtime_event>` pushed
//! before each step's request (the same seam as the LSP / verdict /
//! deliverables notes).
//!
//! Lifecycle, stated once because there is deliberately no reminder
//! framework here (a second reminder should generalize this module, not
//! copy it):
//!
//! * The decision is derived *statelessly* from the transcript each step —
//!   no engine-held counter to keep in sync — and costs one owned
//!   transcript snapshot per plan-mode step, the same seam where the
//!   request snapshot is taken anyway.
//! * The message is **durable**: it lands on the append-only transcript
//!   (mirrored by `Event::SessionUpdated`, persisted with the session,
//!   replayable on resume), which is what "model-visible means logged"
//!   requires. Never the system prompt, so the prefix cache is unaffected.
//! * Cadence is counted in *real user turns* — user-role messages carrying
//!   text, with tool results and engine notes excluded (see
//!   [`is_real_user_turn`]).
//!
//! Known limitations (deliberate):
//!
//! * Compaction can summarize older reminders away; the count then restarts
//!   from the last surviving reminder and the next one arrives sooner than
//!   the cadence intends. Self-healing and bounded by the cadence.
//! * A reminder raises compliance; it cannot *force* fan-out. The
//!   deterministic alternative — detect a serial read-only sweep and steer
//!   — is an enforcement change and is deliberately not built here.
//! * Real user turns are classified by shape, not by a provenance flag the
//!   transcript does not carry: a user message that itself begins with an
//!   XML block (`<…>`) is read as an engine note and not counted. The test
//!   module pins every shape this rule recognizes.
//! * This module is the rule sheet's one copy: `prompts/modes/plan.md` and
//!   `enter_plan_mode`'s result text point here. A host that runs a turn
//!   loop without [`note_plan_reminder`] therefore leaves plan mode with
//!   the pointer and no rules — production wires it in
//!   `HostAgentExecutor`'s pre-request seam.

use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message};

use super::RUNTIME_EVENT_PREFIX;
use crate::mode::AppMode;

/// Real user turns that must pass before the reminder repeats.
pub const TURNS_BETWEEN_PLAN_REMINDERS: usize = 5;

/// Every Nth plan reminder is the full text (#1, #6, #11, …); the rest are
/// the sparse recap. Mirrors the reference harness's rotation.
pub const FULL_EVERY_N_PLAN_REMINDERS: u32 = 5;

/// Runtime-event kind of a plan-mode reminder.
pub const PLAN_REMINDER_KIND: &str = "plan_mode";
/// Runtime-event kind of the one-shot "you have exited plan mode" note.
pub const PLAN_EXIT_REMINDER_KIND: &str = "plan_mode_exit";

/// The authoritative plan-mode rule sheet: the mode prompt
/// (`prompts/modes/plan.md`) and `enter_plan_mode`'s result text point here
/// rather than repeating the workflow, so there is exactly one copy to keep
/// true.
pub const PLAN_REMINDER_FULL: &str = "\
## Plan Mode

Plan mode is active. The user wants a plan before implementation: you MUST NOT \
make any edits, run any non-readonly tools, or otherwise change the system. \
This restates the contract the user selected and supersedes any other \
instructions you have received.

**Fan-out rule.** A deep exploration spanning 3+ modules, services, or \
directories — several areas inside one repository count — runs as one read-only \
sub-agent session per area: put every `agent_open` call in a single message \
(`agent_type: \"explore\"`), then `agent_eval` each session for its findings. Do \
not grind through those areas serially yourself; that is what the session tools \
are for. A single module or a one-file question is your own work — do not fan it \
out. A second repository must be trusted with `/trust add <path>` first, then \
passed as the child's `cwd`.

**Workflow.** Understand first: read, search, and map the code before proposing \
anything. Deep exploration follows the fan-out rule. Persist the plan with \
`write_plan_file`, present it with `update_plan` (the plan is rendered for \
review), and end the mode with `exit_plan_mode`, which asks the user for \
approval.

**Turn discipline.** A turn ends by asking the user with `request_user_input` \
(clarifications, or choosing between approaches) or by presenting the plan. \
Never ask \"is this plan okay?\" in text or through `request_user_input` — only \
`exit_plan_mode` requests plan approval.";

/// The recap injected between full reminders.
pub const PLAN_REMINDER_SPARSE: &str = "\
Plan mode still active (full instructions earlier in this conversation): \
read-only, no edits. Fan out one read-only sub-agent session per area once 3+ \
modules, services, or directories are in play; end turns with \
`request_user_input` (clarify) or `update_plan` / `exit_plan_mode` (plan).";

/// The one-shot note after the session leaves plan mode, so the model knows
/// writing is permitted again (the user-switch exit route has no other
/// signal — an approved plan has the REPL's implementation kickoff).
pub const PLAN_MODE_EXIT_REMINDER: &str = "\
## Exited Plan Mode

You have exited plan mode. You can now make edits, run tools, and take actions.";

/// Which reminder the transcript says is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanReminder {
    Full,
    Sparse,
    Exited,
}

impl PlanReminder {
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Self::Full | Self::Sparse => PLAN_REMINDER_KIND,
            Self::Exited => PLAN_EXIT_REMINDER_KIND,
        }
    }

    #[must_use]
    pub fn body(self) -> &'static str {
        match self {
            Self::Full => PLAN_REMINDER_FULL,
            Self::Sparse => PLAN_REMINDER_SPARSE,
            Self::Exited => PLAN_MODE_EXIT_REMINDER,
        }
    }
}

/// Build the `<codesmith:runtime_event kind="plan_mode">` user message.
///
/// Role is `"user"`, not `"system"`: strict chat templates (vLLM serving
/// Qwen3, among others) require any system message to be `messages[0]`, and
/// a mid-conversation system message breaks the whole turn — the same
/// constraint that shaped the sub-agent completion and verdict notes.
pub fn plan_reminder_runtime_message(reminder: PlanReminder) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: format!(
                "{RUNTIME_EVENT_PREFIX} kind=\"{}\" visibility=\"internal\">\n\
This is an internal runtime event, not user input.\n\n\
{}\n\
</codesmith:runtime_event>",
                reminder.kind(),
                reminder.body()
            ),
            cache_control: None,
        }],
    }
}

/// Push the due reminder, if any. No-op outside plan mode (except the
/// one-shot exit note) and inside the cadence window.
pub(crate) fn note_plan_reminder(history: &mut dyn ChatHistory, mode: AppMode, plan_active: bool) {
    let due = plan_reminder_due(&history.messages(), mode, plan_active);
    if let Some(reminder) = due {
        history.push(plan_reminder_runtime_message(reminder));
    }
}

/// Decide which reminder (if any) the transcript owes for this step.
///
/// `plan_active` is the live model-initiated plan session
/// (`plan_mode_state.active`); `mode` is the user-selected mode. Either
/// being "plan" puts the session under the contract — the two axes the
/// execution gate also ORs.
#[must_use]
pub fn plan_reminder_due(
    messages: &[Message],
    mode: AppMode,
    plan_active: bool,
) -> Option<PlanReminder> {
    let family = scan_family(messages);
    if mode != AppMode::Plan && !plan_active {
        // Leaving plan: one exit note, once — only when plan mode was
        // announced in this transcript, and only while the plan reminder is
        // still the newest member of the family.
        let exit_owed = match (family.last_plan, family.last_exit) {
            (Some(plan), Some(exit)) => exit < plan,
            (Some(_), None) => true,
            (None, _) => false,
        };
        return exit_owed.then_some(PlanReminder::Exited);
    }
    let Some(last_plan) = family.last_plan else {
        // Entering plan (either route): bind immediately.
        return Some(PlanReminder::Full);
    };
    if real_user_turns(messages, last_plan + 1) < TURNS_BETWEEN_PLAN_REMINDERS {
        return None;
    }
    let next = family.plan_count + 1;
    Some(if (next - 1) % FULL_EVERY_N_PLAN_REMINDERS == 0 {
        PlanReminder::Full
    } else {
        PlanReminder::Sparse
    })
}

/// Where the plan-reminder family sits in the transcript.
#[derive(Debug, Default)]
struct FamilyScan {
    plan_count: u32,
    /// Index of the newest `plan_mode` reminder.
    last_plan: Option<usize>,
    /// Index of the newest `plan_mode_exit` note.
    last_exit: Option<usize>,
}

fn scan_family(messages: &[Message]) -> FamilyScan {
    let mut scan = FamilyScan::default();
    for (index, message) in messages.iter().enumerate() {
        let Some(text) = first_text(message) else {
            continue;
        };
        // `kind="plan_mode"` cannot match the exit kind: after `plan_mode`
        // the exit tag carries `_exit`, not the closing quote.
        if text.contains(&format!("kind=\"{PLAN_EXIT_REMINDER_KIND}\"")) {
            scan.last_exit = Some(index);
        } else if text.contains(&format!("kind=\"{PLAN_REMINDER_KIND}\"")) {
            scan.plan_count = scan.plan_count.saturating_add(1);
            scan.last_plan = Some(index);
        }
    }
    scan
}

/// Count the real user turns in `messages[from..]` — see
/// [`is_real_user_turn`].
fn real_user_turns(messages: &[Message], from: usize) -> usize {
    messages
        .get(from..)
        .unwrap_or_default()
        .iter()
        .filter(|message| is_real_user_turn(message))
        .count()
}

/// Whether a message is a turn the *user* took.
///
/// The transcript has no provenance flag, so this classifies by shape:
/// user-role, carrying text, not a tool result, and not an engine note.
/// Engine notes are recognized two ways — the `runtime_event` prefix, and a
/// leading XML block (`<turn_meta>`/`<diagnostics>` style), which is how
/// the LSP flush arrives. A user message whose own text begins with `>`
/// after a `<turn_meta>` wrapper still counts (that is the normal
/// user-text-message shape).
fn is_real_user_turn(message: &Message) -> bool {
    if message.role != "user" {
        return false;
    }
    if message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    {
        return false;
    }
    let Some(text) = first_text(message) else {
        return false;
    };
    let text = text.trim_start();
    if text.starts_with(RUNTIME_EVENT_PREFIX) {
        return false;
    }
    let remainder = strip_turn_meta(text);
    !remainder.trim_start().starts_with('<')
}

/// Strip one leading `<turn_meta>…</turn_meta>` block, when present.
fn strip_turn_meta(text: &str) -> &str {
    text.strip_prefix("<turn_meta>")
        .and_then(|rest| rest.split_once("</turn_meta>"))
        .map_or(text, |(_, after)| after)
}

fn first_text(message: &Message) -> Option<&str> {
    message.content.iter().find_map(|block| match block {
        ContentBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    /// The shape a real user message takes once turn metadata is folded in.
    fn wrapped_user(text: &str) -> Message {
        user(&format!("<turn_meta>\nmode: plan\n</turn_meta>\n{text}"))
    }

    fn assistant() -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "working".to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_result() -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".to_string(),
                content: "ok".to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    fn lsp_note() -> Message {
        user(
            "<turn_meta>\nprobe\n</turn_meta>\n<diagnostics file=\"foo.rs\">\nboom\n</diagnostics>",
        )
    }

    fn texts(reminder: PlanReminder) -> String {
        let message = plan_reminder_runtime_message(reminder);
        assert_eq!(message.role, "user", "notes ride the user role");
        match first_text(&message) {
            Some(text) => text.to_string(),
            None => panic!("expected a text block"),
        }
    }

    #[test]
    fn message_shape_is_a_runtime_event() {
        let text = texts(PlanReminder::Full);
        assert!(text.starts_with(RUNTIME_EVENT_PREFIX));
        assert!(text.contains("kind=\"plan_mode\""));
        assert!(text.contains("visibility=\"internal\""));
        assert!(text.ends_with("</codesmith:runtime_event>"));
        // The binding half the whole mechanism exists for.
        assert!(text.contains("**Fan-out rule.**"));
        assert!(text.contains("one read-only sub-agent session per area"));
        assert!(text.contains("do not fan it out"));
        assert!(text.contains("Never ask \"is this plan okay?\""));
    }

    #[test]
    fn sparse_and_exit_shape() {
        let sparse = texts(PlanReminder::Sparse);
        assert!(sparse.contains("kind=\"plan_mode\""));
        assert!(sparse.contains("Plan mode still active"));
        assert!(sparse.contains("`agent_open`") || sparse.contains("sub-agent session"));
        let exit = texts(PlanReminder::Exited);
        assert!(exit.contains("kind=\"plan_mode_exit\""));
        assert!(exit.contains("You can now make edits"));
    }

    #[test]
    fn full_on_entry_then_cadence_rotates_full_and_sparse() {
        // Entry: nothing in the transcript yet.
        assert_eq!(
            plan_reminder_due(&[], AppMode::Plan, false),
            Some(PlanReminder::Full)
        );

        let mut transcript = vec![user("investigate")];
        let mut reminders = Vec::new();
        // 26 turns carries the cadence past a second full reminder (#6).
        for turn in 1..=26 {
            let due = plan_reminder_due(&transcript, AppMode::Plan, false);
            if let Some(reminder) = due {
                reminders.push((turn, reminder));
                transcript.push(plan_reminder_runtime_message(reminder));
            }
            transcript.push(wrapped_user("keep going"));
            transcript.push(assistant());
        }
        // Full at #1 and #6; sparse in the four-turn gaps between them.
        assert_eq!(
            reminders,
            vec![
                (1, PlanReminder::Full),
                (6, PlanReminder::Sparse),
                (11, PlanReminder::Sparse),
                (16, PlanReminder::Sparse),
                (21, PlanReminder::Sparse),
                (26, PlanReminder::Full),
            ]
        );
    }

    #[test]
    fn runtime_notes_and_tool_results_are_not_user_turns() {
        let transcript = vec![
            plan_reminder_runtime_message(PlanReminder::Full),
            lsp_note(),
            tool_result(),
            tool_result(),
            assistant(),
        ];
        // Zero real turns since the reminder — inside the window either way.
        assert_eq!(real_user_turns(&transcript, 1), 0);
        assert_eq!(plan_reminder_due(&transcript, AppMode::Plan, false), None);
    }

    #[test]
    fn agent_mode_does_not_remind_and_exit_fires_once() {
        let mut transcript = vec![
            user("plan this"),
            plan_reminder_runtime_message(PlanReminder::Full),
            wrapped_user("approved"),
        ];
        // Left plan mode with the plan reminder newest in the family → the
        // one-shot exit note.
        assert_eq!(
            plan_reminder_due(&transcript, AppMode::Agent, false),
            Some(PlanReminder::Exited)
        );
        transcript.push(plan_reminder_runtime_message(PlanReminder::Exited));
        assert_eq!(plan_reminder_due(&transcript, AppMode::Agent, false), None);

        // Never announced → never noted.
        assert_eq!(
            plan_reminder_due(&[user("hello")], AppMode::Agent, false),
            None
        );
    }

    #[test]
    fn live_plan_session_reminds_within_an_agent_turn() {
        // A model-initiated session (`enter_plan_mode`) flips the live flag
        // mid-turn; the mode itself is still Agent.
        assert_eq!(
            plan_reminder_due(&[user("go")], AppMode::Agent, true),
            Some(PlanReminder::Full)
        );
    }

    #[test]
    fn turn_classifier_pins_every_shape() {
        assert!(is_real_user_turn(&user("hello")));
        assert!(is_real_user_turn(&wrapped_user("hello")));
        assert!(!is_real_user_turn(&assistant()));
        assert!(!is_real_user_turn(&tool_result()));
        assert!(!is_real_user_turn(&lsp_note()));
        assert!(!is_real_user_turn(&plan_reminder_runtime_message(
            PlanReminder::Sparse
        )));
    }
}
