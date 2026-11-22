//! # Output parsers
//!
//! The LangChain `BaseOutputParser` analog: turn a model's text answer into
//! a structured value, and tell the model (via
//! [`format_instructions`](OutputParser::format_instructions)) how to shape
//! the answer up front. Kept deliberately small — two methods, one JSON
//! implementation. Typed results are [`crate::structured`]'s job (schema +
//! validation + retry live there); this module is the text-in/value-out half.

use anyhow::Result;
use serde_json::Value;

/// Parse a model's text output into a [`serde_json::Value`], and describe
/// the expected shape for the prompt (`format_instructions`).
pub trait OutputParser: Send + Sync {
    /// Parse `text`. Errors loudly on malformed output — the caller decides
    /// whether to feed the error back and retry.
    fn parse(&self, text: &str) -> Result<Value>;

    /// The instruction sentence appended to a prompt telling the model what
    /// shape to answer in.
    fn format_instructions(&self) -> String;
}

/// Parses JSON, tolerating the markdown fences models love to wrap answers
/// in (```` ```json … ``` ```` or bare ```` ``` … ``` ````).
///
/// Known limitations (deliberate): no partial/streaming JSON assembly and no
/// JSON-array-per-line mode — this parses one complete JSON document.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonOutputParser;

impl JsonOutputParser {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Strip one leading/trailing markdown code fence, if present.
    fn unfence(text: &str) -> &str {
        let trimmed = text.trim();
        let Some(after_open) = trimmed.strip_prefix("```") else {
            return trimmed;
        };
        // Skip the optional language tag on the opening fence (`json`, `ts`…).
        let body_start = after_open.find('\n').map_or(after_open.len(), |n| n + 1);
        let body = &after_open[body_start..];
        body.strip_suffix("```")
            .map(str::trim)
            .unwrap_or(body.trim())
    }
}

impl OutputParser for JsonOutputParser {
    fn parse(&self, text: &str) -> Result<Value> {
        let cleaned = Self::unfence(text);
        serde_json::from_str(cleaned).map_err(|err| {
            anyhow::anyhow!(
                "model output did not parse as JSON ({err}); output head: {:?}",
                cleaned.chars().take(200).collect::<String>()
            )
        })
    }

    fn format_instructions(&self) -> String {
        "Answer with a single JSON document and nothing else — no prose, no \
         markdown fences."
            .to_string()
    }
}

// === Tests ==================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json() {
        let value = JsonOutputParser::new()
            .parse(r#"{"answer": 42}"#)
            .expect("plain JSON parses");
        assert_eq!(value["answer"], 42);
    }

    #[test]
    fn parses_fenced_json_with_language_tag() {
        let fenced = "```json\n{\"a\": [1, 2]}\n```";
        let value = JsonOutputParser::new()
            .parse(fenced)
            .expect("fenced JSON parses");
        assert_eq!(value["a"][1], 2);
    }

    #[test]
    fn malformed_json_fails_loud_with_head() {
        let err = JsonOutputParser::new()
            .parse("the answer is forty-two")
            .expect_err("prose must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("did not parse as JSON"), "{msg}");
        assert!(msg.contains("the answer is"), "{msg}");
    }

    #[test]
    fn instructions_name_the_json_shape() {
        assert!(
            JsonOutputParser::new()
                .format_instructions()
                .contains("JSON")
        );
    }
}
