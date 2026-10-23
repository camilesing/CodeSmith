//! Deferred tool catalog and built-in advanced tool helpers.
//!
//! The streaming turn loop owns when tools are offered or executed. This module
//! owns the catalog-level policy around deferred loading, tool search, missing
//! tool suggestions, and the small set of built-in advanced tools that are not
//! registered by the normal runtime tool registry.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::mode::AppMode;
use crate::models::{ContentBlock, Message, Tool};
use crate::tools::spec::{ToolError, ToolResult, optional_u64, required_str};

use crate::dependencies::ExternalTool;

// The default active native tool set lives in
// `crate::tools::tool_catalog` (same crate, sibling module) — it now feeds the
// prompt's core-tool taxonomy only; deferral is declared per tool.

pub const MULTI_TOOL_PARALLEL_NAME: &str = "multi_tool_use.parallel";
pub const REQUEST_USER_INPUT_NAME: &str = "request_user_input";
pub const CODE_EXECUTION_TOOL_NAME: &str = "code_execution";
const CODE_EXECUTION_TOOL_TYPE: &str = "code_execution_20250825";
pub use crate::tools::js_execution::JS_EXECUTION_TOOL_NAME;
pub const TOOL_SEARCH_REGEX_NAME: &str = "tool_search_tool_regex";
const TOOL_SEARCH_REGEX_TYPE: &str = "tool_search_tool_regex_20251119";
pub const TOOL_SEARCH_BM25_NAME: &str = "tool_search_tool_bm25";
const TOOL_SEARCH_BM25_TYPE: &str = "tool_search_tool_bm25_20251119";
const TOOL_SEARCH_DEFAULT_MAX_RESULTS: usize = 20;
const TOOL_SEARCH_MAX_RESULTS_LIMIT: usize = 100;

/// Canonical name of the client-side tool-search control tool.
pub const TOOL_SEARCH_NAME: &str = "tool_search";

pub fn is_tool_search_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_SEARCH_NAME | TOOL_SEARCH_REGEX_NAME | TOOL_SEARCH_BM25_NAME
    )
}

// `DEFAULT_ACTIVE_NATIVE_TOOLS` relocated to
// `codesmith_agent_runtime::tools::tool_catalog` (re-exported above) — it now
// feeds the prompt's core-tool taxonomy only; deferral is declared per tool.

/// Model-facing description of the `tool_search` control tool.
pub const TOOL_SEARCH_DESCRIPTION: &str = "\
Load the schemas of tools that are not in this request's tool list. Pass \
`query` with keywords (optionally `+term` to require a term), or \
`select:name1,name2` for exact names. Matching tools' schemas are returned and \
those tools become callable from your next step on. Tools already listed in \
this request are resident — call them directly.";

/// The synthetic `tool_search` control tool definition.
///
/// Like `code_execution`, this entry is not a `ToolSpec`: the executor answers
/// it inline (`execute_tool_search`), because only the executor holds the
/// withheld catalog and the active set. It exists in the catalog only while
/// progressive disclosure is on.
#[must_use]
pub fn tool_search_control_tool() -> Tool {
    Tool {
        tool_type: Some("custom".to_string()),
        name: TOOL_SEARCH_NAME.to_string(),
        description: TOOL_SEARCH_DESCRIPTION.to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords (`+term` to require), or `select:name1,name2` for exact names."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum matches to return (default 20)."
                }
            },
            "required": ["query"]
        }),
        output_schema: None,
        allowed_callers: Some(vec!["direct".to_string()]),
        defer_loading: Some(false),
        input_examples: None,
        strict: None,
        cache_control: None,
    }
}

/// The activation marker a search or hydration result carries, naming the tools
/// now callable.
///
/// The **transcript is the activation record** — the log of which tools this
/// session has loaded — so the active set is derived
/// ([`activated_by_transcript`]) instead of held in executor state: it survives
/// a resume for free, and the replay shows exactly why a tool was visible.
#[must_use]
pub fn tool_search_loaded_sentinel(names: &[String]) -> String {
    format!("<codesmith:tool_search loaded=\"{}\" />", names.join(","))
}

/// Tools the transcript says have been loaded (see
/// [`tool_search_loaded_sentinel`]).
#[must_use]
pub fn activated_by_transcript(messages: &[Message]) -> HashSet<String> {
    const PREFIX: &str = "<codesmith:tool_search loaded=\"";
    let mut names = HashSet::new();
    for message in messages {
        for block in &message.content {
            // The sentinel rides a *tool result's* content — that is where a
            // discovery answer lands — and a text block for engine notes; both
            // are read.
            let text = match block {
                ContentBlock::Text { text, .. } => text.as_str(),
                ContentBlock::ToolResult { content, .. } => content.as_str(),
                _ => continue,
            };
            for (index, _) in text.match_indices(PREFIX) {
                let rest = &text[index + PREFIX.len()..];
                let Some(end) = rest.find('"') else {
                    continue;
                };
                for name in rest[..end].split(',') {
                    let name = name.trim();
                    if !name.is_empty() {
                        names.insert(name.to_string());
                    }
                }
            }
        }
    }
    names
}

/// Whether to withhold declared-deferred tools for this turn.
///
/// Off unless the user asked for disclosure **and** something is declarable: no
/// withheld tool means no search step to pay for, and the degraded case is
/// logged so a config that cannot take effect says why.
#[must_use]
pub fn progressive_disclosure_active(catalog: &[Tool], requested: bool) -> bool {
    if !requested {
        return false;
    }
    if catalog.iter().any(|tool| tool.defer_loading == Some(true)) {
        return true;
    }
    tracing::info!(
        "progressive_disclosure is on but no registered tool declares deferral \
         — sending the full catalog"
    );
    false
}

/// Apply the overrides that sit *above* a tool's own deferral declaration.
///
/// The policy is the declaration (`ToolSpec::defer_loading`, propagated into
/// the catalog by the registry): a tool is withheld from the request only
/// because its own definition says so — never because it is missing from a
/// central list. The direction matters: an undeclared tool is resident, so a
/// new tool is visible by default and a stale list cannot silently hide it.
/// This function applies only `[tools] always_load`, which wins over a
/// declaration.
pub fn apply_native_deferral_overrides(catalog: &mut [Tool], always_load: &HashSet<String>) {
    for tool in catalog {
        if always_load.contains(&tool.name) {
            tool.defer_loading = Some(false);
        }
    }
}

/// YOLO turns send the whole MCP surface resident: the mode exists to remove
/// friction, and an on-demand search step is friction. Outside YOLO the MCP
/// adapter's own declaration stands — its discovery helpers stay resident, the
/// rest of a server's tools are discoverable on demand (the adapter in
/// `crates/cli` owns that list, beside the tools it describes).
pub fn apply_mcp_tool_deferral(catalog: &mut [Tool], mode: AppMode) {
    if mode != AppMode::Yolo {
        return;
    }
    for tool in catalog {
        tool.defer_loading = Some(false);
    }
}

pub fn build_model_tool_catalog(
    mut native_tools: Vec<Tool>,
    mut mcp_tools: Vec<Tool>,
    mode: AppMode,
    always_load: &HashSet<String>,
) -> Vec<Tool> {
    apply_native_deferral_overrides(&mut native_tools, always_load);
    apply_mcp_tool_deferral(&mut mcp_tools, mode);
    // Sort each partition by name for prefix-cache stability (#263). The
    // upstream `to_api_tools()` already sorts the registry's HashMap output;
    // this catalog is built from caller-supplied Vecs which the test harness
    // and (future) caller refactors may not pre-sort. Built-ins stay as a
    // contiguous prefix ahead of MCP tools so adding/removing an MCP tool
    // never shifts a built-in's position.
    native_tools.sort_by(|a, b| a.name.cmp(&b.name));
    mcp_tools.sort_by(|a, b| a.name.cmp(&b.name));

    let mut seen = HashSet::new();
    native_tools.retain(|tool| seen.insert(tool.name.clone()));
    mcp_tools.retain(|tool| seen.insert(tool.name.clone()));

    native_tools.extend(mcp_tools);
    native_tools
}

/// Apply an allowlist/denylist pair to a built catalog, in place.
///
/// `allowed` — when `Some`, only tools whose (case-insensitive) name is
/// listed survive; `None` keeps the full surface. `blocked` then removes
/// names regardless of the allowlist, so a mode can express "the default
/// set minus web tools" without enumerating everything. Core
/// infrastructure tools (`multi_tool_use.parallel`, tool-search) are never
/// filtered: they are dispatch machinery the model needs to drive whatever
/// surface remains, not capabilities of their own.
#[cfg(test)]
mod progressive_disclosure_tests {
    use super::*;

    fn tool(name: &str, deferred: Option<bool>) -> Tool {
        Tool {
            tool_type: Some("custom".to_string()),
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({}),
            output_schema: None,
            allowed_callers: None,
            defer_loading: deferred,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    /// The valve: the setting alone must not pay for a search step, and the
    /// transcript is the activation record.
    #[test]
    fn disclosure_requires_something_to_withhold() {
        let resident_only = vec![tool("read_file", Some(false))];
        assert!(!progressive_disclosure_active(&resident_only, false));
        assert!(
            !progressive_disclosure_active(&resident_only, true),
            "nothing to withhold ⇒ off"
        );
        let with_deferred = vec![tool("read_file", Some(false)), tool("niche", Some(true))];
        assert!(progressive_disclosure_active(&with_deferred, true));
    }

    #[test]
    fn loaded_sentinels_round_trip_through_the_transcript() {
        let sentinel = tool_search_loaded_sentinel(&["niche".to_string(), "other".to_string()]);
        // The real shape: a discovery answer is a tool *result*, so the
        // sentinel sits in `ContentBlock::ToolResult::content`.
        let result_message = Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".to_string(),
                content: format!("found them\n\n{sentinel}"),
                is_error: None,
                content_blocks: None,
            }],
        };
        let text_message = Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: format!("note\n\n{sentinel}"),
                cache_control: None,
            }],
        };
        let activated = activated_by_transcript(&[result_message, text_message]);
        assert!(activated.contains("niche"));
        assert!(activated.contains("other"));
        assert!(
            activated_by_transcript(&[]).is_empty(),
            "an empty transcript activates nothing"
        );
    }
}

pub fn apply_tool_selection(
    catalog: &mut Vec<Tool>,
    allowed: Option<&[String]>,
    blocked: &[String],
) {
    if allowed.is_none() && blocked.is_empty() {
        return;
    }
    let allowed_set = allowed.map(|names| {
        names
            .iter()
            .map(|name| name.trim().to_ascii_lowercase())
            .collect::<HashSet<_>>()
    });
    let blocked_set = blocked
        .iter()
        .map(|name| name.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    catalog.retain(|tool| {
        if is_tool_search_tool(&tool.name) || tool.name == MULTI_TOOL_PARALLEL_NAME {
            return true;
        }
        if blocked_set.contains(&tool.name.to_ascii_lowercase()) {
            return false;
        }
        allowed_set
            .as_ref()
            .is_none_or(|set| set.contains(&tool.name.to_ascii_lowercase()))
    });
}

pub fn ensure_advanced_tooling(
    catalog: &mut Vec<Tool>,
    mode: AppMode,
    _always_load: &HashSet<String>,
) {
    // code_execution depends on a locally-installed Python interpreter
    // (python3 / python / py -3). Before v0.8.31, the tool was always
    // advertised and would fail at execution time on Windows where
    // `python3` isn't on PATH — the model treated the tool as reliable
    // once it appeared in the catalog. We now probe at catalog-build
    // time and only advertise when an interpreter resolves. See
    // `crate::dependencies::resolve_python_interpreter` for the probe.
    if mode != AppMode::Plan
        && mode != AppMode::Coordinator
        && !catalog.iter().any(|t| t.name == CODE_EXECUTION_TOOL_NAME)
        && crate::dependencies::resolve_python_interpreter().is_some()
    {
        catalog.push(Tool {
            tool_type: Some(CODE_EXECUTION_TOOL_TYPE.to_string()),
            name: CODE_EXECUTION_TOOL_NAME.to_string(),
            description: "Execute Python code in a local sandboxed runtime and return stdout/stderr/return_code as JSON.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Python source code to execute." }
                },
                "required": ["code"]
            }),
            output_schema: None,
            allowed_callers: Some(vec!["direct".to_string()]),
            // Synthetic catalog entry (no `ToolSpec` to declare deferral):
            // resident, like every undeclared tool.
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        });
    }

    // js_execution mirrors code_execution: gate on Node.js being
    // present locally so the model never sees a runtime it can't
    // actually use. Plan mode hides shell/exec surfaces (including
    // both interpreter tools) by construction; Agent / YOLO advertise
    // the tool only when `resolve_node()` succeeds.
    if mode != AppMode::Plan
        && mode != AppMode::Coordinator
        && !catalog.iter().any(|t| t.name == JS_EXECUTION_TOOL_NAME)
        && crate::dependencies::resolve_node().is_some()
    {
        let mut tool = crate::tools::js_execution::js_execution_tool_definition();
        // Synthetic definition (no `ToolSpec` to declare deferral): resident.
        tool.defer_loading = None;
        catalog.push(tool);
    }

    if !catalog.iter().any(|t| t.name == TOOL_SEARCH_REGEX_NAME) {
        catalog.push(Tool {
            tool_type: Some(TOOL_SEARCH_REGEX_TYPE.to_string()),
            name: TOOL_SEARCH_REGEX_NAME.to_string(),
            description: "Search deferred tool definitions using a regex query and return matching tool references.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Regex pattern to search tool names/descriptions/schema." },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": TOOL_SEARCH_MAX_RESULTS_LIMIT,
                        "default": TOOL_SEARCH_DEFAULT_MAX_RESULTS,
                        "description": "Maximum number of matching tool references to return."
                    }
                },
                "required": ["query"]
            }),
            output_schema: None,
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(false),
            input_examples: None,
            strict: None,
            cache_control: None,
        });
    }

    if !catalog.iter().any(|t| t.name == TOOL_SEARCH_BM25_NAME) {
        catalog.push(Tool {
            tool_type: Some(TOOL_SEARCH_BM25_TYPE.to_string()),
            name: TOOL_SEARCH_BM25_NAME.to_string(),
            description: "Search deferred tool definitions using natural-language matching and return matching tool references.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language query for tool discovery." },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": TOOL_SEARCH_MAX_RESULTS_LIMIT,
                        "default": TOOL_SEARCH_DEFAULT_MAX_RESULTS,
                        "description": "Maximum number of matching tool references to return."
                    }
                },
                "required": ["query"]
            }),
            output_schema: None,
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(false),
            input_examples: None,
            strict: None,
            cache_control: None,
        });
    }
}

pub fn initial_active_tools(catalog: &[Tool]) -> HashSet<String> {
    let mut active = HashSet::new();
    for tool in catalog {
        if !tool.defer_loading.unwrap_or(false) || is_tool_search_tool(&tool.name) {
            active.insert(tool.name.clone());
        }
    }
    if active.is_empty()
        && !catalog.is_empty()
        && let Some(first) = catalog.first()
    {
        active.insert(first.name.clone());
    }
    active
}

pub fn active_tool_list_from_catalog(catalog: &[Tool], active: &HashSet<String>) -> Vec<Tool> {
    // Two-pass for prefix-cache stability (#263). Always-loaded tools come
    // first in their stable catalog order; tools that started life deferred
    // and were activated mid-conversation by ToolSearch get appended at the
    // tail. Otherwise activating a deferred tool shifts every later tool's
    // byte offset and busts the cached prefix from that point onwards.
    let mut head: Vec<Tool> = Vec::new();
    let mut tail: Vec<Tool> = Vec::new();
    for tool in catalog {
        if !active.contains(&tool.name) {
            continue;
        }
        if tool.defer_loading.unwrap_or(false) {
            tail.push(tool.clone());
        } else {
            head.push(tool.clone());
        }
    }
    head.extend(tail);
    head
}

pub fn active_tools_for_step(
    catalog: &[Tool],
    active: &HashSet<String>,
    force_update_plan: bool,
) -> Vec<Tool> {
    // DeepSeek reasoning models reject explicit named tool_choice forcing here,
    // so for obvious quick-plan asks we narrow the first-step tool surface to
    // update_plan instead.
    if force_update_plan {
        let forced: Vec<_> = catalog
            .iter()
            .filter(|tool| tool.name == "update_plan")
            .cloned()
            .collect();
        if !forced.is_empty() {
            return forced;
        }
    }

    active_tool_list_from_catalog(catalog, active)
}

fn tool_search_haystack(tool: &Tool) -> String {
    format!(
        "{}\n{}\n{}",
        tool.name.to_lowercase(),
        tool.description.to_lowercase(),
        tool.input_schema.to_string().to_lowercase()
    )
}

fn discover_tools_with_regex(
    catalog: &[Tool],
    query: &str,
    max_results: usize,
) -> Result<Vec<String>, ToolError> {
    let regex = regex::Regex::new(query)
        .map_err(|err| ToolError::invalid_input(format!("Invalid regex query: {err}")))?;

    let mut matches = Vec::new();
    for tool in catalog {
        if is_tool_search_tool(&tool.name) {
            continue;
        }
        let hay = tool_search_haystack(tool);
        if regex.is_match(&hay) {
            matches.push(tool.name.clone());
        }
        if matches.len() >= max_results {
            break;
        }
    }
    Ok(matches)
}

fn discover_tools_with_bm25_like(catalog: &[Tool], query: &str, max_results: usize) -> Vec<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(i64, String)> = Vec::new();
    for tool in catalog {
        if is_tool_search_tool(&tool.name) {
            continue;
        }
        let hay = tool_search_haystack(tool);
        let mut score = 0i64;
        for term in &terms {
            if hay.contains(term) {
                score += 1;
            }
            if tool.name.to_lowercase().contains(term) {
                score += 2;
            }
        }
        if score > 0 {
            scored.push((score, tool.name.clone()));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(max_results)
        .map(|(_, name)| name)
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.chars().count();
    }
    if b.is_empty() {
        return a.chars().count();
    }

    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0usize; b_chars.len() + 1];

    for (i, a_ch) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = if a_ch == *b_ch { 0 } else { 1 };
            let delete = prev[j + 1] + 1;
            let insert = curr[j] + 1;
            let substitute = prev[j] + cost;
            curr[j + 1] = delete.min(insert).min(substitute);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_chars.len()]
}

fn suggest_tool_names(catalog: &[Tool], requested: &str, limit: usize) -> Vec<String> {
    let requested = requested.trim().to_ascii_lowercase();
    if requested.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut candidates: Vec<(u8, usize, String)> = Vec::new();
    for tool in catalog {
        let candidate = tool.name.to_ascii_lowercase();
        let prefix_match = candidate.starts_with(&requested) || requested.starts_with(&candidate);
        let contains_match = candidate.contains(&requested) || requested.contains(&candidate);
        let distance = edit_distance(&candidate, &requested);
        let close_typo = distance <= 3;

        if !(prefix_match || contains_match || close_typo) {
            continue;
        }

        let rank = if prefix_match {
            0
        } else if contains_match {
            1
        } else {
            2
        };
        candidates.push((rank, distance, tool.name.clone()));
    }

    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    candidates.dedup_by(|a, b| a.2 == b.2);
    candidates
        .into_iter()
        .take(limit)
        .map(|(_, _, name)| name)
        .collect()
}

pub fn missing_tool_error_message(tool_name: &str, catalog: &[Tool]) -> String {
    let suggestions = suggest_tool_names(catalog, tool_name, 3);
    let shell_hint = if is_shell_tool_name(tool_name) {
        Some(shell_tool_allow_shell_hint())
    } else {
        None
    };
    if suggestions.is_empty() {
        if let Some(shell_hint) = shell_hint {
            return format!(
                "Tool '{tool_name}' is not available in the current tool catalog. \
                 {shell_hint}, or use {TOOL_SEARCH_BM25_NAME} with a short query."
            );
        }
        return format!(
            "Tool '{tool_name}' is not available in the current tool catalog. \
             Verify mode/feature flags, or use {TOOL_SEARCH_BM25_NAME} with a short query."
        );
    }

    let suggestion_text = format!("Did you mean: {}?", suggestions.join(", "));
    if let Some(shell_hint) = shell_hint {
        return format!(
            "Tool '{tool_name}' is not available in the current tool catalog. \
             {suggestion_text} {shell_hint}. \
             You can also use {TOOL_SEARCH_BM25_NAME} to discover tools."
        );
    }

    format!(
        "Tool '{tool_name}' is not available in the current tool catalog. \
         {suggestion_text} You can also use {TOOL_SEARCH_BM25_NAME} to discover tools."
    )
}

fn shell_tool_allow_shell_hint() -> &'static str {
    "Shell tools are gated by `allow_shell`; enable `allow_shell = true` for trusted workspaces, \
     or switch to an auto-approve mode that permits shell access"
}

fn is_shell_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "exec_shell"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "task_shell_start"
            | "task_shell_wait"
    )
}

pub fn maybe_activate_requested_deferred_tool(
    tool_name: &str,
    catalog: &[Tool],
    active_tools: &mut HashSet<String>,
) -> bool {
    let Some(def) = catalog.iter().find(|def| def.name == tool_name) else {
        return false;
    };

    if !def.defer_loading.unwrap_or(false) || active_tools.contains(tool_name) {
        return false;
    }

    active_tools.insert(tool_name.to_string())
}

pub fn maybe_hydrate_requested_deferred_tool(
    tool_name: &str,
    tool_input: &Value,
    catalog: &[Tool],
    active_tools_at_batch_start: &HashSet<String>,
    hydrated_tools_this_batch: &mut HashSet<String>,
) -> Option<ToolResult> {
    let def = catalog.iter().find(|def| def.name == tool_name)?;

    if !def.defer_loading.unwrap_or(false) || active_tools_at_batch_start.contains(tool_name) {
        return None;
    }

    hydrated_tools_this_batch.insert(tool_name.to_string());
    Some(deferred_tool_schema_hydration_result(def, tool_input))
}

pub fn preflight_requested_deferred_tool(
    tool_name: &str,
    tool_input: &Value,
    catalog: &[Tool],
    active_tools: &mut HashSet<String>,
) -> Option<ToolResult> {
    let active_tools_at_batch_start = active_tools.clone();
    let mut hydrated_tools_this_batch = HashSet::new();
    let result = maybe_hydrate_requested_deferred_tool(
        tool_name,
        tool_input,
        catalog,
        &active_tools_at_batch_start,
        &mut hydrated_tools_this_batch,
    );
    active_tools.extend(hydrated_tools_this_batch);
    result
}

fn deferred_tool_schema_hydration_result(tool: &Tool, tool_input: &Value) -> ToolResult {
    let expected = schema_fields(&tool.input_schema);
    let required = schema_required_fields(&tool.input_schema);
    let received = received_field_names(tool_input);
    let missing = required
        .iter()
        .filter(|field| !received.contains(field))
        .cloned()
        .collect::<Vec<_>>();
    let unexpected = received
        .iter()
        .filter(|field| !expected.iter().any(|expected| &expected.name == *field))
        .cloned()
        .collect::<Vec<_>>();
    let corrections = likely_field_corrections(&received, &expected, &tool.name);

    let mut lines = vec![
        format!("Tool `{}` was deferred and has now been loaded.", tool.name),
        String::new(),
        "The tool was not executed. Retry with the loaded schema.".to_string(),
        String::new(),
        "Expected fields:".to_string(),
    ];
    if expected.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        for field in &expected {
            let required_marker = if required.contains(&field.name) {
                " required"
            } else {
                ""
            };
            lines.push(format!(
                "  {}: {}{}",
                field.name, field.kind, required_marker
            ));
        }
    }
    lines.push(String::new());
    lines.push("Received fields:".to_string());
    if received.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        lines.push(format!("  {}", received.join(", ")));
    }
    if !missing.is_empty() {
        lines.push(String::new());
        lines.push("Missing required fields:".to_string());
        lines.push(format!("  {}", missing.join(", ")));
    }
    if !unexpected.is_empty() {
        lines.push(String::new());
        lines.push("Unexpected fields:".to_string());
        lines.push(format!("  {}", unexpected.join(", ")));
    }
    if !corrections.is_empty() {
        lines.push(String::new());
        lines.push("Likely corrections:".to_string());
        for correction in &corrections {
            lines.push(format!("  {correction}"));
        }
    }

    ToolResult::success(lines.join("\n")).with_metadata(json!({
        "event": "tool.schema_hydrated",
        "tool": tool.name,
        "executed": false,
        "retry_required": true,
        "reason": "deferred_tool_first_use",
        "deferred_tool_loaded": true,
        "tool_name": tool.name,
        "expected_fields": expected.iter().map(|field| field.name.clone()).collect::<Vec<_>>(),
        "received_fields": received,
        "missing_required_fields": missing,
        "unexpected_fields": unexpected,
        "likely_corrections": corrections,
    }))
}

#[derive(Debug, Clone)]
struct SchemaField {
    name: String,
    kind: String,
}

fn schema_fields(schema: &Value) -> Vec<SchemaField> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut fields = properties
        .iter()
        .map(|(name, spec)| SchemaField {
            name: name.clone(),
            kind: schema_type_label(spec),
        })
        .collect::<Vec<_>>();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

fn schema_required_fields(schema: &Value) -> Vec<String> {
    let mut required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    required.sort();
    required
}

fn schema_type_label(spec: &Value) -> String {
    let Some(kind) = spec.get("type").and_then(Value::as_str) else {
        return "value".to_string();
    };
    if let Some(values) = spec.get("enum").and_then(Value::as_array) {
        let labels = values.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        if !labels.is_empty() {
            return format!("{kind} ({})", labels.join(" | "));
        }
    }
    kind.to_string()
}

fn received_field_names(input: &Value) -> Vec<String> {
    let mut fields = input
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    fields.sort();
    fields
}

fn likely_field_corrections(
    received: &[String],
    expected: &[SchemaField],
    tool_name: &str,
) -> Vec<String> {
    let has_expected = |name: &str| expected.iter().any(|field| field.name == name);
    let has_received = |name: &str| received.iter().any(|field| field == name);
    let mut corrections = Vec::new();

    if has_received("old_string") && has_expected("search") {
        corrections.push("old_string -> search".to_string());
    } else if has_received("old_str") && has_expected("search") {
        corrections.push("old_str -> search".to_string());
    }
    if has_received("new_string") && has_expected("replace") {
        corrections.push("new_string -> replace".to_string());
    } else if has_received("new_str") && has_expected("replace") {
        corrections.push("new_str -> replace".to_string());
    } else if has_received("replacement") && has_expected("replace") {
        corrections.push("replacement -> replace".to_string());
    }
    if tool_name == "checklist_update" && has_received("todos") {
        corrections.push(
            "Use checklist_write to replace the full list, or retry checklist_update with id and status."
                .to_string(),
        );
    }
    corrections
}

pub fn execute_tool_search(
    tool_name: &str,
    input: &serde_json::Value,
    catalog: &[Tool],
    active_tools: &mut HashSet<String>,
) -> Result<ToolResult, ToolError> {
    let query = required_str(input, "query")?;
    let max_results = usize::try_from(optional_u64(
        input,
        "max_results",
        TOOL_SEARCH_DEFAULT_MAX_RESULTS as u64,
    )?)
    .unwrap_or(TOOL_SEARCH_DEFAULT_MAX_RESULTS)
    .clamp(1, TOOL_SEARCH_MAX_RESULTS_LIMIT);
    let discovered = if let Some(select) = query.strip_prefix("select:") {
        // Exact names, order-preserving and never truncated: a model that knows
        // the tool name should not depend on the keyword ranker, which is the
        // whole point of naming it. Case-insensitive; unknown names drop out.
        let mut seen = HashSet::new();
        select
            .split(',')
            .map(str::trim)
            .filter(|wanted| !wanted.is_empty())
            .filter_map(|wanted| {
                catalog
                    .iter()
                    .find(|tool| tool.name.eq_ignore_ascii_case(wanted))
                    .map(|tool| tool.name.clone())
            })
            .filter(|name| seen.insert(name.clone()))
            .collect::<Vec<_>>()
    } else if tool_name == TOOL_SEARCH_REGEX_NAME {
        discover_tools_with_regex(catalog, query, max_results)?
    } else {
        discover_tools_with_bm25_like(catalog, query, max_results)
    };

    for name in &discovered {
        active_tools.insert(name.clone());
    }

    let references = discovered
        .iter()
        .map(|name| json!({"type": "tool_reference", "tool_name": name}))
        .collect::<Vec<_>>();

    let payload = json!({
        "type": "tool_search_tool_search_result",
        "tool_references": references,
    });

    Ok(ToolResult {
        canonical: None,
        content: serde_json::to_string(&payload).unwrap_or_else(|_| payload.to_string()),
        success: true,
        metadata: Some(json!({
            "tool_references": discovered,
        })),
    })
}

pub async fn execute_code_execution_tool(
    input: &serde_json::Value,
    workspace: &Path,
) -> Result<ToolResult, ToolError> {
    let code = required_str(input, "code")?;

    // Resolve the locally-installed Python interpreter we cached at
    // catalog-build time. If it's absent now (somehow registered but
    // disappeared between startup and this call — concurrent uninstall,
    // PATH change, etc.) the ExternalTool::tokio_command() will return
    // None and we fail fast with a clear message.
    //
    // Write the code to a temp file and execute it as a script rather
    // than passing it via `-c "<code>"`. Reasons:
    //   * `-c` has length limits (argv) on Windows.
    //   * Multiline code with quote nesting is brittle through `-c`.
    //   * Tracebacks reference a real filename instead of `<string>`,
    //     so the model can interpret line numbers correctly.
    // Tempfile lives only for the duration of this execution; Drop
    // removes it. We use `.py` so any shebang / encoding-sniffer
    // logic in the interpreter behaves normally.
    let temp_dir = tempfile::tempdir()
        .map_err(|e| ToolError::execution_failed(format!("tempdir failed: {e}")))?;
    let script_path = temp_dir.path().join("code_execution.py");
    tokio::fs::write(&script_path, code)
        .await
        .map_err(|e| ToolError::execution_failed(format!("tempfile write failed: {e}")))?;

    let mut cmd = crate::dependencies::Python::tokio_command().ok_or_else(|| {
        ToolError::execution_failed(
            "code_execution: Python interpreter became unavailable".to_string(),
        )
    })?;
    cmd.arg(&script_path).current_dir(workspace);
    // Same process-group kill contract as `execute_js_execution_tool`
    // (see there): the group kill on timeout reaches interpreter-spawned
    // grandchildren; kill_on_drop is the direct-child backstop.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    cmd.kill_on_drop(true);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| ToolError::execution_failed(e.to_string()))?;
    let child_pid = child.id();
    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| {
            crate::shell_manager::kill_process_group_of_opt(child_pid);
            ToolError::Timeout { seconds: 120 }
        })
        .and_then(|res| res.map_err(|e| ToolError::execution_failed(e.to_string())))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let return_code = output.status.code().unwrap_or(-1);
    let success = output.status.success();
    let payload = json!({
        "type": "code_execution_result",
        "stdout": stdout,
        "stderr": stderr,
        "return_code": return_code,
        "content": [],
    });

    Ok(ToolResult {
        canonical: None,
        content: serde_json::to_string(&payload).unwrap_or_else(|_| payload.to_string()),
        success,
        metadata: Some(payload),
    })
}

#[cfg(test)]
mod apply_tool_selection_tests {
    use super::*;
    use crate::models::Tool;

    fn tool(name: &str) -> Tool {
        Tool {
            tool_type: Some("function".to_string()),
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
            output_schema: None,
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    fn names(catalog: &[Tool]) -> Vec<String> {
        catalog.iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn no_selection_is_noop() {
        let mut catalog = vec![tool("read_file"), tool("exec_shell")];
        apply_tool_selection(&mut catalog, None, &[]);
        assert_eq!(names(&catalog), vec!["read_file", "exec_shell"]);
    }

    #[test]
    fn allowlist_keeps_only_listed_tools() {
        let mut catalog = vec![tool("read_file"), tool("exec_shell"), tool("web_search")];
        let allowed = vec!["read_file".to_string(), "EXEC_SHELL".to_string()];
        apply_tool_selection(&mut catalog, Some(&allowed), &[]);
        assert_eq!(names(&catalog), vec!["read_file", "exec_shell"]);
    }

    #[test]
    fn denylist_removes_after_allowlist() {
        let mut catalog = vec![tool("read_file"), tool("exec_shell"), tool("web_search")];
        let allowed = vec!["read_file".to_string(), "exec_shell".to_string()];
        let blocked = vec!["exec_shell".to_string()];
        apply_tool_selection(&mut catalog, Some(&allowed), &blocked);
        // "the default set minus web tools" pattern: allowlist picks the
        // base surface, blocked trims within it.
        assert_eq!(names(&catalog), vec!["read_file"]);
    }

    #[test]
    fn denylist_alone_works_without_allowlist() {
        let mut catalog = vec![tool("read_file"), tool("web_search"), tool("fetch_url")];
        let blocked = vec!["web_search".to_string(), "fetch_url".to_string()];
        apply_tool_selection(&mut catalog, None, &blocked);
        assert_eq!(names(&catalog), vec!["read_file"]);
    }

    #[test]
    fn infrastructure_tools_survive_everything() {
        let mut catalog = vec![
            tool(MULTI_TOOL_PARALLEL_NAME),
            tool(TOOL_SEARCH_REGEX_NAME),
            tool(TOOL_SEARCH_BM25_NAME),
            tool("read_file"),
            tool("web_search"),
        ];
        let allowed = vec!["read_file".to_string()];
        // Even an explicit block cannot remove dispatch machinery — it is
        // exempt from both lists by design.
        let blocked = vec![MULTI_TOOL_PARALLEL_NAME.to_string()];
        apply_tool_selection(&mut catalog, Some(&allowed), &blocked);
        assert!(names(&catalog).contains(&MULTI_TOOL_PARALLEL_NAME.to_string()));
        assert!(names(&catalog).contains(&"tool_search_tool_regex".to_string()));
        assert!(names(&catalog).contains(&"read_file".to_string()));
        assert!(!names(&catalog).contains(&"web_search".to_string()));
        assert_eq!(names(&catalog).len(), 4);
    }

    #[test]
    fn empty_allowlist_clears_surface_except_infrastructure() {
        let mut catalog = vec![tool("read_file"), tool(MULTI_TOOL_PARALLEL_NAME)];
        apply_tool_selection(&mut catalog, Some(&[]), &[]);
        assert_eq!(names(&catalog), vec![MULTI_TOOL_PARALLEL_NAME]);
    }
}
