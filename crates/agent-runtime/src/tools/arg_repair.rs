//! Deterministic JSON argument repair for malformed tool-call inputs.
//!
//! DeepSeek streams `tool_calls.function.arguments` as deltas. Two failure
//! shapes are common: (a) SSE chunk boundary cuts inside a JSON string and
//! reassembly leaves a trailing comma or unclosed brace; (b) some local
//! backends emit literal control characters inside JSON string values.
//!
//! The repair ladder runs five stages before falling back to an empty object:
//!
//!  1. Strict parse — done if it parses.
//!  2. Strip literal control chars inside string values.
//!  3. Strip trailing commas before `}` or `]`.
//!  4. Balance braces/brackets (append closers).
//!  5. Strip excess closers if delta is negative.
//!  6. Fallback: empty object `{}`.

use serde_json::{Map, Value};

/// Maximum raw argument length we'll attempt to repair (1 MiB).
const MAX_ARG_LEN: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ArgRepairError {
    #[error("argument exceeded {0} chars; refusing to repair")]
    TooLarge(usize),
}

/// Repair a raw JSON argument string into a valid `serde_json::Value`.
///
/// Runs the deterministic ladder; on success returns the parsed value.
/// The final fallback is an empty object `{}` so dispatch always proceeds.
pub fn repair(raw: &str) -> Result<Value, ArgRepairError> {
    if raw.len() > MAX_ARG_LEN {
        return Err(ArgRepairError::TooLarge(raw.len()));
    }
    // Stage 1: strict parse
    if let Ok(v) = serde_json::from_str(raw) {
        return Ok(v);
    }
    // Stage 2: strip control chars inside strings
    let mut s = strip_control_chars_in_strings(raw);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Stage 3: strip trailing commas
    s = strip_trailing_commas(&s);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Stage 4: balance braces
    s = balance_braces(&s, 50);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Stage 5: strip excess closers
    s = strip_excess_closers(&s);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Fallback: empty object. Log only the length — the raw model tool
    // arguments may be sensitive and must never reach the logs.
    tracing::debug!(
        target: "codesmith_agent_runtime::arg_repair",
        "arg repair ladder exhausted after {} chars; falling back to empty object",
        raw.len()
    );
    Ok(Value::Object(Map::new()))
}

/// Strip ASCII control characters (0x00–0x1F except \t, \n, \r) that appear
/// inside JSON string values. We walk character-by-character tracking whether
/// we're inside a string (between unescaped double-quotes).
fn strip_control_chars_in_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            out.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            out.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if in_string && (ch as u32) < 0x20 {
            // Escape the three serde_json rejects in escaped form
            // (RFC 8259 forbids ALL raw control chars in strings — literal
            // tab/newline/CR included, which are the most common form of
            // the documented local-backend failure mode); drop the rest.
            match ch {
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => {}
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Strip trailing commas before `}` or `]` (or at end of input). A
/// quote/escape-aware scan so `,}` / `,]` sequences inside string values are
/// left untouched.
fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if escape {
            out.push(ch);
            escape = false;
            i += 1;
            continue;
        }
        if in_string {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            i += 1;
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            ',' => {
                let mut next = i + 1;
                while next < chars.len() && chars[next].is_ascii_whitespace() {
                    next += 1;
                }
                if next == chars.len() || chars[next] == '}' || chars[next] == ']' {
                    i += 1;
                    continue;
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
        i += 1;
    }
    out
}

/// Balance braces and brackets: append a closer for every unclosed opener,
/// in reverse (LIFO) order. A quote/escape-aware scan so brackets inside
/// string values don't count. Caps iterations so a catastrophically broken
/// input doesn't loop forever.
fn balance_braces(s: &str, max_iter: usize) -> String {
    let mut out = s.to_string();
    for _ in 0..max_iter {
        let mut stack: Vec<char> = Vec::new();
        let mut in_string = false;
        let mut escape = false;
        for ch in out.chars() {
            if escape {
                escape = false;
            } else if in_string {
                if ch == '\\' {
                    escape = true;
                } else if ch == '"' {
                    in_string = false;
                }
            } else {
                match ch {
                    '"' => in_string = true,
                    '{' | '[' => stack.push(ch),
                    '}' => {
                        if stack.last() == Some(&'{') {
                            stack.pop();
                        }
                    }
                    ']' => {
                        if stack.last() == Some(&'[') {
                            stack.pop();
                        }
                    }
                    _ => {}
                }
            }
        }
        if stack.is_empty() || in_string {
            break;
        }
        while let Some(open) = stack.pop() {
            out.push(match open {
                '{' => '}',
                '[' => ']',
                _ => unreachable!(),
            });
        }
    }
    out
}

/// Strip excess closers when the delta is negative (more closes than opens).
/// A quote/escape-aware scan so closers inside string values are kept and do
/// not consume depth budget.
fn strip_excess_closers(s: &str) -> String {
    let mut brace_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if escape {
            out.push(ch);
            escape = false;
            continue;
        }
        if in_string {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '}' => {
                if brace_depth > 0 {
                    brace_depth -= 1;
                    out.push(ch);
                }
                // else drop excess closer
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                    out.push(ch);
                }
            }
            '{' => {
                brace_depth += 1;
                out.push(ch);
            }
            '[' => {
                bracket_depth += 1;
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_parse_passes_through() {
        let v = repair(r#"{"path": "hello.txt"}"#).unwrap();
        assert_eq!(v, json!({"path": "hello.txt"}));
    }

    #[test]
    fn repairs_trailing_comma() {
        let v = repair(r#"{"path": "hello.txt",}"#).unwrap();
        assert_eq!(v, json!({"path": "hello.txt"}));
    }

    #[test]
    fn repairs_trailing_comma_in_array() {
        let v = repair(r#"["a", "b",]"#).unwrap();
        assert_eq!(v, json!(["a", "b"]));
    }

    #[test]
    fn repairs_missing_close_brace() {
        let v = repair(r#"{"path": "hello.txt""#).unwrap();
        assert_eq!(v, json!({"path": "hello.txt"}));
    }

    #[test]
    fn repairs_missing_close_bracket() {
        let v = repair(r#"["a", "b""#).unwrap();
        assert_eq!(v, json!(["a", "b"]));
    }

    #[test]
    fn escapes_literal_newline_and_tab_in_values() {
        // serde_json rejects ALL raw control chars in strings (RFC 8259) —
        // literal \n/\t used to survive stage 2 and the whole repair
        // silently fell back to `{}`.
        let raw = "{\"note\": \"line one\nline two\tindented\"}";
        let v = repair(raw).unwrap();
        assert_eq!(v, json!({"note": "line one\nline two\tindented"}));
    }

    #[test]
    fn strips_embedded_control_chars() {
        // Raw \x0B (vertical tab) inside a string value
        let raw = "{\"key\": \"val\x0Bue\"}";
        let v = repair(raw).unwrap();
        assert_eq!(v, json!({"key": "value"}));
    }

    #[test]
    fn handles_empty_string() {
        let v = repair("").unwrap();
        assert_eq!(v, json!({}));
    }

    #[test]
    fn handles_gibberish() {
        let v = repair("not json at all").unwrap();
        assert_eq!(v, json!({}));
    }

    #[test]
    fn balances_nested_braces() {
        let v = repair(r#"{"outer": {"inner": "val""#).unwrap();
        assert_eq!(v, json!({"outer": {"inner": "val"}}));
    }

    #[test]
    fn strips_excess_closers() {
        let v = repair(r#"{"key": "val"}}"#).unwrap();
        assert_eq!(v, json!({"key": "val"}));
    }

    #[test]
    fn handles_double_encoded_json() {
        // This is a valid JSON string containing a JSON object literal.
        // repair parses it as a string; the engine's existing fallback
        // (parse_tool_input) will unwrap the string and re-parse.
        let v = repair(r#""{\"path\": \"hello.txt\"}""#).unwrap();
        assert_eq!(v, Value::String(r#"{"path": "hello.txt"}"#.to_string()));
    }

    #[test]
    fn oversize_input_rejected() {
        let big = "x".repeat(MAX_ARG_LEN + 1);
        assert!(repair(&big).is_err());
    }

    #[test]
    fn repairs_brace_balance_with_trailing_comma() {
        let v = repair(r#"{"a": 1,"#).unwrap();
        assert_eq!(v, json!({"a": 1}));
    }

    #[test]
    fn trailing_comma_strip_skips_comma_closer_inside_strings() {
        let v = repair(r#"{"a": "x,}", "b": 1"#).unwrap();
        assert_eq!(v, json!({"a": "x,}", "b": 1}));
        let v = repair(r##"{"a": "x,]", "b": ["y,]""##).unwrap();
        assert_eq!(v, json!({"a": "x,]", "b": ["y,]"]}));
    }

    #[test]
    fn balances_unclosed_openers_in_lifo_order() {
        let v = repair(r#"[{"a": 1"#).unwrap();
        assert_eq!(v, json!([{"a": 1}]));
    }

    #[test]
    fn brace_balance_ignores_brackets_inside_strings() {
        let v = repair(r##"{"a": "[""##).unwrap();
        assert_eq!(v, json!({"a": "["}));
    }

    #[test]
    fn excess_closer_strip_ignores_closers_inside_strings() {
        let v = repair(r#"{"a": "}", "b": 1}}"#).unwrap();
        assert_eq!(v, json!({"a": "}", "b": 1}));
    }
}
