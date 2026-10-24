//! `lsp_navigate` — type-aware read-only navigation over the session's LSP
//! server (round-4 TODO-45).
//!
//! Three operations — `definition`, `references`, `hover` — issued against a
//! 1-based line/column position in a file (converted to the LSP wire's
//! 0-based coordinates by this tool). Complements the index-backed
//! `symbol_search` / `find_references` (name-based, tree-sitter): those
//! answer "where is a symbol with this name"; this tool resolves the symbol
//! AT a position through the language server, so it is type-aware (a trait
//! method's impl site, a call site's definition, a re-export's origin) and
//! carries signature/docs on hover.
//!
//! Fails closed with a "not available" error when the LSP manager is absent
//! from the context, LSP is disabled, no server exists for the file's
//! language, or the request fails/times out (same family style as the index
//! tools). A server answer of `null` / empty is a successful empty result,
//! not an error. Locations carry 1-based coordinates, are grouped by file
//! (sorted so same-file entries are adjacent), and are bounded by a dual
//! cap: `limit` locations (default 50, max 200) and a hover-content ceiling
//! with a truncation marker.
//!
//! ## Known limitations
//!
//! * Coordinates are 1-based in input/output; the LSP wire is 0-based UTF-16
//!   code units. On lines containing astral-plane characters (emoji), a
//!   column counted in display characters may name a position the server
//!   resolves one code unit off.
//! * One server per language, reused for the session's lifetime (the
//!   manager's transport map). Every call re-syncs the document
//!   (didOpen/didChange with the full text) — correctness over bandwidth.
//! * `goToImplementation` and server-capability probing are deliberately
//!   not implemented (narrow benefit; revisit alongside the index tools).
//! * Sub-agent availability depends on the child's `ToolContext` carrying
//!   the LSP manager; otherwise the tool fails closed there too.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use codesmith_agent_runtime::host_services::LspNavigationMethod;
use codesmith_agent_runtime::tools::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64, required_str,
    required_u64,
};
use serde_json::{Value, json};

/// Default and hard cap on returned locations (definition / references).
const DEFAULT_LOCATION_LIMIT: u64 = 50;
const MAX_LOCATION_LIMIT: u64 = 200;

/// Ceiling on hover content kept in the result (a truncation marker is
/// appended beyond it) — the bounded-model-visible-payload family style of
/// the symbol/search tools' caps.
const MAX_HOVER_CHARS: usize = 8 * 1024;

/// Type-aware navigation through the session's LSP server.
pub struct LspNavigateTool;

#[async_trait]
impl ToolSpec for LspNavigateTool {
    fn name(&self) -> &'static str {
        "lsp_navigate"
    }

    fn description(&self) -> &'static str {
        "Type-aware code navigation through the session's LSP server. Given a file and a 1-based position, operation=definition resolves where the symbol at that position is defined, operation=references lists its usage sites, and operation=hover returns its signature and documentation. More precise than symbol_search for resolving the exact symbol at a call site (trait methods, re-exports, overloads). Requires LSP enabled with a server for the file's language."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to navigate in (absolute or workspace-relative)."
                },
                "line": {
                    "type": "integer",
                    "description": "1-based line of the position."
                },
                "column": {
                    "type": "integer",
                    "description": "1-based column of the position."
                },
                "operation": {
                    "type": "string",
                    "enum": ["definition", "references", "hover"],
                    "description": "Navigation operation."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum locations returned for definition/references (default 50, max 200)."
                }
            },
            "required": ["path", "line", "column", "operation"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let path_str = required_str(&input, "path")?.trim().to_string();
        if path_str.is_empty() {
            return Err(ToolError::invalid_input("path must not be empty"));
        }
        let line = required_u64(&input, "line")?;
        let column = required_u64(&input, "column")?;
        if line == 0 || column == 0 {
            return Err(ToolError::invalid_input(
                "line and column are 1-based and must be >= 1",
            ));
        }
        let operation = required_str(&input, "operation")?
            .trim()
            .to_ascii_lowercase();
        // The trait-level enum makes the read-only contract structural: a
        // mutating LSP method is unrepresentable past this mapping.
        let method = match operation.as_str() {
            "definition" => LspNavigationMethod::Definition,
            "references" => LspNavigationMethod::References,
            "hover" => LspNavigationMethod::Hover,
            other => {
                return Err(ToolError::invalid_input(format!(
                    "unknown operation '{other}' (expected definition, references, or hover)"
                )));
            }
        };
        let limit = optional_u64(&input, "limit", DEFAULT_LOCATION_LIMIT)?
            .clamp(1, MAX_LOCATION_LIMIT) as usize;

        let manager = context
            .lsp_manager
            .as_ref()
            .ok_or_else(|| {
                ToolError::not_available(
                    "lsp_navigate is not available (LSP disabled via [lsp] config or not built into this session)",
                )
            })?
            .clone();

        let file = context.resolve_path(&path_str)?;

        // 1-based model input → 0-based LSP wire position. The document
        // URI is filled in by the manager (URI encoding is
        // transport-side knowledge).
        let mut params = json!({
            "textDocument": {},
            "position": { "line": line - 1, "character": column - 1 }
        });
        if operation == "references" {
            // Include the declaration site in the listing — the useful
            // default for "where does this symbol live"; fixed (not model
            // input) like the other provider-side knobs.
            params["context"] = json!({ "includeDeclaration": true });
        }

        let result = manager
            .navigation_request(&file, method, params)
            .await
            .ok_or_else(|| {
                ToolError::not_available(
                    "lsp_navigate could not run: no LSP server is available for this file \
                     (LSP disabled, language unsupported, file unreadable, or the request \
                     failed)",
                )
            })?;

        let payload = if operation == "hover" {
            render_hover(&result, &file, line, column)
        } else {
            render_locations(&result, &file, line, column, limit, &operation)
        };
        ToolResult::json(&payload)
            .map_err(|err| ToolError::execution_failed(format!("serializing result: {err}")))
    }
}

/// Normalize an LSP location payload (`Location` / `Location[]` /
/// `LocationLink[]` / `null`) into 1-based `(path, start, end)` tuples.
fn parse_locations(result: &Value) -> Vec<(PathBuf, u32, u32, u32, u32)> {
    let entries: Vec<&Value> = match result {
        Value::Null => return Vec::new(),
        Value::Array(items) => items.iter().collect(),
        single => vec![single],
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        // `Location`: {uri, range}; `LocationLink`: {targetUri,
        // targetRange, targetSelectionRange} — prefer the selection range
        // (where the symbol's name sits) and fall back to the whole target.
        let uri = entry
            .get("uri")
            .and_then(Value::as_str)
            .or_else(|| entry.get("targetUri").and_then(Value::as_str));
        let range = entry.get("range").or_else(|| {
            entry
                .get("targetSelectionRange")
                .or_else(|| entry.get("targetRange"))
        });
        let (Some(uri), Some(range)) = (uri, range) else {
            continue;
        };
        let Some(path) = path_from_uri(uri) else {
            continue;
        };
        let position = |key: &str| {
            range
                .get(key)
                .and_then(|p| {
                    Some((
                        p.get("line").and_then(Value::as_u64)?,
                        p.get("character").and_then(Value::as_u64)?,
                    ))
                })
                .map(|(l, c)| (l as u32, c as u32))
        };
        let (Some((sl, sc)), Some((el, ec))) = (position("start"), position("end")) else {
            continue;
        };
        out.push((path, sl + 1, sc + 1, el + 1, ec + 1));
    }
    out
}

/// Render a definition/references payload: locations sorted by (path,
/// line) — grouped by file — capped at `limit` with a truncation flag.
fn render_locations(
    result: &Value,
    file: &Path,
    line: u64,
    column: u64,
    limit: usize,
    operation: &str,
) -> Value {
    let mut locations = parse_locations(result);
    locations.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
    let truncated = locations.len() > limit;
    locations.truncate(limit);
    json!({
        "operation": operation,
        "file": file.display().to_string(),
        "position": { "line": line, "column": column },
        "results": locations
            .iter()
            .map(|(path, sl, sc, el, ec)| {
                json!({
                    "path": path.display().to_string(),
                    "line": sl,
                    "column": sc,
                    "end_line": el,
                    "end_column": ec,
                })
            })
            .collect::<Vec<_>>(),
        "total": locations.len(),
        "truncated": truncated,
        "note": if truncated {
            format!("results capped at {limit} locations; re-run with a higher limit (max {MAX_LOCATION_LIMIT}) or narrow the query")
        } else {
            String::new()
        },
    })
}

/// Render a hover payload. `contents` per the LSP spec is `MarkupContent`
/// ({kind, value}), a plain `MarkedString` (string), a fenced
/// `MarkedString` ({language, value}), or an array mixing both.
fn render_hover(result: &Value, file: &Path, line: u64, column: u64) -> Value {
    let content = result.get("contents").and_then(hover_text);
    match content {
        None => json!({
            "operation": "hover",
            "file": file.display().to_string(),
            "position": { "line": line, "column": column },
            "content": Value::Null,
            "note": "no hover information at this position",
        }),
        Some(text) => {
            let truncated = text.chars().count() > MAX_HOVER_CHARS;
            let text = if truncated {
                let mut cut: String = text.chars().take(MAX_HOVER_CHARS).collect();
                cut.push_str("\n[truncated to ");
                cut.push_str(&MAX_HOVER_CHARS.to_string());
                cut.push_str(" chars]");
                cut
            } else {
                text
            };
            json!({
                "operation": "hover",
                "file": file.display().to_string(),
                "position": { "line": line, "column": column },
                "content": text,
                "truncated": truncated,
            })
        }
    }
}

/// Extract hover text from one `contents` shape (see [`render_hover`]).
fn hover_text(contents: &Value) -> Option<String> {
    match contents {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(marked_string_text).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        }
        other => marked_string_text(other),
    }
}

/// `MarkupContent` / `MarkedString` object → text (fenced when it names a
/// language).
fn marked_string_text(value: &Value) -> Option<String> {
    let text = value.get("value").and_then(Value::as_str)?;
    match value.get("language").and_then(Value::as_str) {
        Some(language) => Some(format!("```{language}\n{text}\n```")),
        None => Some(text.to_string()),
    }
}

/// Decode a `file://` URI into a filesystem path (percent-decoding the
/// path component). Returns `None` for other schemes. Local mirror of the
/// transport-side encoder — the tool crate cannot reach the CLI's private
/// helper, and dragging it into a shared crate for two callers would be
/// the wrong direction.
fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode_path(stripped)))
}

/// Best-effort `%XX` decoding; invalid escapes stay literal.
fn percent_decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = if bytes[i] == b'%' && i + 2 < bytes.len() {
            match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(hi), Some(lo)) => Some(hi * 16 + lo),
                _ => None,
            }
        } else {
            None
        };
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::host_services::{LspManagerApi, LspNavigationMethod};
    use codesmith_agent_runtime::lsp_config::LspConfig;
    use codesmith_agent_runtime::lsp_diagnostics::DiagnosticBlock;
    use std::sync::{Arc, Mutex};

    /// Scripted `LspManagerApi` double: records `(file, method, params)`
    /// and replays one fixed response (`None` = unavailable).
    struct FakeNavManager {
        response: Option<Value>,
        requests: Mutex<Vec<(PathBuf, String, Value)>>,
    }

    impl FakeNavManager {
        fn responding(response: Value) -> Arc<Self> {
            Arc::new(Self {
                response: Some(response),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn unavailable() -> Arc<Self> {
            Arc::new(Self {
                response: None,
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<(PathBuf, String, Value)> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LspManagerApi for FakeNavManager {
        fn config(&self) -> &LspConfig {
            unreachable!("lsp_navigate never reads config through the trait")
        }

        async fn diagnostics_for(&self, _file: &Path, _edit_seq: u64) -> Option<DiagnosticBlock> {
            None
        }

        async fn navigation_request(
            &self,
            file: &Path,
            method: LspNavigationMethod,
            params: Value,
        ) -> Option<Value> {
            self.requests.lock().unwrap().push((
                file.to_path_buf(),
                method.wire_name().to_string(),
                params,
            ));
            self.response.clone()
        }
    }

    fn run_tool(
        input: Value,
        manager: Option<Arc<FakeNavManager>>,
    ) -> Result<ToolResult, ToolError> {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        if let Some(manager) = manager {
            ctx = ctx.with_lsp_manager(manager);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(LspNavigateTool.execute(input, &ctx))
    }

    fn payload(result: &ToolResult) -> Value {
        serde_json::from_str(&result.content).unwrap()
    }

    fn location(uri: &str, line: u64, character: u64) -> Value {
        json!({
            "uri": uri,
            "range": {
                "start": { "line": line, "character": character },
                "end": { "line": line, "character": character + 5 }
            }
        })
    }

    #[test]
    fn definition_parses_locations_one_based_and_grouped() {
        let response = json!([
            location("file:///ws/src/b.rs", 29, 9),
            location("file:///ws/src/a.rs", 3, 4),
            // LocationLink shape (targetUri/targetSelectionRange).
            json!({
                "targetUri": "file:///ws/src/a.rs",
                "targetSelectionRange": {
                    "start": { "line": 10, "character": 2 },
                    "end": { "line": 10, "character": 6 }
                },
                "targetRange": {
                    "start": { "line": 9, "character": 0 },
                    "end": { "line": 11, "character": 0 }
                }
            })
        ]);
        let manager = FakeNavManager::responding(response);
        let result = run_tool(
            json!({"path": "src/a.rs", "line": 12, "column": 8, "operation": "definition"}),
            Some(manager.clone()),
        )
        .unwrap();

        let body = payload(&result);
        assert_eq!(body["operation"], "definition");
        assert_eq!(body["total"], 3);
        let results = body["results"].as_array().unwrap();
        // Sorted by (path, line) — grouped by file.
        assert_eq!(results[0]["path"], json!("/ws/src/a.rs"));
        assert_eq!(results[0]["line"], 4, "0-based wire → 1-based output");
        assert_eq!(results[0]["column"], 5);
        assert_eq!(
            results[1]["line"], 11,
            "LocationLink selection range preferred"
        );
        assert_eq!(results[2]["path"], json!("/ws/src/b.rs"));

        // Wire params: 0-based position, definition method.
        let (file, method, params) = manager.requests()[0].clone();
        assert!(file.ends_with("src/a.rs"));
        assert_eq!(method, "textDocument/definition");
        assert_eq!(params["position"]["line"], 11);
        assert_eq!(params["position"]["character"], 7);
    }

    #[test]
    fn references_request_carries_include_declaration() {
        let manager = FakeNavManager::responding(json!([]));
        let result = run_tool(
            json!({"path": "src/a.rs", "line": 1, "column": 1, "operation": "references"}),
            Some(manager.clone()),
        )
        .unwrap();

        let (_, method, params) = manager.requests()[0].clone();
        assert_eq!(method, "textDocument/references");
        assert_eq!(params["context"]["includeDeclaration"], true);

        let body = payload(&result);
        assert_eq!(body["total"], 0);
        assert_eq!(body["truncated"], false);
        assert!(body["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hover_renders_markup_content() {
        let response = json!({
            "contents": { "kind": "markdown", "value": "```rust\npub fn foo(x: u32) -> u32\n```\n\nDoubles." },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } }
        });
        let manager = FakeNavManager::responding(response);
        let result = run_tool(
            json!({"path": "src/a.rs", "line": 1, "column": 5, "operation": "hover"}),
            Some(manager),
        )
        .unwrap();

        let body = payload(&result);
        let content = body["content"].as_str().unwrap();
        assert!(
            content.contains("pub fn foo(x: u32) -> u32"),
            "got: {content}"
        );
        assert!(content.contains("Doubles."));
        assert_eq!(body["truncated"], false);
    }

    #[test]
    fn hover_null_is_a_success_with_note() {
        let manager = FakeNavManager::responding(Value::Null);
        let result = run_tool(
            json!({"path": "src/a.rs", "line": 1, "column": 1, "operation": "hover"}),
            Some(manager),
        )
        .unwrap();
        let body = payload(&result);
        assert!(body["content"].is_null());
        assert_eq!(body["note"], "no hover information at this position");
    }

    #[test]
    fn fails_closed_when_manager_missing() {
        let err = run_tool(
            json!({"path": "src/a.rs", "line": 1, "column": 1, "operation": "definition"}),
            None,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not available"), "got: {msg}");
    }

    #[test]
    fn manager_none_maps_to_not_available() {
        let err = run_tool(
            json!({"path": "src/a.rs", "line": 1, "column": 1, "operation": "definition"}),
            Some(FakeNavManager::unavailable()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no LSP server"), "got: {err}");
    }

    #[test]
    fn rejects_zero_line_or_column() {
        let err = run_tool(
            json!({"path": "src/a.rs", "line": 0, "column": 1, "operation": "hover"}),
            Some(FakeNavManager::unavailable()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("1-based"), "got: {err}");
    }

    #[test]
    fn caps_locations_with_truncation_note() {
        let response = json!([
            location("file:///ws/a.rs", 1, 0),
            location("file:///ws/a.rs", 2, 0),
            location("file:///ws/a.rs", 3, 0),
        ]);
        let manager = FakeNavManager::responding(response);
        let result = run_tool(
            json!({
                "path": "src/a.rs", "line": 1, "column": 1,
                "operation": "references", "limit": 2
            }),
            Some(manager),
        )
        .unwrap();
        let body = payload(&result);
        assert_eq!(body["total"], 2);
        assert_eq!(body["truncated"], true);
        assert!(body["note"].as_str().unwrap().contains("capped at 2"));
    }

    #[test]
    fn hover_truncates_over_cap() {
        let big = "x".repeat(MAX_HOVER_CHARS + 100);
        let manager = FakeNavManager::responding(
            json!({ "contents": { "kind": "plaintext", "value": big } }),
        );
        let result = run_tool(
            json!({"path": "src/a.rs", "line": 1, "column": 1, "operation": "hover"}),
            Some(manager),
        )
        .unwrap();
        let body = payload(&result);
        assert_eq!(body["truncated"], true);
        assert!(body["content"].as_str().unwrap().contains("[truncated to"));
    }

    #[test]
    fn uri_decoding_unescapes_percent_sequences() {
        assert_eq!(
            path_from_uri("file:///tmp/my%20file.rs").as_deref(),
            Some(Path::new("/tmp/my file.rs"))
        );
        assert!(path_from_uri("https://example.com/x").is_none());
    }
}
