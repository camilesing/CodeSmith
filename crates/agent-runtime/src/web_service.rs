//! Unified web retrieval service: one `search` + one `fetch` contract for
//! the web-facing tools (`web_search`, `fetch_url`, `web.run`) — TODO-49.
//!
//! This module is the definition half of the seam: the trait, the
//! request/outcome value types, and the error taxonomy live here in the
//! framework; the reqwest-backed provider (`HttpWebService` in
//! `codesmith-tool-impls::web_service`) lives in the implementation group
//! and is attached to `ToolContext` / `EngineConfig` by the host.
//! Predecessors replaced by this seam: the three per-tool HTTP stacks —
//! `crates/cli/src/tools/web_search.rs` (search providers + DuckDuckGo/Bing
//! scrape), `crates/tool-impls/src/tools/fetch_url.rs` and
//! `crates/tool-impls/src/tools/web_run.rs` (two private fetch loops, two
//! copies of the scrape parsers) — which each built their own client and
//! mapped their own errors.
//!
//! # What the seam centralizes
//!
//! - **Timeouts** — every request carries a `Duration`; the provider builds
//!   its client from it (Volcengine keeps its documented 90 s floor).
//! - **Cancellation** — requests carry an optional `CancellationToken`;
//!   the provider races the transport against it and returns
//!   [`WebError::Cancelled`] instead of finishing the call.
//! - **Error classification** — [`WebErrorKind`] is the single taxonomy, and
//!   the `From<WebError> for ToolError` impl below is the only place a web
//!   failure becomes a tool error.
//! - **Network policy** — requests carry the caller's
//!   [`NetworkPolicyDecider`]; the provider evaluates it per host (per
//!   redirect hop for fetch) under the request's audit label.
//!
//! # Known limitations
//!
//! - `web.run`'s `image_query` is not part of this service — it stays local
//!   to that tool as a DuckDuckGo-images-specific scrape.
//! - Fetch is anonymous HTTP by design; there is no authenticated fetch
//!   provider and no config key for one until a second backend exists.
//! - Fetch outcomes return non-2xx statuses as `Ok` (callers such as
//!   `fetch_url` want the error body); only search maps HTTP failures to
//!   [`WebError::Status`].

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::network_policy::NetworkPolicyDecider;

/// Default request timeout shared by the web tools (15 s, matching the
/// pre-seam tool defaults).
pub const DEFAULT_WEB_TIMEOUT: Duration = Duration::from_millis(15_000);

/// One search result row, provider-neutral.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

/// Result of a successful [`WebService::search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOutcome {
    /// Ranked results, already truncated to the request's `max_results`.
    pub results: Vec<SearchHit>,
    /// Provider label for the response (`"duckduckgo"`, `"bing"`,
    /// `"tavily"`, …) — names which backend actually answered, including
    /// when a fallback fired.
    pub source: String,
    /// Non-fatal notes (fallback used, fallback skipped by policy, …) the
    /// caller should surface in its result message.
    pub warnings: Vec<String>,
}

/// Result of a successful [`WebService::fetch`]. Non-2xx statuses are
/// delivered here (with the body) rather than as errors — see the module's
/// known limitations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    /// Final URL after any followed redirects.
    pub url: String,
    pub status: u16,
    /// Response headers, lowercased names.
    pub headers: BTreeMap<String, String>,
    pub content_type: String,
    pub body: Vec<u8>,
    /// `true` when the body exceeded the request's `max_bytes` and was cut.
    pub truncated: bool,
}

/// Classification of a [`WebError`]. One taxonomy for every web failure the
/// tools can hit; mapping into `ToolError` happens exactly once (the `From`
/// impl below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebErrorKind {
    /// Host blocked by the network policy (hard deny).
    PolicyDenied,
    /// URL failed shape/scheme validation.
    InvalidUrl,
    /// Transport-level timeout.
    Timeout,
    /// Cancelled through the request's cancellation token.
    Cancelled,
    /// DNS / connect / TLS / transport failure.
    Transport,
    /// Upstream answered a non-success status the caller must see as an
    /// error (search paths; fetch delivers statuses in the outcome).
    Status(u16),
    /// Response body could not be read or parsed.
    Body,
    /// Provider configuration is missing or invalid (e.g. missing API key).
    Config,
}

/// A classified web-service failure. `message` is the full user/model-facing
/// text (including the remedy where one exists); `kind` drives the
/// `ToolError` mapping.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WebError {
    pub kind: WebErrorKind,
    pub message: String,
}

impl WebError {
    #[must_use]
    pub fn new(kind: WebErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn kind(&self) -> WebErrorKind {
        self.kind
    }
}

/// The single mapping from web failures to tool errors: policy denials stay
/// permission errors, URL problems stay input errors, everything else is an
/// execution failure carrying the provider's full message.
impl From<WebError> for codesmith_tools::ToolError {
    fn from(err: WebError) -> Self {
        match err.kind {
            WebErrorKind::PolicyDenied => Self::permission_denied(err.message),
            WebErrorKind::InvalidUrl => Self::invalid_input(err.message),
            // Timeouts stay on the execution-failure path: the provider's
            // message already names the timeout, and ToolError::Timeout would
            // re-render it as "timed out after Ns" without the transport detail.
            _ => Self::execution_failed(err.message),
        }
    }
}

/// A web search request. Built through the `with_*` chain; `new` supplies
/// the shared defaults (5 results, 15 s timeout).
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub max_results: usize,
    pub timeout: Duration,
    pub cancel: Option<CancellationToken>,
    pub policy: Option<NetworkPolicyDecider>,
    /// Audit-label under which policy decisions for this search are logged
    /// (the calling tool's name).
    pub tool: &'static str,
}

impl SearchRequest {
    #[must_use]
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            max_results: 5,
            timeout: DEFAULT_WEB_TIMEOUT,
            cancel: None,
            policy: None,
            tool: "web_search",
        }
    }

    #[must_use]
    pub fn with_max_results(mut self, max_results: usize) -> Self {
        self.max_results = max_results;
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_cancel(mut self, cancel: Option<CancellationToken>) -> Self {
        self.cancel = cancel;
        self
    }

    #[must_use]
    pub fn with_policy(mut self, policy: Option<NetworkPolicyDecider>) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub fn for_tool(mut self, tool: &'static str) -> Self {
        self.tool = tool;
        self
    }
}

/// An anonymous HTTP GET request. `new` supplies the shared defaults
/// (15 s timeout, no byte cap, no redirects followed beyond what the
/// provider policy loop allows).
#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub url: String,
    pub timeout: Duration,
    pub max_bytes: u64,
    pub max_redirects: usize,
    pub cancel: Option<CancellationToken>,
    pub policy: Option<NetworkPolicyDecider>,
    /// Audit-label under which policy decisions for this fetch are logged
    /// (the calling tool's name).
    pub tool: &'static str,
}

impl FetchRequest {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            timeout: DEFAULT_WEB_TIMEOUT,
            max_bytes: u64::MAX,
            max_redirects: 0,
            cancel: None,
            policy: None,
            tool: "fetch_url",
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    #[must_use]
    pub fn with_max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    #[must_use]
    pub fn with_cancel(mut self, cancel: Option<CancellationToken>) -> Self {
        self.cancel = cancel;
        self
    }

    #[must_use]
    pub fn with_policy(mut self, policy: Option<NetworkPolicyDecider>) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub fn for_tool(mut self, tool: &'static str) -> Self {
        self.tool = tool;
        self
    }
}

/// Unified web retrieval contract for the web-facing tools (TODO-49).
///
/// The concrete provider is host-constructed from config —
/// `[search] provider` selects the search backend (switching is
/// config-only), fetch is anonymous HTTP — and attached to the
/// `ToolContext`. Tools fail closed with a clear error when no service is
/// attached; there is no silent fallback transport.
#[async_trait]
pub trait WebService: Send + Sync {
    /// Run a web search through the configured provider. A hard policy deny
    /// on the provider's primary host is a [`WebError`] with kind
    /// [`WebErrorKind::PolicyDenied`]; a denied *fallback* host degrades to
    /// a warning in the outcome rather than failing the search.
    async fn search(&self, req: SearchRequest) -> Result<SearchOutcome, WebError>;

    /// Fetch one URL (anonymous HTTP GET). Redirect hops up to the
    /// request's limit are followed, each re-checked against the request
    /// policy; the outcome carries the final URL and status.
    async fn fetch(&self, req: FetchRequest) -> Result<FetchOutcome, WebError>;

    /// Hosts the configured search provider will contact (primary + known
    /// fallbacks). Consumed by the tools' per-input approval gates to
    /// decide whether a search needs the inline network approval prompt.
    #[must_use]
    fn search_hosts(&self) -> Vec<String>;
}

/// Test double for [`WebService`] — the seam's mocking strategy mirrors
/// `test_support::mock_llm`: tests mock at the trait boundary, never at the
/// reqwest layer. Compiled behind the `test-support` feature (auto-on for
/// in-crate `cfg(test)`; downstream crates enable it on their dev-dependency)
/// so both in-crate unit tests and cross-crate tool tests can script the
/// service.
#[cfg(any(test, feature = "test-support"))]
pub mod mock {
    use super::*;

    /// Scripted [`WebService`]: every `search`/`fetch` call returns the
    /// configured outcome (cloned) or error, and `search_hosts` reports the
    /// configured list. Construct one per scenario; it records no history.
    #[derive(Debug, Clone)]
    pub struct MockWebService {
        search_result: Result<SearchOutcome, WebError>,
        fetch_result: Result<FetchOutcome, WebError>,
        hosts: Vec<String>,
    }

    impl MockWebService {
        /// Happy-path default: one search hit from `mock`, an empty 200
        /// text/plain page.
        #[must_use]
        pub fn ok() -> Self {
            Self {
                search_result: Ok(SearchOutcome {
                    results: vec![SearchHit {
                        title: "mock result".to_string(),
                        url: "https://example.com/mock".to_string(),
                        snippet: Some("mock snippet".to_string()),
                    }],
                    source: "mock".to_string(),
                    warnings: Vec::new(),
                }),
                fetch_result: Ok(FetchOutcome {
                    url: "https://example.com/mock".to_string(),
                    status: 200,
                    headers: BTreeMap::new(),
                    content_type: "text/plain".to_string(),
                    body: b"mock page body".to_vec(),
                    truncated: false,
                }),
                hosts: vec!["example.com".to_string()],
            }
        }

        /// Script the `search` result (or error) for every call.
        #[must_use]
        pub fn with_search(mut self, result: Result<SearchOutcome, WebError>) -> Self {
            self.search_result = result;
            self
        }

        /// Script the `fetch` result (or error) for every call.
        #[must_use]
        pub fn with_fetch(mut self, result: Result<FetchOutcome, WebError>) -> Self {
            self.fetch_result = result;
            self
        }

        /// Script the `search_hosts` answer used by approval gates.
        #[must_use]
        pub fn with_search_hosts(mut self, hosts: Vec<String>) -> Self {
            self.hosts = hosts;
            self
        }
    }

    #[async_trait]
    impl WebService for MockWebService {
        async fn search(&self, _req: SearchRequest) -> Result<SearchOutcome, WebError> {
            self.search_result.clone()
        }

        async fn fetch(&self, _req: FetchRequest) -> Result<FetchOutcome, WebError> {
            self.fetch_result.clone()
        }

        fn search_hosts(&self) -> Vec<String> {
            self.hosts.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_tools::ToolError;

    #[test]
    fn web_error_maps_policy_denied_to_permission_error() {
        let err = ToolError::from(WebError::new(
            WebErrorKind::PolicyDenied,
            "network call to 'evil.example' blocked by network policy",
        ));
        assert!(format!("{err}").contains("authorize"));
    }

    #[test]
    fn web_error_maps_invalid_url_to_input_error() {
        let err = ToolError::from(WebError::new(
            WebErrorKind::InvalidUrl,
            "only http:// and https:// URLs are supported",
        ));
        assert!(format!("{err}").contains("validate input"));
    }

    #[test]
    fn web_error_maps_transport_to_execution_failure_verbatim() {
        let err = ToolError::from(WebError::new(
            WebErrorKind::Transport,
            "Web search request failed: dns stub",
        ));
        assert!(format!("{err}").contains("dns stub"));
    }

    #[test]
    fn search_request_defaults_match_tool_defaults() {
        let req = SearchRequest::new("rust");
        assert_eq!(req.max_results, 5);
        assert_eq!(req.timeout, DEFAULT_WEB_TIMEOUT);
        assert_eq!(req.tool, "web_search");
    }

    #[test]
    fn fetch_request_defaults_are_unbounded() {
        let req = FetchRequest::new("https://example.com");
        assert_eq!(req.max_bytes, u64::MAX);
        assert_eq!(req.max_redirects, 0);
    }
}
