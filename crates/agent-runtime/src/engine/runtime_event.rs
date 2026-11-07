//! Runtime-event frames — the typed catalog of the sentinel user messages
//! the engine injects into the transcript (the Source row of
//! `docs/DSH_CONCEPTS.md`, the DSH concept map).
//!
//! Every internal input that becomes a user-role message — sub-agent and
//! shell-job completions, verification verdicts, plan-mode reminders,
//! deliverables notes, idle wakes — rides the same XML envelope, so three
//! parties can tell runtime data from the user's instruction: the model
//! (`visibility="internal"`), the "latest user message" extractors
//! (`RUNTIME_EVENT_PREFIX` in `engine/mod.rs`), and the session journal
//! (the frame text is what gets recorded, per "model-visible means
//! logged").
//!
//! Until this module each construction site hand-assembled the envelope
//! with its own `format!` (escaped quotes, hand-written closing tag). What
//! lives here is the vocabulary that was missing: the closed set of kinds,
//! and one frame builder. The guidance prose stays with its owning site —
//! it is content, not frame.
//!
//! # Known limitations
//!
//! - `<teammate-message>` (`engine/team_inbox.rs`) is a separate envelope
//!   on purpose: its attributes and body are teammate-supplied and need
//!   escaping / closing-tag defusing, which this frame has no slots for.
//! - `<turn_meta>` (`engine/turn_meta.rs`) is steer metadata, not a
//!   runtime event; `<codesmith:tool_search ...>`
//!   (`engine/tool_catalog.rs`) is a tool-discovery activation marker.
//!   Neither folds in.
//! - The kind set is closed and owned here: a new injected input adds a
//!   variant (and updates `docs/DSH_CONCEPTS.md`), never a raw
//!   `kind="..."` string at a construction site.

use super::RUNTIME_EVENT_PREFIX;

/// One injected runtime-event kind. The wire value ([`Self::as_str`]) is
/// load-bearing: transcript extractors and tests key on it, so changing it
/// is a protocol change, not a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeEventKind {
    /// A sub-agent settled; the payload carries its result summary.
    SubagentCompletion,
    /// A background shell job finished; the payload carries exit status
    /// and output tail.
    ShellJobCompletion,
    /// The engine re-ran a verification command from the previous turn
    /// and is returning the verdict.
    ResultVerification,
    /// A plan-mode reminder is due (plan still active).
    PlanMode,
    /// Plan mode just exited.
    PlanModeExit,
    /// Named deliverable paths are still missing on disk.
    DeliverablesCheck,
    /// An idle-wake synthesized turn; the parked completions follow as
    /// their own frames.
    IdleWake,
}

impl RuntimeEventKind {
    /// The `kind="..."` wire value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SubagentCompletion => "subagent_completion",
            Self::ShellJobCompletion => "shell_job_completion",
            Self::ResultVerification => "result_verification",
            Self::PlanMode => "plan_mode",
            Self::PlanModeExit => "plan_mode_exit",
            Self::DeliverablesCheck => "deliverables_check",
            Self::IdleWake => "idle_wake",
        }
    }
}

/// Compose a runtime-event frame: opening tag, the fixed
/// internal-visibility lead sentence, the site-owned `middle` prose,
/// closing tag.
///
/// `middle` begins where the lead sentence ends. A site that continues on
/// the same line starts `middle` with a space; one that wants a blank line
/// starts it with `\n\n`. The builder supplies the newline before the
/// closing tag, so `middle` must not end with one.
pub(crate) fn runtime_event_frame(kind: RuntimeEventKind, middle: &str) -> String {
    format!(
        "{RUNTIME_EVENT_PREFIX} kind=\"{}\" visibility=\"internal\">\n\
This is an internal runtime event, not user input.{middle}\n\
</codesmith:runtime_event>",
        kind.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_the_frame() {
        // Exhaustive mirror: a new variant added to the enum without a row
        // here fails compilation in this test, not silently in production.
        fn wire(kind: RuntimeEventKind) -> &'static str {
            match kind {
                RuntimeEventKind::SubagentCompletion => "subagent_completion",
                RuntimeEventKind::ShellJobCompletion => "shell_job_completion",
                RuntimeEventKind::ResultVerification => "result_verification",
                RuntimeEventKind::PlanMode => "plan_mode",
                RuntimeEventKind::PlanModeExit => "plan_mode_exit",
                RuntimeEventKind::DeliverablesCheck => "deliverables_check",
                RuntimeEventKind::IdleWake => "idle_wake",
            }
        }
        let all = [
            RuntimeEventKind::SubagentCompletion,
            RuntimeEventKind::ShellJobCompletion,
            RuntimeEventKind::ResultVerification,
            RuntimeEventKind::PlanMode,
            RuntimeEventKind::PlanModeExit,
            RuntimeEventKind::DeliverablesCheck,
            RuntimeEventKind::IdleWake,
        ];
        for kind in all {
            let frame = runtime_event_frame(kind, " Prose.\n\nPayload");
            assert_eq!(
                frame,
                format!(
                    "{RUNTIME_EVENT_PREFIX} kind=\"{}\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. Prose.\n\nPayload\n\
</codesmith:runtime_event>",
                    wire(kind),
                )
            );
            assert_eq!(kind.as_str(), wire(kind));
        }
    }
}
