//! Transcript-analysis primitives shared by the session data plane:
//! provider tool-pairing predicates, path extraction from message content,
//! and error markers. Extracted from `codesmith-agent-runtime`'s
//! compaction module during the round-9 split; the engine re-exports them
//! for call-site compatibility.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use codesmith_agent::models::{ContentBlock, Message};
use regex::Regex;

pub fn path_regex() -> &'static Regex {
    static PATH_RE: OnceLock<Regex> = OnceLock::new();
    PATH_RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            (?:
                (?P<root>
                    Cargo\.toml|
                    Cargo\.lock|
                    README\.md|
                    CHANGELOG\.md|
                    AGENTS\.md|
                    config\.example\.toml
                )
            )
            |
            (?P<path>
                (?:[A-Za-z0-9._-]+/)+
                [A-Za-z0-9._-]+
                \.(?:rs|toml|md|json|ya?ml|txt|lock)
            )
        ",
        )
        .expect("path regex is valid")
    })
}

pub fn normalize_path_candidate(candidate: &str, workspace: Option<&Path>) -> Option<String> {
    if candidate.is_empty() {
        return None;
    }

    let cleaned = candidate.replace('\\', "/");
    let mut path = PathBuf::from(cleaned);

    if path.is_absolute() {
        let ws = workspace?;
        if let Ok(stripped) = path.strip_prefix(ws) {
            path = stripped.to_path_buf();
        } else {
            return None;
        }
    }

    let rel = path.to_string_lossy().trim_start_matches("./").to_string();
    if rel.is_empty() || rel.contains("..") {
        return None;
    }

    if let Some(ws) = workspace {
        let repo_path = ws.join(&rel);
        if repo_path.exists() || looks_repo_relative(&rel) {
            return Some(rel);
        }
        return None;
    }

    if looks_repo_relative(&rel) {
        return Some(rel);
    }

    None
}

pub fn looks_repo_relative(path: &str) -> bool {
    matches!(
        path,
        "Cargo.toml"
            | "Cargo.lock"
            | "README.md"
            | "CHANGELOG.md"
            | "AGENTS.md"
            | "config.example.toml"
    ) || path.starts_with("src/")
        || path.starts_with("tests/")
        || path.starts_with("docs/")
        || path.starts_with("examples/")
        || path.starts_with("benches/")
        || path.starts_with("crates/")
        || path.starts_with(".github/")
        || (path.contains('/') && path.rsplit('.').next().is_some())
}

pub fn extract_paths_from_text(text: &str, workspace: Option<&Path>) -> Vec<String> {
    path_regex()
        .captures_iter(text)
        .filter_map(|caps| {
            let candidate = caps
                .name("path")
                .or_else(|| caps.name("root"))
                .map(|m| m.as_str())?;
            normalize_path_candidate(candidate, workspace)
        })
        .collect()
}

pub fn extract_paths_from_tool_input(
    input: &serde_json::Value,
    workspace: Option<&Path>,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(obj) = input.as_object() else {
        return out;
    };

    for key in ["path", "file", "target", "cwd"] {
        if let Some(val) = obj.get(key).and_then(serde_json::Value::as_str)
            && let Some(path) = normalize_path_candidate(val, workspace)
        {
            out.push(path);
        }
    }

    for key in ["paths", "files", "targets"] {
        if let Some(vals) = obj.get(key).and_then(serde_json::Value::as_array) {
            for val in vals {
                if let Some(s) = val.as_str()
                    && let Some(path) = normalize_path_candidate(s, workspace)
                {
                    out.push(path);
                }
            }
        }
    }

    out
}

pub fn extract_paths_from_message(message: &Message, workspace: Option<&Path>) -> Vec<String> {
    let mut paths = Vec::new();
    for block in &message.content {
        let candidates = match block {
            ContentBlock::Text { text, .. } => extract_paths_from_text(text, workspace),
            ContentBlock::ToolResult { content, .. } => extract_paths_from_text(content, workspace),
            ContentBlock::ToolUse { input, .. } => extract_paths_from_tool_input(input, workspace),
            ContentBlock::Thinking { .. } => Vec::new(),
            // The `[Attached image: … at <path>]` placeholder line in the user
            // text already feeds the path to the text extractor.
            ContentBlock::Image { .. } => Vec::new(),
            ContentBlock::ServerToolUse { .. }
            | ContentBlock::ToolSearchToolResult { .. }
            | ContentBlock::CodeExecutionToolResult { .. } => Vec::new(),
        };
        paths.extend(candidates);
    }
    paths
}

/// Lowercase markers identifying error-bearing content. Shared by the pin
/// planner (`should_pin_message`), the fact ledger's failure extraction,
/// and the summary's failed-attempts section.
pub const ERROR_MARKERS: &[&str] = &[
    "error:",
    "error ",
    "failed",
    "panic",
    "traceback",
    "stack trace",
    "assertion failed",
    "test failed",
];

/// Tool-call ids in `messages` that have no matching `tool_result` anywhere
/// in the slice, in first-appearance order (duplicates collapse to the
/// first). Order-agnostic on purpose: a terminator appended at the tail
/// (crash-repair) answers a call recorded mid-transcript just as well.
/// Shared by the session-load repair vocabulary and the balance predicate.
#[must_use]
pub fn unanswered_tool_use_ids(messages: &[Message]) -> Vec<String> {
    let mut calls: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut answered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in messages {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    // Set membership on the dedup side: transcripts carry
                    // thousands of tool ids, and a linear scan per call made
                    // this O(n²).
                    if seen.insert(id.clone()) {
                        calls.push(id.clone());
                    }
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    answered.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }
    calls.retain(|id| !answered.contains(id));
    calls
}

/// Whether every `tool_use` in `messages` is answered by a `tool_result`
/// and no `tool_result` orphans a missing call — the pairing invariant a
/// provider-acceptable transcript (and every compaction cut) must hold
/// (the general agent's `toolPairingBalanced` counterpart). Compaction paths enforce this
/// via `enforce_tool_call_pairs`; the session-load repair enforces it by
/// appending `TOOL_OUTCOME_UNKNOWN` terminators. This predicate is the
/// shared post-condition both are checked against.
#[must_use]
pub fn tool_call_pairs_balanced(messages: &[Message]) -> bool {
    if !unanswered_tool_use_ids(messages).is_empty() {
        return false;
    }
    let mut calls: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse { id, .. } = block {
                calls.insert(id.clone());
            }
        }
    }
    messages.iter().all(|message| {
        message.content.iter().all(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => calls.contains(tool_use_id),
            _ => true,
        })
    })
}
