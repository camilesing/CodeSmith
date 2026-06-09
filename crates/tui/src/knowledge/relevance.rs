//! Side-query relevance ranking for memory selection.
//!
//! Uses a DeepSeek API call to rank memory files by relevance to the
//! user's current query. Returns up to 5 most relevant filenames,
//! mirroring the TypeScript `findRelevantMemories` / `selectRelevantMemories`
//! pattern.

use std::path::PathBuf;

use std::pin::Pin;
use std::future::Future;

use tokio_util::sync::CancellationToken;

use super::budget::MAX_MEMORIES_PER_TURN;
use super::scan::MemoryHeader;

/// Error type for relevance ranking operations.
#[derive(Debug)]
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

/// System prompt for the side-query model that selects relevant memories.
const SELECT_MEMORIES_SYSTEM_PROMPT: &str = "\
You are selecting memories that will be useful to an AI coding assistant \
as it processes a user's query. You will be given the user's query and a \
list of available memory files with their filenames and descriptions.

Return a JSON object with a \"selected_memories\" array of filenames \
for the memories that will clearly be useful (up to 5). Only include \
memories that you are certain will be helpful based on their name and \
description. If unsure, do not include it. If none are clearly useful, \
return an empty list.

Example response:
{\"selected_memories\": [\"user_role.md\", \"project_build.md\"]}";

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
/// client implementation.
pub async fn select_relevant_memories(
    user_query: &str,
    headers: &[MemoryHeader],
    recent_tools: &[String],
    // The caller provides a function that makes the actual API call.
    // This decouples relevance.rs from the DeepSeek client type.
    side_query_fn: impl FnOnce(String, String) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>,
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

    let user_message = format!(
        "Query: {user_query}\n\nAvailable memories:\n{manifest}{tools_section}"
    );

    let result = tokio::select! {
        _ = cancel_token.cancelled() => return Err(RelevanceError::Cancelled),
        response = side_query_fn(SELECT_MEMORIES_SYSTEM_PROMPT.to_string(), user_message) => response,
    };

    let response_text = result.map_err(RelevanceError::ApiError)?;

    // Parse the JSON response.
    parse_selected_memories(&response_text, headers)
}

/// Parse the side-query response and validate filenames against the manifest.
fn parse_selected_memories(response: &str, headers: &[MemoryHeader]) -> Result<Vec<String>, RelevanceError> {
    // The model might return the JSON inline or with surrounding text.
    // Try to extract JSON from the response.
    let json_str = extract_json(response);

    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| RelevanceError::ParseError(format!("failed to parse JSON: {e}")))?;

    let selected = parsed
        .get("selected_memories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Validate: only return filenames that exist in the manifest.
    let valid_filenames: Vec<String> = headers.iter().map(|h| h.filename.clone()).collect();
    let filtered: Vec<String> = selected
        .into_iter()
        .filter(|name| valid_filenames.contains(name))
        .take(MAX_MEMORIES_PER_TURN)
        .collect();

    Ok(filtered)
}

/// Extract JSON object from potentially mixed text response.
fn extract_json(response: &str) -> &str {
    // Try to find a JSON object delimited by { and }.
    if let Some(start) = response.find('{') {
        // Find the matching closing brace by counting depth.
        let mut depth = 0;
        for (i, c) in response[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
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
}