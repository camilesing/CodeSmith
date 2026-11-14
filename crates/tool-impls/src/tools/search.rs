//! Search tools: `grep_files` for code search
//!
//! These tools provide powerful code search capabilities within the workspace,
//! similar to ripgrep/grep functionality.

use async_trait::async_trait;
use codesmith_agent_runtime::tools::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_bool, optional_str,
    optional_u64, required_str,
};
use codesmith_agent_runtime::tools::tool_catalog::GREP_FILES_NAME;
use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Maximum number of results to return to avoid overwhelming output
const MAX_RESULTS: usize = 100;

/// Hard cap on caller-supplied context_lines. Anything larger is a
/// misbehaving caller; the real value of more context decays long before
/// this and the cap keeps range arithmetic overflow-free.
const MAX_CONTEXT_LINES: u64 = 1_000;

/// Maximum file size to search (skip large binaries)
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB

/// Hard cap on a single grep_files run. The directory walk plus per-file regex
/// is synchronous blocking work; without this it can run for minutes on a large
/// tree. Mirrors the file_search tool so both blocking searches behave the same.
const GREP_FILES_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of a grep match
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    pub file: String,
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Tool for searching files using regex patterns
pub struct GrepFilesTool;

#[async_trait]
impl ToolSpec for GrepFilesTool {
    fn name(&self) -> &'static str {
        GREP_FILES_NAME
    }

    fn description(&self) -> &'static str {
        "Search for a regex pattern in workspace files. Use this instead of `grep -r`, `rg`, or `find ... -exec grep` in `exec_shell` — ripgrep-engine, faster, and respects `.gitignore`. Returns matching lines with context (default: 2 lines before/after each match). Set `multiline: true` to match patterns that span line breaks (e.g. `fn\\s+\\w+\\(\\s*[^)]*\\)\\s*\\{`); prefix the pattern with `(?s)` when `.` itself must match newlines.\n\nExamples: `{\"pattern\": \"fn parse_config\", \"include\": [\"*.rs\"]}` (find definitions Rust-wide); `{\"pattern\": \"TODO|FIXME\", \"path\": \"src\", \"context_lines\": 0, \"max_results\": 50}` (sweep a subtree); `{\"pattern\": \"async fn\\s+\\w+\", \"case_insensitive\": false}`."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search (relative to workspace, default: .)"
                },
                "include": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Glob patterns for files to include (e.g., ['*.rs', '*.ts'])"
                },
                "exclude": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Glob patterns for files to exclude (e.g., ['*.min.js', 'node_modules/*'])"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of context lines before and after each match (default: 2)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Whether to perform case-insensitive matching (default: false)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 100)"
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Search the whole file buffer so patterns can span line breaks (default: false). Each match is reported at its starting line. Prefix the pattern with `(?s)` if `.` must match newlines."
                },
                "literal": {
                    "type": "boolean",
                    "description": "Match `pattern` as a literal string instead of a regex (default: false). Use this when searching for text that contains regex metacharacters ( ( ) [ ] { } . * + ? ^ $ | \\ ) — e.g. searching for `foo(bar)`."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches"],
                    "description": "\"files_with_matches\" returns only the list of matching files, newest-modified first — much cheaper when locating where a pattern lives; \"content\" (default) returns matched lines with context."
                }
            },
            "required": ["pattern"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    /// Path this search targets that `resolve_path` would refuse (outside the
    /// workspace, untrusted) — the engine asks the user about it instead of
    /// failing the search. Same input field `execute` resolves.
    fn escaped_path_for_input(&self, input: &Value, context: &ToolContext) -> Option<PathBuf> {
        context.escaped_path(optional_str(input, "path").unwrap_or("."))
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let pattern_str = required_str(&input, "pattern")?;
        let path_str = optional_str(&input, "path").unwrap_or(".");
        // Clamp at the parse site: a huge caller-supplied value used to
        // saturate to usize::MAX and overflow `line_idx + context_lines`
        // inside collect_context (panic in debug, wrapped range in release).
        let context_lines =
            usize::try_from(optional_u64(&input, "context_lines", 2)?.min(MAX_CONTEXT_LINES))
                .unwrap_or(2);
        let case_insensitive = optional_bool(&input, "case_insensitive", false)?;
        let multiline = optional_bool(&input, "multiline", false)?;
        let literal = optional_bool(&input, "literal", false)?;
        // The default match cap rides the `tool-output-gate` policy seam
        // (omp-style unified tiers, docs/MODS.md); the compiled constant is
        // the fallback when no policy is published.
        let gate_max_results = codesmith_agent::policy::policy_usize(
            codesmith_agent::policy::PolicyKind::ToolOutputGate,
            "grep_max_results",
            MAX_RESULTS,
            1_000,
        ) as u64;
        let max_results = usize::try_from(optional_u64(&input, "max_results", gate_max_results)?)
            .unwrap_or(MAX_RESULTS);
        // "files_with_matches" (minimax's default output tier): return only
        // the matching-file list — far cheaper than content rows when the
        // question is "where does this pattern live".
        let files_with_matches = match optional_str(&input, "output_mode").unwrap_or("content") {
            "content" => false,
            "files_with_matches" => true,
            other => {
                return Err(ToolError::invalid_input(format!(
                    "output_mode must be \"content\" or \"files_with_matches\", got \"{other}\""
                )));
            }
        };

        // Parse include patterns
        let include_patterns: Vec<String> = input
            .get("include")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Parse exclude patterns
        let exclude_patterns: Vec<String> =
            input.get("exclude").and_then(|v| v.as_array()).map_or_else(
                || {
                    // Default exclusions for common non-code directories, plus
                    // credential files (live secrets must not ride a default
                    // search into the transcript; pass an explicit `exclude`
                    // array to override). Bare directory names skip the
                    // directory traversal entirely; `dir/*` filters files
                    // inside if the directory is already being walked
                    // (belt-and-suspenders — see #2200).
                    vec![
                        "node_modules".to_string(),
                        "node_modules/*".to_string(),
                        ".git".to_string(),
                        ".git/*".to_string(),
                        "target".to_string(),
                        "target/*".to_string(),
                        "*.min.js".to_string(),
                        "*.min.css".to_string(),
                        "dist".to_string(),
                        "dist/*".to_string(),
                        "build".to_string(),
                        "build/*".to_string(),
                        "__pycache__".to_string(),
                        "__pycache__/*".to_string(),
                        ".venv".to_string(),
                        ".venv/*".to_string(),
                        "venv".to_string(),
                        "venv/*".to_string(),
                        ".env".to_string(),
                        ".env.*".to_string(),
                        "*.pem".to_string(),
                        "*.key".to_string(),
                        "id_rsa*".to_string(),
                        "id_ed25519*".to_string(),
                        "id_ecdsa*".to_string(),
                        ".netrc".to_string(),
                        ".npmrc".to_string(),
                        ".aws".to_string(),
                        ".aws/*".to_string(),
                    ]
                },
                |arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                },
            );

        // Build the ripgrep matcher. `grep-regex` is the same engine `rg`
        // uses, so pattern semantics (including `(?i)` inline flags) carry
        // over; `case_insensitive` and `multi_line` map to builder knobs.
        // `fixed_strings` is the literal escape hatch: a compile failure on
        // a metacharacter-laden pattern names it in the error below.
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(case_insensitive)
            .multi_line(multiline)
            .fixed_strings(literal)
            .build(pattern_str)
            .map_err(|e| {
                let mut message = format!("Invalid regex pattern: {e}");
                if !literal {
                    message.push_str(
                        " — if you meant to search for the text literally (it contains regex \
                         metacharacters like ( ) [ ] { } . * + ? ^ $ | \\), retry with \
                         literal=true or escape them with a backslash",
                    );
                }
                ToolError::invalid_input(message)
            })?;

        // Resolve search path
        let search_path = context.resolve_path(path_str)?;

        let workspace = context.workspace.clone();
        let cancel_token = context.cancel_token.clone();

        // The directory walk and per-file regex are synchronous blocking work.
        // Run them on a blocking worker bounded by a hard timeout so a huge tree
        // can't pin the async runtime and leave the stop button unresponsive.
        let aborted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let aborted_flag = aborted.clone();
        let result = run_blocking_grep(
            GREP_FILES_TIMEOUT,
            cancel_token.clone(),
            aborted,
            move || {
                let cancel_token = cancel_token.as_ref();
                let aborted = aborted_flag.as_ref();

                // Collect files to search
                let files = collect_files(
                    &search_path,
                    &include_patterns,
                    &exclude_patterns,
                    cancel_token,
                    aborted,
                )?;

                // The ripgrep walker canonicalizes its root (e.g. /var ->
                // /private/var on macOS), which can defeat strip_prefix against
                // the raw workspace path; fall back to the canonical form before
                // surfacing an absolute path.
                let canonical_workspace = workspace
                    .canonicalize()
                    .unwrap_or_else(|_| workspace.clone());

                let relative_path_of = |file_path: &std::path::Path| -> String {
                    file_path
                        .strip_prefix(&workspace)
                        .or_else(|_| file_path.strip_prefix(&canonical_workspace))
                        .unwrap_or(file_path)
                        .to_string_lossy()
                        .to_string()
                };

                // The cheap tier: first-match detection per file, list-only
                // output. One `is_match` per file (the matcher anchors ^/$ to
                // line boundaries over the whole buffer, so this agrees with
                // the content path for single- and multi-line patterns alike).
                if files_with_matches {
                    let mut matched: Vec<(String, u64)> = Vec::new();
                    let mut files_searched = 0usize;
                    for file_path in files {
                        check_cancelled(cancel_token, aborted)?;
                        if let Ok(metadata) = fs::metadata(&file_path)
                            && metadata.len() > MAX_FILE_SIZE
                        {
                            continue;
                        }
                        let Ok(file_content) = fs::read_to_string(&file_path) else {
                            continue;
                        };
                        files_searched += 1;
                        let has_match = matcher
                            .is_match(file_content.as_bytes())
                            .map_err(|e| {
                                ToolError::execution_failed(format!("grep matcher error: {e}"))
                            })?;
                        if !has_match {
                            continue;
                        }
                        let mtime_ms = fs::metadata(&file_path)
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0u64, |d| d.as_millis() as u64);
                        matched.push((relative_path_of(&file_path), mtime_ms));
                    }
                    // Newest first (minimax's ordering): the most recently
                    // touched matching file is the likeliest target.
                    matched.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    let matching_files = matched.len();
                    let truncated = matching_files > max_results;
                    let files_json: Vec<Value> = matched
                        .into_iter()
                        .take(max_results)
                        .map(|(file, last_modified_ms)| {
                            json!({"file": file, "last_modified_ms": last_modified_ms})
                        })
                        .collect();
                    let mut result = json!({
                        "output_mode": "files_with_matches",
                        "files": files_json,
                        "matching_files": matching_files,
                        "files_searched": files_searched,
                        "truncated": truncated,
                    });
                    if truncated {
                        result["truncation_hint"] = json!(
                            "file list truncated at the max_results cap — narrow the pattern or the path, or raise max_results and re-run"
                        );
                    }
                    return Ok(result);
                }

                // Search files
                let mut results: Vec<GrepMatch> = Vec::new();
                let mut files_searched = 0;
                let mut total_matches = 0;

                // Early-exit marker: matches beyond the cap in LATER files must
                // still report `truncated` (total_matches == max_results alone reads as
                // exhaustive when the cap lands exactly at a file boundary).
                let mut stopped_early = false;
                for file_path in files {
                    check_cancelled(cancel_token, aborted)?;

                    if results.len() >= max_results {
                        stopped_early = true;
                        break;
                    }

                    // Skip files that are too large
                    if let Ok(metadata) = fs::metadata(&file_path)
                        && metadata.len() > MAX_FILE_SIZE
                    {
                        continue;
                    }

                    // Read file content
                    let Ok(file_content) = fs::read_to_string(&file_path) else {
                        continue; // Skip binary or unreadable files
                    };

                    files_searched += 1;
                    let lines: Vec<&str> = file_content.lines().collect();
                    let relative_path = relative_path_of(&file_path);

                    if multiline {
                        // Whole-buffer matching: `.` and `^`/`$` cross line
                        // breaks; each match is reported at its starting line.
                        // `find_iter` drives a callback (return false to stop);
                        // keep returning true so `total_matches` stays accurate
                        // past `max_results` within this file.
                        matcher
                            .find_iter(file_content.as_bytes(), |m| {
                                if check_cancelled(cancel_token, aborted).is_err() {
                                    return false;
                                }
                                let line_idx = file_content.as_bytes()[..m.start()]
                                    .iter()
                                    .filter(|b| **b == b'\n')
                                    .count();
                                let Some(line) = lines.get(line_idx) else {
                                    // Zero-width artifact at EOF (e.g. empty
                                    // pattern against a trailing newline).
                                    return true;
                                };
                                total_matches += 1;
                                if results.len() < max_results {
                                    let (context_before, context_after) =
                                        collect_context(&lines, line_idx, context_lines);
                                    results.push(GrepMatch {
                                        file: relative_path.clone(),
                                        line_number: line_idx + 1,
                                        line: (*line).to_string(),
                                        context_before,
                                        context_after,
                                    });
                                }
                                true
                            })
                            .map_err(|e| {
                                ToolError::execution_failed(format!("grep matcher error: {e}"))
                            })?;
                        // `find_iter` stops silently when the callback returns
                        // false (cancellation); surface the error here so a
                        // cancel during the last file cannot report success.
                        check_cancelled(cancel_token, aborted)?;
                    } else {
                        for (line_idx, line) in lines.iter().enumerate() {
                            check_cancelled(cancel_token, aborted)?;

                            let matched = matcher.is_match(line.as_bytes()).map_err(|e| {
                                ToolError::execution_failed(format!("grep matcher error: {e}"))
                            })?;
                            if matched {
                                total_matches += 1;
                                if results.len() < max_results {
                                    let (context_before, context_after) =
                                        collect_context(&lines, line_idx, context_lines);
                                    results.push(GrepMatch {
                                        file: relative_path.clone(),
                                        line_number: line_idx + 1,
                                        line: (*line).to_string(),
                                        context_before,
                                        context_after,
                                    });
                                }
                            }
                        }
                    }
                }

                let matches_json: Vec<Value> = results
                    .iter()
                    .map(|item| grep_match_to_json(item, context_lines))
                    .collect();

                // Build result. When context_lines == 1, return the single context
                // line as a string instead of a one-item array. That keeps the common
                // "show just the adjacent line" case easy for model callers to read.
                // A truncated result carries an executable continuation: what to
                // change to see more, not just the fact that something was cut.
                let truncated = total_matches > max_results || stopped_early;
                let mut result = json!({
                    "matches": matches_json,
                    "total_matches": total_matches,
                    "files_searched": files_searched,
                    "truncated": truncated,
                });
                if truncated {
                    result["truncation_hint"] = json!(
                        "results truncated at the max_results cap — narrow the pattern or the path, or raise max_results and re-run"
                    );
                }
                Ok(result)
            },
        )
        .await?;

        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

/// Run the synchronous grep walk on a blocking worker, cancellable via the
/// token and bounded by `timeout`. Mirrors `run_blocking_file_search`.
async fn run_blocking_grep<F>(
    timeout: Duration,
    cancel_token: Option<CancellationToken>,
    aborted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    search: F,
) -> Result<Value, ToolError>
where
    F: FnOnce() -> Result<Value, ToolError> + Send + 'static,
{
    if cancel_token
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(grep_cancelled());
    }

    let task = tokio::task::spawn_blocking(search);
    let result = match cancel_token {
        Some(token) => {
            tokio::select! {
                biased;
                () = token.cancelled() => {
                    aborted.store(true, std::sync::atomic::Ordering::Relaxed);
                    return Err(grep_cancelled());
                }
                result = tokio::time::timeout(timeout, task) => result,
            }
        }
        None => tokio::time::timeout(timeout, task).await,
    };

    let joined = match result {
        Ok(joined) => joined,
        Err(_) => {
            aborted.store(true, std::sync::atomic::Ordering::Relaxed);
            return Err(grep_timeout(timeout));
        }
    };
    joined.map_err(|err| {
        ToolError::execution_failed(format!("grep_files worker failed before completion: {err}"))
    })?
}

fn grep_cancelled() -> ToolError {
    ToolError::execution_failed("grep_files cancelled before completion")
}

fn grep_timeout(timeout: Duration) -> ToolError {
    ToolError::Timeout {
        seconds: timeout.as_secs().max(1),
    }
}

fn grep_match_to_json(item: &GrepMatch, context_lines: usize) -> Value {
    if context_lines == 1 {
        json!({
            "file": item.file,
            "line_number": item.line_number,
            "line": item.line,
            "context_before": item.context_before.first().cloned().unwrap_or_default(),
            "context_after": item.context_after.first().cloned().unwrap_or_default(),
        })
    } else {
        json!(item)
    }
}

/// Context lines around `line_idx` (0-based), `context_lines` before/after.
fn collect_context(
    lines: &[&str],
    line_idx: usize,
    context_lines: usize,
) -> (Vec<String>, Vec<String>) {
    let context_before: Vec<String> = (line_idx.saturating_sub(context_lines)..line_idx)
        .filter_map(|i| lines.get(i).map(|s| (*s).to_string()))
        .collect();
    let context_after: Vec<String> = ((line_idx + 1)
        ..=line_idx
            .saturating_add(context_lines)
            .min(lines.len().saturating_sub(1)))
        .filter_map(|i| lines.get(i).map(|s| (*s).to_string()))
        .collect();
    (context_before, context_after)
}

/// Collect files to search based on include/exclude patterns.
///
/// The walk is ripgrep's `ignore` walker, so `.gitignore` rules are honoured
/// (`require_git(false)` applies them in plain directories too, matching the
/// tool's documented behavior) and gitignored subtrees are pruned natively.
/// Bare directory-name excludes are pruned from the walk itself via
/// `filter_entry`; include/exclude globs still go through the tool's own
/// matcher per file so caller-facing semantics are unchanged from the
/// hand-rolled walk (the default exclude list carries both `dir` and `dir/*`
/// forms for belt-and-suspenders, #2200).
fn collect_files(
    root: &Path,
    include_patterns: &[String],
    exclude_patterns: &[String],
    cancel_token: Option<&CancellationToken>,
    aborted: &std::sync::atomic::AtomicBool,
) -> Result<Vec<PathBuf>, ToolError> {
    check_cancelled(cancel_token, aborted)?;

    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }

    let prune: Vec<String> = exclude_patterns
        .iter()
        .filter(|p| !p.contains('*') && !p.contains('/') && !p.contains('?'))
        .cloned()
        .collect();
    let mut files = Vec::new();
    let it = ignore::WalkBuilder::new(root)
        .hidden(false) // keep the historic inclusive behavior for dotfiles
        .follow_links(false) // matches the old walk's symlink skip
        .require_git(false) // apply .gitignore even outside a git repo
        .filter_entry(move |entry| {
            if entry.depth() != 0 && entry.file_type().is_some_and(|ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy();
                return !prune.iter().any(|p| *p == name);
            }
            true
        })
        .build();

    for entry in it {
        check_cancelled(cancel_token, aborted)?;
        let Ok(entry) = entry else {
            continue; // unreadable entry: skip like rg's warnings
        };
        // Depth 0 is the search root itself.
        if entry.depth() == 0 {
            continue;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        let relative_str = relative.to_string_lossy();
        if should_exclude(&relative_str, exclude_patterns) {
            continue;
        }
        if !include_patterns.is_empty() && !should_include(&relative_str, include_patterns) {
            continue;
        }
        files.push(entry.into_path());
    }
    Ok(files)
}

fn check_cancelled(
    cancel_token: Option<&CancellationToken>,
    aborted: &std::sync::atomic::AtomicBool,
) -> Result<(), ToolError> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled)
        || aborted.load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(ToolError::execution_failed(
            "search cancelled before completion",
        ));
    }
    Ok(())
}

/// Check if a path matches any of the exclude patterns
fn should_exclude(path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if matches_glob(path, pattern) {
            return true;
        }
    }
    false
}

/// Check if a path matches any of the include patterns
fn should_include(path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if matches_glob(path, pattern) {
            return true;
        }
    }
    false
}

/// Simple glob pattern matching
/// Supports: * (any chars), ** (any path), ? (single char)
pub fn matches_glob(path: &str, pattern: &str) -> bool {
    // The walker yields OS-native separators; normalize so directory globs
    // (e.g. "src/*.rs" and the default "node_modules/*" excludes) also
    // match on Windows, where relative paths come out with '\'.
    let normalized;
    let path = if path.contains('\\') {
        normalized = path.replace('\\', "/");
        normalized.as_str()
    } else {
        path
    };
    // Handle ** for any path
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let prefix = parts[0].trim_end_matches('/');
            let suffix = parts[1].trim_start_matches('/');

            if !prefix.is_empty() && !path.starts_with(prefix) {
                return false;
            }
            if !suffix.is_empty() {
                return path.ends_with(suffix)
                    || path
                        .split('/')
                        .any(|part| matches_simple_glob(part, suffix));
            }
            let dir_prefix = format!("{prefix}/");
            return prefix.is_empty() || path.starts_with(&dir_prefix);
        }
        if parts.len() > 2 && matches_multi_double_star(path, &parts) {
            return true;
        }
    }

    // Handle patterns like "*.rs" - match against filename only
    if pattern.starts_with('*') && !pattern.contains('/') {
        let filename = path.rsplit('/').next().unwrap_or(path);
        return matches_simple_glob(filename, pattern);
    }

    // Handle patterns with path components
    if pattern.contains('/') {
        return matches_simple_glob(path, pattern);
    }

    // Match against filename
    let filename = path.rsplit('/').next().unwrap_or(path);
    matches_simple_glob(filename, pattern)
}

/// `**` matching for patterns with more than one `**`: the literal segments
/// around each `**` must appear in order — the first anchored at the start of
/// the path, the last at the end, and the middle ones as whole path
/// components.
fn matches_multi_double_star(path: &str, parts: &[&str]) -> bool {
    let components: Vec<&str> = path.split('/').collect();
    let first: Vec<&str> = parts[0]
        .trim_end_matches('/')
        .split('/')
        .filter(|c| !c.is_empty())
        .collect();
    let last: Vec<&str> = parts[parts.len() - 1]
        .trim_start_matches('/')
        .split('/')
        .filter(|c| !c.is_empty())
        .collect();
    if components.len() < first.len() + last.len()
        || components[..first.len()] != first[..]
        || components[components.len() - last.len()..] != last[..]
    {
        return false;
    }
    let mut window_start = first.len();
    let window_end = components.len() - last.len();
    for part in &parts[1..parts.len() - 1] {
        let segment: Vec<&str> = part
            .trim_matches('/')
            .split('/')
            .filter(|c| !c.is_empty())
            .collect();
        if segment.is_empty() {
            continue;
        }
        let mut next = window_start;
        while next + segment.len() <= window_end
            && components[next..next + segment.len()] != segment[..]
        {
            next += 1;
        }
        if next + segment.len() > window_end {
            return false;
        }
        window_start = next + segment.len();
    }
    true
}

/// Simple glob matching for single path component
fn matches_simple_glob(text: &str, pattern: &str) -> bool {
    // Iterative wildcard match: on mismatch, backtrack only to the most
    // recent `*` — the recursive alternative is exponential for k stars.
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (usize::MAX, 0usize);
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            star_ti += 1;
            ti = star_ti;
            pi = star_pi + 1;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use codesmith_agent_runtime::tools::spec::{ApprovalRequirement, ToolContext, ToolSpec};

    use super::{GrepFilesTool, matches_glob};

    #[test]
    fn test_matches_glob_star() {
        assert!(matches_glob("test.rs", "*.rs"));
        assert!(matches_glob("foo.rs", "*.rs"));
        assert!(!matches_glob("test.ts", "*.rs"));
        assert!(!matches_glob("test.rs.bak", "*.rs"));
    }

    #[test]
    fn test_matches_glob_question() {
        assert!(matches_glob("test.rs", "test.??"));
        assert!(!matches_glob("test.rs", "test.?"));
    }

    #[test]
    fn test_matches_glob_double_star() {
        assert!(matches_glob("src/main.rs", "src/**"));
        assert!(matches_glob("src/lib/mod.rs", "src/**"));
        assert!(matches_glob("node_modules/pkg/index.js", "node_modules/*"));
    }

    #[test]
    fn test_matches_glob_double_star_requires_directory_boundary() {
        assert!(!matches_glob("srcfoo/main.rs", "src/**"));
        assert!(!matches_glob("srcfoo/lib/main.rs", "src/**"));
        assert!(matches_glob("src/main.rs", "src/**"));
    }

    #[test]
    fn test_matches_glob_multiple_double_star_segments() {
        assert!(matches_glob("a/b/c", "a/**/b/**/c"));
        assert!(matches_glob("a/x/b/y/z/c", "a/**/b/**/c"));
        assert!(!matches_glob("a/x/c", "a/**/b/**/c"));
        assert!(!matches_glob("afoo/b/c", "a/**/b/**/c"));
    }

    #[test]
    fn test_matches_glob_path() {
        assert!(matches_glob("src/main.rs", "src/*.rs"));
        assert!(!matches_glob("lib/main.rs", "src/*.rs"));
    }

    /// Regression for #249: byte-index slicing panics on multi-byte
    /// characters inside filenames like `dialogue_line__冰糖.mp3`.
    #[test]
    fn test_matches_glob_unicode_filename() {
        let filename = "dialogue_line__冰糖.mp3";
        // The filename should match *.mp3 without panicking.
        assert!(matches_glob(filename, "*.mp3"));
        // Asterisk matching against multi-byte characters must succeed.
        assert!(matches_glob(filename, "dialogue_line__*"));
        // Literal multi-byte characters inside the pattern must match.
        assert!(matches_glob(filename, "*冰糖*"));
        // Non-matching pattern must not panic either.
        assert!(!matches_glob(filename, "nonexistent*"));
    }

    /// Many `*`s must stay polynomial — the recursive matcher blew up
    /// exponentially on patterns like this.
    #[test]
    fn test_matches_glob_many_stars_stay_linear() {
        let path = "a".repeat(200);
        assert!(!matches_glob(&path, "*a*a*a*a*a*a*a*a*a*b"));
        assert!(matches_glob(&path, "*a*a*a*a*a*a*a*a*a*"));
    }

    #[tokio::test]
    async fn test_grep_files_skips_default_excluded_directories() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::create_dir_all(tmp.path().join("node_modules/pkg")).expect("mkdir");
        fs::write(tmp.path().join("node_modules/pkg/index.js"), "NEEDLE\n").expect("write");
        fs::write(tmp.path().join("visible.js"), "NEEDLE\n").expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "NEEDLE"}), &ctx)
            .await
            .expect("execute");

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"].as_str().unwrap(), "visible.js");
    }

    #[tokio::test]
    async fn test_grep_files_with_matches_lists_files_newest_first() {
        use std::time::{Duration, SystemTime};

        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        for (name, age) in [("old.txt", 3600), ("new.txt", 0)] {
            let path = root.join(name);
            fs::write(&path, "NEEDLE\n").expect("write");
            let file = fs::File::options().write(true).open(&path).expect("open");
            let modified = SystemTime::now()
                .checked_sub(Duration::from_secs(age))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            file.set_times(fs::FileTimes::new().set_modified(modified))
                .expect("set mtime");
        }
        fs::write(root.join("miss.txt"), "nope\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = GrepFilesTool;
        let result = tool
            .execute(
                json!({"pattern": "NEEDLE", "output_mode": "files_with_matches"}),
                &ctx,
            )
            .await
            .expect("execute");

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["output_mode"], json!("files_with_matches"));
        assert_eq!(parsed["matching_files"], json!(2));
        let files = parsed["files"].as_array().unwrap();
        assert_eq!(
            files[0]["file"].as_str().unwrap(),
            "new.txt",
            "newest match first: {files:?}"
        );
        assert_eq!(files[1]["file"].as_str().unwrap(), "old.txt");
        assert!(
            files[0]["last_modified_ms"].as_u64().unwrap()
                > files[1]["last_modified_ms"].as_u64().unwrap()
        );

        // Truncation in files mode caps the file list and carries the hint.
        let capped = tool
            .execute(
                json!({"pattern": "NEEDLE", "output_mode": "files_with_matches", "max_results": 1}),
                &ctx,
            )
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&capped.content).unwrap();
        assert_eq!(parsed["truncated"], json!(true));
        assert!(parsed["truncation_hint"].is_string());

        // An unknown mode is a loud input error, not a silent default.
        let bad = tool
            .execute(json!({"pattern": "x", "output_mode": "filenames"}), &ctx)
            .await;
        assert!(bad.is_err());
    }

    #[tokio::test]
    async fn test_grep_files_truncated_results_carry_continuation_hint() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        for i in 0..5 {
            fs::write(root.join(format!("f{i}.txt")), "NEEDLE\n").expect("write");
        }
        let ctx = ToolContext::new(root.to_path_buf());

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "NEEDLE", "max_results": 2}), &ctx)
            .await
            .expect("execute");

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["truncated"], json!(true));
        assert!(
            parsed["truncation_hint"]
                .as_str()
                .unwrap()
                .contains("narrow"),
            "hint must be an executable continuation: {}",
            parsed["truncation_hint"]
        );

        // Untruncated results carry no hint (nothing to continue from).
        let full = tool
            .execute(json!({"pattern": "NEEDLE"}), &ctx)
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&full.content).unwrap();
        assert!(parsed.get("truncation_hint").is_none());
    }

    #[tokio::test]
    async fn test_grep_files_default_excludes_credential_files() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join(".env"), "NEEDLE\n").expect("write");
        std::fs::write(root.join("id_rsa"), "NEEDLE\n").expect("write");
        std::fs::write(root.join(".env.local"), "NEEDLE\n").expect("write");
        std::fs::write(root.join("visible.js"), "NEEDLE\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "NEEDLE"}), &ctx)
            .await
            .expect("execute");

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "only the visible file matches");
        assert_eq!(matches[0]["file"].as_str().unwrap(), "visible.js");

        // An explicit `exclude` array overrides the defaults — the escape
        // hatch for a deliberate secrets audit must exist.
        let override_result = tool
            .execute(json!({"pattern": "NEEDLE", "exclude": []}), &ctx)
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&override_result.content).unwrap();
        assert_eq!(
            parsed["matches"].as_array().unwrap().len(),
            4,
            "exclude=[] restores the credential files"
        );
    }

    #[tokio::test]
    async fn test_grep_files_bare_exclude_name_prunes_walk() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::create_dir_all(tmp.path().join("vendored/lib")).expect("mkdir");
        fs::write(tmp.path().join("vendored/lib/a.js"), "NEEDLE\n").expect("write");
        fs::write(tmp.path().join("visible.js"), "NEEDLE\n").expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "NEEDLE", "exclude": ["vendored"]}), &ctx)
            .await
            .expect("execute");

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"].as_str().unwrap(), "visible.js");
    }

    #[tokio::test]
    async fn test_grep_files_basic() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        // Create test files
        fs::write(
            tmp.path().join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .expect("write");
        fs::write(
            tmp.path().join("lib.rs"),
            "pub fn hello() {}\npub fn world() {}\n",
        )
        .expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "fn"}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert!(result.content.contains("main"));
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn test_grep_files_with_context() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(
            tmp.path().join("test.txt"),
            "line1\nline2\nMATCH\nline4\nline5\n",
        )
        .expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "MATCH", "context_lines": 1}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert!(result.content.contains("line2")); // context before
        assert!(result.content.contains("line4")); // context after

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["context_before"], "line2");
        assert_eq!(matches[0]["context_after"], "line4");
        assert!(matches[0]["context_before"].is_string());
        assert!(matches[0]["context_after"].is_string());
    }

    #[tokio::test]
    async fn test_grep_files_multi_line_context_remains_arrays() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join("test.txt"), "a\nb\nMATCH\nd\ne\n").expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "MATCH", "context_lines": 2}), &ctx)
            .await
            .expect("execute");

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["context_before"], json!(["a", "b"]));
        assert_eq!(matches[0]["context_after"], json!(["d", "e"]));
    }

    #[tokio::test]
    async fn test_grep_files_case_insensitive() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(
            tmp.path().join("test.txt"),
            "Hello World\nHELLO WORLD\nhello world\n",
        )
        .expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "hello", "case_insensitive": true}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        // Should find all 3 lines
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_matches"].as_u64().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_grep_files_include_filter() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join("test.rs"), "fn test() {}\n").expect("write");
        fs::write(tmp.path().join("test.js"), "function test() {}\n").expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "test", "include": ["*.rs"]}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        // Should only match .rs file
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let file = matches[0]["file"].as_str().unwrap();
        assert!(
            file.rsplit('.')
                .next()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        );
    }

    #[tokio::test]
    async fn test_grep_files_multiline_matches_across_line_breaks() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join("multi.txt"), "alpha\nbeta\n").expect("write");

        let tool = GrepFilesTool;

        // Line mode never sees the newline, so the pattern cannot match.
        let result = tool
            .execute(json!({"pattern": "alpha\\nbeta", "multiline": false}), &ctx)
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_matches"].as_u64().unwrap(), 0);

        // Multiline mode searches the whole buffer; the match is reported at
        // its starting line.
        let result = tool
            .execute(json!({"pattern": "alpha\\nbeta", "multiline": true}), &ctx)
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_matches"].as_u64().unwrap(), 1);
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches[0]["line_number"].as_u64().unwrap(), 1);
        assert_eq!(matches[0]["line"].as_str().unwrap(), "alpha");

        // Inline `(?s)` lets `.` cross newlines in multiline mode.
        let result = tool
            .execute(
                json!({"pattern": "(?s)alpha.beta", "multiline": true}),
                &ctx,
            )
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_matches"].as_u64().unwrap(), 1);
    }

    /// Regression: a byte-class pattern (`(?-u:...)`) can match mid-codepoint,
    /// so `m.start()` is not a char boundary — the line computation must slice
    /// the byte view, not the `String`.
    #[tokio::test]
    async fn test_grep_files_multiline_match_at_non_char_boundary() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        // é/ö are two-byte sequences; their continuation bytes (0xA9/0xB6)
        // sit inside a char, not at a boundary.
        fs::write(tmp.path().join("utf8.txt"), "héllo\nwörld\n").expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(
                json!({"pattern": "(?-u:\\xa9|\\xb6)", "multiline": true}),
                &ctx,
            )
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_matches"].as_u64().unwrap(), 2);
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches[0]["line_number"].as_u64().unwrap(), 1);
        assert_eq!(matches[1]["line_number"].as_u64().unwrap(), 2);
    }

    #[tokio::test]
    async fn test_grep_files_respects_gitignore() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join(".gitignore"), "ignored_dir/\n").expect("write");
        fs::create_dir_all(tmp.path().join("ignored_dir")).expect("mkdir");
        fs::write(tmp.path().join("ignored_dir/hidden.rs"), "NEEDLE\n").expect("write");
        fs::write(tmp.path().join("visible.rs"), "NEEDLE\n").expect("write");

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "NEEDLE"}), &ctx)
            .await
            .expect("execute");

        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let matches = parsed["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "gitignore'd directory must be skipped");
        assert_eq!(matches[0]["file"].as_str().unwrap(), "visible.rs");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_grep_files_does_not_follow_symlinked_files() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).expect("mkdir workspace");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        let outside_file = outside.join("secret.txt");
        fs::write(&outside_file, "NEEDLE\n").expect("write outside");
        std::os::unix::fs::symlink(&outside_file, root.join("secret.txt")).expect("symlink");

        let ctx = ToolContext::new(root);
        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "NEEDLE"}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_matches"].as_u64().unwrap(), 0);
        assert_eq!(parsed["files_searched"].as_u64().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_grep_files_invalid_regex() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let tool = GrepFilesTool;
        let result = tool.execute(json!({"pattern": "[invalid"}), &ctx).await;

        assert!(result.is_err());
        let message = format!("{}", result.expect_err("invalid regex must error"));
        assert!(
            message.contains("literal=true"),
            "the error must name the literal escape hatch: {message}"
        );
    }

    #[tokio::test]
    async fn test_grep_files_literal_matches_metacharacters_verbatim() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("call.txt"), "foo(bar) baz\nno match here\n").expect("write");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let tool = GrepFilesTool;
        let result = tool
            .execute(json!({"pattern": "foo(bar)", "literal": true}), &ctx)
            .await
            .expect("literal search executes");

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["total_matches"].as_u64().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_grep_files_respects_cancel_token() {
        let tmp = tempdir().expect("tempdir");
        fs::write(tmp.path().join("test.txt"), "needle\n").expect("write");
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();
        let ctx = ToolContext::new(tmp.path().to_path_buf()).with_cancel_token(cancel_token);

        let tool = GrepFilesTool;
        let err = tool
            .execute(json!({"pattern": "needle"}), &ctx)
            .await
            .expect_err("cancelled grep should return an error");

        assert!(
            format!("{err:?}").contains("cancelled"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn test_grep_files_tool_properties() {
        let tool = GrepFilesTool;
        assert_eq!(tool.name(), "grep_files");
        assert!(tool.is_read_only());
        assert!(tool.is_sandboxable());
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Auto);
    }

    #[test]
    fn test_parallel_support_flags() {
        let tool = GrepFilesTool;
        assert!(tool.supports_parallel());
    }
}
