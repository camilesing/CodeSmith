//! Chat-completions prompt-construction and cache-inspection helpers.
//!
//! DeepSeek traffic now routes through the rig-based provider adapter; this
//! submodule retains only the prompt-building primitives (`PromptBuilder`,
//! `build_chat_messages_with_reasoning`), the cache-inspection surface
//! (`inspect_prompt_for_request`, `build_cache_warmup_request`,
//! `inspect_wire_request` and their supporting types), and the tool-result /
//! turn-meta wire-compaction helpers those roots depend on.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::logging;
use crate::models::{ContentBlock, Message, MessageRequest, SystemPrompt, Tool};
use crate::prompt_runtime::{
    PromptCachePolicy, PromptSectionStability, parse_rendered_sections, system_prompt_to_text,
};

use super::{system_to_instructions, to_api_tool_name};

#[cfg(test)]
pub(super) fn build_chat_messages_for_request(request: &MessageRequest) -> Vec<Value> {
    PromptBuilder::for_request(request).build()
}
pub(crate) fn inspect_prompt_for_request(request: &MessageRequest) -> PromptInspection {
    PromptBuilder::for_request(request).inspect()
}

pub(crate) fn build_cache_warmup_request(request: &MessageRequest) -> MessageRequest {
    PromptBuilder::for_request(request).build_cache_warmup_request()
}

struct PromptBuilder<'a> {
    system: Option<&'a SystemPrompt>,
    messages: &'a [Message],
    tools: Option<&'a [Tool]>,
    model: &'a str,
    reasoning_effort: Option<&'a str>,
}

impl<'a> PromptBuilder<'a> {
    fn for_request(request: &'a MessageRequest) -> Self {
        Self {
            system: request.system.as_ref(),
            messages: &request.messages,
            tools: request.tools.as_deref(),
            model: &request.model,
            reasoning_effort: request.reasoning_effort.as_deref(),
        }
    }

    #[cfg(test)]
    fn build(self) -> Vec<Value> {
        build_chat_messages_with_reasoning(
            self.system,
            self.messages,
            self.model,
            should_replay_reasoning_content(self.model, self.reasoning_effort),
            false,
        )
    }

    fn inspect(self) -> PromptInspection {
        let messages = build_chat_messages_with_reasoning(
            self.system,
            self.messages,
            self.model,
            should_replay_reasoning_content(self.model, self.reasoning_effort),
            true,
        );
        inspect_wire_request(self.tools, &messages)
    }

    fn build_cache_warmup_request(self) -> MessageRequest {
        let system = stable_system_prompt(self.system);
        let mut messages = stable_history_messages(self.messages);
        let tools = self
            .tools
            .filter(|tools| !tools.is_empty())
            .map(<[Tool]>::to_vec);
        let tool_choice = tools.as_ref().map(|_| json!("none"));
        messages.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: CACHE_WARMUP_USER_TAIL.to_string(),
                cache_control: None,
            }],
        });

        MessageRequest {
            model: self.model.to_string(),
            messages,
            max_tokens: 8,
            system,
            tools,
            tool_choice,
            metadata: None,
            thinking: None,
            reasoning_effort: self.reasoning_effort.map(str::to_string),
            stream: None,
            temperature: Some(0.0),
            top_p: None,
        }
    }
}

pub(crate) const CACHE_WARMUP_USER_TAIL: &str = "请只回复 OK";
const TOOL_RESULT_SENT_CHAR_BUDGET: usize = 12_000;
const TOOL_RESULT_HEAD_CHARS: usize = 4_000;
const TOOL_RESULT_TAIL_CHARS: usize = 4_000;
/// Tool results shorter than this stay inline even when repeated. The
/// extra prompt bytes are cheaper than forcing the model through an
/// unnecessary retrieval hop for tiny command outputs.
const TOOL_RESULT_DEDUP_MIN_CHARS: usize = 1_024;
/// Tool results shorter than this are also exempt from disk persistence —
/// no SHA file is written. The wire-dedup path won't fire for them
/// anyway (see `TOOL_RESULT_DEDUP_MIN_CHARS`), so there's no retrieval
/// burden to satisfy. Keeps `~/.deepseek/tool_outputs/` from filling
/// up with tiny `gh auth status` and `cat package.json` files.
const TOOL_RESULT_SHA_PERSIST_MIN_CHARS: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PromptInspection {
    pub base_static_prefix_hash: String,
    pub full_request_prefix_hash: String,
    /// Hash of the rendered tool catalog JSON, or empty when no tools were supplied.
    pub tool_catalog_hash: String,
    pub layers: Vec<PromptLayerInspection>,
}

/// Identifies the stable prefix that a cache warmup primes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CacheWarmupKey {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub static_prefix_hash: String,
    pub tool_catalog_hash: String,
    pub project_pack_hash: String,
    pub skills_hash: String,
}

impl CacheWarmupKey {
    pub(crate) fn from_inspection(
        provider: &str,
        model: &str,
        base_url: &str,
        inspection: &PromptInspection,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: base_url.to_string(),
            static_prefix_hash: inspection.base_static_prefix_hash.clone(),
            tool_catalog_hash: inspection.tool_catalog_hash.clone(),
            project_pack_hash: layer_hash(inspection, "Project context pack"),
            skills_hash: layer_hash(inspection, "Skills"),
        }
    }

    pub(crate) fn hash_short(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let hash = sha256_hex(json.as_bytes());
        hash[..hash.len().min(12)].to_string()
    }
}

fn layer_hash(inspection: &PromptInspection, name: &str) -> String {
    inspection
        .layers
        .iter()
        .find(|layer| layer.name == name)
        .map(|layer| layer.sha256.clone())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PromptLayerInspection {
    pub name: String,
    pub stability: PromptLayerStability,
    pub char_len: usize,
    pub byte_len: usize,
    /// Rough token estimate for quick before/after cache-hit reports.
    pub token_estimate: usize,
    pub sha256: String,
    pub tool_result: Option<ToolResultInspection>,
    pub turn_meta: Option<TurnMetaInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolResultInspection {
    pub original_chars: usize,
    pub sent_chars: usize,
    pub truncated: bool,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TurnMetaInspection {
    pub original_chars: usize,
    pub sent_chars: usize,
    pub deduplicated: bool,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PromptLayerStability {
    Static,
    History,
    Dynamic,
}

impl PromptLayerStability {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::History => "history",
            Self::Dynamic => "dynamic",
        }
    }
}

fn inspect_wire_request(tools: Option<&[Tool]>, messages: &[Value]) -> PromptInspection {
    let mut layers = Vec::new();
    let mut base_static_prefix_parts = Vec::new();
    let mut full_request_prefix_parts = Vec::new();
    let mut tool_catalog_hash = String::new();
    let mut start_index = 0;

    if let Some(message) = messages.first() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let content = message_content_for_inspect(message);
        if role == "system" {
            for (name, stability, body) in split_system_layers(&content) {
                if stability == PromptLayerStability::Static {
                    base_static_prefix_parts.push(body.to_string());
                }
                if stability != PromptLayerStability::Dynamic {
                    full_request_prefix_parts.push(body.to_string());
                }
                layers.push(prompt_layer(name, stability, body));
            }
            start_index = 1;
        }
    }

    if let Some(tool_catalog) = tool_catalog_for_inspect(tools) {
        tool_catalog_hash = sha256_hex(tool_catalog.as_bytes());
        base_static_prefix_parts.push(tool_catalog.clone());
        full_request_prefix_parts.push(tool_catalog.clone());
        layers.push(prompt_layer(
            "Tool catalog".to_string(),
            PromptLayerStability::Static,
            &tool_catalog,
        ));
    }

    for (index, message) in messages.iter().enumerate().skip(start_index) {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let content = message_content_for_inspect(message);
        let is_last = index + 1 == messages.len();
        let stability = if (is_last && role == "user") || role == "tool" {
            PromptLayerStability::Dynamic
        } else {
            PromptLayerStability::History
        };
        let name = if is_last && role == "user" {
            "User task".to_string()
        } else {
            format!("Message #{index} {role}")
        };
        if stability != PromptLayerStability::Dynamic {
            full_request_prefix_parts.push(content.clone());
        }
        let mut layer = prompt_layer(name, stability, &content);
        layer.tool_result = tool_result_inspection_for_message(message);
        layer.turn_meta = turn_meta_inspection_for_message(message);
        layers.push(layer);
    }

    let base_static_prefix = base_static_prefix_parts.join("\n");
    let full_request_prefix = full_request_prefix_parts.join("\n");

    PromptInspection {
        base_static_prefix_hash: sha256_hex(base_static_prefix.as_bytes()),
        full_request_prefix_hash: sha256_hex(full_request_prefix.as_bytes()),
        tool_catalog_hash,
        layers,
    }
}

fn tool_catalog_for_inspect(tools: Option<&[Tool]>) -> Option<String> {
    let tools = tools.filter(|tools| !tools.is_empty())?;
    serde_json::to_string(&tools.iter().map(tool_to_chat).collect::<Vec<_>>()).ok()
}

fn message_content_for_inspect(message: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(content) = message.get("content").and_then(Value::as_str)
        && !content.is_empty()
    {
        parts.push(content.to_string());
    }
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        parts.push(reasoning.to_string());
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        parts.push(tool_calls.to_string());
    }
    parts.join("\n")
}

fn tool_result_inspection_for_message(message: &Value) -> Option<ToolResultInspection> {
    if message.get("role").and_then(Value::as_str) != Some("tool") {
        return None;
    }
    let budget = message.get("_tool_result_budget")?;
    Some(ToolResultInspection {
        original_chars: budget
            .get("original_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        sent_chars: budget
            .get("sent_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        truncated: budget
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        deduplicated: budget
            .get("deduplicated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn turn_meta_inspection_for_message(message: &Value) -> Option<TurnMetaInspection> {
    let budget = message.get("_turn_meta_budget")?;
    Some(TurnMetaInspection {
        original_chars: budget
            .get("original_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        sent_chars: budget
            .get("sent_chars")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())?,
        deduplicated: budget
            .get("deduplicated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        sha256: budget
            .get("sha256")
            .and_then(Value::as_str)
            .map(str::to_string)?,
    })
}

fn split_system_layers(content: &str) -> Vec<(String, PromptLayerStability, &str)> {
    if let Some(sections) = parse_rendered_sections(content) {
        return sections
            .into_iter()
            .map(|section| {
                let name = section
                    .title
                    .or(section.id)
                    .unwrap_or_else(|| "System prompt section".to_string());
                let stability =
                    prompt_layer_stability_for_section(section.stability, section.cache_policy);
                (name, stability, section.body)
            })
            .collect();
    }

    let markers = [
        ("Project context", "<project_instructions"),
        ("Project context pack", "## Project Context Pack"),
        ("Environment", "## Environment"),
        ("Configured instructions", "<instructions "),
        ("User memory", "## User Memory"),
        ("Current session goal", "## Current Session Goal"),
        ("Skills", "## Skills"),
        ("Context management", "## Context Management"),
        ("Compact template", "## Compact"),
        ("Previous session relay", "## Previous Session Relay"),
    ];

    let mut starts: Vec<(usize, &str)> = markers
        .iter()
        .filter_map(|(name, marker)| content.find(marker).map(|idx| (idx, *name)))
        .collect();
    starts.sort_by_key(|(idx, _)| *idx);

    let mut layers = Vec::new();
    let first_marker = starts.first().map_or(content.len(), |(idx, _)| *idx);
    if first_marker > 0 {
        layers.push((
            "Global system prefix".to_string(),
            PromptLayerStability::Static,
            content[..first_marker].trim(),
        ));
    }

    for (i, (start, name)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(content.len(), |(idx, _)| *idx);
        let stability = if *name == "Previous session relay" {
            PromptLayerStability::Dynamic
        } else if is_static_base_layer(name) {
            PromptLayerStability::Static
        } else {
            PromptLayerStability::History
        };
        layers.push(((*name).to_string(), stability, content[*start..end].trim()));
    }

    if layers.is_empty() {
        layers.push((
            "Global system prefix".to_string(),
            PromptLayerStability::Static,
            content.trim(),
        ));
    }
    layers
}

fn prompt_layer_stability_for_section(
    stability: Option<PromptSectionStability>,
    cache_policy: Option<PromptCachePolicy>,
) -> PromptLayerStability {
    if cache_policy.is_some_and(|policy| policy != PromptCachePolicy::Cacheable) {
        return PromptLayerStability::Dynamic;
    }
    match stability {
        Some(PromptSectionStability::Static | PromptSectionStability::Workspace) => {
            PromptLayerStability::Static
        }
        Some(PromptSectionStability::Session) => PromptLayerStability::History,
        Some(PromptSectionStability::Dynamic) => PromptLayerStability::Dynamic,
        None => PromptLayerStability::History,
    }
}

fn is_static_base_layer(name: &str) -> bool {
    matches!(
        name,
        "Global system prefix"
            | "Environment"
            | "Skills"
            | "Project context"
            | "Project context pack"
            | "Context management"
            | "Compact template"
    )
}

fn stable_system_prompt(system: Option<&SystemPrompt>) -> Option<SystemPrompt> {
    let instructions = system
        .as_ref()
        .and_then(|system| system_prompt_to_text(system))?;
    let stable = split_system_layers(&instructions)
        .into_iter()
        .filter_map(|(_, stability, body)| {
            (stability == PromptLayerStability::Static).then_some(body)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if stable.trim().is_empty() {
        None
    } else {
        Some(SystemPrompt::Text(stable))
    }
}

fn stable_history_messages(messages: &[Message]) -> Vec<Message> {
    let mut end = messages.len();
    if messages
        .last()
        .is_some_and(|message| message.role.as_str() == "user")
    {
        end = end.saturating_sub(1);
    }
    messages[..end].to_vec()
}

fn prompt_layer(
    name: String,
    stability: PromptLayerStability,
    content: &str,
) -> PromptLayerInspection {
    let char_len = content.chars().count();
    let token_estimate = if char_len == 0 {
        0
    } else if content.is_ascii() {
        (char_len / 4).max(1)
    } else {
        char_len.max(1)
    };
    PromptLayerInspection {
        name,
        stability,
        char_len,
        byte_len: content.len(),
        token_estimate,
        sha256: sha256_hex(content.as_bytes()),
        tool_result: None,
        turn_meta: None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Persist a SHA-addressed copy of `content` to
/// `~/.deepseek/tool_outputs/sha_<sha>.txt` so the model can retrieve
/// the original bytes after the wire-dedup compactor has replaced
/// later occurrences with a `<TOOL_RESULT_REF sha="..." />` block.
///
/// Returns `true` when the persist succeeded (or the content is
/// below `TOOL_RESULT_SHA_PERSIST_MIN_CHARS` — there's no retrieval
/// need to satisfy). Returns `false` when the write failed and the
/// caller MUST skip dedup, because emitting a SHA ref the model
/// can't retrieve is worse than inlining the content twice. The
/// no-home-dir edge case (InvalidInput) is treated as a real
/// failure: we can't promise retrieval works without a writable
/// store.
fn persist_tool_result_for_sha(sha: &str, content: &str) -> bool {
    if content.chars().count() < TOOL_RESULT_SHA_PERSIST_MIN_CHARS {
        return true;
    }
    match crate::tools::truncate::write_sha_spillover(sha, content) {
        Ok(_) => true,
        Err(err) => {
            logging::warn(format!(
                "tool-result SHA spillover write failed for sha={sha}: {err} — dedup skipped"
            ));
            false
        }
    }
}

#[derive(Clone)]
struct PendingToolCallInfo {
    tool_name: String,
    input: Value,
}

struct SeenToolResult {
    message_label: String,
    original_chars: usize,
}

struct WireToolResult {
    content: String,
    original_chars: usize,
    sent_chars: usize,
    truncated: bool,
    deduplicated: bool,
}

#[derive(Clone)]
struct TurnMetaBudget {
    original_chars: usize,
    sent_chars: usize,
    deduplicated: bool,
    sha256: String,
}

struct LastFullTurnMeta {
    sha256: String,
}

fn render_turn_meta_for_wire(
    text: &str,
    last_full_turn_meta: &mut Option<LastFullTurnMeta>,
) -> (String, TurnMetaBudget) {
    let original_chars = text.chars().count();
    let sha = sha256_hex(text.as_bytes());

    if last_full_turn_meta
        .as_ref()
        .is_some_and(|previous| previous.sha256 == sha)
    {
        // Keep the repeated metadata slot short without surfacing an
        // opaque hash the model cannot resolve.
        let rendered = "<turn_meta_unchanged />".to_string();
        let budget = TurnMetaBudget {
            original_chars,
            sent_chars: rendered.chars().count(),
            deduplicated: true,
            sha256: sha,
        };
        return (rendered, budget);
    }

    *last_full_turn_meta = Some(LastFullTurnMeta {
        sha256: sha.clone(),
    });
    (
        text.to_string(),
        TurnMetaBudget {
            original_chars,
            sent_chars: original_chars,
            deduplicated: false,
            sha256: sha,
        },
    )
}

fn is_turn_meta_text(text: &str) -> bool {
    text.trim_start().starts_with("<turn_meta>")
}

fn turn_meta_budget_json(turn_meta: &TurnMetaBudget) -> Value {
    json!({
        "original_chars": turn_meta.original_chars,
        "sent_chars": turn_meta.sent_chars,
        "deduplicated": turn_meta.deduplicated,
        "sha256": turn_meta.sha256,
    })
}

/// Mutating/write tools whose result body is a *confirmation* (it embeds
/// the unified diff + summary of what was just written), not retrievable
/// reference data. Two identical large `write_file` calls must each keep
/// their full confirmation inline: collapsing the later one to a
/// `<TOOL_RESULT_REF sha="..." />` makes the model lose the write-success
/// context and behave as if the file is missing (issue #1695). Read-style
/// tools (`read_file`, `grep_files`, `exec_shell`, …) are unaffected and
/// still dedup normally.
fn is_mutation_tool(tool_name: &str) -> bool {
    matches!(tool_name, "write_file" | "edit_file" | "apply_patch")
}

fn compact_tool_result_for_wire(
    tool_name: &str,
    input: &Value,
    content: &str,
    message_label: &str,
    seen_tool_results: &mut HashMap<String, SeenToolResult>,
) -> WireToolResult {
    let original_chars = content.chars().count();
    let sha = sha256_hex(content.as_bytes());

    // Two independent size-and-kind predicates, deliberately decoupled:
    //
    // * `persist_eligible` — size only. Any large result (including a
    //   mutation tool's big diff) is written to the SHA-addressed store
    //   so that, if it gets truncated below, the elided middle stays
    //   retrievable via `retrieve_tool_result`. Mutation tools must NOT
    //   be excluded here: a >12k-char `write_file` diff that we truncate
    //   without persisting would leave the model unable to recover it.
    // * `dedup_eligible` — size AND non-mutation. Only this predicate
    //   gates collapsing a later identical result to a
    //   `<TOOL_RESULT_REF>`. Mutation-tool results are write
    //   *confirmations*, never dedup-eligible (#1695): two identical
    //   large `write_file` calls must each keep their full confirmation
    //   inline.
    //
    // Below the threshold, repeating the content is safer than asking
    // the model to chase a reference, and there's no retrieval burden to
    // satisfy, so both predicates are false.
    let persist_eligible = original_chars >= TOOL_RESULT_DEDUP_MIN_CHARS;
    let dedup_eligible = persist_eligible && !is_mutation_tool(tool_name);

    if dedup_eligible && let Some(previous) = seen_tool_results.get(&sha) {
        // Re-check persistence before emitting a ref. If the file is
        // already present this is a cheap no-op; if the write now fails,
        // inline the content rather than producing an orphan reference.
        if !persist_tool_result_for_sha(&sha, content) {
            return WireToolResult {
                content: content.to_string(),
                original_chars,
                sent_chars: original_chars,
                truncated: false,
                deduplicated: false,
            };
        }
        let content = format!(
            "<TOOL_RESULT_REF sha=\"{sha}\" original_message=\"{label}\" chars=\"{chars}\">\n\
             retrieve: retrieve_tool_result ref=sha:{sha}\n\
             </TOOL_RESULT_REF>",
            label = previous.message_label,
            chars = previous.original_chars,
        );
        return WireToolResult {
            sent_chars: content.chars().count(),
            content,
            original_chars,
            truncated: false,
            deduplicated: true,
        };
    }

    if persist_eligible {
        // Persist any large result so a later truncation below stays
        // retrievable by SHA — this includes mutation tools, whose big
        // diffs are NOT dedup-eligible but still must be recoverable
        // when elided. Only register the SHA as dedup-able (eligible to
        // be replaced by a back-reference later) when `dedup_eligible`:
        // if the write fails, skip registration so later occurrences
        // stay inline instead of pointing at a file that was never
        // created.
        let persisted = persist_tool_result_for_sha(&sha, content);
        if persisted && dedup_eligible {
            seen_tool_results.insert(
                sha.clone(),
                SeenToolResult {
                    message_label: message_label.to_string(),
                    original_chars,
                },
            );
        }
    }

    if original_chars <= TOOL_RESULT_SENT_CHAR_BUDGET {
        return WireToolResult {
            content: content.to_string(),
            original_chars,
            sent_chars: original_chars,
            truncated: false,
            deduplicated: false,
        };
    }

    let head = first_chars(content, TOOL_RESULT_HEAD_CHARS);
    let tail = last_chars(content, TOOL_RESULT_TAIL_CHARS);
    let kept = head.chars().count() + tail.chars().count();
    let omitted = original_chars.saturating_sub(kept);
    let compacted = format!(
        "[TOOL_RESULT_TRUNCATED]\n\
         tool_name: {tool_name}\n\
         command_or_query: {}\n\
         exit_status: {}\n\
         original_chars: {original_chars}\n\
         sha256: {sha}\n\
         first_chars:\n\
         {head}\n\n\
         [... truncated {omitted} chars from middle ...]\n\n\
         last_chars:\n\
         {tail}",
        tool_command_or_query(input),
        tool_exit_status(content)
    );

    WireToolResult {
        sent_chars: compacted.chars().count(),
        content: compacted,
        original_chars,
        truncated: true,
        deduplicated: false,
    }
}

fn tool_command_or_query(input: &Value) -> String {
    for key in ["command", "cmd", "query", "q", "pattern", "path", "url"] {
        if let Some(value) = input.get(key) {
            return summarize_for_metadata(value, 500);
        }
    }
    summarize_for_metadata(input, 500)
}

fn tool_exit_status(content: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(content) {
        for key in ["exit_code", "exit_status", "status", "code"] {
            if let Some(value) = value.get(key) {
                return summarize_for_metadata(value, 120);
            }
        }
    }

    for line in content.lines().take(20) {
        let trimmed = line.trim();
        for prefix in ["Exit code:", "exit code:", "Exit status:", "exit status:"] {
            if let Some(value) = trimmed.strip_prefix(prefix) {
                return value.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

fn summarize_for_metadata(value: &Value, max_chars: usize) -> String {
    let raw = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    let mut summarized = first_chars(&raw.replace('\n', "\\n"), max_chars);
    if raw.chars().count() > max_chars {
        summarized.push_str("...");
    }
    summarized
}

fn first_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

fn last_chars(value: &str, count: usize) -> String {
    let mut chars: Vec<char> = value.chars().rev().take(count).collect();
    chars.reverse();
    chars.into_iter().collect()
}

fn build_chat_messages_with_reasoning(
    system: Option<&SystemPrompt>,
    messages: &[Message],
    _model: &str,
    include_reasoning: bool,
    include_tool_budget_metadata: bool,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pending_tool_calls: HashMap<String, PendingToolCallInfo> = HashMap::new();
    let mut seen_tool_results: HashMap<String, SeenToolResult> = HashMap::new();
    let mut last_full_turn_meta: Option<LastFullTurnMeta> = None;

    if let Some(instructions) = system_to_instructions(system.cloned())
        && !instructions.trim().is_empty()
    {
        out.push(json!({
            "role": "system",
            "content": instructions,
        }));
    }

    for (message_index, message) in messages.iter().enumerate() {
        let role = message.role.as_str();
        let mut text_parts = Vec::new();
        let mut thinking_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut tool_call_infos = Vec::new();
        let mut tool_results: Vec<(String, String, String)> = Vec::new();
        let mut turn_meta_budget: Option<TurnMetaBudget> = None;

        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    if is_turn_meta_text(text) {
                        let (rendered, budget) =
                            render_turn_meta_for_wire(text, &mut last_full_turn_meta);
                        text_parts.push(rendered);
                        turn_meta_budget = Some(budget);
                    } else {
                        text_parts.push(text.clone());
                    }
                }
                ContentBlock::Thinking { thinking } => thinking_parts.push(thinking.clone()),
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    caller,
                    ..
                } => {
                    let args = serde_json::to_string(input).unwrap_or_else(|_| input.to_string());
                    let mut call = json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": to_api_tool_name(name),
                            "arguments": args,
                        }
                    });
                    if let Some(caller) = caller {
                        call["caller"] = json!({
                            "type": caller.caller_type,
                            "tool_id": caller.tool_id,
                        });
                    }
                    tool_calls.push(call);
                    tool_call_infos.push((
                        id.clone(),
                        PendingToolCallInfo {
                            tool_name: name.clone(),
                            input: input.clone(),
                        },
                    ));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let message_label = format!("Message #{message_index}");
                    tool_results.push((tool_use_id.clone(), content.clone(), message_label));
                }
                ContentBlock::ServerToolUse { .. }
                | ContentBlock::ToolSearchToolResult { .. }
                | ContentBlock::CodeExecutionToolResult { .. } => {}
            }
        }

        if role == "assistant" {
            let content = text_parts.join("\n");
            let mut reasoning_content = thinking_parts.join("\n");
            let has_text = !content.trim().is_empty();
            let has_tool_calls = !tool_calls.is_empty();
            // Reasoning replay must be a function of the stored message ONLY,
            // never of later history. DeepSeek's prefix cache hashes the raw
            // bytes of every message; flipping `reasoning_content` on/off
            // depending on whether a follow-up user turn exists rewrites a
            // historical message between turns and busts the cache from that
            // point onwards. Always emit `reasoning_content` when the model
            // requires replay AND the stored message carries thinking text.
            // Tool-call messages with empty thinking still need a placeholder
            // (DeepSeek 400s without it), but text-only assistant messages
            // simply omit the field when there's nothing to replay.
            let mut has_reasoning = include_reasoning && !reasoning_content.trim().is_empty();
            if include_reasoning && has_tool_calls && !has_reasoning {
                logging::warn(
                    "Substituting placeholder reasoning_content for DeepSeek tool-call assistant message",
                );
                reasoning_content = String::from("(reasoning omitted)");
                has_reasoning = true;
            }

            // DeepSeek rejects assistant messages where both `content` and
            // `tool_calls` are missing/null. Skip such entries even if they
            // carry reasoning-only metadata unless we can send a non-null
            // placeholder content field.
            if !has_text && !has_tool_calls && !has_reasoning {
                pending_tool_calls.clear();
                continue;
            }

            let mut msg = json!({
                "role": "assistant",
                "content": if has_text {
                    json!(content)
                } else if has_reasoning {
                    json!("")
                } else {
                    Value::Null
                },
            });
            if has_reasoning {
                msg["reasoning_content"] = json!(reasoning_content);
            }
            if has_tool_calls {
                msg["tool_calls"] = json!(tool_calls);
                pending_tool_calls = tool_call_infos.into_iter().collect();
            } else {
                pending_tool_calls.clear();
            }
            out.push(msg);
        } else if role == "system" {
            let content = text_parts.join("\n");
            if !content.trim().is_empty() {
                let mut msg = json!({
                    "role": "system",
                    "content": content,
                });
                if include_tool_budget_metadata && let Some(turn_meta) = &turn_meta_budget {
                    msg["_turn_meta_budget"] = turn_meta_budget_json(turn_meta);
                }
                out.push(msg);
            }
        } else if role == "user" {
            let content = text_parts.join("\n");
            if !content.trim().is_empty() {
                let mut msg = json!({
                    "role": "user",
                    "content": content,
                });
                if include_tool_budget_metadata && let Some(turn_meta) = &turn_meta_budget {
                    msg["_turn_meta_budget"] = turn_meta_budget_json(turn_meta);
                }
                out.push(msg);
            }
        }

        if !tool_results.is_empty() {
            if pending_tool_calls.is_empty() {
                logging::warn("Dropping tool results without matching tool_calls");
            } else {
                for (tool_id, content, message_label) in tool_results {
                    if let Some(tool_info) = pending_tool_calls.remove(&tool_id) {
                        let wire_result = compact_tool_result_for_wire(
                            &tool_info.tool_name,
                            &tool_info.input,
                            &content,
                            &message_label,
                            &mut seen_tool_results,
                        );
                        let mut tool_msg = json!({
                            "role": "tool",
                            "tool_call_id": tool_id,
                            "content": wire_result.content,
                        });
                        if include_tool_budget_metadata {
                            tool_msg["_tool_result_budget"] = json!({
                                "original_chars": wire_result.original_chars,
                                "sent_chars": wire_result.sent_chars,
                                "truncated": wire_result.truncated,
                                "deduplicated": wire_result.deduplicated,
                            });
                        }
                        out.push(tool_msg);
                    } else {
                        logging::warn(format!(
                            "Dropping tool result for unknown tool_call_id: {tool_id}"
                        ));
                    }
                }
            }
        } else if role != "assistant" {
            pending_tool_calls.clear();
        }
    }

    // Safety net: after compaction, an assistant message may have tool_calls
    // whose results were summarized away. The API rejects these, so strip
    // the tool_calls (downgrading to a plain assistant message) and remove
    // the now-orphaned tool result messages.
    let mut i = 0;
    while i < out.len() {
        let is_assistant_with_tools = out[i].get("role").and_then(Value::as_str)
            == Some("assistant")
            && out[i].get("tool_calls").is_some();

        if is_assistant_with_tools {
            let expected_ids: HashSet<String> = out[i]
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|c| c.get("id").and_then(Value::as_str).map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // Collect tool result IDs immediately following this assistant message.
            let mut found_ids: HashSet<String> = HashSet::new();
            let mut tool_result_end = i + 1;
            while tool_result_end < out.len() {
                if out[tool_result_end].get("role").and_then(Value::as_str) == Some("tool") {
                    if let Some(id) = out[tool_result_end]
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                    {
                        found_ids.insert(id.to_string());
                    }
                    tool_result_end += 1;
                } else {
                    break;
                }
            }

            // Also scan non-contiguous tool results up to the next assistant message
            // in case compaction left gaps.
            let mut scan = tool_result_end;
            while scan < out.len() {
                if out[scan].get("role").and_then(Value::as_str) == Some("assistant") {
                    break;
                }
                if out[scan].get("role").and_then(Value::as_str) == Some("tool")
                    && let Some(id) = out[scan].get("tool_call_id").and_then(Value::as_str)
                {
                    found_ids.insert(id.to_string());
                }
                scan += 1;
            }

            if !expected_ids.is_subset(&found_ids) {
                let missing: Vec<_> = expected_ids.difference(&found_ids).collect();
                logging::warn(format!(
                    "Stripping orphaned tool_calls from assistant message \
                     (expected {} tool results, found {}, missing: {:?})",
                    expected_ids.len(),
                    found_ids.len(),
                    missing
                ));
                if let Some(obj) = out[i].as_object_mut() {
                    obj.remove("tool_calls");
                }
                // If tool_calls were the only assistant content, remove the now-invalid
                // assistant message entirely (DeepSeek requires content or tool_calls).
                let assistant_content_empty = out[i]
                    .get("content")
                    .is_none_or(|v| v.is_null() || v.as_str().is_some_and(str::is_empty));
                if assistant_content_empty {
                    // Remove orphaned tool results tied to this stripped assistant call set.
                    let mut j = out.len();
                    while j > i + 1 {
                        j -= 1;
                        if out[j].get("role").and_then(Value::as_str) == Some("tool")
                            && let Some(id) = out[j].get("tool_call_id").and_then(Value::as_str)
                            && expected_ids.contains(id)
                        {
                            out.remove(j);
                        }
                    }
                    out.remove(i);
                    i = i.saturating_sub(1);
                    continue;
                }
                // Remove contiguous tool results first
                if tool_result_end > i + 1 {
                    out.drain((i + 1)..tool_result_end);
                }
                // Remove any remaining non-contiguous tool results referencing expected_ids
                // (scan backward to avoid index shifting issues)
                let mut j = out.len();
                while j > i + 1 {
                    j -= 1;
                    if out[j].get("role").and_then(Value::as_str) == Some("tool")
                        && let Some(id) = out[j].get("tool_call_id").and_then(Value::as_str)
                        && expected_ids.contains(id)
                    {
                        out.remove(j);
                    }
                }
            }
        }
        i += 1;
    }

    out
}

pub(super) fn tool_to_chat(tool: &Tool) -> Value {
    let mut value = json!({
        "type": "function",
        "function": {
            "name": to_api_tool_name(&tool.name),
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    });
    if let Some(strict) = tool.strict
        && let Some(function) = value.get_mut("function")
    {
        function["strict"] = json!(strict);
    }
    value
}
fn requires_reasoning_content(model: &str) -> bool {
    let lower = model.to_lowercase();
    // V4-family direct model IDs.
    lower.contains("deepseek-v4")
        // Public DeepSeek API aliases routed server-side to the V4 family.
        // `deepseek-chat` resolves to `deepseek-v4-flash` and `deepseek-reasoner`
        // resolves to `deepseek-v4-pro`; both have thinking mode enabled by
        // default, so any assistant message carrying tool_calls must replay
        // `reasoning_content` on subsequent turns or the API returns 400.
        || lower.starts_with("deepseek-chat")
        || lower.starts_with("deepseek-reasoner")
        // Generic reasoning markers used by custom/proxied deployments.
        || lower.contains("reasoner")
        || lower.contains("-reasoning")
        || lower.contains("-thinking")
        || has_deepseek_r_series_marker(&lower)
}

fn should_replay_reasoning_content(model: &str, effort: Option<&str>) -> bool {
    if effort
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "disabled" | "none" | "false"
            )
        })
        .unwrap_or(false)
    {
        return false;
    }

    requires_reasoning_content(model)
}
fn has_deepseek_r_series_marker(model_lower: &str) -> bool {
    const PREFIX: &str = "deepseek-r";
    model_lower.match_indices(PREFIX).any(|(idx, _)| {
        model_lower[idx + PREFIX.len()..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
    })
}
#[cfg(test)]
mod stream_decoder_tests {
    //! Cache-inspection coverage for turn-meta deduplication and tool-result
    //! budget metadata surfaced by `inspect_prompt_for_request`.
    use super::*;
    fn tool_use_message(id: &str, name: &str, input: Value) -> Message {
        Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
            }],
        }
    }

    fn tool_result_message(id: &str, content: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    fn user_message_with_turn_meta(turn_meta: &str, task: &str) -> Message {
        Message {
            role: "user".to_string(),
            content: vec![
                ContentBlock::Text {
                    text: turn_meta.to_string(),
                    cache_control: None,
                },
                ContentBlock::Text {
                    text: task.to_string(),
                    cache_control: None,
                },
            ],
        }
    }

    fn with_tool_result_sha_spillover_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prior = crate::tools::truncate::set_test_spillover_root(Some(
            tmp.path().join(".deepseek").join("tool_outputs"),
        ));
        struct Restore(Option<std::path::PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::tools::truncate::set_test_spillover_root(self.0.take());
            }
        }
        let _restore = Restore(prior);
        f()
    }
    #[test]
    fn cache_inspect_reports_turn_meta_dedup_metadata() {
        let turn_meta = format!(
            "<turn_meta>\nCurrent local date: 2026-05-09\n{}\n</turn_meta>",
            "Working set: src/lib.rs\n".repeat(20)
        );
        let request = MessageRequest {
            model: "deepseek-v4-flash".to_string(),
            messages: vec![
                user_message_with_turn_meta(&turn_meta, "first task"),
                user_message_with_turn_meta(&turn_meta, "second task"),
            ],
            max_tokens: 0,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: None,
            temperature: None,
            top_p: None,
        };

        let inspection = inspect_prompt_for_request(&request);
        let turn_meta_layers: Vec<_> = inspection
            .layers
            .iter()
            .filter_map(|layer| layer.turn_meta.as_ref())
            .collect();

        assert_eq!(turn_meta_layers.len(), 2);
        assert_eq!(
            turn_meta_layers[0].original_chars,
            turn_meta.chars().count()
        );
        assert_eq!(turn_meta_layers[0].sent_chars, turn_meta.chars().count());
        assert!(!turn_meta_layers[0].deduplicated);
        assert_eq!(turn_meta_layers[0].sha256, sha256_hex(turn_meta.as_bytes()));
        assert_eq!(
            turn_meta_layers[1].original_chars,
            turn_meta.chars().count()
        );
        assert!(turn_meta_layers[1].sent_chars < turn_meta_layers[1].original_chars);
        assert!(turn_meta_layers[1].deduplicated);
        assert_eq!(turn_meta_layers[1].sha256, turn_meta_layers[0].sha256);
    }

    #[test]
    fn cache_inspect_reports_tool_result_budget_metadata() {
        with_tool_result_sha_spillover_root(|| {
            let long_output = format!("{}{}", "A".repeat(7_000), "Z".repeat(7_000));
            let request = MessageRequest {
                model: "deepseek-v4-flash".to_string(),
                messages: vec![
                    tool_use_message("tool-1", "shell_command", json!({"command": "cargo test"})),
                    tool_result_message("tool-1", &long_output),
                    tool_use_message("tool-2", "shell_command", json!({"command": "cargo test"})),
                    tool_result_message("tool-2", &long_output),
                ],
                max_tokens: 0,
                system: None,
                tools: None,
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: None,
                stream: None,
                temperature: None,
                top_p: None,
            };

            let inspection = inspect_prompt_for_request(&request);
            let tool_layers: Vec<_> = inspection
                .layers
                .iter()
                .filter_map(|layer| layer.tool_result.as_ref())
                .collect();

            assert_eq!(tool_layers.len(), 2);
            assert_eq!(tool_layers[0].original_chars, 14_000);
            assert!(tool_layers[0].sent_chars < tool_layers[0].original_chars);
            assert!(tool_layers[0].truncated);
            assert!(!tool_layers[0].deduplicated);
            assert_eq!(tool_layers[1].original_chars, 14_000);
            // Keep the reference far smaller than the original 14K output
            // even with a copyable retrieval hint included.
            assert!(
                tool_layers[1].sent_chars < 300,
                "deduplicated ref grew unexpectedly large: {}",
                tool_layers[1].sent_chars
            );
            assert!(!tool_layers[1].truncated);
            assert!(tool_layers[1].deduplicated);
        });
    }
}
#[cfg(test)]
mod alias_thinking_detection_tests {
    //! Regression coverage for the DeepSeek public model aliases.
    //!
    //! `deepseek-chat` and `deepseek-reasoner` are the canonical alias names
    //! published in DeepSeek's API docs. Server-side they resolve to V4-flash
    //! and V4-pro respectively, both of which have thinking mode enabled by
    //! default. The prompt builder must classify those aliases as reasoning
    //! models so `reasoning_content` is replayed on tool-call assistant
    //! messages (otherwise DeepSeek's thinking-mode API returns a 400 on the
    //! second turn). See upstream API docs:
    //! https://api-docs.deepseek.com/guides/thinking_mode
    use super::{requires_reasoning_content, should_replay_reasoning_content};

    #[test]
    fn aliases_routed_to_v4_require_reasoning_content() {
        // Documented public aliases.
        assert!(requires_reasoning_content("deepseek-chat"));
        assert!(requires_reasoning_content("deepseek-reasoner"));
        // Case-insensitive: users sometimes copy/paste with capitalisation.
        assert!(requires_reasoning_content("DeepSeek-Chat"));
        assert!(requires_reasoning_content("DEEPSEEK-REASONER"));
    }

    #[test]
    fn explicit_v4_ids_still_require_reasoning_content() {
        // Direct V4 IDs continue to match (regression guard for the existing
        // `lower.contains("deepseek-v4")` branch).
        assert!(requires_reasoning_content("deepseek-v4-flash"));
        assert!(requires_reasoning_content("deepseek-v4-pro"));
    }

    #[test]
    fn non_thinking_aliases_remain_excluded() {
        // Legacy non-thinking IDs and unrelated provider models must not be
        // misclassified, otherwise we would force a placeholder
        // `reasoning_content` on providers that reject the field.
        assert!(!requires_reasoning_content("deepseek-v3"));
        assert!(!requires_reasoning_content("deepseek-coder"));
        assert!(!requires_reasoning_content("qwen3-coder"));
        assert!(!requires_reasoning_content("claude-sonnet-4-6"));
    }

    #[test]
    fn alias_prefix_handles_suffixed_variants() {
        // OpenRouter / proxy deployments occasionally suffix the canonical
        // alias (e.g. `deepseek-chat:free`). Those routes still hit V4
        // server-side, so they must continue to require reasoning_content.
        assert!(requires_reasoning_content("deepseek-chat:free"));
        assert!(requires_reasoning_content("deepseek-reasoner-2025-05"));
    }

    #[test]
    fn explicit_reasoning_off_overrides_alias_detection() {
        // `reasoning_effort = "off"` is the documented escape hatch: even when
        // the model is in the thinking family, the user can opt out and the
        // sanitizer must respect that choice.
        assert!(!should_replay_reasoning_content(
            "deepseek-chat",
            Some("off")
        ));
        assert!(!should_replay_reasoning_content(
            "deepseek-reasoner",
            Some("disabled")
        ));
        // Without an explicit override, alias models still trigger replay.
        assert!(should_replay_reasoning_content("deepseek-chat", None));
        assert!(should_replay_reasoning_content(
            "deepseek-reasoner",
            Some("medium")
        ));
    }

}
