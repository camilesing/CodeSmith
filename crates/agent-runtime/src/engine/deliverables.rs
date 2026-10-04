//! Deliverables watchdog: periodic disk check of the paths the task
//! instruction names as outputs, pushed back into the transcript as a
//! runtime note when one is still missing.
//!
//! Failure mode this closes (TB4.0 freight/medical): the agent works for
//! hundreds of tool rounds and ends the run without ever creating a
//! deliverable the verifier requires — discovered only at grading. The
//! engine re-surfaces the gap mid-run instead, while there is still budget
//! to close it.
//!
//! Known limitations (deliberate):
//!
//! * Extraction is rule-based and absolute-path only — a deliverable named
//!   by relative path, or a directory named without a deliverable verb
//!   nearby, is not tracked.
//! * The check is existence-only: a present-but-corrupt deliverable passes
//!   until the agent's own verification (or the verifier) catches it.
//! * Pushes are user-role runtime events on the append-only transcript
//!   (never the system prompt), so the prefix cache is unaffected.

use codesmith_agent::memory::ChatHistory;
use codesmith_agent::models::{ContentBlock, Message};
use regex::Regex;
use std::sync::OnceLock;

/// How many deliverable paths are tracked at most.
pub const MAX_DELIVERABLES: usize = 8;

/// Steps between disk checks (and the first check's offset).
pub const DELIVERABLES_CHECK_CADENCE_STEPS: u32 = 20;

/// Steps a persistent gap waits before being re-noted (3 cadences).
const RE_NOTE_INTERVAL_STEPS: u32 = 3 * DELIVERABLES_CHECK_CADENCE_STEPS;

/// Verbs whose nearby mention marks an absolute path as a deliverable
/// (for extension-less paths — directories etc.).
const DELIVERABLE_VERBS: &[&str] = &[
    "write", "save", "create", "deliver", "output", "produce", "generate", "submit", "place",
];

fn deliverable_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"/[A-Za-z0-9_][A-Za-z0-9_.@/-]*").expect("deliverable path regex is valid")
    })
}

/// Extract deliverable paths from the task instruction. Absolute file
/// paths always count; extension-less absolute paths count when a
/// deliverable verb appears on the same line. Paths under an
/// `inputs`-style directory are skipped (they are graded inputs, not
/// outputs — and they exist anyway).
pub fn parse_deliverables(instruction: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in instruction.lines() {
        let lower = line.to_lowercase();
        let has_verb = DELIVERABLE_VERBS.iter().any(|v| lower.contains(v));
        for candidate in deliverable_path_regex().find_iter(line) {
            let raw = candidate
                .as_str()
                .trim_end_matches(['.', ',', ';', ':', ')']);
            if raw.len() < 2 {
                continue;
            }
            if raw.contains("/inputs") || raw.ends_with("/inputs") {
                continue;
            }
            let looks_like_file = raw.rsplit('/').next().is_some_and(|name| {
                name.rsplit_once('.').is_some_and(|(_, ext)| {
                    !ext.is_empty()
                        && ext.len() <= 8
                        && ext.chars().all(|c| c.is_ascii_alphanumeric())
                })
            });
            if !looks_like_file && !has_verb {
                continue;
            }
            if !out.iter().any(|p| p == raw) {
                out.push(raw.to_string());
                if out.len() >= MAX_DELIVERABLES {
                    return out;
                }
            }
        }
    }
    out
}

/// Return the subset of `paths` that do not exist on disk (or cannot be
/// stat'd). Async (`tokio::fs`) — safe to call from the turn loop.
pub async fn missing_deliverables(paths: &[String]) -> Vec<String> {
    let mut missing = Vec::new();
    for path in paths {
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            continue;
        }
        missing.push(path.clone());
    }
    missing
}

/// The runtime note pushed into the transcript when deliverables are
/// missing. Mirrors `result_verifier::verdict_runtime_message`: role
/// `"user"` for strict chat-template compatibility, `visibility="internal"`
/// marking runtime data, never user input.
pub fn deliverables_runtime_message(missing: &[String], total: usize) -> Message {
    let present = total.saturating_sub(missing.len());
    let list = missing
        .iter()
        .map(|p| format!("- {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: format!(
                "<codesmith:runtime_event kind=\"deliverables_check\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. The engine checked the deliverable paths \
named in the task instruction against disk. The following are still missing:\n\
{list}\n\
Create them before finishing — a missing deliverable fails the task regardless of other \
work. ({present}/{total} present.)\n\
</codesmith:runtime_event>"
            ),
            cache_control: None,
        }],
    }
}

/// Executor-side probe: holds the parsed deliverables and the note-schedule
/// state. Cadence-gated; re-notes a persistent gap only after
/// [`RE_NOTE_INTERVAL_STEPS`] so an agent that ignores the note is nudged,
/// not spammed.
pub(crate) struct DeliverablesProbe {
    deliverables: Vec<String>,
    progress: std::sync::Mutex<DeliverablesProgress>,
}

#[derive(Default)]
struct DeliverablesProgress {
    last_note_step: u32,
    last_noted_missing: Vec<String>,
}

impl DeliverablesProbe {
    pub(crate) fn new(deliverables: Vec<String>) -> Self {
        Self {
            deliverables,
            progress: std::sync::Mutex::new(DeliverablesProgress::default()),
        }
    }

    /// Cadence gate: run the disk check only every
    /// [`DELIVERABLES_CHECK_CADENCE_STEPS`] steps.
    fn due(&self, step: u32) -> bool {
        !self.deliverables.is_empty()
            && step >= DELIVERABLES_CHECK_CADENCE_STEPS
            && step.is_multiple_of(DELIVERABLES_CHECK_CADENCE_STEPS)
    }

    /// Check the deliverables against disk and push a runtime note when
    /// something is missing. The lock is never held across the disk await.
    pub(crate) async fn check_and_note(&self, history: &mut dyn ChatHistory, step: u32) {
        if !self.due(step) {
            return;
        }
        let missing = missing_deliverables(&self.deliverables).await;
        let mut progress = self
            .progress
            .lock()
            .expect("deliverables progress poisoned");
        if missing.is_empty() {
            progress.last_noted_missing.clear();
            return;
        }
        let unchanged = progress.last_noted_missing == missing;
        let recently_noted = progress.last_note_step > 0
            && step.saturating_sub(progress.last_note_step) < RE_NOTE_INTERVAL_STEPS;
        if unchanged && recently_noted {
            return;
        }
        tracing::info!(
            target: "deliverables",
            missing = missing.len(),
            total = self.deliverables.len(),
            "deliverables check: {} of {} missing",
            missing.len(),
            self.deliverables.len()
        );
        history.push(deliverables_runtime_message(
            &missing,
            self.deliverables.len(),
        ));
        progress.last_note_step = step;
        progress.last_noted_missing = missing;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_file_paths_from_instruction() {
        let instruction = "Write the results to /app/output/sa_ccr_results.csv and the workbook \
                           to /app/output/sa_ccr_workings.xlsx. Inputs live under /app/inputs/.";
        let deliverables = parse_deliverables(instruction);

        assert_eq!(
            deliverables,
            vec![
                "/app/output/sa_ccr_results.csv".to_string(),
                "/app/output/sa_ccr_workings.xlsx".to_string(),
            ]
        );
    }

    #[test]
    fn extensionless_path_needs_deliverable_verb_on_the_line() {
        let with_verb = parse_deliverables("Deliver the CLI at /workspace/dispatch with tests.");
        assert!(with_verb.contains(&"/workspace/dispatch".to_string()));

        let without_verb = parse_deliverables("The runtime mounts /usr/local/bin for tools.");
        assert!(without_verb.is_empty());
    }

    #[test]
    fn caps_and_dedups() {
        let instruction = (0..12)
            .map(|i| format!("write /app/out/file{i}.txt"))
            .collect::<Vec<_>>()
            .join("\n");
        let deliverables = parse_deliverables(&instruction);
        assert_eq!(deliverables.len(), MAX_DELIVERABLES);
        let dedup = parse_deliverables("save /a/b.json and /a/b.json again");
        assert_eq!(dedup.len(), 1);
    }

    #[tokio::test]
    async fn missing_deliverables_reports_only_absent_paths() {
        let missing = missing_deliverables(&["/definitely/not/here/file.bin".to_string()]).await;
        assert_eq!(missing, vec!["/definitely/not/here/file.bin".to_string()]);
    }

    #[test]
    fn runtime_message_lists_missing_and_counts() {
        let message = deliverables_runtime_message(&["/app/output/late.csv".to_string()], 3);
        let ContentBlock::Text { text, .. } = &message.content[0] else {
            panic!("expected text block");
        };
        assert!(text.contains("deliverables_check"));
        assert!(text.contains("- /app/output/late.csv"));
        assert!(text.contains("2/3 present"));
        assert_eq!(message.role, "user");
    }

    #[test]
    fn probe_due_only_on_cadence_boundary() {
        let probe = DeliverablesProbe::new(vec!["/app/x.txt".to_string()]);
        assert!(!probe.due(0));
        assert!(!probe.due(19));
        assert!(probe.due(20));
        assert!(!probe.due(21));
        assert!(!probe.due(0)); // empty probe is never due
        let empty = DeliverablesProbe::new(Vec::new());
        assert!(!empty.due(20));
    }
}
