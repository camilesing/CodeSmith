//! Fact ledger: rule-extracted must-not-lose facts that survive compaction.
//!
//! Recording-layer role (one of four layers; each owning module states its
//! role): the ledger is **runtime state**, not a log — it cannot be
//! re-folded from the transcript once compaction drops the source
//! messages, so it persists as a snapshot field of the session file
//! instead of registering as a transcript projection
//! (`crate::projections`). That foldability boundary is the deciding
//! test: foldable state becomes a projection, non-foldable state a
//! snapshot.
//!
//! Long sessions lose early constraints through summarization: the first
//! task instruction gets summarized away, schema docs read from disk are
//! pruned from tool results, and the one-line reason an approach failed
//! disappears with the message that carried it. The ledger deterministically
//! extracts those facts from messages a compaction is about to drop and
//! re-renders them as a section of every compaction summary, so they are
//! never more than one compaction away from the model's context. It also
//! rides across cycle resets as a seed message (its usual injection vehicle,
//! the compaction summary, is dropped there).
//!
//! Extraction is rule-based only — fenced schema blocks, workspace paths,
//! error-marker lines — with one deliberate exception: the model itself
//! reflects lessons at compaction time (the layered summary's "Refuted
//! Assumptions & Invariants" section) and those lines are captured verbatim
//! as `RefutedAssumption` entries. That is the self-reflection loop: no
//! external/playbook knowledge is ever injected, the agent learns from its
//! own failures and the ledger keeps the lessons alive. It deliberately
//! does not:
//!
//! * extract "confirmed decisions" (not rule-derivable; planned follow-up),
//! * run an LLM extraction pass over messages (cost/latency; rules first,
//!   per the plan),
//! * cover sub-agent transcripts (only the main-loop transcript feeds it),
//! * bound `TaskConstraint` entries — they are never evicted when the token
//!   cap is reached, so a pathological instruction corpus can exceed the
//!   cap rather than silently drop a constraint.

use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

use crate::models::{ContentBlock, Message};

use super::compact::{ERROR_MARKERS, extract_paths_from_message};
use super::estimate_text_tokens_conservative;

/// Soft cap on the ledger's rendered size. When exceeded, the oldest
/// `FailureCause` entries are evicted first; `TaskConstraint` entries are
/// never evicted (see the module doc).
pub const FACT_LEDGER_MAX_TOKENS: usize = 4_000;

/// Maximum distinct path entries kept in the ledger.
pub const FACT_LEDGER_MAX_PATHS: usize = 40;

/// Maximum characters of a single fenced block captured verbatim.
pub const FACT_BLOCK_MAX_CHARS: usize = 2_000;

/// Maximum fenced blocks captured from one message.
pub const FACT_BLOCKS_PER_MESSAGE: usize = 3;

/// Maximum characters of a single failure-cause line.
pub const FACT_FAILURE_LINE_MAX_CHARS: usize = 240;

/// Substrings that mark a tool-result block as constraint-bearing (schema
/// docs, output specs) rather than plain code read-back. Lowercase compare.
const CONSTRAINT_KEYWORDS: &[&str] = &[
    "schema",
    "column",
    "field",
    "format",
    "required",
    "expected",
    "output",
    "constraint",
    "verifier",
    "must",
];

/// Header of the reflection section in a layered summary. Shared by the
/// summarization instruction (compact.rs) and the parser below — the
/// instruction tells the model to reflect, the parser captures what it
/// wrote, the ledger keeps it alive.
pub const REFUTED_ASSUMPTIONS_HEADER: &str = "### Refuted Assumptions & Invariants";

/// Maximum reflection lines captured from one summary.
pub const FACT_REFUTED_MAX_PER_COMPACTION: usize = 10;

/// Header of the optional file-map section in a layered summary — the
/// model-authored "path — what it owns" table for multi-file projects.
pub const FILE_MAP_HEADER: &str = "### File Map";

/// Maximum file-map lines captured from one summary.
pub const FACT_FILE_MAP_MAX_PER_COMPACTION: usize = 15;

/// Parse one titled section (`### <title>` prefix match) out of a layered
/// summary text: the lines under the header (bullets stripped) until the
/// next header, empty lines skipped.
fn extract_titled_section(summary: &str, title: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in summary.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            in_section = trimmed.starts_with(title);
            continue;
        }
        if !in_section {
            continue;
        }
        let entry = trimmed.trim_start_matches(['-', '*']).trim();
        if entry.is_empty() {
            continue;
        }
        out.push(entry.to_string());
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// Parse the reflection section out of a layered summary text (see
/// [`REFUTED_ASSUMPTIONS_HEADER`]).
pub fn extract_refuted_assumptions(summary: &str) -> Vec<String> {
    extract_titled_section(
        summary,
        "### Refuted Assumptions",
        FACT_REFUTED_MAX_PER_COMPACTION,
    )
    .into_iter()
    .map(|entry| truncate(&entry, 2 * FACT_FAILURE_LINE_MAX_CHARS).to_string())
    .collect()
}

/// Parse the optional file-map section out of a layered summary text (see
/// [`FILE_MAP_HEADER`]). Empty when the model omitted the section — it is
/// optional precisely so single-file work never pads it.
pub fn extract_file_map(summary: &str) -> Vec<String> {
    extract_titled_section(summary, FILE_MAP_HEADER, FACT_FILE_MAP_MAX_PER_COMPACTION)
        .into_iter()
        .map(|entry| truncate(&entry, 2 * FACT_FAILURE_LINE_MAX_CHARS).to_string())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FactKind {
    /// Verbatim constraint material: fenced blocks from the task instruction
    /// or from constraint-looking tool results.
    TaskConstraint,
    /// A workspace-relative path seen in dropped messages.
    PathSignature,
    /// A one-line "why it failed" extracted from an error-bearing message.
    FailureCause,
    /// A refuted assumption or invariant the **model itself derived** from a
    /// failure, reflected in the layered summary's "Refuted Assumptions"
    /// section at compaction time and captured verbatim here. This is the
    /// self-reflection loop: lessons are learned from the run's own
    /// failures — never injected from outside — and survive every later
    /// compaction and cycle reset so they are not relearned.
    RefutedAssumption,
    /// One "path — what it owns" line from the model-authored file map in
    /// the layered summary — the externalized project structure, so the
    /// agent does not re-invent it hundreds of rounds in.
    FileIntent,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactEntry {
    pub kind: FactKind,
    pub text: String,
}

/// Accumulating ledger of must-not-lose facts. Lives on
/// `Session` behind `Arc<Mutex<…>>` (the `recent_read_files` precedent) so
/// both the executor's mid-run compaction and the host-side compaction
/// paths can feed it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FactLedger {
    entries: Vec<FactEntry>,
    /// Whitespace-normalized, lowercased dedup keys for `entries`.
    seen: HashSet<String>,
}

impl FactLedger {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Extract facts from messages about to be dropped by a compaction.
    /// Returns the number of new entries added. Idempotent: repeated
    /// accumulation of the same messages adds nothing.
    pub fn accumulate<'a, I>(&mut self, messages: I, workspace: Option<&Path>) -> usize
    where
        I: IntoIterator<Item = &'a Message>,
    {
        let before = self.entries.len();

        let mut path_count = self
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::PathSignature)
            .count();

        for message in messages {
            if message.role == "assistant" {
                // Assistant prose and code echo implementation state, not
                // constraints; paths and failures arrive via user-side tool
                // traffic anyway.
                continue;
            }
            for block in &message.content {
                match block {
                    ContentBlock::Text { text, .. } => {
                        self.accumulate_fenced_blocks(text, false);
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        self.accumulate_fenced_blocks(content, true);
                        self.accumulate_failure_lines(content);
                    }
                    _ => {}
                }
            }
            for path in extract_paths_from_message(message, workspace) {
                if path_count >= FACT_LEDGER_MAX_PATHS {
                    break;
                }
                if self.push(FactKind::PathSignature, path) {
                    path_count += 1;
                }
            }
        }

        self.enforce_capacity();
        self.entries.len() - before
    }

    /// Capture fenced ``` blocks. When `from_tool_result`, only blocks that
    /// look constraint-bearing are kept (a plain code read-back is not a
    /// constraint); instruction/steer text is always captured.
    fn accumulate_fenced_blocks(&mut self, text: &str, from_tool_result: bool) {
        let mut captured = 0;
        for block in fenced_blocks(text) {
            if captured >= FACT_BLOCKS_PER_MESSAGE {
                break;
            }
            if from_tool_result && !looks_like_constraint(block) {
                continue;
            }
            let truncated = truncate(block, FACT_BLOCK_MAX_CHARS);
            if self.push(FactKind::TaskConstraint, truncated.to_string()) {
                captured += 1;
            }
        }
    }

    fn accumulate_failure_lines(&mut self, content: &str) {
        let lower = content.to_lowercase();
        if !ERROR_MARKERS.iter().any(|m| lower.contains(m)) {
            return;
        }
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let line_lower = trimmed.to_lowercase();
            if ERROR_MARKERS.iter().any(|m| line_lower.contains(m))
                && self.push(
                    FactKind::FailureCause,
                    truncate(trimmed, FACT_FAILURE_LINE_MAX_CHARS).to_string(),
                )
            {
                return; // one line per result is enough
            }
        }
    }

    fn push(&mut self, kind: FactKind, text: String) -> bool {
        let key = normalize_key(&text);
        if key.is_empty() || !self.seen.insert(key) {
            return false;
        }
        self.entries.push(FactEntry { kind, text });
        true
    }

    /// Record model-reflected lessons (parsed out of the layered summary's
    /// reflection section). Returns the number of new entries. Idempotent —
    /// the same lesson reflected at two compactions records once.
    pub fn record_refuted_assumptions<I: IntoIterator<Item = String>>(
        &mut self,
        lines: I,
    ) -> usize {
        let before = self.entries.len();
        for text in lines {
            self.push(FactKind::RefutedAssumption, text);
        }
        self.enforce_capacity();
        self.entries.len() - before
    }

    /// Record the model-authored file map (parsed out of the layered
    /// summary's optional file-map section). Same dedup semantics as
    /// [`Self::record_refuted_assumptions`].
    pub fn record_file_map<I: IntoIterator<Item = String>>(&mut self, lines: I) -> usize {
        let before = self.entries.len();
        for text in lines {
            self.push(FactKind::FileIntent, text);
        }
        self.enforce_capacity();
        self.entries.len() - before
    }

    /// Evict oldest `FailureCause` entries until the rendered ledger fits
    /// the token cap, then oldest `FileIntent` entries, then oldest
    /// `RefutedAssumption` entries. Raw failure lines are the cheapest to
    /// lose (their distilled lesson, if any, is a `RefutedAssumption`);
    /// distilled lessons are the most expensive; `TaskConstraint` /
    /// `PathSignature` entries are never evicted.
    fn enforce_capacity(&mut self) {
        loop {
            let total: usize = self
                .entries
                .iter()
                .map(|e| estimate_text_tokens_conservative(&e.text))
                .sum();
            if total <= FACT_LEDGER_MAX_TOKENS {
                return;
            }
            let Some(index) = [
                FactKind::FailureCause,
                FactKind::FileIntent,
                FactKind::RefutedAssumption,
            ]
            .iter()
            .find_map(|kind| self.entries.iter().position(|e| e.kind == *kind)) else {
                return; // only non-evictable kinds remain; let it exceed
            };
            self.seen.remove(&normalize_key(&self.entries[index].text));
            self.entries.remove(index);
        }
    }

    /// Render the ledger as a summary-block section. Empty when no facts.
    pub fn summary_section(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut section = String::from(
            "## 🔒 Fact Ledger (must-not-lose facts)\n\n\
             The facts below were extracted from earlier context that has since been \
             compacted away. Treat them as ground truth — do not re-derive, contradict, \
             or quietly drop them.\n\n",
        );

        let constraints: Vec<&FactEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::TaskConstraint)
            .collect();
        if !constraints.is_empty() {
            section.push_str("**Task constraints / schemas (verbatim):**\n");
            for entry in constraints {
                let _ = writeln!(section, "\n{entry}");
            }
            section.push('\n');
        }

        let paths: Vec<&FactEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::PathSignature)
            .collect();
        if !paths.is_empty() {
            section.push_str("**Key paths:** ");
            let joined = paths
                .iter()
                .map(|e| format!("`{}`", e.text))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(section, "{joined}");
            section.push('\n');
        }

        let failures: Vec<&FactEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::FailureCause)
            .collect();
        if !failures.is_empty() {
            section.push_str("**Known failure causes:**\n");
            for entry in failures {
                let _ = writeln!(section, "- {}", entry.text);
            }
            section.push('\n');
        }

        let lessons: Vec<&FactEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::RefutedAssumption)
            .collect();
        if !lessons.is_empty() {
            section.push_str("**Refuted assumptions / invariants (learned the hard way):**\n");
            for entry in lessons {
                let _ = writeln!(section, "- {}", entry.text);
            }
            section.push('\n');
        }

        let file_map: Vec<&FactEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::FileIntent)
            .collect();
        if !file_map.is_empty() {
            section.push_str("**Project file map (path — what it owns):**\n");
            for entry in file_map {
                let _ = writeln!(section, "- {}", entry.text);
            }
            section.push('\n');
        }

        section.push_str("\n---\n\n");
        section
    }
}

impl std::fmt::Display for FactEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            // Constraint material is re-shown as a fenced block so the
            // model reads it as verbatim spec, not prose.
            FactKind::TaskConstraint => write!(f, "```\n{}\n```", self.text),
            FactKind::PathSignature
            | FactKind::FailureCause
            | FactKind::RefutedAssumption
            | FactKind::FileIntent => write!(f, "{}", self.text),
        }
    }
}

/// Yield the inner text of ``` fenced blocks.
fn fenced_blocks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let start = rest.find("```")?;
        let after_fence = &rest[start + 3..];
        // Skip the info string on the opening fence line.
        let body_start = after_fence.find('\n').map_or(0, |i| i + 1);
        let body = &after_fence[body_start..];
        let end = body.find("\n```")?;
        let block = &body[..end];
        rest = &body[end + 4..];
        Some(block)
    })
}

fn looks_like_constraint(block: &str) -> bool {
    let lower = block.to_lowercase();
    CONSTRAINT_KEYWORDS.iter().any(|k| lower.contains(k))
}

fn truncate(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

fn normalize_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    /// Event-sourcing slice 4 — the ledger round-trips through JSON so it
    /// can ride `SavedSession` (lessons survive restarts).
    #[test]
    fn fact_ledger_serde_round_trip() {
        let mut ledger = FactLedger::default();
        ledger.record_refuted_assumptions(vec![
            "the API validates on POST, not GET — always POST".to_string(),
        ]);
        ledger.record_file_map(vec!["crates/foo.rs — owns the parser".to_string()]);
        assert!(!ledger.is_empty());

        let json = serde_json::to_string(&ledger).expect("serialize");
        let back: FactLedger = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.len(), ledger.len());
        // Dedup keys survive too: re-recording the same lesson is a no-op.
        let mut back = back;
        let added = back.record_refuted_assumptions(vec![
            "the API validates on POST, not GET — always POST".to_string(),
        ]);
        assert_eq!(added, 0, "seen-set survived the round trip");
    }

    use super::*;

    fn text_msg(text: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn result_msg(content: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    #[test]
    fn captures_fenced_schema_from_instruction() {
        let mut ledger = FactLedger::default();
        let added = ledger.accumulate(
            std::iter::once(&text_msg(
                "Build the CLI.\n```\ncounterparty_id,ead_usd,risk_weight\n```\nmore prose",
            )),
            None,
        );

        assert_eq!(added, 1);
        assert_eq!(ledger.entries[0].kind, FactKind::TaskConstraint);
        assert!(ledger.entries[0].text.contains("counterparty_id"));
    }

    #[test]
    fn tool_result_blocks_need_constraint_keyword() {
        let mut ledger = FactLedger::default();
        let plain_code = result_msg("```\nfn main() { println!(\"hi\"); }\n```");
        let schema = result_msg("```\ncolumns: a,b,c\n```");

        let added = ledger.accumulate([&plain_code, &schema], None);

        assert_eq!(added, 1);
        assert!(ledger.entries[0].text.contains("columns"));
    }

    #[test]
    fn failure_line_extracted_from_error_result() {
        let mut ledger = FactLedger::default();
        let failed = result_msg("running tests\nerror: cannot find function `foo` in scope");

        let added = ledger.accumulate(std::iter::once(&failed), None);

        assert_eq!(added, 1);
        assert_eq!(ledger.entries[0].kind, FactKind::FailureCause);
        assert!(
            ledger.entries[0]
                .text
                .contains("error: cannot find function")
        );
    }

    #[test]
    fn accumulate_is_idempotent_and_case_insensitive() {
        let mut ledger = FactLedger::default();
        let m = result_msg("Error: boom");

        assert_eq!(ledger.accumulate(std::iter::once(&m), None), 1);
        assert_eq!(ledger.accumulate(std::iter::once(&m), None), 0);
    }

    #[test]
    fn path_cap_bounds_path_entries() {
        let mut ledger = FactLedger::default();
        let messages: Vec<Message> = (0..60)
            .map(|i| text_msg(&format!("see src/module_{i}/lib.rs for part {i}")))
            .collect();
        let refs: Vec<&Message> = messages.iter().collect();

        ledger.accumulate(refs, None);

        let paths = ledger
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::PathSignature)
            .count();
        assert_eq!(paths, FACT_LEDGER_MAX_PATHS);
    }

    #[test]
    fn eviction_drops_oldest_failure_causes_keeps_constraints() {
        let mut ledger = FactLedger::default();
        // One constraint + enough failures to blow past the token cap
        // (each entry is char-capped, so eviction needs volume).
        let big_constraint = format!("```schema\n{}\n```", "col_a\n".repeat(400));
        let constraint_msg = text_msg(&big_constraint);
        let failures: Vec<Message> = (0..120)
            .map(|i| {
                result_msg(&format!(
                    "error: failure number {i} {}",
                    "padding token ".repeat(17)
                ))
            })
            .collect();
        let mut all = vec![constraint_msg];
        all.extend(failures);
        let refs: Vec<&Message> = all.iter().collect();

        ledger.accumulate(refs, None);

        assert!(
            ledger
                .entries
                .iter()
                .any(|e| e.kind == FactKind::TaskConstraint && e.text.contains("col_a"))
        );
        let failures = ledger
            .entries
            .iter()
            .filter(|e| e.kind == FactKind::FailureCause)
            .count();
        assert!(
            failures < 120,
            "capacity eviction must have dropped failures (kept {failures})"
        );
    }

    #[test]
    fn summary_section_renders_all_kinds_and_skips_empty() {
        assert!(FactLedger::default().summary_section().is_empty());

        let mut ledger = FactLedger::default();
        let m = text_msg("Schema:\n```\ncolumns: x\n```\nsee src/main.rs\nerror: nope");
        // error line only comes from tool results; feed one separately.
        let f = result_msg("error: builder exited 1");
        ledger.accumulate([&m, &f], None);

        let section = ledger.summary_section();
        assert!(section.contains("Fact Ledger"));
        assert!(section.contains("columns: x"));
        assert!(section.contains("`src/main.rs`"));
        assert!(section.contains("error: builder exited 1"));
    }

    #[test]
    fn assistant_messages_are_skipped() {
        let mut ledger = FactLedger::default();
        let m = Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "```\ncolumns: x\n```".to_string(),
                cache_control: None,
            }],
        };

        assert_eq!(ledger.accumulate(std::iter::once(&m), None), 0);
    }

    #[test]
    fn fenced_blocks_parses_multiple_blocks() {
        let text = "a\n```\nfirst\n```\nb\n```rust\nsecond\n```\nc";
        let blocks: Vec<&str> = fenced_blocks(text).collect();
        assert_eq!(blocks, vec!["first", "second"]);
    }

    #[test]
    fn extract_refuted_assumptions_parses_section_only() {
        let summary = "### Decisions & Confirmed Facts\n- keep A\n\
            ### Failed Approaches\n- x failed\n\
            ### Refuted Assumptions & Invariants\n\
            - assumption: socket swap needs no drain — refuted: in-flight reads saw a half-written table; invariant: drain requests before version switch\n\
            plain line without bullet\n\
            ### Brief Process\nexplored\n";
        let lines = extract_refuted_assumptions(summary);

        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("assumption: socket swap"));
        assert!(!lines[0].starts_with('-'));
        assert!(lines[1] == "plain line without bullet");
        assert!(extract_refuted_assumptions("no headers here").is_empty());
    }

    #[test]
    fn record_refuted_assumptions_dedups_across_compactions() {
        let mut ledger = FactLedger::default();
        let lesson = "assumption A refuted; invariant B must hold".to_string();

        assert_eq!(ledger.record_refuted_assumptions([lesson.clone()]), 1);
        assert_eq!(ledger.record_refuted_assumptions([lesson]), 0);

        let section = ledger.summary_section();
        assert!(section.contains("Refuted assumptions / invariants"));
        assert!(section.contains("invariant B must hold"));
    }

    #[test]
    fn extract_file_map_parses_optional_section_only() {
        let with_map = "### Refuted Assumptions & Invariants\n- lesson one\n\
            ### File Map\n- src/state.rs — owns the state machine, all commands depend on it\n\
            src/cli.rs — argument parsing only\n\
            ### Brief Process\ndone";
        let lines = extract_file_map(with_map);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("src/state.rs"));
        assert!(!lines[0].starts_with('-'));

        // Absent section (single-file work) → empty, no noise.
        assert!(extract_file_map("### Brief Process\nonly process").is_empty());
    }

    #[test]
    fn record_file_map_dedups_and_renders() {
        let mut ledger = FactLedger::default();
        let line = "src/engine.rs — turn loop".to_string();

        assert_eq!(ledger.record_file_map([line.clone()]), 1);
        assert_eq!(ledger.record_file_map([line]), 0);

        let section = ledger.summary_section();
        assert!(section.contains("Project file map"));
        assert!(section.contains("src/engine.rs — turn loop"));
    }

    #[test]
    fn eviction_prefers_raw_failures_over_refuted_lessons() {
        let mut ledger = FactLedger::default();
        ledger.record_refuted_assumptions(["lesson: invariant Z must hold".to_string()]);
        // Enough raw failure volume to force eviction past the cap.
        let failures: Vec<Message> = (0..120)
            .map(|i| {
                result_msg(&format!(
                    "error: failure {i} {}",
                    "padding token ".repeat(17)
                ))
            })
            .collect();
        let refs: Vec<&Message> = failures.iter().collect();
        ledger.accumulate(refs, None);

        assert!(
            ledger
                .entries
                .iter()
                .any(|e| e.kind == FactKind::RefutedAssumption),
            "the distilled lesson must outlive its raw failure lines"
        );
    }
}
