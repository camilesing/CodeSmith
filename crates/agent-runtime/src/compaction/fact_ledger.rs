//! Fact ledger: rule-extracted must-not-lose facts that survive compaction.
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
//! error-marker lines. It deliberately does not:
//!
//! * extract "confirmed decisions" (not rule-derivable; planned follow-up),
//! * run an LLM extraction pass (cost/latency; rules first, per the plan),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactKind {
    /// Verbatim constraint material: fenced blocks from the task instruction
    /// or from constraint-looking tool results.
    TaskConstraint,
    /// A workspace-relative path seen in dropped messages.
    PathSignature,
    /// A one-line "why it failed" extracted from an error-bearing message.
    FailureCause,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactEntry {
    pub kind: FactKind,
    pub text: String,
}

/// Accumulating ledger of must-not-lose facts. Lives on
/// `Session` behind `Arc<Mutex<…>>` (the `recent_read_files` precedent) so
/// both the executor's mid-run compaction and the host-side compaction
/// paths can feed it.
#[derive(Debug, Clone, Default)]
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

    /// Evict oldest `FailureCause` entries until the rendered ledger fits
    /// the token cap. `TaskConstraint`/`PathSignature` entries are kept.
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
            let Some(index) = self
                .entries
                .iter()
                .position(|e| e.kind == FactKind::FailureCause)
            else {
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
            FactKind::PathSignature | FactKind::FailureCause => write!(f, "{}", self.text),
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
}
