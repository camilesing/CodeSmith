//! Heavy compaction engine: planning, summary generation, and the safe retry
//! loop (`should_compact`, `plan_compaction`, `compact_messages_safe`,
//! `create_summary`, `merge_system_prompts`). Moved here from
//! `codesmith-tui::compaction`; the TUI keeps a re-export shim plus the test
//! module, which relies on TUI-local `MockLlmClient` / `HookExecutor`.
//!
//! Test-referenced helpers are `pub` (rather than `pub(crate)`) so the TUI test
//! module can still reach them through the re-export shim while the migration
//! is in progress. This is transitional: once the engine body moves into this
//! crate, the visibility can be tightened again.

use anyhow::Result;
use regex::Regex;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::hooks::{HookContext, HookHost};
use crate::llm_client::LlmClient;
use crate::models::{
    CacheControl, ContentBlock, Message, MessageRequest, SystemBlock, SystemPrompt,
    context_window_for_model,
};
use super::{CompactionConfig, estimate_tokens, estimate_tokens_for_message, session_memory_compact};

pub const KEEP_RECENT_MESSAGES: usize = 4;
pub const RECENT_WORKING_SET_WINDOW: usize = 12;
pub const MAX_WORKING_SET_PATHS: usize = 24;
pub const MIN_SUMMARIZE_MESSAGES: usize = 6;
pub const SUMMARY_TEXT_SNIPPET_CHARS: usize = 800;
pub const SUMMARY_TOOL_RESULT_SNIPPET_CHARS: usize = 240;
pub const SUMMARY_INPUT_MAX_CHARS: usize = 24_000;
pub const SUMMARY_INPUT_HEAD_CHARS: usize = 14_000;
pub const SUMMARY_INPUT_TAIL_CHARS: usize = 6_000;
pub const LARGE_CONTEXT_SUMMARY_TEXT_SNIPPET_CHARS: usize = 2_000;
pub const LARGE_CONTEXT_SUMMARY_TOOL_RESULT_SNIPPET_CHARS: usize = 4_000;
pub const LARGE_CONTEXT_SUMMARY_INPUT_MAX_CHARS: usize = 120_000;
pub const LARGE_CONTEXT_SUMMARY_INPUT_HEAD_CHARS: usize = 72_000;
pub const LARGE_CONTEXT_SUMMARY_INPUT_TAIL_CHARS: usize = 36_000;
pub const TOOL_PRUNE_STOP_CHECK_BYTES: usize = 16 * 1024;
pub const LARGE_CONTEXT_SUMMARY_MAX_TOKENS: u32 = 2_048;
pub const LARGE_CONTEXT_WINDOW_TOKENS: u32 = 500_000;
pub const CACHE_ALIGNED_SUMMARY_CONTEXT_BUDGET_PERCENT: usize = 85;
pub const SUMMARY_PROMPT_TOO_LONG_MAX_RETRIES: u32 = 4;
pub const SUMMARY_PROMPT_TOO_LONG_PEEL_PERCENT: usize = 20;

#[derive(Debug, Clone, Copy)]
pub struct SummaryInputLimits {
    pub text_snippet_chars: usize,
    pub tool_result_snippet_chars: usize,
    pub input_max_chars: usize,
    pub input_head_chars: usize,
    pub input_tail_chars: usize,
    pub max_tokens: u32,
    pub word_limit: usize,
}

pub fn summary_input_limits_for_model(model: &str) -> SummaryInputLimits {
    let is_large_context =
        context_window_for_model(model).is_some_and(|window| window >= LARGE_CONTEXT_WINDOW_TOKENS);
    if is_large_context {
        SummaryInputLimits {
            text_snippet_chars: LARGE_CONTEXT_SUMMARY_TEXT_SNIPPET_CHARS,
            tool_result_snippet_chars: LARGE_CONTEXT_SUMMARY_TOOL_RESULT_SNIPPET_CHARS,
            input_max_chars: LARGE_CONTEXT_SUMMARY_INPUT_MAX_CHARS,
            input_head_chars: LARGE_CONTEXT_SUMMARY_INPUT_HEAD_CHARS,
            input_tail_chars: LARGE_CONTEXT_SUMMARY_INPUT_TAIL_CHARS,
            max_tokens: LARGE_CONTEXT_SUMMARY_MAX_TOKENS,
            word_limit: 900,
        }
    } else {
        SummaryInputLimits {
            text_snippet_chars: SUMMARY_TEXT_SNIPPET_CHARS,
            tool_result_snippet_chars: SUMMARY_TOOL_RESULT_SNIPPET_CHARS,
            input_max_chars: SUMMARY_INPUT_MAX_CHARS,
            input_head_chars: SUMMARY_INPUT_HEAD_CHARS,
            input_tail_chars: SUMMARY_INPUT_TAIL_CHARS,
            max_tokens: 1_024,
            word_limit: 500,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompactionPlan {
    pub pinned_indices: BTreeSet<usize>,
    pub summarize_indices: Vec<usize>,
}

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

pub fn message_text(msg: &Message) -> String {
    let mut text = String::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text: t, .. } => {
                let _ = writeln!(text, "{t}");
            }
            ContentBlock::Thinking { .. } => {}
            ContentBlock::ToolUse { name, input, .. } => {
                let _ = writeln!(text, "[tool_use:{name}] {input}");
            }
            ContentBlock::ToolResult { content, .. } => {
                let _ = writeln!(text, "{content}");
            }
            ContentBlock::Image { source } => {
                let _ = writeln!(text, "[{}]", source.summary());
            }
            ContentBlock::ServerToolUse { .. }
            | ContentBlock::ToolSearchToolResult { .. }
            | ContentBlock::CodeExecutionToolResult { .. } => {}
        }
    }
    text
}

pub fn is_user_text_query(msg: &Message) -> bool {
    msg.role == "user"
        && msg
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { .. }))
}

pub fn extract_paths_from_message(
    message: &Message,
    workspace: Option<&Path>,
) -> Vec<String> {
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

pub fn derive_working_set_paths(
    messages: &[Message],
    workspace: Option<&Path>,
    seed_indices: &[usize],
) -> HashSet<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut seeds: Vec<usize> = seed_indices
        .iter()
        .copied()
        .filter(|idx| *idx < messages.len())
        .collect();
    seeds.sort_unstable_by(|a, b| b.cmp(a));

    for idx in seeds {
        for candidate in extract_paths_from_message(&messages[idx], workspace) {
            if seen.insert(candidate.clone()) {
                paths.push(candidate);
                if paths.len() >= MAX_WORKING_SET_PATHS {
                    return paths.into_iter().collect();
                }
            }
        }
    }

    for msg in messages.iter().rev().take(RECENT_WORKING_SET_WINDOW) {
        for candidate in extract_paths_from_message(msg, workspace) {
            if seen.insert(candidate.clone()) {
                paths.push(candidate);
                if paths.len() >= MAX_WORKING_SET_PATHS {
                    return paths.into_iter().collect();
                }
            }
        }
    }

    paths.into_iter().collect()
}

pub fn should_pin_message(text: &str, working_set_paths: &HashSet<String>) -> bool {
    let lower = text.to_lowercase();

    let mentions_working_set = working_set_paths.iter().any(|p| text.contains(p));
    if mentions_working_set {
        return true;
    }

    let error_markers = [
        "error:",
        "error ",
        "failed",
        "panic",
        "traceback",
        "stack trace",
        "assertion failed",
        "test failed",
    ];
    if error_markers.iter().any(|m| lower.contains(m)) {
        return true;
    }

    let patch_markers = [
        "diff --git",
        "+++ b/",
        "--- a/",
        "*** begin patch",
        "*** update file:",
        "*** add file:",
        "*** delete file:",
        "```diff",
        "apply_patch",
    ];
    patch_markers.iter().any(|m| lower.contains(m))
}

pub fn plan_compaction(
    messages: &[Message],
    workspace: Option<&Path>,
    keep_recent: usize,
    external_pins: Option<&[usize]>,
    external_working_set_paths: Option<&[String]>,
) -> CompactionPlan {
    let mut pinned_indices: BTreeSet<usize> = BTreeSet::new();
    let len = messages.len();
    if len == 0 {
        return CompactionPlan::default();
    }

    // Always pin the tail of the conversation to preserve immediate context.
    let recent_start = len.saturating_sub(keep_recent);
    pinned_indices.extend(recent_start..len);

    // Derive a repo-aware working set from recent messages/tool calls and
    // merge it with any externally provided working-set paths.
    let seed_indices = external_pins.unwrap_or(&[]);
    let mut working_set_paths = derive_working_set_paths(messages, workspace, seed_indices);
    if let Some(paths) = external_working_set_paths {
        for path in paths {
            if let Some(normalized) = normalize_path_candidate(path, workspace) {
                let _ = working_set_paths.insert(normalized);
            }
        }
    }

    for (idx, msg) in messages.iter().enumerate() {
        if pinned_indices.contains(&idx) {
            continue;
        }
        let text = message_text(msg);
        if should_pin_message(&text, &working_set_paths) {
            pinned_indices.insert(idx);
        }
    }

    // External pins are authoritative and should be preserved even if they
    // were not detected by the heuristics above.
    if let Some(pins) = external_pins {
        pinned_indices.extend(pins.iter().copied().filter(|idx| *idx < len));
    }

    // Ensure tool result messages are not kept without their corresponding tool call.
    enforce_tool_call_pairs(messages, &mut pinned_indices);

    // Some OpenAI-compatible chat templates require at least one user text
    // message. Tool-heavy tails can otherwise compact down to only tool calls
    // and tool results, which makes those backends reject the next request.
    if !pinned_indices
        .iter()
        .any(|&idx| is_user_text_query(&messages[idx]))
        && let Some(idx) = messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, msg)| is_user_text_query(msg).then_some(idx))
    {
        pinned_indices.insert(idx);
    }

    let summarize_indices = (0..len)
        .filter(|idx| !pinned_indices.contains(idx))
        .collect();

    // `working_set_paths` was used only for pinning decisions above.
    drop(working_set_paths);

    CompactionPlan {
        pinned_indices,
        summarize_indices,
    }
}

pub fn enforce_tool_call_pairs(messages: &[Message], pinned_indices: &mut BTreeSet<usize>) {
    if pinned_indices.is_empty() {
        return;
    }

    // Build maps: tool_id → message index across ALL messages (not just pinned).
    let mut call_id_to_idx: HashMap<String, usize> = HashMap::new();
    let mut result_id_to_idx: HashMap<String, usize> = HashMap::new();

    for (idx, msg) in messages.iter().enumerate() {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    call_id_to_idx.insert(id.clone(), idx);
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    result_id_to_idx.insert(tool_use_id.clone(), idx);
                }
                _ => {}
            }
        }
    }

    // Fixpoint loop: re-check until stable.
    // Newly pinned messages may introduce new pair requirements;
    // removed messages may orphan their counterparts.
    // Track permanently removed indices so they cannot be re-added
    // by a counterpart in a later iteration (prevents oscillation).
    let mut permanently_removed: HashSet<usize> = HashSet::new();

    let max_iters = messages.len().max(10);
    let mut converged = false;
    for _ in 0..max_iters {
        let mut to_add = Vec::new();
        let mut to_remove = Vec::new();

        let snapshot: Vec<usize> = pinned_indices.iter().copied().collect();

        for idx in snapshot {
            let msg = &messages[idx];
            for block in &msg.content {
                match block {
                    // Pinned result → its call must also be pinned (or remove result)
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        match call_id_to_idx.get(tool_use_id) {
                            Some(&call_idx) if !permanently_removed.contains(&call_idx) => {
                                to_add.push(call_idx);
                            }
                            _ => {
                                to_remove.push(idx);
                            }
                        }
                    }
                    // Pinned call → its result must also be pinned (or remove call)
                    ContentBlock::ToolUse { id, .. } => match result_id_to_idx.get(id) {
                        Some(&result_idx) if !permanently_removed.contains(&result_idx) => {
                            to_add.push(result_idx);
                        }
                        _ => {
                            to_remove.push(idx);
                        }
                    },
                    _ => {}
                }
            }
        }

        // Removals take priority: if a message is both needed and orphaned,
        // remove it now; the fixpoint loop will cascade the orphaning.
        let remove_set: HashSet<usize> = to_remove.iter().copied().collect();
        let mut changed = false;
        for idx in to_add {
            if !remove_set.contains(&idx) && pinned_indices.insert(idx) {
                changed = true;
            }
        }
        for idx in to_remove {
            if pinned_indices.remove(&idx) {
                permanently_removed.insert(idx);
                changed = true;
            }
        }

        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        tracing::warn!("{}", format!(
            "enforce_tool_call_pairs did not converge after {max_iters} iterations \
             ({} messages, {} pinned)",
            messages.len(),
            pinned_indices.len()
        ));
    }
}


pub fn should_compact(
    messages: &[Message],
    config: &CompactionConfig,
    workspace: Option<&Path>,
    external_pins: Option<&[usize]>,
    external_working_set_paths: Option<&[String]>,
) -> bool {
    if !config.enabled {
        return false;
    }

    // Optional hard floor enforcement. The provider-neutral default is zero so
    // 128K/200K providers can auto-compact before hard context rejection. Manual
    // `/compact` and the `compact_now` tool bypass this floor by going through
    // different code paths.
    if config.auto_floor_tokens > 0 {
        let total_session_tokens: usize = messages
            .iter()
            .map(|m| estimate_tokens_for_message(m, false))
            .sum();
        if total_session_tokens < config.auto_floor_tokens {
            return false;
        }
    }

    let plan = plan_compaction(
        messages,
        workspace,
        KEEP_RECENT_MESSAGES,
        external_pins,
        external_working_set_paths,
    );
    let pinned_tokens: usize = plan
        .pinned_indices
        .iter()
        .map(|&idx| estimate_tokens_for_message(&messages[idx], false))
        .sum();

    let token_estimate: usize = plan
        .summarize_indices
        .iter()
        .map(|&idx| estimate_tokens_for_message(&messages[idx], false))
        .sum();
    let message_count = plan.summarize_indices.len();

    // Pinned messages consume part of the budget, so compact earlier when needed.
    let effective_token_threshold = config.token_threshold.saturating_sub(pinned_tokens);

    // Token-only trigger (v0.8.11): the prior message-count branch was a
    // 128K-era heuristic that fired compaction on long chats of small
    // messages — exactly the case where rewriting the V4 prefix cache is
    // most wasteful. Token budget is the only signal that maps to actual
    // model context pressure.
    if effective_token_threshold == 0 {
        return message_count >= MIN_SUMMARIZE_MESSAGES;
    }
    if message_count < MIN_SUMMARIZE_MESSAGES {
        return false;
    }
    token_estimate > effective_token_threshold
}

pub fn truncate_chars(text: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

pub fn tail_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }
    let start_char = total_chars.saturating_sub(max_chars);
    let start_idx = text
        .char_indices()
        .nth(start_char)
        .map_or(0, |(idx, _)| idx);
    text[start_idx..].to_string()
}

#[derive(Debug, Clone)]
pub struct ToolUseInfo {
    name: String,
    key: String,
    args_preview: String,
}

pub fn tool_use_key(name: &str, input: &serde_json::Value) -> String {
    format!(
        "{name}:{}",
        serde_json::to_string(input).unwrap_or_else(|_| input.to_string())
    )
}

pub fn tool_args_preview(input: &serde_json::Value) -> String {
    let raw = serde_json::to_string(input).unwrap_or_else(|_| input.to_string());
    truncate_chars(&raw, 120).to_string()
}

pub fn collect_tool_uses(messages: &[Message]) -> HashMap<String, ToolUseInfo> {
    let mut tool_uses = HashMap::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse {
                id, name, input, ..
            } = block
            {
                tool_uses.insert(
                    id.clone(),
                    ToolUseInfo {
                        name: name.clone(),
                        key: tool_use_key(name, input),
                        args_preview: tool_args_preview(input),
                    },
                );
            }
        }
    }
    tool_uses
}

pub struct ToolResultPruneCandidate {
    message_idx: usize,
    block_idx: usize,
    key: String,
    tool_name: String,
    args_preview: String,
    original_len: usize,
}

/// Mechanically prune old verbose tool results before paying for an LLM summary.
///
/// The most recent `protected_window` messages stay byte-for-byte intact. Older
/// duplicate tool results keep the freshest full body and replace earlier
/// copies with one-line summaries; non-duplicate old results are summarized only
/// when they exceed the normal summary snippet size.
pub fn prune_tool_results_until<F>(
    messages: &mut [Message],
    protected_window: usize,
    mut should_stop: F,
) -> usize
where
    F: FnMut(&[Message], usize) -> bool,
{
    let cutoff = messages.len().saturating_sub(protected_window);
    if cutoff == 0 {
        return 0;
    }

    let tool_uses = collect_tool_uses(messages);
    let mut candidates = Vec::new();
    let mut latest_by_key: HashMap<String, usize> = HashMap::new();
    let mut count_by_key: HashMap<String, usize> = HashMap::new();

    for (message_idx, message) in messages.iter().take(cutoff).enumerate() {
        for (block_idx, block) in message.content.iter().enumerate() {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = block
            else {
                continue;
            };
            let Some(info) = tool_uses.get(tool_use_id) else {
                continue;
            };
            latest_by_key.insert(info.key.clone(), message_idx);
            *count_by_key.entry(info.key.clone()).or_insert(0) += 1;
            candidates.push(ToolResultPruneCandidate {
                message_idx,
                block_idx,
                key: info.key.clone(),
                tool_name: info.name.clone(),
                args_preview: info.args_preview.clone(),
                original_len: content.len(),
            });
        }
    }

    // The maps above are fully populated before pruning starts, so the order below
    // only changes which message bytes are rewritten first. Pruning from newest to
    // oldest lets callers stop as soon as enough bytes were saved, preserving the
    // earlier JSON request prefix for byte-level KV caches.
    candidates.reverse();

    let mut bytes_saved = 0usize;
    for candidate in candidates {
        let duplicate_count = count_by_key.get(&candidate.key).copied().unwrap_or(0);
        let is_latest_duplicate = duplicate_count > 1
            && latest_by_key.get(&candidate.key) == Some(&candidate.message_idx);
        if is_latest_duplicate {
            continue;
        }
        if duplicate_count <= 1 && candidate.original_len <= SUMMARY_TOOL_RESULT_SNIPPET_CHARS {
            continue;
        }

        let summary = format!(
            "[{}] tool result pruned ({} bytes; args: {})",
            candidate.tool_name, candidate.original_len, candidate.args_preview
        );
        if summary.len() >= candidate.original_len {
            continue;
        }

        if let ContentBlock::ToolResult {
            content,
            content_blocks,
            ..
        } = &mut messages[candidate.message_idx].content[candidate.block_idx]
        {
            bytes_saved = bytes_saved.saturating_add(content.len().saturating_sub(summary.len()));
            *content = summary;
            *content_blocks = None;

            if should_stop(messages, bytes_saved) {
                break;
            }
        }
    }

    bytes_saved
}

/// Result of a compaction operation with metadata.
#[derive(Debug)]
pub struct CompactionResult {
    /// Compacted messages
    pub messages: Vec<Message>,
    /// Summary system prompt
    pub summary_prompt: Option<SystemPrompt>,
    /// Messages that were removed from the active window
    #[allow(dead_code)]
    pub removed_messages: Vec<Message>,
    /// Number of retries used before success
    pub retries_used: u32,
}

/// Check if an error is transient and worth retrying. Categories that map to
/// transient retry: Network, RateLimit, Timeout. Anything else (auth, parse,
/// invalid request, etc.) is permanent and propagates.
pub fn is_transient_error(e: &anyhow::Error) -> bool {
    let category = crate::error_taxonomy::classify_error_message(&e.to_string());
    matches!(
        category,
        crate::error_taxonomy::ErrorCategory::Network
            | crate::error_taxonomy::ErrorCategory::RateLimit
            | crate::error_taxonomy::ErrorCategory::Timeout
    )
}

/// Optional enhancements applied around the LLM compaction retry loop.
///
/// Passed to [`compact_messages_safe`] to enable two Claude-Code-parity
/// behaviors (#485):
///
/// - `hooks`: fire `PreCompact` hooks and merge their stdout
///   ("context to preserve") into the compaction summary so key facts
///   survive summarization.
/// - `session_memory`: try session-memory compaction *first*, using the
///   `MEMORY.md` / Knowledge-on-Demand content as the summary. Returns
///   early (no LLM call) when it clears the compaction threshold.
///
/// The struct is **owned** — it clones the hook handle (`Arc<dyn HookHost>`)
/// and memory content — so the caller is free to mutate session state after
/// the compaction call returns without holding a borrow.
#[derive(Clone, Default)]
pub struct CompactionEnhancements {
    /// `PreCompact` hook executor + context. `None` skips the hook. The
    /// executor is `Arc<dyn HookHost>` so the engine body can assemble this
    /// from a trait-erased host (`HostServices::hooks`) without naming the
    /// concrete (TUI-local) `HookExecutor`.
    pub hooks: Option<(Arc<dyn HookHost>, HookContext)>,
    /// Session-memory sidecar. `None` skips session-memory-first.
    pub session_memory: Option<SessionMemorySidecar>,
}

impl std::fmt::Debug for CompactionEnhancements {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Arc<dyn HookHost>` is not `Debug` — the trait surface stays free of
        // a `Debug` supertrait to match the other host-service contracts — so
        // report hook presence by boolean instead of the inner executor.
        f.debug_struct("CompactionEnhancements")
            .field("hooks", &self.hooks.is_some())
            .field("session_memory", &self.session_memory)
            .finish()
    }
}

/// Memory content + config for session-memory-first compaction.
#[derive(Debug, Clone)]
pub struct SessionMemorySidecar {
    /// Raw memory text (e.g. `MEMORY.md` contents or KoD entrypoint).
    pub memory_content: String,
    /// Retain-budget configuration.
    pub config: session_memory_compact::SessionMemoryCompactConfig,
}

/// Merge hook-provided "context to preserve" into a compaction summary.
///
/// `None`/empty `preserve` leaves `summary` untouched. Otherwise the
/// preserve text is appended as a labeled [`SystemBlock`] so it survives
/// summarization and is visible to the model on the next turn.
pub fn merge_preserve_context(
    summary: Option<SystemPrompt>,
    preserve: Option<&str>,
) -> Option<SystemPrompt> {
    let Some(preserve) = preserve else {
        return summary;
    };
    let preserve = preserve.trim();
    if preserve.is_empty() {
        return summary;
    }
    let block = SystemBlock {
        block_type: "text".to_string(),
        text: format!("## Context to Preserve (PreCompact hook)\n\n{preserve}"),
        cache_control: None,
    };
    match summary {
        None => Some(SystemPrompt::Blocks(vec![block])),
        Some(SystemPrompt::Text(t)) => Some(SystemPrompt::Blocks(vec![
            SystemBlock {
                block_type: "text".to_string(),
                text: t,
                cache_control: None,
            },
            block,
        ])),
        Some(SystemPrompt::Blocks(mut blocks)) => {
            blocks.push(block);
            Some(SystemPrompt::Blocks(blocks))
        }
    }
}

/// Compact messages with retry and backoff for transient errors.
///
/// This function wraps `compact_messages` with retry logic to handle
/// transient network errors and rate limits. It uses exponential backoff
/// with delays of 1s, 2s, 4s between retries.
///
/// # Safety
/// - Never panics
/// - Never corrupts the original messages (returns error instead)
/// - Only retries on transient errors (network, rate limit, etc.)
pub async fn compact_messages_safe(
    client: &dyn LlmClient,
    messages: &[Message],
    config: &CompactionConfig,
    workspace: Option<&Path>,
    external_pins: Option<&[usize]>,
    external_working_set_paths: Option<&[String]>,
    enhancements: Option<&CompactionEnhancements>,
) -> Result<CompactionResult> {
    const MAX_RETRIES: u32 = 3;
    const BASE_DELAY_MS: u64 = 1000;

    let was_over_threshold = should_compact(
        messages,
        config,
        workspace,
        external_pins,
        external_working_set_paths,
    );

    // Fire PreCompact hooks once, up front, so their "context to preserve"
    // is available on every return path (local-prune early return,
    // session-memory early return, and the LLM summary). Hooks are
    // non-blocking: failures log a warning and contribute nothing (#485).
    let preserve_context: Option<String> = enhancements
        .and_then(|e| e.hooks.as_ref())
        .and_then(|(executor, context)| executor.execute_pre_compact_hook(context));

    let mut pruned_messages = messages.to_vec();
    let mut now_under_threshold = false;
    let mut next_stop_check_bytes = 0usize;
    let pruned_bytes = prune_tool_results_until(
        &mut pruned_messages,
        KEEP_RECENT_MESSAGES,
        |candidate_messages, bytes_saved| {
            if !was_over_threshold || bytes_saved < next_stop_check_bytes {
                return false;
            }

            // Stop at the first suffix-side prune check that clears the threshold.
            // The check itself is a full compaction-plan pass, so bound it by saved
            // bytes instead of running it after every candidate in huge sessions.
            next_stop_check_bytes = bytes_saved.saturating_add(TOOL_PRUNE_STOP_CHECK_BYTES);
            now_under_threshold = !should_compact(
                candidate_messages,
                config,
                workspace,
                external_pins,
                external_working_set_paths,
            );
            now_under_threshold
        },
    );
    if was_over_threshold && pruned_bytes > 0 && !now_under_threshold {
        // The throttled in-loop check may skip the exact candidate that clears the
        // budget. Do one final pass so a successful local prune still avoids LLM compaction.
        now_under_threshold = !should_compact(
            &pruned_messages,
            config,
            workspace,
            external_pins,
            external_working_set_paths,
        );
    }

    let compaction_input: &[Message] = if pruned_bytes > 0 {
        tracing::info!("{}", format!(
            "Local tool-result prune saved {pruned_bytes} bytes before LLM compaction"
        ));
        if was_over_threshold && now_under_threshold {
            return Ok(CompactionResult {
                messages: pruned_messages,
                summary_prompt: merge_preserve_context(None, preserve_context.as_deref()),
                removed_messages: Vec::new(),
                retries_used: 0,
            });
        }
        &pruned_messages
    } else {
        messages
    };

    // Session-memory-first: when memory content is available and the
    // conversation exceeds the session-memory threshold, compact using the
    // memory file as the summary — no LLM call. Only returns early when it
    // actually clears the compaction threshold; otherwise fall through to
    // the LLM path which can summarize the full transcript.
    if let Some(sidecar) = enhancements.and_then(|e| e.session_memory.as_ref()) {
        if session_memory_compact::should_use_session_memory_compact(
            &sidecar.memory_content,
            compaction_input,
            &sidecar.config,
        ) {
            let sm = session_memory_compact::session_memory_compact(
                compaction_input,
                &sidecar.memory_content,
                &sidecar.config,
            );
            if sm.removed_count > 0
                && !should_compact(
                    &sm.messages,
                    config,
                    workspace,
                    external_pins,
                    external_working_set_paths,
                )
            {
                tracing::info!("{}", format!(
                    "Session-memory-first compaction removed {} messages without an LLM call",
                    sm.removed_count
                ));
                return Ok(CompactionResult {
                    messages: sm.messages,
                    summary_prompt: merge_preserve_context(
                        sm.summary_prompt,
                        preserve_context.as_deref(),
                    ),
                    removed_messages: Vec::new(),
                    retries_used: 0,
                });
            }
        }
    }

    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            // Exponential backoff: 1s, 2s, 4s
            let delay = Duration::from_millis(BASE_DELAY_MS * (1 << (attempt - 1)));
            tokio::time::sleep(delay).await;
        }

        match compact_messages(
            client,
            compaction_input,
            config,
            workspace,
            external_pins,
            external_working_set_paths,
        )
        .await
        {
            Ok((msgs, prompt, removed, summary_retries)) => {
                return Ok(CompactionResult {
                    messages: msgs,
                    summary_prompt: merge_preserve_context(prompt, preserve_context.as_deref()),
                    removed_messages: removed,
                    retries_used: attempt.saturating_add(summary_retries),
                });
            }
            Err(e) => {
                // Only retry on transient errors
                if !is_transient_error(&e) {
                    return Err(e);
                }
                last_error = Some(e);
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("Compaction failed after {MAX_RETRIES} retries")))
}

pub fn read_workspace_anchors(workspace: Option<&Path>) -> Vec<String> {
    let Some(ws) = workspace else {
        return Vec::new();
    };

    // Prefer .codesmith, fall back to .deepseek
    let primary = ws.join(".codesmith").join("anchors.md");
    let anchors_path = if primary.exists() {
        primary
    } else {
        ws.join(".deepseek").join("anchors.md")
    };
    let Ok(content) = std::fs::read_to_string(anchors_path) else {
        return Vec::new();
    };

    content
        .split("\n---\n")
        .map(str::trim)
        .filter(|anchor| !anchor.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn anchor_summary_section(workspace: Option<&Path>) -> String {
    let anchors = read_workspace_anchors(workspace);
    if anchors.is_empty() {
        return String::new();
    }

    let mut section = String::from(
        "## Pinned Facts (User Anchors)\n\n\
         The following facts were explicitly anchored by the user with `/anchor`. \
         Preserve them across compaction cycles.\n\n",
    );

    for anchor in anchors {
        let _ = writeln!(section, "- {anchor}");
    }

    section.push_str("\n---\n\n");
    section
}

#[derive(Debug, Clone)]
pub struct SummaryResult {
    pub text: String,
    pub retries_used: u32,
}

pub async fn compact_messages(
    client: &dyn LlmClient,
    messages: &[Message],
    config: &CompactionConfig,
    workspace: Option<&Path>,
    external_pins: Option<&[usize]>,
    external_working_set_paths: Option<&[String]>,
) -> Result<(Vec<Message>, Option<SystemPrompt>, Vec<Message>, u32)> {
    if messages.is_empty() {
        return Ok((Vec::new(), None, Vec::new(), 0));
    }

    let plan = plan_compaction(
        messages,
        workspace,
        KEEP_RECENT_MESSAGES,
        external_pins,
        external_working_set_paths,
    );
    if plan.summarize_indices.is_empty() {
        return Ok((messages.to_vec(), None, Vec::new(), 0));
    }

    let to_summarize: Vec<Message> = plan
        .summarize_indices
        .iter()
        .map(|&idx| messages[idx].clone())
        .collect();

    // Create a summary of the unpinned portion of the conversation
    let summary_result = create_summary(client, &to_summarize, &config.model).await?;
    let summary = summary_result.text;

    // Extract workflow context (files touched, tasks in progress, etc.)
    let workflow_context = extract_workflow_context(&to_summarize, workspace);

    let anchors_section = anchor_summary_section(workspace);

    // Build new message list with enhanced summary as system block
    let summary_block = SystemBlock {
        block_type: "text".to_string(),
        text: format!(
            "{anchors_section}\
             ## 📋 Conversation Summary (Auto-Generated)\n\n\
             {summary}\n\n\
             ---\n\n\
             ## 🔍 Workflow Context\n\n\
             {workflow_context}\n\n\
             ---\n\n\
             ## 💡 What to Do Next\n\n\
             You have just resumed from a context compaction. The conversation above was summarized to save space. \
             Review the summary and workflow context, then continue helping the user with their task. \
             If you need more details about the summarized portion, ask the user to clarify.\n\n\
             ---\n\n\
             Pinned messages follow:"
        ),
        cache_control: if config.cache_summary {
            Some(CacheControl {
                cache_type: "ephemeral".to_string(),
            })
        } else {
            None
        },
    };

    let pinned_messages = messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| plan.pinned_indices.contains(&idx).then_some(msg.clone()))
        .collect();

    Ok((
        pinned_messages,
        Some(SystemPrompt::Blocks(vec![summary_block])),
        to_summarize,
        summary_result.retries_used,
    ))
}

pub async fn create_summary(
    client: &dyn LlmClient,
    messages: &[Message],
    model: &str,
) -> Result<SummaryResult> {
    let limits = summary_input_limits_for_model(model);
    let used_cache_aligned = should_use_cache_aligned_summary(model, messages);
    if used_cache_aligned {
        let request = build_cache_aligned_summary_request(model, messages, limits);
        match client.create_message(request).await {
            Ok(response) => {
                crate::cost_status::report(&response.model, &response.usage);
                log_summary_cache_telemetry(true, &response.usage);
                return Ok(SummaryResult {
                    text: extract_summary_text(&response),
                    retries_used: 0,
                });
            }
            Err(err) if is_context_window_error(&err) => {
                tracing::warn!("{}", format!(
                    "Cache-aligned compaction summary exceeded the model context window ({err}); \
                     retrying with bounded formatted summary input"
                ));
            }
            Err(err) => return Err(err),
        }
    }

    create_formatted_summary_with_peel_retry(
        client,
        messages.to_vec(),
        model,
        limits,
        u32::from(used_cache_aligned),
    )
    .await
}

pub async fn create_formatted_summary_with_peel_retry(
    client: &dyn LlmClient,
    mut candidate_messages: Vec<Message>,
    model: &str,
    limits: SummaryInputLimits,
    initial_retries_used: u32,
) -> Result<SummaryResult> {
    let mut retries_used = initial_retries_used;
    loop {
        let request = build_formatted_summary_request(model, &candidate_messages, limits);
        match client.create_message(request).await {
            Ok(response) => {
                crate::cost_status::report(&response.model, &response.usage);
                log_summary_cache_telemetry(false, &response.usage);
                return Ok(SummaryResult {
                    text: extract_summary_text(&response),
                    retries_used,
                });
            }
            Err(err) if is_context_window_error(&err) => {
                if retries_used >= SUMMARY_PROMPT_TOO_LONG_MAX_RETRIES {
                    return Err(err);
                }
                let before = candidate_messages.len();
                let peeled = peel_summary_messages_for_retry(&candidate_messages);
                if peeled.len() >= before {
                    return Err(err);
                }
                retries_used = retries_used.saturating_add(1);
                tracing::warn!("{}", format!(
                    "Formatted compaction summary exceeded the model context window ({err}); \
                     peeled old messages for retry {retries_used}/{} ({} -> {} messages)",
                    SUMMARY_PROMPT_TOO_LONG_MAX_RETRIES,
                    before,
                    peeled.len()
                ));
                candidate_messages = peeled;
            }
            Err(err) => return Err(err),
        }
    }
}

pub fn extract_summary_text(response: &crate::models::MessageResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn peel_summary_messages_for_retry(messages: &[Message]) -> Vec<Message> {
    if messages.len() <= 1 {
        return messages.to_vec();
    }

    let mut pinned = BTreeSet::new();
    let recent_start = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
    pinned.extend(recent_start..messages.len());
    enforce_tool_call_pairs(messages, &mut pinned);

    let removable: Vec<usize> = (0..messages.len())
        .filter(|idx| !pinned.contains(idx))
        .collect();
    if removable.is_empty() {
        return messages.to_vec();
    }

    let remove_count = removable
        .len()
        .saturating_mul(SUMMARY_PROMPT_TOO_LONG_PEEL_PERCENT)
        .div_ceil(100)
        .max(1);
    let remove_set: HashSet<usize> = removable.into_iter().take(remove_count).collect();

    messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| (!remove_set.contains(&idx)).then_some(msg.clone()))
        .collect()
}

pub fn is_context_window_error(e: &anyhow::Error) -> bool {
    let text = e.to_string();
    if crate::error_taxonomy::classify_error_message(&text)
        != crate::error_taxonomy::ErrorCategory::InvalidInput
    {
        return false;
    }

    let lower = text.to_lowercase();
    lower.contains("context")
        || lower.contains("token")
        || lower.contains("prompt is too long")
        || lower.contains("requested")
        || lower.contains("maximum")
}

/// Cache-hit percentage for a compaction summary call.
///
/// Denominator is `input_tokens` (the total prompt size), not
/// `cache_hit + cache_miss`. Some providers populate
/// `prompt_cache_hit_tokens` but not `prompt_cache_miss_tokens` — using
/// the sum as the denominator there reports an inflated 100% even when
/// most of the prompt was uncached. Anchoring on `input_tokens` matches
/// how the rest of the codebase (cost reporting, `/cache`) infers
/// missing miss counts. (#584)
pub fn summary_cache_hit_percent(cache_hit: u32, input_tokens: u32) -> f64 {
    if input_tokens > 0 {
        (f64::from(cache_hit) * 100.0) / f64::from(input_tokens)
    } else {
        0.0
    }
}

/// Emit one `tracing::debug!` event per compaction summary call so the
/// path choice (cache-aligned vs fallback) and the resulting cache-hit
/// rate are observable. Both raw token counts and the percentage are
/// included; on providers that don't return cache-token fields the
/// counts are reported as `0` and the percentage as `0.0`. (#584)
pub fn log_summary_cache_telemetry(used_cache_aligned: bool, usage: &crate::models::Usage) {
    let path = if used_cache_aligned {
        "cache_aligned"
    } else {
        "fallback"
    };
    let cache_hit = usage.prompt_cache_hit_tokens.unwrap_or(0);
    let cache_miss = usage.prompt_cache_miss_tokens.unwrap_or(0);
    let cache_hit_pct = summary_cache_hit_percent(cache_hit, usage.input_tokens);
    tracing::debug!(
        target: "compaction",
        "compaction summary call: path={} prompt_tokens={} cache_hit_tokens={} cache_miss_tokens={} cache_hit_pct={:.1}",
        path,
        usage.input_tokens,
        cache_hit,
        cache_miss,
        cache_hit_pct,
    );
}

/// Decide whether to use the cache-aligned summary path
/// ([`build_cache_aligned_summary_request`]) or the fallback
/// ([`build_formatted_summary_request`]). Returns `true` when both
/// gates hold:
///
/// 1. The model has a known large context window
///    (≥ `LARGE_CONTEXT_WINDOW_TOKENS`, currently V4-scale).
/// 2. Replaying the message prefix plus a ~512-token instruction
///    still fits within `CACHE_ALIGNED_SUMMARY_CONTEXT_BUDGET_PERCENT`
///    of that budget.
///
/// ## Why the two paths produce slightly different prompts (#584)
///
/// The two summary requests are *intentionally* framed differently:
///
/// - **Cache-aligned** replays the original `messages` verbatim
///   with `system: None` and appends the summary instruction as
///   the final `user` turn. The model sees the conversation as if
///   it were its own history. This is what lets the V4 prefix cache
///   hit on the bulk of the request (#572).
/// - **Fallback** reformats the conversation into a flat
///   `User:/Assistant:` transcript inside a single `user` message
///   and adds a "You are a helpful assistant that creates concise
///   conversation summaries." system prompt. The model sees a
///   transcript of someone else's conversation.
///
/// The empirical bar is that V4 produces equivalent summaries
/// either way; the post-#572 review noted this fork is worth
/// documenting but not yet worth unifying. The fallback's
/// external-transcript framing is also more conservative for the
/// older / smaller models the cache-aligned path explicitly
/// excludes, so dropping the system prompt would risk regressing
/// those models without a corresponding gain. If we ever want to
/// unify, land it in a separate PR backed by an A/B summary-quality
/// evaluation rather than as a drive-by cleanup.
///
/// `create_summary` emits a `tracing::debug!` event under
/// `target = "compaction"` after each call so the path choice and
/// cache-hit rate are observable post-deploy without UI surface.
pub fn should_use_cache_aligned_summary(model: &str, messages: &[Message]) -> bool {
    let Some(window) = context_window_for_model(model) else {
        return false;
    };
    if window < LARGE_CONTEXT_WINDOW_TOKENS {
        return false;
    }

    let budget = usize::try_from(window).unwrap_or(usize::MAX)
        * CACHE_ALIGNED_SUMMARY_CONTEXT_BUDGET_PERCENT
        / 100;
    let summary_prompt_tokens = 512usize;
    estimate_tokens(messages).saturating_add(summary_prompt_tokens) <= budget
}

pub fn summary_instruction(word_limit: usize) -> String {
    format!(
        "Summarize the conversation above in a concise but comprehensive way. \
         Preserve key information, decisions made, exact file paths, commands, \
         errors, and tool-result facts needed to continue the work. \
         Tool outputs may be abbreviated only when they are repetitive. \
         Keep it under {word_limit} words."
    )
}

pub fn build_cache_aligned_summary_request(
    model: &str,
    messages: &[Message],
    limits: SummaryInputLimits,
) -> MessageRequest {
    let mut request_messages = messages.to_vec();
    request_messages.push(Message {
        role: "user".to_string(),
        content: vec![ContentBlock::Text {
            text: summary_instruction(limits.word_limit),
            cache_control: None,
        }],
    });

    MessageRequest {
        model: model.to_string(),
        messages: request_messages,
        max_tokens: limits.max_tokens,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: Some(false),
        temperature: Some(0.3),
        top_p: None,
    }
}

pub fn build_formatted_summary_request(
    model: &str,
    messages: &[Message],
    limits: SummaryInputLimits,
) -> MessageRequest {
    // Format messages for summarization
    let mut conversation_text = String::new();
    for msg in messages {
        let role = if msg.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        for block in &msg.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    let snippet = truncate_chars(text, limits.text_snippet_chars);
                    let _ = write!(conversation_text, "{role}: {snippet}\n\n");
                }
                ContentBlock::ToolUse { name, .. } => {
                    let _ = write!(conversation_text, "{role}: [Used tool: {name}]\n\n");
                }
                ContentBlock::ToolResult { content, .. } => {
                    let snippet = truncate_chars(content, limits.tool_result_snippet_chars);
                    let _ = write!(conversation_text, "Tool result: {snippet}\n\n");
                }
                ContentBlock::Thinking { .. } => {
                    // Skip thinking blocks in summary
                }
                ContentBlock::Image { source } => {
                    let _ = write!(conversation_text, "{role}: [attached {}]\n\n", source.summary());
                }
                ContentBlock::ServerToolUse { .. }
                | ContentBlock::ToolSearchToolResult { .. }
                | ContentBlock::CodeExecutionToolResult { .. } => {}
            }
        }
    }

    let conversation_chars = conversation_text.chars().count();
    if conversation_chars > limits.input_max_chars {
        let head = truncate_chars(&conversation_text, limits.input_head_chars).to_string();
        let tail = tail_chars(&conversation_text, limits.input_tail_chars);
        let omitted = conversation_chars
            .saturating_sub(head.chars().count())
            .saturating_sub(tail.chars().count());
        conversation_text =
            format!("{head}\n\n[... {omitted} characters omitted before summary ...]\n\n{tail}");
    }

    MessageRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: format!(
                    "{}\n\n---\n\n{conversation_text}",
                    summary_instruction(limits.word_limit)
                ),
                cache_control: None,
            }],
        }],
        max_tokens: limits.max_tokens,
        system: Some(SystemPrompt::Text(
            "You are a helpful assistant that creates concise conversation summaries.".to_string(),
        )),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: Some(false),
        temperature: Some(0.3),
        top_p: None,
    }
}

/// Extract workflow context from messages (files touched, tasks, etc.)
pub fn extract_workflow_context(messages: &[Message], workspace: Option<&Path>) -> String {
    let mut files_touched: Vec<String> = Vec::new();
    let mut tools_used: Vec<String> = Vec::new();
    let mut tasks_identified: Vec<String> = Vec::new();

    for msg in messages {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    tools_used.push(name.clone());

                    // Extract file paths from tool inputs
                    if let Some(path) = extract_path_from_input(input)
                        && !files_touched.contains(&path)
                    {
                        files_touched.push(path);
                    }
                }
                ContentBlock::Text { text, .. }
                    // Look for task/todo mentions
                    if (text.contains("TODO") || text.contains("task") || text.contains("need to")) => {
                        let task = truncate_chars(text, 200).to_string();
                        if !tasks_identified.contains(&task) {
                            tasks_identified.push(task);
                        }
                    }
                _ => {}
            }
        }
    }

    let mut context = String::new();

    if !files_touched.is_empty() {
        context.push_str("**Files Modified/Read:**\n");
        for file in &files_touched {
            if let Some(ws) = workspace {
                let relative = Path::new(file)
                    .strip_prefix(ws)
                    .unwrap_or(Path::new(file))
                    .display();
                context.push_str(&format!("- `{relative}`\n"));
            } else {
                context.push_str(&format!("- `{file}`\n"));
            }
        }
        context.push('\n');
    }

    if !tools_used.is_empty() {
        context.push_str("**Tools Used:** ");
        context.push_str(&tools_used.join(", "));
        context.push_str("\n\n");
    }

    if !tasks_identified.is_empty() {
        context.push_str("**Tasks/TODOs Identified:**\n");
        for task in &tasks_identified {
            context.push_str(&format!("- {task}\n"));
        }
        context.push('\n');
    }

    if context.is_empty() {
        context.push_str("No specific workflow context detected. Continue assisting the user with their current task.\n");
    }

    context
}

/// Extract file path from tool input JSON
pub fn extract_path_from_input(input: &serde_json::Value) -> Option<String> {
    // Try common path field names
    for key in ["path", "file", "file_path", "filename"] {
        if let Some(path) = input.get(key).and_then(|v| v.as_str()) {
            return Some(path.to_string());
        }
    }

    // Try to find path in nested objects
    if let Some(obj) = input.as_object() {
        for (_, value) in obj {
            if let Some(path) = value.as_str()
                && (path.contains('/') || path.contains('\\') || path.contains('.'))
            {
                return Some(path.to_string());
            }
        }
    }

    None
}

pub fn merge_system_prompts(
    original: Option<&SystemPrompt>,
    summary: Option<SystemPrompt>,
) -> Option<SystemPrompt> {
    match (original, summary) {
        (None, None) => None,
        (Some(orig), None) => Some(orig.clone()),
        (None, Some(sum)) => Some(sum),
        (Some(SystemPrompt::Text(orig_text)), Some(SystemPrompt::Blocks(mut sum_blocks))) => {
            // Prepend original system prompt
            sum_blocks.insert(
                0,
                SystemBlock {
                    block_type: "text".to_string(),
                    text: orig_text.clone(),
                    cache_control: None,
                },
            );
            Some(SystemPrompt::Blocks(sum_blocks))
        }
        (Some(SystemPrompt::Blocks(orig_blocks)), Some(SystemPrompt::Blocks(mut sum_blocks))) => {
            // Prepend original blocks
            for (i, block) in orig_blocks.iter().enumerate() {
                sum_blocks.insert(i, block.clone());
            }
            Some(SystemPrompt::Blocks(sum_blocks))
        }
        (Some(orig), Some(SystemPrompt::Text(sum_text))) => {
            let mut blocks = match orig {
                SystemPrompt::Text(t) => vec![SystemBlock {
                    block_type: "text".to_string(),
                    text: t.clone(),
                    cache_control: None,
                }],
                SystemPrompt::Blocks(b) => b.clone(),
            };
            blocks.push(SystemBlock {
                block_type: "text".to_string(),
                text: sum_text,
                cache_control: None,
            });
            Some(SystemPrompt::Blocks(blocks))
        }
    }
}
