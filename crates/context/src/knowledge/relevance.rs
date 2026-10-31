//! Side-query relevance ranking for memory selection.
//!
//! Uses a DeepSeek API call to rank memory files by relevance to the
//! user's current query. Returns up to `MAX_MEMORIES_PER_TURN` most
//! relevant filenames, mirroring the TypeScript `findRelevantMemories` /
//! `selectRelevantMemories` pattern.

use std::collections::HashSet;
#[cfg(test)]
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::budget::MAX_MEMORIES_PER_TURN;
use super::scan::MemoryHeader;

/// Error type for relevance ranking operations.
#[derive(Debug)]
#[non_exhaustive]
pub enum RelevanceError {
    /// The side-query API call failed.
    ApiError(String),
    /// The response could not be parsed as valid JSON.
    ParseError(String),
    /// The operation was cancelled.
    Cancelled,
}

impl std::fmt::Display for RelevanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiError(msg) => write!(f, "relevance API error: {msg}"),
            Self::ParseError(msg) => write!(f, "relevance parse error: {msg}"),
            Self::Cancelled => write!(f, "relevance query cancelled"),
        }
    }
}

impl std::error::Error for RelevanceError {}

/// Timeout for the side-query API call — a hung or slow request must not
/// stall memory selection indefinitely.
const SIDE_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// System prompt for the side-query model that selects relevant memories.
/// Built from `MAX_MEMORIES_PER_TURN` so the instructed limit cannot drift
/// from the truncation enforced in `parse_selected_memories`.
fn select_memories_system_prompt() -> String {
    format!(
        "You are selecting memories that will be useful to an AI coding \
         assistant as it processes a user's query. You will be given the \
         user's query and a list of available memory files with their \
         filenames and descriptions.\n\n\
         Return a JSON object with a \"selected_memories\" array of filenames \
         for the memories that will clearly be useful (up to \
         {MAX_MEMORIES_PER_TURN}). Only include memories that you are certain \
         will be helpful based on their name and description. If unsure, do \
         not include it. If none are clearly useful, return an empty list.\n\n\
         Example response:\n\
         {{\"selected_memories\": [\"user_role.md\", \"project_build.md\"]}}"
    )
}

/// Format memory headers into a manifest string for the side-query.
fn format_memory_manifest(headers: &[MemoryHeader]) -> String {
    headers
        .iter()
        .map(|h| {
            let desc = h.description.as_deref().unwrap_or("(no description)");
            let typ = h.memory_type.map(|t| t.to_string()).unwrap_or_default();
            if typ.is_empty() {
                format!("{}: {}", h.filename, desc)
            } else {
                format!("{} [{}]: {}", h.filename, typ, desc)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Select relevant memories using a side-query to the DeepSeek API.
///
/// This is the core ranking function. It sends a non-streaming request
/// with the user query and memory manifest, asking the model to return
/// a JSON array of selected filenames.
///
/// The caller provides a `create_message` function that wraps the
/// actual API call, so this module stays decoupled from the specific
/// client implementation. The side-query future is dropped when the
/// cancel token fires or the timeout elapses, so `side_query_fn` must
/// be cancellation-safe.
pub async fn select_relevant_memories(
    user_query: &str,
    headers: &[MemoryHeader],
    recent_tools: &[String],
    // The caller provides a function that makes the actual API call.
    // This decouples relevance.rs from the DeepSeek client type.
    side_query_fn: impl FnOnce(
        String,
        String,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
    >,
    cancel_token: CancellationToken,
) -> Result<Vec<String>, RelevanceError> {
    if headers.is_empty() {
        return Ok(vec![]);
    }

    let manifest = format_memory_manifest(headers);
    let tools_section = if recent_tools.is_empty() {
        String::new()
    } else {
        format!("\n\nRecently used tools: {}", recent_tools.join(", "))
    };

    let user_message =
        format!("Query: {user_query}\n\nAvailable memories:\n{manifest}{tools_section}");

    let result = tokio::select! {
        _ = cancel_token.cancelled() => return Err(RelevanceError::Cancelled),
        response = tokio::time::timeout(
            SIDE_QUERY_TIMEOUT,
            side_query_fn(select_memories_system_prompt(), user_message),
        ) => response.map_err(|_| RelevanceError::ApiError("side-query timed out".to_string())),
    };

    // `result` is `Result<Result<String, String>, RelevanceError>`: the outer
    // error is the timeout, the inner one the side-query failure.
    let response_text = result.and_then(|inner| inner.map_err(RelevanceError::ApiError))?;

    // Parse the JSON response.
    parse_selected_memories(&response_text, headers)
}

/// Parse the side-query response and validate filenames against the manifest.
fn parse_selected_memories(
    response: &str,
    headers: &[MemoryHeader],
) -> Result<Vec<String>, RelevanceError> {
    // The model might return the JSON inline or with surrounding text.
    // Try to extract JSON from the response.
    let json_str = extract_json(response);

    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| RelevanceError::ParseError(format!("failed to parse JSON: {e}")))?;

    let selected = parsed
        .get("selected_memories")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            RelevanceError::ParseError(
                "missing or non-array \"selected_memories\" field".to_string(),
            )
        })?;

    // Validate: only return filenames that exist in the manifest, deduped.
    let valid_filenames: HashSet<&str> = headers.iter().map(|h| h.filename.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let filtered: Vec<String> = selected
        .iter()
        .filter_map(|item| item.as_str())
        .filter(|name| valid_filenames.contains(*name))
        .filter(|name| seen.insert(*name))
        .take(MAX_MEMORIES_PER_TURN)
        .map(String::from)
        .collect();

    Ok(filtered)
}

/// Extract JSON object from potentially mixed text response.
fn extract_json(response: &str) -> &str {
    // Try to find a JSON object delimited by { and }.
    if let Some(start) = response.find('{') {
        // Find the matching closing brace by counting depth, ignoring
        // braces that appear inside JSON string values.
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (i, c) in response[start..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        return &response[start..start + i + 1];
                    }
                }
                _ => {}
            }
        }
    }
    // Fallback: return the whole response and let JSON parsing fail naturally.
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_manifest_with_description() {
        let headers = vec![MemoryHeader {
            filename: "user_role.md".to_string(),
            file_path: PathBuf::from("/tmp/user_role.md"),
            mtime_ms: 0,
            description: Some("User is a Rust developer".to_string()),
            memory_type: Some(super::super::types::MemoryType::User),
        }];
        let manifest = format_memory_manifest(&headers);
        assert!(manifest.contains("user_role.md [user]: User is a Rust developer"));
    }

    #[test]
    fn format_manifest_without_description() {
        let headers = vec![MemoryHeader {
            filename: "bare.md".to_string(),
            file_path: PathBuf::from("/tmp/bare.md"),
            mtime_ms: 0,
            description: None,
            memory_type: None,
        }];
        let manifest = format_memory_manifest(&headers);
        assert!(manifest.contains("bare.md: (no description)"));
    }

    #[test]
    fn parse_selected_memories_valid_response() {
        let headers = vec![
            MemoryHeader {
                filename: "role.md".to_string(),
                file_path: PathBuf::from("/tmp/role.md"),
                mtime_ms: 0,
                description: Some("role".to_string()),
                memory_type: None,
            },
            MemoryHeader {
                filename: "build.md".to_string(),
                file_path: PathBuf::from("/tmp/build.md"),
                mtime_ms: 0,
                description: Some("build".to_string()),
                memory_type: None,
            },
        ];
        let response = "{\"selected_memories\": [\"role.md\", \"build.md\"]}";
        let result = parse_selected_memories(response, &headers).unwrap();
        assert_eq!(result, vec!["role.md", "build.md"]);
    }

    #[test]
    fn parse_selected_memories_filters_invalid_names() {
        let headers = vec![MemoryHeader {
            filename: "role.md".to_string(),
            file_path: PathBuf::from("/tmp/role.md"),
            mtime_ms: 0,
            description: Some("role".to_string()),
            memory_type: None,
        }];
        let response = "{\"selected_memories\": [\"role.md\", \"nonexistent.md\"]}";
        let result = parse_selected_memories(response, &headers).unwrap();
        assert_eq!(result, vec!["role.md"]);
    }

    #[test]
    fn parse_selected_memories_empty_array() {
        let headers = vec![MemoryHeader {
            filename: "role.md".to_string(),
            file_path: PathBuf::from("/tmp/role.md"),
            mtime_ms: 0,
            description: Some("role".to_string()),
            memory_type: None,
        }];
        let response = "{\"selected_memories\": []}";
        let result = parse_selected_memories(response, &headers).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_selected_memories_missing_field_is_error() {
        let headers = vec![MemoryHeader {
            filename: "role.md".to_string(),
            file_path: PathBuf::from("/tmp/role.md"),
            mtime_ms: 0,
            description: Some("role".to_string()),
            memory_type: None,
        }];
        let response = "{\"other\": []}";
        assert!(matches!(
            parse_selected_memories(response, &headers),
            Err(RelevanceError::ParseError(_))
        ));
    }

    #[test]
    fn parse_selected_memories_dedups_before_cap() {
        let headers = vec![
            MemoryHeader {
                filename: "role.md".to_string(),
                file_path: PathBuf::from("/tmp/role.md"),
                mtime_ms: 0,
                description: Some("role".to_string()),
                memory_type: None,
            },
            MemoryHeader {
                filename: "build.md".to_string(),
                file_path: PathBuf::from("/tmp/build.md"),
                mtime_ms: 0,
                description: Some("build".to_string()),
                memory_type: None,
            },
        ];
        let response = "{\"selected_memories\": [\"role.md\", \"role.md\", \"role.md\", \"role.md\", \"role.md\", \"build.md\"]}";
        let result = parse_selected_memories(response, &headers).unwrap();
        assert_eq!(result, vec!["role.md", "build.md"]);
    }

    #[test]
    fn extract_json_from_mixed_text() {
        let text = "Here are the selected memories:\n{\"selected_memories\": [\"a.md\"]}\nDone.";
        let json = extract_json(text);
        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));
    }

    #[test]
    fn extract_json_pure_json() {
        let text = "{\"selected_memories\": []}";
        let json = extract_json(text);
        assert_eq!(json, text);
    }

    #[test]
    fn extract_json_ignores_braces_inside_strings() {
        let text = "Here: {\"selected_memories\": [\"config{prod}.md\"]}\nDone.";
        assert_eq!(
            extract_json(text),
            "{\"selected_memories\": [\"config{prod}.md\"]}"
        );
    }

    #[test]
    fn extract_json_handles_escaped_quotes_in_strings() {
        let text = "prefix {\"k\": \"a\\\"b}\"} suffix";
        assert_eq!(extract_json(text), "{\"k\": \"a\\\"b}\"}");
    }
}
