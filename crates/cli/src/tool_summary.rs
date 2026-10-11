//! One-line summaries of tool args and outputs for terminal output.
//!
//! The retired ratatui transcript rendered rich cards; the exec runner and
//! the REPL event pump only need plain single-line summaries, which is what
//! lives here. The arg-summarizer's key priority (path, command, query,
//! prompt, text, pattern) mirrors the retired `tui/history.rs` behavior.

use serde_json::Value;

/// Summarize tool-call arguments as `key: value` pairs for one line.
pub fn summarize_tool_args(input: &Value) -> Option<String> {
    let obj = input.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for key in [
        "path", "command", "query", "prompt", "text", "pattern", "url", "name",
    ] {
        if let Some(value) = obj.get(key) {
            parts.push(format!("{key}: {}", summarize_inline_value(value, 80)));
        }
    }
    if parts.is_empty() {
        // Fall back to the first few keys so unknown tools still show
        // something recognizable.
        for (key, value) in obj.iter().take(3) {
            parts.push(format!("{key}: {}", summarize_inline_value(value, 60)));
        }
    }
    Some(parts.join(", "))
}

fn summarize_inline_value(value: &Value, max_chars: usize) -> String {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    truncate_chars(&text, max_chars)
}

/// Summarize a tool output for one line: the first informative line,
/// truncated. Multiline outputs collapse to their first line plus a count.
pub fn summarize_tool_output(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut lines = trimmed.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("").to_string();
    let remaining = lines.count();
    let mut summary = truncate_chars(&first, 200);
    if remaining > 0 {
        summary.push_str(&format!(
            " (+{remaining} line{s})",
            s = if remaining == 1 { "" } else { "s" }
        ));
    }
    summary
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.replace('\n', " ⏎ ");
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}

/// How many completed tool calls the REPL keeps for `/expand` replay.
pub const TOOL_LOG_CAP: usize = 200;

/// Cap on replayed output lines per tool call (`/expand`, Ctrl+O replay).
pub const DETAIL_MAX_LINES: usize = 50;

/// One recorded tool call: enough to re-render its collapsed line or
/// reprint full detail later (`/expand`, the in-turn Ctrl+O toggle).
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    /// Position in the session-wide log, 1-based (`/expand <n>` addresses it).
    pub seq: usize,
    /// Turn index the call ran in (`ReplState::tool_log_turn`).
    pub turn: usize,
    pub name: String,
    /// One-line arg summary (`summarize_tool_args`), when the call had args.
    pub args_summary: Option<String>,
    /// Full output content on success.
    pub output: Option<String>,
    /// Error text on failure.
    pub error: Option<String>,
    /// Detail already replayed once by an in-turn Ctrl+O expand (guards
    /// duplicate reprints when the user toggles more than once).
    pub replayed: bool,
}

/// Multi-line detail block for one recorded tool call — the `/expand` and
/// Ctrl+O replay form. Uncolored; callers style it. Output is capped at
/// `max_lines` with a remainder note.
pub fn tool_detail_block(record: &ToolCallRecord, max_lines: usize) -> String {
    let args = record
        .args_summary
        .as_deref()
        .filter(|summary| !summary.is_empty())
        .map(|summary| format!("({summary})"))
        .unwrap_or_default();
    let mut block = format!("#{} {}{}", record.seq, record.name, args);
    match (&record.output, &record.error) {
        (Some(output), _) => {
            let lines: Vec<&str> = output.lines().collect();
            if lines.is_empty() {
                block.push_str("\n  (no output)");
            } else {
                let shown = lines.len().min(max_lines);
                for line in lines.iter().take(shown) {
                    block.push_str("\n  ");
                    block.push_str(line);
                }
                if lines.len() > shown {
                    block.push_str(&format!("\n  … (+{} more lines)", lines.len() - shown));
                }
            }
        }
        (_, Some(error)) => {
            block.push_str("\n  error: ");
            block.push_str(error);
        }
        (None, None) => block.push_str("\n  (no output)"),
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(output: Option<&str>, error: Option<&str>) -> ToolCallRecord {
        ToolCallRecord {
            seq: 3,
            turn: 2,
            name: "read_file".to_string(),
            args_summary: Some("path: src/main.rs".to_string()),
            output: output.map(str::to_string),
            error: error.map(str::to_string),
            replayed: false,
        }
    }

    #[test]
    fn detail_block_caps_output_lines() {
        let rec = record(Some("l1\nl2\nl3\nl4"), None);
        let block = tool_detail_block(&rec, 2);
        assert!(block.contains("#3 read_file(path: src/main.rs)"));
        assert!(block.contains("\n  l1"));
        assert!(block.contains("\n  l2"));
        assert!(!block.contains("l3"));
        assert!(block.contains("(+2 more lines)"));
    }

    #[test]
    fn detail_block_shows_error_and_empty_output() {
        let err = record(None, Some("boom"));
        assert!(tool_detail_block(&err, 10).contains("error: boom"));
        let empty = record(Some(""), None);
        assert!(tool_detail_block(&empty, 10).contains("(no output)"));
        let bare = record(None, None);
        assert!(tool_detail_block(&bare, 10).contains("(no output)"));
    }

    #[test]
    fn detail_block_omits_empty_args() {
        let mut rec = record(Some("ok"), None);
        rec.args_summary = None;
        assert!(tool_detail_block(&rec, 10).starts_with("#3 read_file\n"));
    }
}
