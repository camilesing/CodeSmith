//! Web search tool — the model-facing half of the unified web service's
//! `search` operation (TODO-49). Provider dispatch, transport, timeout,
//! cancellation, and error classification live in
//! `crates/tool-impls/src/web_service.rs` (`HttpWebService`); this tool owns
//! input parsing (compatibility aliases included) and result formatting.
//!
//! This is the primary web search surface for agents. For browsing workflows
//! (page open, click, screenshot) use a direct URL approach instead.
//!
//! Set `[search]` in config.toml to switch providers:
//!   provider = "duckduckgo"  # or tavily/bocha/metaso/baidu/volcengine
//!   api_key = "tvly-..."

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64,
};
use async_trait::async_trait;
use codesmith_agent_runtime::tools::tool_catalog::WEB_SEARCH_NAME;
use codesmith_agent_runtime::web_service::{SearchHit, SearchRequest};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS: usize = 10;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
/// Hosts the default (DuckDuckGo) provider contacts — the approval-gate
/// stand-in when no service is attached (embeds; the execute path then fails
/// closed anyway).
const DEFAULT_SEARCH_HOST: &str = "html.duckduckgo.com";

#[derive(Debug, Clone, Serialize)]
struct WebSearchEntry {
    title: String,
    url: String,
    snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WebSearchResponse {
    query: String,
    source: String,
    count: usize,
    message: String,
    results: Vec<WebSearchEntry>,
}

pub struct WebSearchTool;

#[async_trait]
impl ToolSpec for WebSearchTool {
    fn name(&self) -> &'static str {
        WEB_SEARCH_NAME
    }

    fn description(&self) -> &'static str {
        "Search the web and return ranked results with URLs and snippets. Default backend is DuckDuckGo with Bing fallback; set `[search] provider = \"bing\" | \"tavily\" | \"bocha\" | \"metaso\" | \"baidu\"` in config.toml to switch backends. Use this instead of scraping search engines with `curl` in `exec_shell`. For a known canonical URL, prefer `fetch_url` directly."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query. Compatibility aliases: q, or search_query[0].q."
                },
                "q": {
                    "type": "string",
                    "description": "Search query."
                },
                "search_query": {
                    "type": "array",
                    "description": "Array form for advanced queries: [{\"q\":\"...\", \"max_results\": 5}]",
                    "items": {
                        "type": "object",
                        "properties": {
                            "q": { "type": "string" },
                            "query": { "type": "string" },
                            "max_results": { "type": "integer" }
                        }
                    }
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5, max: 10)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default: 15000, max: 60000)"
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Network]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    /// P1-5: gate the search-backend hosts for this session's provider. The
    /// attached web service reports the hosts the configured provider will
    /// contact (including the Bing→DuckDuckGo fallback pair); a `Prompt`
    /// policy decision on any of them raises the standard inline approval
    /// prompt. `Allow` / `Deny` / no decider stay `Auto` (a `Deny` backend
    /// hard-fails at execute time inside the service).
    fn approval_requirement_for_input(
        &self,
        _input: &Value,
        context: &ToolContext,
    ) -> ApprovalRequirement {
        let hosts: Vec<String> = match context.web_service.as_ref() {
            Some(service) => service.search_hosts(),
            None => vec![DEFAULT_SEARCH_HOST.to_string()],
        };
        for host in &hosts {
            if codesmith_agent_runtime::network_policy::network_approval_requirement(
                context.network_policy.as_ref(),
                host,
                "web_search",
            ) == ApprovalRequirement::Required
            {
                return ApprovalRequirement::Required;
            }
        }
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = extract_search_query(&input)?;
        if query.is_empty() {
            return Err(ToolError::invalid_input("Query cannot be empty"));
        }
        let max_results =
            usize::try_from(optional_search_max_results(&input)).unwrap_or(DEFAULT_MAX_RESULTS);
        let max_results = max_results.clamp(1, MAX_RESULTS);
        let timeout_ms = optional_u64(&input, "timeout_ms", DEFAULT_TIMEOUT_MS)?.min(60_000);

        let service = context.web_service.as_ref().ok_or_else(|| {
            ToolError::not_available(
                "web_search: no web service attached to this context; web tools are unavailable",
            )
        })?;

        let outcome = service
            .search(
                SearchRequest::new(query.clone())
                    .with_max_results(max_results)
                    .with_timeout(Duration::from_millis(timeout_ms))
                    .with_policy(context.network_policy.clone())
                    .with_cancel(context.cancel_token.clone())
                    .for_tool(WEB_SEARCH_NAME),
            )
            .await
            .map_err(ToolError::from)?;

        let results: Vec<WebSearchEntry> = outcome
            .results
            .into_iter()
            .map(
                |SearchHit {
                     title,
                     url,
                     snippet,
                 }| WebSearchEntry {
                    title,
                    url,
                    snippet,
                },
            )
            .collect();
        let message_suffix = (!outcome.warnings.is_empty()).then(|| outcome.warnings.join("; "));

        search_tool_result(query, outcome.source, results, message_suffix.as_deref())
    }
}

fn search_tool_result(
    query: String,
    source: String,
    results: Vec<WebSearchEntry>,
    message_suffix: Option<&str>,
) -> Result<ToolResult, ToolError> {
    let message = if results.is_empty() {
        "No results found".to_string()
    } else if let Some(suffix) = message_suffix {
        format!("Found {} result(s). {suffix}", results.len())
    } else {
        format!("Found {} result(s)", results.len())
    };

    let response = WebSearchResponse {
        query,
        source,
        count: results.len(),
        message,
        results,
    };

    ToolResult::json(&response).map_err(|e| ToolError::execution_failed(e.to_string()))
}

fn extract_search_query(input: &Value) -> Result<String, ToolError> {
    for key in ["query", "q"] {
        if let Some(value) = input.get(key) {
            let Some(query) = value.as_str() else {
                return Err(ToolError::invalid_input(format!(
                    "Field '{key}' must be a string"
                )));
            };
            let query = query.trim();
            if !query.is_empty() {
                return Ok(query.to_string());
            }
        }
    }

    for item in search_query_items(input) {
        for key in ["q", "query"] {
            if let Some(value) = item.get(key) {
                let Some(query) = value.as_str() else {
                    return Err(ToolError::invalid_input(format!(
                        "Field 'search_query[].{key}' must be a string"
                    )));
                };
                let query = query.trim();
                if !query.is_empty() {
                    return Ok(query.to_string());
                }
            }
        }
    }

    Err(ToolError::missing_field("query"))
}

fn optional_search_max_results(input: &Value) -> u64 {
    if let Some(value) = input.get("max_results").and_then(Value::as_u64) {
        return value;
    }
    search_query_items(input)
        .filter_map(|item| item.get("max_results").and_then(Value::as_u64))
        .next()
        .unwrap_or(DEFAULT_MAX_RESULTS as u64)
}

fn search_query_items(input: &Value) -> impl Iterator<Item = &Value> {
    input
        .get("search_query")
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|items| items.iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::web_service::mock::MockWebService;
    use codesmith_agent_runtime::web_service::{SearchOutcome, WebError, WebErrorKind};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx() -> crate::tools::spec::ToolContext {
        crate::tools::spec::ToolContext::new(PathBuf::from("."))
    }

    fn service_ctx(service: MockWebService) -> crate::tools::spec::ToolContext {
        ctx().with_web_service(Arc::new(service))
    }

    #[test]
    fn extract_search_query_accepts_legacy_query() {
        let query =
            extract_search_query(&json!({"query": " deepseek v4 "})).expect("query should parse");
        assert_eq!(query, "deepseek v4");
    }

    #[test]
    fn extract_search_query_accepts_q_alias() {
        let query =
            extract_search_query(&json!({"q": "deepseek v4 pro"})).expect("q alias should parse");
        assert_eq!(query, "deepseek v4 pro");
    }

    #[test]
    fn extract_search_query_accepts_array_form() {
        let input = json!({"search_query": [{"q": "deepseek api", "max_results": 3}]});
        let query = extract_search_query(&input).expect("array form should parse");
        assert_eq!(query, "deepseek api");
        assert_eq!(optional_search_max_results(&input), 3);
    }

    #[test]
    fn extract_search_query_rejects_missing_query() {
        let err = extract_search_query(&json!({"max_results": 2}))
            .expect_err("missing query should fail");
        assert!(format!("{err}").contains("missing required field 'query'"));
    }

    #[test]
    fn optional_max_results_prefers_top_level_value() {
        // Top-level `max_results` wins over the array-form sibling
        // because callers using the array form usually copy-paste it
        // wholesale and then tweak the outer max_results afterwards.
        assert_eq!(
            optional_search_max_results(
                &json!({"query": "x", "max_results": 8, "search_query": [{"q": "y", "max_results": 2}]})
            ),
            8,
        );
    }

    #[test]
    fn optional_max_results_falls_back_to_array_form() {
        // When only the array form sets max_results, that value is the
        // one that should reach the caller. This is the path V4 uses
        // when it emits the structured `search_query: [{…}]` shape.
        assert_eq!(
            optional_search_max_results(&json!({"search_query": [{"q": "y", "max_results": 3}]})),
            3,
        );
    }

    #[test]
    fn optional_max_results_uses_default_when_neither_set() {
        // No explicit bound anywhere → the DEFAULT (currently 5)
        // applies, so the model can't accidentally pull MAX_RESULTS
        // worth of bandwidth just by omitting the field.
        assert_eq!(optional_search_max_results(&json!({"query": "x"})), 5);
        assert_eq!(
            optional_search_max_results(&json!({"search_query": [{"q": "y"}]})),
            5,
        );
    }

    #[test]
    fn optional_max_results_only_reads_first_array_entry() {
        // Sub-search support is a future feature; for now the array
        // entries beyond the first are ignored. Pin so a future
        // multi-query implementation has to update this test
        // intentionally rather than silently start fanning out.
        assert_eq!(
            optional_search_max_results(
                &json!({"search_query": [{"q": "first", "max_results": 1}, {"q": "second", "max_results": 9}]})
            ),
            1,
        );
    }

    #[test]
    fn extract_search_query_trims_whitespace_from_array_form_q_alias() {
        // The "trimmed" contract is part of the helper's invariant —
        // a model sometimes pads `q` with newlines from a heredoc.
        let q = extract_search_query(&json!({"search_query": [{"q": "  deepseek tui  "}]}))
            .expect("array form should parse with trim");
        assert_eq!(q, "deepseek tui");
    }

    #[test]
    fn extract_search_query_rejects_empty_query() {
        // A "" query lands in extract_search_query → propagates as
        // missing_field rather than a confusing engine error a few
        // layers down. Lock the failure mode.
        for body in [json!({"query": ""}), json!({"q": "   "}), json!({})] {
            let err = extract_search_query(&body).expect_err("empty query must reject");
            let msg = format!("{err}");
            assert!(
                msg.contains("missing required field 'query'") || msg.contains("Query"),
                "expected query-missing error, got `{msg}`"
            );
        }
    }

    // === unified web service paths (TODO-49) ==============================

    #[tokio::test]
    async fn search_through_the_service_formats_the_outcome() {
        let service = MockWebService::ok().with_search(Ok(SearchOutcome {
            results: vec![
                SearchHit {
                    title: "Rust".to_string(),
                    url: "https://www.rust-lang.org/".to_string(),
                    snippet: Some("a language".to_string()),
                },
                SearchHit {
                    title: "Cargo".to_string(),
                    url: "https://doc.rust-lang.org/cargo/".to_string(),
                    snippet: None,
                },
            ],
            source: "mock".to_string(),
            warnings: Vec::new(),
        }));
        let result = WebSearchTool
            .execute(json!({"query": "rust"}), &service_ctx(service))
            .await
            .expect("search through service");
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["query"], "rust");
        assert_eq!(parsed["source"], "mock");
        assert_eq!(parsed["count"], 2);
        assert_eq!(parsed["message"], "Found 2 result(s)");
        assert_eq!(parsed["results"][0]["url"], "https://www.rust-lang.org/");
    }

    #[tokio::test]
    async fn service_warnings_become_the_message_suffix() {
        let service = MockWebService::ok().with_search(Ok(SearchOutcome {
            results: vec![SearchHit {
                title: "t".to_string(),
                url: "https://example.com/".to_string(),
                snippet: None,
            }],
            source: "bing".to_string(),
            warnings: vec![
                "Bing returned no results".to_string(),
                "used DuckDuckGo fallback".to_string(),
            ],
        }));
        let result = WebSearchTool
            .execute(json!({"query": "x"}), &service_ctx(service))
            .await
            .expect("search with warnings");
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(
            parsed["message"],
            "Found 1 result(s). Bing returned no results; used DuckDuckGo fallback"
        );
    }

    #[tokio::test]
    async fn empty_outcome_reports_no_results() {
        let service = MockWebService::ok().with_search(Ok(SearchOutcome {
            results: Vec::new(),
            source: "mock".to_string(),
            warnings: Vec::new(),
        }));
        let result = WebSearchTool
            .execute(json!({"query": "void"}), &service_ctx(service))
            .await
            .expect("empty search still formats");
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["message"], "No results found");
        assert_eq!(parsed["count"], 0);
    }

    #[tokio::test]
    async fn service_error_surfaces_as_tool_error() {
        let service = MockWebService::ok().with_search(Err(WebError::new(
            WebErrorKind::Config,
            "Tavily search requires an API key. Set `[search] api_key = \"tvly-...\"` in config.toml.",
        )));
        let err = WebSearchTool
            .execute(json!({"query": "anything"}), &service_ctx(service))
            .await
            .expect_err("config error must surface");
        assert!(
            format!("{err}").contains("Tavily") && format!("{err}").contains("API key"),
            "error must name the provider and missing key; got `{err}`"
        );
    }

    #[tokio::test]
    async fn missing_service_fails_loud_not_silently() {
        // No service attached (embeds / bare test contexts): the tool must
        // fail closed with a clear error, never fall back to a built-in
        // transport.
        let err = WebSearchTool
            .execute(json!({"query": "anything"}), &ctx())
            .await
            .expect_err("no service attached must fail");
        assert!(format!("{err}").contains("no web service attached"));
    }

    // === P1-5: per-input approval gate ===================================

    use crate::tools::spec::{ApprovalRequirement, ToolSpec};
    use codesmith_agent_runtime::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

    fn prompt_decider(allow: &[&str]) -> NetworkPolicyDecider {
        NetworkPolicyDecider::new(
            NetworkPolicy {
                default: Decision::Prompt.into(),
                allow: allow.iter().map(|s| (*s).to_string()).collect(),
                deny: Vec::new(),
                proxy: Vec::new(),
                audit: false,
            },
            None,
        )
    }

    #[test]
    fn approval_for_input_prompt_backend_requires_gate() {
        let mut ctx = ctx();
        ctx.network_policy = Some(prompt_decider(&[]));
        let req = WebSearchTool.approval_requirement_for_input(&json!({"query": "rust"}), &ctx);
        assert_eq!(req, ApprovalRequirement::Required);
    }

    #[test]
    fn approval_for_input_allowed_backend_is_auto() {
        let ctx =
            service_ctx(MockWebService::ok().with_search_hosts(vec!["api.tavily.com".to_string()]))
                .with_network_policy(prompt_decider(&["api.tavily.com"]));
        let req = WebSearchTool.approval_requirement_for_input(&json!({"query": "rust"}), &ctx);
        assert_eq!(req, ApprovalRequirement::Auto);
    }

    #[test]
    fn approval_for_input_without_decider_is_auto() {
        let req = WebSearchTool.approval_requirement_for_input(&json!({"query": "rust"}), &ctx());
        assert_eq!(req, ApprovalRequirement::Auto);
    }

    #[test]
    fn approval_gate_uses_the_service_host_list() {
        // The gate consults the attached service's provider hosts (not a
        // hardcoded engine list): a mock reporting an allowed host keeps
        // the gate Auto under a Prompt-default policy.
        let ctx =
            service_ctx(MockWebService::ok().with_search_hosts(vec!["api.tavily.com".to_string()]))
                .with_network_policy(prompt_decider(&["api.tavily.com"]));
        let req = WebSearchTool.approval_requirement_for_input(&json!({"query": "rust"}), &ctx);
        assert_eq!(req, ApprovalRequirement::Auto);
    }
}
