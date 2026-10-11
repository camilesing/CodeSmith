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
