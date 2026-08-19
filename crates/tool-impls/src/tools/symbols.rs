//! Index-backed navigation tools: `symbol_search` and `find_references`.
//!
//! Both tools read the per-workspace code index attached to
//! `ToolContext::runtime::index_service` (built by the host from `[index]`
//! config). They fail closed with a "not available" error when the index is
//! disabled, and they surface index freshness (`stale_files`) in their
//! output so the model knows how current the results are. Division of
//! labor: these tools answer "where is this defined / where is it used";
//! `grep_files` stays the tool for arbitrary content matching.

use std::time::Duration;

use async_trait::async_trait;
use codesmith_agent_runtime::tools::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_str, optional_u64,
    required_str,
};
use codesmith_index::{IndexStats, SymbolKind, SymbolQuery};
use serde_json::{Value, json};

/// Default and hard cap for symbol search results.
const DEFAULT_SYMBOL_LIMIT: u64 = 50;
const MAX_SYMBOL_LIMIT: u64 = 200;

/// Default and hard cap for reference listings.
const DEFAULT_REFERENCE_LIMIT: u64 = 100;
const MAX_REFERENCE_LIMIT: u64 = 500;

/// Hard cap on one index-tool call. The service refreshes lazily before
/// querying; a cold index on a huge tree must not hang the turn.
const INDEX_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Search symbol definitions by case-insensitive name substring.
pub struct SymbolSearchTool;

#[async_trait]
impl ToolSpec for SymbolSearchTool {
    fn name(&self) -> &'static str {
        "symbol_search"
    }

    fn description(&self) -> &'static str {
        "Search the persistent workspace symbol index (functions, methods, structs, enums, traits, classes, interfaces, type aliases, constants, macros, modules) by case-insensitive name substring. This is indexed, so it is much faster and more precise than grep_files for locating definitions in large repositories — prefer it for questions like 'where is X defined?'. Use grep_files for arbitrary content matching and find_references for usage sites of a known symbol name."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Case-insensitive substring of the symbol name (e.g. 'Registry', 'tool_registry')"
                },
                "kind": {
                    "type": "string",
                    "description": "Optional symbol kind filter (function, method, struct, enum, trait, class, interface, type_alias, constant, macro, module, field)"
                },
                "file_glob": {
                    "type": "string",
                    "description": "Optional glob filter on workspace-relative paths (e.g. 'crates/tui/**/*.rs')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results (default 50, max 200)"
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let query_str = required_str(&input, "query")?.trim().to_string();
        if query_str.is_empty() {
            return Err(ToolError::invalid_input("query must not be empty"));
        }
        let kind = match optional_str(&input, "kind") {
            Some(kind) => Some(SymbolKind::parse(kind).ok_or_else(|| {
                ToolError::invalid_input(format!(
                    "unknown symbol kind '{kind}'; valid kinds: {}",
                    SymbolKind::all()
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?),
            None => None,
        };
        let limit =
            optional_u64(&input, "limit", DEFAULT_SYMBOL_LIMIT).clamp(1, MAX_SYMBOL_LIMIT) as usize;

        let service = context
            .runtime
            .index_service
            .as_ref()
            .ok_or_else(|| ToolError::not_available(
                "code index is not available (disabled via [index] config or not built into this session)",
            ))?
            .clone();

        let query = SymbolQuery {
            query: query_str,
            kind,
            file_glob: optional_str(&input, "file_glob").map(str::to_string),
            limit,
        };
        let symbols = tokio::time::timeout(INDEX_TOOL_TIMEOUT, service.search_symbols(query))
            .await
            .map_err(|_| ToolError::Timeout { seconds: 30 })?
            .map_err(|err| ToolError::execution_failed(format!("index search failed: {err}")))?;

        let stats = service.stats();
        let truncated = symbols.len() >= limit;
        ToolResult::json(&json!({
            "symbols": symbols,
            "total": symbols.len(),
            "truncated": truncated,
            "index": index_status(&stats),
        }))
        .map_err(|err| ToolError::execution_failed(format!("serializing result: {err}")))
    }
}

/// List definitions and lexical usage sites of a symbol name.
pub struct FindReferencesTool;

#[async_trait]
impl ToolSpec for FindReferencesTool {
    fn name(&self) -> &'static str {
        "find_references"
    }

    fn description(&self) -> &'static str {
        "Find where a symbol is defined and used across the workspace, using the persistent code index. Given an exact symbol name (case-insensitive), returns the definition sites plus every lexical occurrence (name appearances in code — imports, call sites, type usages). Much faster than repeated grep_files on large repositories. Occurrences are name-based: rare same-name symbols in unrelated scopes may appear; verify by reading the listed locations."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Exact symbol name to look up (case-insensitive, e.g. 'ToolRegistry')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum occurrences returned (default 100, max 500)"
                }
            },
            "required": ["name"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let name = required_str(&input, "name")?.trim().to_string();
        if name.is_empty() {
            return Err(ToolError::invalid_input("name must not be empty"));
        }
        let limit = optional_u64(&input, "limit", DEFAULT_REFERENCE_LIMIT)
            .clamp(1, MAX_REFERENCE_LIMIT) as usize;

        let service = context
            .runtime
            .index_service
            .as_ref()
            .ok_or_else(|| ToolError::not_available(
                "code index is not available (disabled via [index] config or not built into this session)",
            ))?
            .clone();

        // Definitions and occurrences are two service calls; issue them
        // together so the (lazy) freshness pass runs once.
        let (definitions, occurrences) = tokio::time::timeout(INDEX_TOOL_TIMEOUT, async {
            let definitions = service.find_definition(&name).await;
            let occurrences = service.find_references(&name).await;
            (definitions, occurrences)
        })
        .await
        .map_err(|_| ToolError::Timeout { seconds: 30 })?;

        let definitions = definitions
            .map_err(|err| ToolError::execution_failed(format!("index lookup failed: {err}")))?;
        let mut occurrences = occurrences
            .map_err(|err| ToolError::execution_failed(format!("index lookup failed: {err}")))?;
        let truncated = occurrences.len() > limit;
        occurrences.truncate(limit);

        let stats = service.stats();
        ToolResult::json(&json!({
            "name": name,
            "definitions": definitions,
            "occurrences": occurrences,
            "total_occurrences": occurrences.len(),
            "truncated": truncated,
            "index": index_status(&stats),
        }))
        .map_err(|err| ToolError::execution_failed(format!("serializing result: {err}")))
    }
}

fn index_status(stats: &IndexStats) -> Value {
    json!({
        "backend": stats.backend,
        "files": stats.files,
        "symbols": stats.symbols,
        "stale_files": stats.stale_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_index::{
        FileEntry, FileQuery, IndexServiceApi, Location, Occurrence, OccurrenceRole, RefreshBudget,
        RefreshOutcome, Symbol,
    };
    use serde_json::json;
    use std::sync::Arc;

    struct StubIndex;

    #[async_trait]
    impl IndexServiceApi for StubIndex {
        async fn search_symbols(&self, query: SymbolQuery) -> anyhow::Result<Vec<Symbol>> {
            Ok(vec![Symbol {
                name: "ToolRegistry".into(),
                kind: SymbolKind::Struct,
                container: None,
                path: "src/registry.rs".into(),
                location: Location {
                    line: 10,
                    col: 1,
                    end_line: 40,
                    end_col: 2,
                },
                signature: Some("struct ToolRegistry".into()),
            }]
            .into_iter()
            .filter(|s| s.name.to_lowercase().contains(&query.query.to_lowercase()))
            .take(query.limit)
            .collect())
        }

        async fn find_definition(&self, name: &str) -> anyhow::Result<Vec<Symbol>> {
            if name.eq_ignore_ascii_case("ToolRegistry") {
                Ok(self.search_symbols(SymbolQuery::default()).await?)
            } else {
                Ok(Vec::new())
            }
        }

        async fn find_references(&self, name: &str) -> anyhow::Result<Vec<Occurrence>> {
            if name.eq_ignore_ascii_case("ToolRegistry") {
                Ok(vec![
                    Occurrence {
                        name: "ToolRegistry".into(),
                        role: OccurrenceRole::Definition,
                        path: "src/registry.rs".into(),
                        line: 10,
                    },
                    Occurrence {
                        name: "ToolRegistry".into(),
                        role: OccurrenceRole::Reference,
                        path: "src/main.rs".into(),
                        line: 3,
                    },
                ])
            } else {
                Ok(Vec::new())
            }
        }

        async fn list_files(&self, _query: FileQuery) -> anyhow::Result<Vec<FileEntry>> {
            Ok(Vec::new())
        }

        async fn refresh(&self, _budget: RefreshBudget) -> anyhow::Result<RefreshOutcome> {
            Ok(RefreshOutcome {
                stats: self.stats(),
                refreshed_files: 0,
                duration_ms: 0,
            })
        }

        fn stats(&self) -> IndexStats {
            IndexStats {
                files: 10,
                symbols: 100,
                stale_files: 0,
                last_refresh: None,
                backend: "stub".into(),
            }
        }
    }

    fn context_with_index() -> ToolContext {
        let mut context = ToolContext::new("/tmp/workspace");
        context.runtime.index_service = Some(Arc::new(StubIndex));
        context
    }

    #[tokio::test]
    async fn symbol_search_returns_serialized_symbols_with_index_status() {
        let tool = SymbolSearchTool;
        let context = context_with_index();
        let result = tool
            .execute(json!({"query": "registry", "limit": 10}), &context)
            .await
            .expect("execute");
        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).expect("json content");
        assert_eq!(parsed["symbols"][0]["name"], "ToolRegistry");
        assert_eq!(parsed["symbols"][0]["path"], "src/registry.rs");
        assert_eq!(parsed["symbols"][0]["location"]["line"], 10);
        assert_eq!(parsed["index"]["backend"], "stub");
        assert_eq!(parsed["index"]["stale_files"], 0);
    }

    #[tokio::test]
    async fn symbol_search_validates_kind_and_query() {
        let tool = SymbolSearchTool;
        let context = context_with_index();
        let err = tool
            .execute(json!({"query": "x", "kind": "planet"}), &context)
            .await
            .expect_err("unknown kind");
        assert!(err.to_string().contains("planet"), "{err}");

        let err = tool
            .execute(json!({"query": "   "}), &context)
            .await
            .expect_err("empty query");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[tokio::test]
    async fn find_references_returns_definitions_and_occurrences() {
        let tool = FindReferencesTool;
        let context = context_with_index();
        let result = tool
            .execute(json!({"name": "toolregistry"}), &context)
            .await
            .expect("execute");
        let parsed: Value = serde_json::from_str(&result.content).expect("json content");
        assert_eq!(parsed["definitions"].as_array().map(Vec::len), Some(1));
        assert_eq!(parsed["total_occurrences"], 2);
        assert_eq!(parsed["occurrences"][1]["path"], "src/main.rs");
    }

    #[tokio::test]
    async fn tools_fail_closed_without_index_service() {
        let context = ToolContext::new("/tmp/workspace");
        let err = SymbolSearchTool
            .execute(json!({"query": "x"}), &context)
            .await
            .expect_err("no service");
        assert!(err.to_string().contains("not available"), "{err}");
        let err = FindReferencesTool
            .execute(json!({"name": "x"}), &context)
            .await
            .expect_err("no service");
        assert!(err.to_string().contains("not available"), "{err}");
    }
}
